/**
 * Test model of the runner's webview flow control: `EmissionGate` plus
 * `TerminalSession::{ack, reset_flow_control}` in
 * `src-tauri/src/terminal/session.rs`, and the reader pipeline around them (the
 * ring, the grid, the handler). The counters and hysteresis are mirrored
 * exactly. Shared by `ackedWriteQueue.test.ts`, `paneOutputStream.test.ts` and
 * `ringResync.test.ts`.
 *
 * A `.testkit` module: test-only, so the UI Bridge action-surface scan
 * (`src/lib/ui-bridge/actionSurfaces.ts`) does not count its `handler(…)`
 * calls, which deliver chunks to a fake pane and are no registry handler; and
 * `actionSurfaces.enforcement.test.ts` asserts no app module imports it.
 */

import { RenderAckAccumulator } from "../flowControl";
import { AckedWriteQueue, type RenderedWriteSink } from "../ackedWriteQueue";
import type { OffsetChunk } from "../scrollbackReplay";

/** `FLOW_HIGH_WATERMARK` / `FLOW_LOW_WATERMARK` in `session.rs`. */
export const FLOW_HIGH_WATERMARK = 100_000;
export const FLOW_LOW_WATERMARK = 5_000;

export class RunnerGate {
  sent = 0;
  acked = 0;
  paused = false;
  emissionSkipped = false;
  resumeMarkers = 0;

  get gap(): number {
    return Math.max(0, this.sent - this.acked);
  }

  /**
   * A `focused`-tier send under the gate: `EmissionGate::should_emit`, then
   * `bytes_sent += len` (`session.rs`, the `WebviewAdmission::Now` arm).
   */
  tryEmit(len: number): boolean {
    if (this.paused) {
      if (this.gap <= FLOW_LOW_WATERMARK) this.paused = false;
    } else if (this.gap > FLOW_HIGH_WATERMARK) {
      this.paused = true;
    }
    if (this.paused) {
      this.emissionSkipped = true;
      return false;
    }
    this.sent += len;
    return true;
  }

  // A send the runner does NOT count (a background-tier hold window, or the
  // window drained ahead of a focused send) is not gated and adds nothing to
  // `sent`, so the model has no method for it: such a send leaves it unchanged.

  /** `TerminalSession::ack`: clamped, so acked never passes sent. */
  ack(bytes: number): void {
    this.acked = Math.max(this.acked, Math.min(this.acked + bytes, this.sent));
    if (this.gap <= FLOW_LOW_WATERMARK && this.emissionSkipped) {
      this.emissionSkipped = false;
      this.resumeMarkers += 1;
    }
  }

  /**
   * `reset_flow_control`: what `terminal_get_scrollback`, `terminal_flow_reset`
   * and the reconnect snapshot (`terminal_get_grid` with `resetFlowControl`) do.
   */
  reset(): void {
    this.resetAround(() => {});
  }

  /**
   * `reset_flow_control_around`: read `sent`, run `between` (what can
   * interleave in the runner), then raise acked to what was read. `"fetch_max"`
   * is the runner's algorithm, and re-clamps to the current sent afterwards;
   * `"store"` is the earlier one, which overwrites acked, so tests can show
   * what it got wrong.
   */
  resetAround(between: () => void, mode: "fetch_max" | "store" = "fetch_max"): void {
    const sent = this.sent;
    between();
    if (mode === "store") {
      this.acked = sent;
    } else {
      this.acked = Math.min(Math.max(this.acked, sent), this.sent);
    }
    this.emissionSkipped = false;
  }

  /** `uncount`: take back `len` counted bytes whose emit failed. */
  uncount(len: number): void {
    this.sent -= len;
    this.acked = Math.min(this.acked, this.sent);
  }

  // ---- The reader pipeline: offsets, the ring, the grid, the handler ----
  //
  // The reader tees each read into the scrollback ring and absorbs it into the
  // grid at once, and counts and emits it LATER: right after, or once a held
  // DEC-2026 frame closes. So a byte can be inside a ring snapshot yet counted
  // after the reset that came with it. These methods model exactly that.

  /** Stream bytes produced so far: the ring's end, and what the grid absorbed. */
  produced = 0;
  /** Produced but not yet counted or emitted (a read in flight, a held frame). */
  private unemitted: OffsetChunk[] = [];
  /** The pane's `terminal-output` handler, when one is registered. */
  private handler: ((c: OffsetChunk) => void) | null = null;
  /**
   * Whether the runner-side listener exists. Tauri's `listen()` resolves after
   * the JS handler is registered: until then an emit reaches nobody.
   */
  listening = true;
  /** Emitted chunks not yet delivered, when delivery is deferred. */
  private inbox: OffsetChunk[] = [];
  /** Queue deliveries for {@link deliver} instead of making them at once. */
  deferDelivery = false;
  /**
   * Where the grid's boundary tracker stands, when it lags the ring (a read
   * boundary left a sequence open). `null` means "at the ring's end".
   */
  gridBoundary: number | null = null;

  /**
   * Register (or with `null`, drop) the pane's output handler. With
   * `{ live: false }` the runner-side listener is not up yet: call
   * {@link goLive} when its `listen()` resolves.
   */
  attach(handler: ((c: OffsetChunk) => void) | null, opts: { live?: boolean } = {}): void {
    this.handler = handler;
    this.listening = opts.live ?? true;
  }

  /** The runner-side listener came up (`listen()` resolved). */
  goLive(): void {
    this.listening = true;
  }

  /** Deliver the emitted chunks that {@link deferDelivery} queued. */
  deliver(): void {
    const inbox = this.inbox;
    this.inbox = [];
    for (const c of inbox) this.handler?.(c);
  }

  /** The PTY produced `len` bytes: into the ring and the grid now, emitted by {@link flush}. */
  produce(len: number): void {
    this.unemitted.push(chunk(this.produced, len));
    this.produced += len;
  }

  /**
   * The reader emits what it holds, oldest first: each chunk through the gate
   * (counted, then emitted), delivered to the handler if one is registered and
   * lost otherwise. A chunk the gate skips is lost too (the gate flags it).
   */
  flush(): void {
    const pending = this.unemitted;
    this.unemitted = [];
    for (const c of pending) {
      if (!this.tryEmit(c.bytes.length) || !this.handler || !this.listening) continue;
      if (this.deferDelivery) this.inbox.push(c);
      else this.handler(c);
    }
  }

  /** `terminal_get_scrollback`: reset, THEN snapshot the ring. Returns its end. */
  readRing(): number {
    this.reset();
    return this.produced;
  }

  /** `terminal_get_grid`: the snapshot's `stream_offset`, resetting first when asked. */
  snapshotGrid(resetFlowControl: boolean): number {
    if (resetFlowControl) this.reset();
    return this.gridBoundary ?? this.produced;
  }
}

/** An xterm stand-in: writes queue up, and `renderAll` fires their callbacks in order. */
export class FakeSink implements RenderedWriteSink {
  written: number[] = [];
  private pending: Array<() => void> = [];
  write(data: Uint8Array, onRendered: () => void): void {
    this.written.push(data.length);
    this.pending.push(onRendered);
  }
  renderAll(): void {
    const cbs = this.pending;
    this.pending = [];
    for (const cb of cbs) cb();
  }
  /** Render the oldest pending write. Returns false when nothing is pending. */
  renderOne(): boolean {
    const cb = this.pending.shift();
    cb?.();
    return cb !== undefined;
  }
  /** Writes whose render callback has not fired yet. */
  get unrendered(): number {
    return this.pending.length;
  }
}

/** A pane's write half: an accumulator and an acked queue wired to `gate`. */
export function paneWriter(gate: RunnerGate) {
  const acks = new RenderAckAccumulator();
  const queue = new AckedWriteQueue(acks, (n) => {
    if (n > 0) gate.ack(n);
  });
  const sink = new FakeSink();
  return {
    acks,
    queue,
    sink,
    /** Render everything, then let the 250 ms floor timer drain the remainder. */
    settle() {
      queue.flush(sink);
      sink.renderAll();
      const rest = acks.flush();
      if (rest > 0) gate.ack(rest);
    },
  };
}

/**
 * Emit `chunkLen`-byte chunks until the gate pauses, calling `onEmit` with each
 * admitted chunk's start offset (relative to the first). Returns the bytes
 * admitted. Bounded, so a regression that keeps the gate open fails the
 * assertion after it instead of hanging the test run.
 */
export function emitUntilPause(
  gate: RunnerGate,
  chunkLen: number,
  onEmit: (relativeOffset: number) => void = () => {},
): number {
  let admitted = 0;
  for (let i = 0; i < 1_000 && gate.tryEmit(chunkLen); i++) {
    onEmit(admitted);
    admitted += chunkLen;
  }
  return admitted;
}

export const chunk = (offset: number, len: number): OffsetChunk => ({
  bytes: new Uint8Array(len).fill(0x41),
  offset,
});
