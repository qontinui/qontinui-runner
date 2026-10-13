/**
 * Test model of the runner's webview flow control: `EmissionGate` plus
 * `TerminalSession::{ack, reset_flow_control}` in
 * `src-tauri/src/terminal/session.rs`. The counters and hysteresis are mirrored
 * exactly. Shared by `ackedWriteQueue.test.ts` and `paneOutputStream.test.ts`.
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

  /**
   * A send the runner does NOT count. Two cases: a background-tier hold window,
   * and the window drained ahead of a focused send. Only `Now` sends under the
   * gate add to `bytes_sent`. It is not gated either.
   */
  sendUncounted(_len: number): true {
    return true;
  }

  ack(bytes: number): void {
    this.acked += bytes;
    if (this.gap <= FLOW_LOW_WATERMARK && this.emissionSkipped) {
      this.emissionSkipped = false;
      this.resumeMarkers += 1;
    }
  }

  /** `reset_flow_control`: what `terminal_get_scrollback` and `terminal_flow_reset` do. */
  reset(): void {
    this.acked = this.sent;
    this.emissionSkipped = false;
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

export const chunk = (offset: number, len: number): OffsetChunk => ({
  bytes: new Uint8Array(len).fill(0x41),
  offset,
});
