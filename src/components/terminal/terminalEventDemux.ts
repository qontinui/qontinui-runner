/**
 * One `terminal-output` / `terminal-exit` listener per WINDOW, demuxed to
 * per-terminal handlers (plan `2026-07-28-runner-many-sessions-performance`
 * Phase 2, root cause A3).
 *
 * Before: every mounted `TerminalInstance` called `listen("terminal-output")`
 * itself and filtered by id *after* delivery, and every `PageSessionScope`
 * added another global tap. Rust emits app-wide (`AppHandle::emit` reaches all
 * webviews), so one busy pane cost ~(M mounted + P pages) × W windows IPC
 * deserializations + JS callbacks per chunk — the id filter runs far too late
 * to save any of it.
 *
 * After: exactly ONE `listen()` per event name per window. The demux routes a
 * payload to the handlers registered for `payload.terminalId` and nobody else
 * is woken. A pane registers a HANDLER, not a listener.
 *
 * Built on `acquireSingletonListener`, which now holds a handler *set* rather
 * than a single slot — so the per-terminal demux and the page-wide output tap
 * (`subscribeTerminalOutputStream`) coexist behind that same single `listen()`
 * instead of overwriting each other. One mechanism, not two.
 */

import type { Event } from "@tauri-apps/api/event";
import type { TerminalOutputEvent, TerminalExitEvent } from "@qontinui/shared-types/tauri-events";
import { acquireSingletonListener } from "@/hooks/ui-bridge-events/singleton-listener";

/**
 * The runner's reader thread stamps each `terminal-output` event with the
 * chunk's absolute byte offset in the session's output stream (a runner-local
 * extension — the shared `TerminalOutputEvent` schema is deny_unknown_fields,
 * so the field rides outside it). Used to dedup the scrollback-ring replay
 * against live chunks; `undefined` (older runner build) degrades to the
 * pre-replay write-everything behavior.
 */
export type TerminalOutputPayload = TerminalOutputEvent & { offset?: number };

type PayloadHandler<P> = (payload: P) => void;

/**
 * One registration's handler. Boxed for the same reason
 * `singleton-listener.ts` boxes its own: two registrations passing the
 * identical function reference must count — and unregister — separately.
 */
interface HandlerBox<P> {
  fn: PayloadHandler<P>;
}

/** Demultiplexer for one Tauri event name whose payload carries a terminalId. */
class TerminalEventDemux<P extends { terminalId: string }> {
  private readonly handlers = new Map<string, Set<HandlerBox<P>>>();
  /** Release fn for the shared listener; non-null exactly while any handler is registered. */
  private release: (() => void) | null = null;

  constructor(private readonly eventName: string) {}

  register(terminalId: string, handler: PayloadHandler<P>): () => void {
    let set = this.handlers.get(terminalId);
    if (!set) {
      set = new Set();
      this.handlers.set(terminalId, set);
    }
    const box: HandlerBox<P> = { fn: handler };
    set.add(box);
    // First handler in this window installs the one real listener.
    if (!this.release) {
      this.release = acquireSingletonListener<P>(this.eventName, (event: Event<P>) =>
        this.dispatch(event.payload),
      );
    }

    let unregistered = false;
    return () => {
      if (unregistered) return;
      unregistered = true;
      const live = this.handlers.get(terminalId);
      if (live) {
        live.delete(box);
        if (live.size === 0) this.handlers.delete(terminalId);
      }
      if (this.handlers.size === 0 && this.release) {
        const release = this.release;
        this.release = null;
        release();
      }
    };
  }

  /** Route one payload to its terminal's handlers — nobody else is woken. */
  private dispatch(payload: P): void {
    const set = this.handlers.get(payload.terminalId);
    if (!set) return;
    // Direct Set iteration (no array copy) on the per-chunk hot path; JS Set
    // iteration tolerates a handler unregistering itself mid-dispatch.
    for (const box of set) {
      try {
        box.fn(payload);
      } catch (e) {
        console.error(`[terminalEventDemux] ${this.eventName} handler threw:`, e);
      }
    }
  }

  /** Test/diagnostic visibility. */
  stats(): { terminals: number; handlers: number; listening: boolean } {
    let handlers = 0;
    for (const set of this.handlers.values()) handlers += set.size;
    return { terminals: this.handlers.size, handlers, listening: this.release !== null };
  }

  /** Test-only hard reset (drops handlers AND the shared listener). */
  resetForTest(): void {
    this.handlers.clear();
    const release = this.release;
    this.release = null;
    release?.();
  }
}

const outputDemux = new TerminalEventDemux<TerminalOutputPayload>("terminal-output");
const exitDemux = new TerminalEventDemux<TerminalExitEvent>("terminal-exit");

/**
 * Receive `terminal-output` payloads for ONE terminal. Returns an unregister
 * function. Registration is synchronous — unlike a bare `listen()`, there is
 * no async attach window in which bytes can be missed.
 */
export function registerTerminalOutputHandler(
  terminalId: string,
  handler: (payload: TerminalOutputPayload) => void,
): () => void {
  return outputDemux.register(terminalId, handler);
}

/** Receive `terminal-exit` payloads for ONE terminal. */
export function registerTerminalExitHandler(
  terminalId: string,
  handler: (payload: TerminalExitEvent) => void,
): () => void {
  return exitDemux.register(terminalId, handler);
}

/**
 * Subscribe to the whole `terminal-output` stream (every terminal in this
 * window) — the page-level state-tracking tap, which is deliberately
 * mount-independent and applies its own tab-roster filter.
 *
 * Shares the demux's single `listen()`; it does not add a second one.
 */
export function subscribeTerminalOutputStream(
  handler: (payload: TerminalOutputEvent) => void,
): () => void {
  return acquireSingletonListener<TerminalOutputEvent>(
    "terminal-output",
    (event: Event<TerminalOutputEvent>) => handler(event.payload),
  );
}

/**
 * The runner's ≤1 Hz activity digest for a terminal it has stopped emitting
 * output for (visibility tier `unwatched`; plan Phase 5 / A4).
 *
 * `bytesDelta` is what the PTY produced since the previous digest for this
 * terminal; `lines` is the trailing non-empty rows of the runner's own rendered
 * VT grid.
 */
export interface TerminalActivityPayload {
  terminalId: string;
  totalBytesProduced: number;
  bytesDelta: number;
  lines: string[];
}

/**
 * Subscribe to the whole `terminal-activity` stream for this window — the
 * state-tracking feed for tabs no pane is mounted for. One `listen()` per
 * window via the same singleton listener the output demux uses.
 */
export function subscribeTerminalActivityStream(
  handler: (payload: TerminalActivityPayload) => void,
): () => void {
  return acquireSingletonListener<TerminalActivityPayload>(
    "terminal-activity",
    (event: Event<TerminalActivityPayload>) => handler(event.payload),
  );
}

/** Test-only: registration counts per demux. */
export function __terminalDemuxStats(): {
  output: { terminals: number; handlers: number; listening: boolean };
  exit: { terminals: number; handlers: number; listening: boolean };
} {
  return { output: outputDemux.stats(), exit: exitDemux.stats() };
}

/** Test-only: drop every handler and the shared listeners. */
export function __resetTerminalDemuxForTest(): void {
  outputDemux.resetForTest();
  exitDemux.resetForTest();
}
