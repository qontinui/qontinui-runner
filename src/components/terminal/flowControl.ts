/**
 * Render-completion flow control + write coalescing primitives for the
 * in-app terminal, extracted as a leaf module (zero imports) so the
 * load-bearing accounting can be unit-tested without importing
 * `TerminalInstance` — which transitively pulls `@xterm/addon-canvas`
 * (touches `self` at module init and crashes under Node).
 *
 * Mirrors VS Code's `FlowControlConstants` (microsoft/vscode terminal):
 * the frontend acks bytes the renderer has actually drained — not bytes
 * merely received off the wire — and the Rust reader pauses/resumes around
 * char-count watermarks so a burst can't outrun xterm's input buffer
 * (~50MB cap, which silently drops overflow) before backpressure engages.
 */

/**
 * Ack to the Rust reader once this many rendered bytes have accumulated.
 * VS Code uses 5000 chars (`FlowControlConstants.CharCountAckSize`). The
 * Rust side resumes once `sent - acked` drops back under its Low watermark,
 * so acking in ~5000-byte units keeps the pause window short.
 */
export const ACK_SIZE = 5000;

/**
 * Slow timer floor (ms). The render-based ack fires as soon as accumulated
 * rendered bytes cross {@link ACK_SIZE}; this timer is the floor that flushes
 * a trailing sub-AckSize remainder (so the last few bytes of a burst still
 * ack and the reader resumes) and also carries the title poll the periodic
 * idle paint used to host. Matches the prior 250ms ack cadence.
 */
export const ACK_FLOOR_INTERVAL_MS = 250;

/**
 * Render-based ack accumulator. Tracks bytes that have been rendered by the
 * backend (the `onRendered` callback fired) but not yet acked to the Rust
 * reader, and decides when to flush an ack.
 *
 * Lifecycle per coalesced write:
 *   1. `onWriteIssued(n)` when a write is dispatched to the backend — bumps
 *      the in-flight (issued-but-not-yet-rendered) counter.
 *   2. `onRendered(n)` inside the backend's render-completion callback —
 *      moves those `n` bytes from in-flight into the pending-ack accumulator.
 *   3. `takeAck()` returns the bytes to ack (and zeroes the accumulator) once
 *      it has crossed {@link ACK_SIZE}, or when forced by the floor timer.
 */
export class RenderAckAccumulator {
  /** Bytes written to the backend whose render callback hasn't fired yet. */
  private inFlight = 0;
  /** Rendered-but-not-yet-acked bytes. */
  private pendingAck = 0;

  constructor(private readonly ackSize: number = ACK_SIZE) {}

  /** A write of `bytes` length was dispatched to the backend. */
  onWriteIssued(bytes: number): void {
    if (bytes > 0) this.inFlight += bytes;
  }

  /**
   * The backend finished rendering `bytes` (its `onRendered` callback). Moves
   * them out of in-flight and into the pending-ack accumulator. Returns the
   * ack amount to send now (>0) when the accumulator has crossed AckSize,
   * else 0. `bytes` is clamped to the outstanding in-flight count so a
   * double-fired callback can't over-ack.
   */
  onRendered(bytes: number): number {
    const settled = Math.min(bytes, this.inFlight);
    this.inFlight -= settled;
    this.pendingAck += settled;
    if (this.pendingAck >= this.ackSize) {
      return this.flush();
    }
    return 0;
  }

  /**
   * Flush the floor: return any pending-ack bytes (sub-AckSize trailing
   * remainder) and zero the accumulator. Returns 0 when nothing is pending.
   */
  flush(): number {
    const ack = this.pendingAck;
    this.pendingAck = 0;
    return ack;
  }

  /** Bytes rendered but not yet acked (test/diagnostic visibility). */
  get pending(): number {
    return this.pendingAck;
  }

  /** Bytes written but not yet rendered (test/diagnostic visibility). */
  get outstanding(): number {
    return this.inFlight;
  }
}
