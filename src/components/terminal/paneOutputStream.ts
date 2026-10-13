/**
 * One pane's view of its terminal's output stream: where each live chunk goes,
 * and the offsets that make a ring replay and the live stream line up.
 *
 * This logic was inlined in `TerminalInstance`'s mount effect. It is a leaf
 * module (no xterm / Tauri imports) because the component cannot be imported
 * under the runner's `environment: "node"`. That left the routing untestable,
 * and that is how two defects shipped:
 *
 *  - Received bytes were never acked unless they were written. That is fixed in
 *    `AckedWriteQueue`, which this class stages every chunk through.
 *  - The reconnect path attached its handler late. Bytes the runner sent between
 *    the mount's ring read (a flow-control reset) and the attach reached no
 *    handler, and `nextExpectedOffset` was still null, so nothing noticed the
 *    hole. See {@link PaneOutputStream.applyMountRing} and
 *    {@link PaneOutputStream.attachedLate}.
 *
 * The component keeps the side effects that need the backend: the resync's
 * ring read and write, and the flush. This class owns the bookkeeping those
 * side effects read and advance.
 */

import type { AckedWriteQueue } from "./ackedWriteQueue";
import { routeOutputChunk, type OutputRoute } from "./paneVisibilityService";
import { drainHeldChunks, isEmissionGap, type OffsetChunk } from "./scrollbackReplay";

/** A `terminal-output` payload's wire shape (base64 data + absolute offset). */
export interface EncodedOutputPayload {
  data: string;
  offset?: number;
}

/** Decode one `terminal-output` payload to its bytes and offset. */
export function decodeOutputPayload(payload: EncodedOutputPayload): OffsetChunk {
  const raw = atob(payload.data);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) {
    bytes[i] = raw.charCodeAt(i);
  }
  return { bytes, offset: payload.offset };
}

/** What the stream needs from its pane, injected so tests can drive it. */
export interface PaneOutputHooks {
  /** The pane is hidden (its visibility tier is quiet). */
  isPaused(): boolean;
  /** The backend exists (a held or written chunk needs one). */
  hasBackend(): boolean;
  /** A chunk was dropped while hidden; `endOffset` is its exclusive end. */
  noteMissedOutput(endOffset: number | undefined): void;
  /** Start (or queue) a ring resync: a hole precedes the held chunks. */
  requestResync(): void;
  /** Flush the write queue on the next microtask. */
  scheduleFlush(): void;
  /** `terminal_flow_reset`: set the runner's acked count to its sent count. */
  resetFlowControl(): void;
}

/** {@link PaneOutputStream.receive}'s result: the route, or `discard` when there is no backend. */
export type ReceiveRoute = OutputRoute | "discard";

export class PaneOutputStream {
  /** Chunks received before the backend was ready, drained after the mount ring replay. */
  private pending: OffsetChunk[] = [];
  /** Post-gap chunks held until a resync's ring slice covers the hole. */
  private held: OffsetChunk[] = [];
  /** The backend exists and the mount drain has run. */
  backendReady = false;
  /** A resync's ring read is in flight. The component sets this. */
  resyncInFlight = false;
  /** Exclusive end of the newest ring replay written into the pane. */
  replayedThrough = 0;
  /** Exclusive end of the bytes the pane has (ring replays plus live chunks). */
  writtenThrough = 0;
  /** Exclusive end of the last chunk observed. `null` means no baseline yet, so no gap can be seen. */
  nextExpectedOffset: number | null = null;

  constructor(
    private readonly queue: AckedWriteQueue,
    private readonly hooks: PaneOutputHooks,
  ) {}

  /** Route one received chunk, and do the bookkeeping that route needs. */
  receive(chunk: OffsetChunk): ReceiveRoute {
    const { bytes, offset } = chunk;
    // Emission-gap detection: the runner gates webview emission (never the PTY
    // read) under backpressure, so a dropped chunk shows up as the next one
    // starting past the end of the last. Pre-ready holes are covered wholesale
    // by the mount ring replay, so only a ready pane looks.
    const gapDetected = this.backendReady && isEmissionGap(this.nextExpectedOffset, offset);
    // Advance in EVERY tier, hidden included: it is what stops the first chunk
    // after a reveal from reading as a fresh gap.
    if (offset !== undefined) {
      this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, offset + bytes.length);
    }

    const route = routeOutputChunk({
      paused: this.hooks.isPaused(),
      backendReady: this.backendReady,
      gapDetected,
      resyncInFlight: this.resyncInFlight,
    });

    if (route === "drop") {
      // Hidden: no parse, no write, no ack (the runner does not count a
      // background-tier send). `writtenThrough` stays put, so the reveal resync
      // replays exactly this window from the ring.
      this.hooks.noteMissedOutput(offset === undefined ? undefined : offset + bytes.length);
      return route;
    }
    if (route === "buffer") {
      this.pending.push(chunk);
      return route;
    }
    if (!this.hooks.hasBackend()) return "discard";
    if (route === "hold") {
      this.held.push(chunk);
      if (gapDetected) this.hooks.requestResync();
      return route;
    }
    // A chunk emitted before a ring snapshot can still be DELIVERED after the
    // replay (event delivery and invoke responses are not mutually ordered).
    // The queue trims it to its unreplayed suffix and acks all of it.
    this.queue.stageChunk(chunk, this.replayedThrough);
    if (!this.queue.isEmpty) this.hooks.scheduleFlush();
    this.advanceWritten(chunk);
    return route;
  }

  /**
   * The mount's ring replay landed through `endOffset` (`null`: no ring, or an
   * empty one).
   *
   * Seeding `nextExpectedOffset` is what makes a late attach safe. The ring
   * read reset the runner's counters. Anything sent after it that reached no
   * handler leaves the first live chunk starting past `endOffset`. That reads
   * as a gap, so the pane resyncs from the ring instead of splicing over a
   * hole it never saw.
   */
  applyMountRing(endOffset: number | null): void {
    if (endOffset === null) return;
    this.replayedThrough = endOffset;
    this.writtenThrough = Math.max(this.writtenThrough, endOffset);
    this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, endOffset);
  }

  /**
   * The backend is ready: stage every chunk buffered during init (trimmed
   * against the mount ring, acked in full) and start routing live chunks to
   * the write path. Returns the bytes drained. The caller flushes
   * synchronously, so they land right behind the ring replay.
   */
  drainPending(): number {
    let drained = 0;
    for (const c of this.pending) {
      this.queue.stageChunk(c, this.replayedThrough);
      this.advanceWritten(c);
      drained += c.bytes.length;
    }
    this.pending = [];
    this.backendReady = true;
    return drained;
  }

  /**
   * The output handler was registered AFTER the mount ring read: the reconnect
   * path defers it past the bootstrap paint. Bytes the runner sent in between
   * reached no handler, so nothing will ever ack them. If they pushed the gap
   * past the High watermark, the gate is already paused and no chunk will ever
   * arrive to reveal the hole. Reset the counters here. The seeded
   * `nextExpectedOffset` (see {@link applyMountRing}) then turns the first chunk
   * after the reset into a gap, and the resync replays the missed window.
   */
  attachedLate(): void {
    this.hooks.resetFlowControl();
  }

  /** A resync's ring window through `endOffset` was written into the pane. */
  noteRingReplayed(endOffset: number): void {
    this.replayedThrough = Math.max(this.replayedThrough, endOffset);
    this.writtenThrough = Math.max(this.writtenThrough, endOffset);
    this.nextExpectedOffset = Math.max(this.nextExpectedOffset ?? 0, endOffset);
  }

  /**
   * Drain held chunks up to the first remaining hole, staging them (acked in
   * full). Returns whether nothing is left held.
   */
  drainHeld(): boolean {
    const drained = drainHeldChunks(this.held, this.writtenThrough, this.replayedThrough);
    this.held = drained.reheld;
    this.writtenThrough = drained.writtenThrough;
    // A held sync frame can sit wholly inside the snapshot, so ack by what was
    // consumed, not by what was written.
    this.queue.stage(drained.writable, drained.receivedBytes);
    if (!this.queue.isEmpty) this.hooks.scheduleFlush();
    return drained.reheld.length === 0;
  }

  /**
   * The resync gave up or finished: stage whatever is still held even though a
   * hole may precede it (spliced beats frozen), acked in full.
   */
  releaseHeld(): void {
    const rest = this.held;
    this.held = [];
    for (const c of rest) {
      this.queue.stageChunk(c, this.replayedThrough);
      this.advanceWritten(c);
    }
    if (!this.queue.isEmpty) this.hooks.scheduleFlush();
  }

  /**
   * The pane was re-rendered from a fresh ring ending at `endOffset` (remote
   * earlier-output). Set, not max'd: a live chunk that landed between the ring
   * snapshot and the reset was wiped with the old buffer, and anchoring below
   * it lets the ordinary gap detection pull it back.
   */
  reanchor(endOffset: number): void {
    this.replayedThrough = endOffset;
    this.writtenThrough = endOffset;
    this.nextExpectedOffset = endOffset;
  }

  private advanceWritten(c: OffsetChunk): void {
    if (c.offset !== undefined) {
      this.writtenThrough = Math.max(this.writtenThrough, c.offset + c.bytes.length);
    }
  }
}
