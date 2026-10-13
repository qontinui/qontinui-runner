/**
 * The termhost-bench freeze (CI run 38351621760, qontinui-runner PR #2181): a
 * visible pane stopped repainting while its PTY kept flowing. It froze at the
 * first large repaint burst, and froze again after `restore-zone` remounted it.
 *
 * Mechanism: the runner's emission gate (`EmissionGate` in
 * `src-tauri/src/terminal/session.rs`) pauses once `sent − acked` exceeds
 * 100 000 and reopens only at or below 5 000. The pane never acked bytes that
 * were sent to it but not WRITTEN: chunks trimmed because a ring replay already
 * covered them, and chunks buffered before the backend was ready. That leaked
 * remainder survived until the next burst tripped the gate. After that, the
 * pane acked everything it rendered, the gap settled at the remainder, which
 * was above 5 000, and the runner never sent another byte.
 *
 * `RunnerGate` below mirrors the Rust gate's hysteresis and counters. The
 * scenarios drive `AckedWriteQueue` exactly as `TerminalInstance` does, and
 * assert the property that matters: once the pane has rendered everything it
 * received, the gate can reopen. The last suite pins the wiring. Every
 * live-output path in `TerminalInstance` goes through the queue, because the
 * component cannot be mounted under the runner's `environment: "node"`. See
 * `TerminalInstance.test.tsx`.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it, expect } from "vitest";
import { AckedWriteQueue, type RenderedWriteSink } from "./ackedWriteQueue";
import { RenderAckAccumulator } from "./flowControl";
import { drainHeldChunks, type OffsetChunk } from "./scrollbackReplay";

/** `FLOW_HIGH_WATERMARK` / `FLOW_LOW_WATERMARK` in `src-tauri/src/terminal/session.rs`. */
const FLOW_HIGH_WATERMARK = 100_000;
const FLOW_LOW_WATERMARK = 5_000;

/** The runner side: `EmissionGate` + `TerminalSession::{ack, reset_flow_control}`. */
class RunnerGate {
  sent = 0;
  acked = 0;
  paused = false;
  emissionSkipped = false;
  resumeMarkers = 0;

  get gap(): number {
    return Math.max(0, this.sent - this.acked);
  }

  /** `EmissionGate::should_emit` + the reader's `bytes_sent` bookkeeping. */
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

  ack(bytes: number): void {
    this.acked += bytes;
    if (this.gap <= FLOW_LOW_WATERMARK && this.emissionSkipped) {
      this.emissionSkipped = false;
      this.resumeMarkers += 1;
    }
  }

  /** `reset_flow_control` — what `terminal_get_scrollback` does before snapshotting. */
  reset(): void {
    this.acked = this.sent;
    this.emissionSkipped = false;
  }
}

/** An xterm stand-in: writes queue up, `renderAll` fires their callbacks in order. */
class FakeSink implements RenderedWriteSink {
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

function pane(gate: RunnerGate) {
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

const chunk = (offset: number, len: number): OffsetChunk => ({
  bytes: new Uint8Array(len).fill(0x41),
  offset,
});

/**
 * A repaint burst, as the pane sees it: each chunk the gate admits is staged
 * and flushed, but the renderer lags, so nothing renders until the burst ends.
 * That is what drives the gap past the High watermark.
 */
function burst(gate: RunnerGate, p: ReturnType<typeof pane>, from: number, chunks: number) {
  let offset = from;
  for (let i = 0; i < chunks; i++) {
    const c = chunk(offset, 40_000);
    offset += 40_000;
    if (gate.tryEmit(c.bytes.length)) {
      p.queue.stageChunk(c, 0);
      p.queue.flush(p.sink);
    }
  }
  return offset;
}

describe("AckedWriteQueue — a remounted pane can always reopen the emission gate", () => {
  it("acks the bytes the mount drain trims and the bytes it buffered before ready", () => {
    const gate = new RunnerGate();
    gate.sent = 2_000_000; // a long-lived session
    gate.acked = gate.sent;
    const p = pane(gate);

    // The new pane mounts (maximize or restore-zone). Its ring read resets the
    // counters and snapshots the ring through offset 1_000_000.
    gate.reset();
    const replayedThrough = 1_000_000;
    // Emitted AFTER the reset: one coalesced sync frame wholly tee'd into the
    // ring before the snapshot, then a chunk past it. Both arrive before the
    // backend is ready and are buffered as `pendingBytes`.
    const pendingBytes = [chunk(replayedThrough - 9_000, 9_000), chunk(replayedThrough, 6_000)];
    for (const c of pendingBytes) expect(gate.tryEmit(c.bytes.length)).toBe(true);

    // The mount drain.
    for (const c of pendingBytes) p.queue.stageChunk(c, replayedThrough);
    p.queue.flush(p.sink);
    p.sink.renderAll();

    // A big repaint burst trips the gate while the renderer lags.
    burst(gate, p, replayedThrough + 6_000, 10);
    expect(gate.paused).toBe(true);

    // The renderer catches up and acks everything it received.
    p.settle();
    expect(gate.gap).toBeLessThanOrEqual(FLOW_LOW_WATERMARK);
    // The ack that brought the gap under Low emitted the resume marker the
    // pane's gap detection resyncs from, so the pane is not left frozen.
    expect(gate.resumeMarkers).toBe(1);
    expect(gate.tryEmit(100)).toBe(true);
  });

  it("acks a held frame that a resync's ring snapshot already covered", () => {
    const gate = new RunnerGate();
    const p = pane(gate);

    // A resync's ring fetch resets the counters while a burst is in flight.
    gate.reset();
    const replayedThrough = 500_000;
    // Held during the fetch: a 43 KB sync frame (50 ms at ~870 KB/s) emitted
    // after the reset but entirely inside the snapshot, then a contiguous one.
    const held = [chunk(457_000, 43_000), chunk(500_000, 20_000)];
    for (const c of held) gate.tryEmit(c.bytes.length);

    const drained = drainHeldChunks(held, 457_000, replayedThrough);
    expect(drained.writable.map((w) => w.length)).toEqual([20_000]);
    expect(drained.receivedBytes).toBe(63_000);
    p.queue.stage(drained.writable, drained.receivedBytes);
    p.queue.flush(p.sink);

    burst(gate, p, 520_000, 10);
    expect(gate.paused).toBe(true);
    p.settle();
    expect(gate.gap).toBeLessThanOrEqual(FLOW_LOW_WATERMARK);
    expect(gate.tryEmit(100)).toBe(true);
  });

  it("settles a fully-replayed chunk even when nothing is left to write", () => {
    const gate = new RunnerGate();
    const p = pane(gate);
    gate.tryEmit(12_000);
    p.queue.stageChunk(chunk(0, 12_000), 50_000);
    expect(p.queue.isEmpty).toBe(false);
    p.queue.flush(p.sink);
    expect(p.sink.written).toEqual([]);
    expect(gate.gap).toBe(0); // 12_000 >= AckSize, acked at once
  });
});

describe("AckedWriteQueue — coalescing", () => {
  it("writes everything staged as one write and acks the received total once", () => {
    const gate = new RunnerGate();
    const p = pane(gate);
    p.queue.stageChunk(chunk(0, 3_000), 0);
    p.queue.stageChunk(chunk(3_000, 4_000), 5_000); // 2_000 replayed, 2_000 written
    p.queue.flush(p.sink);
    expect(p.sink.written).toEqual([5_000]);
    expect(p.acks.outstanding).toBe(7_000);
    p.sink.renderAll();
    expect(gate.acked).toBe(7_000);
    expect(p.queue.isEmpty).toBe(true);
  });

  it("drops staged bytes when the pane is gone (its unmount resets the runner)", () => {
    const gate = new RunnerGate();
    const p = pane(gate);
    p.queue.stageChunk(chunk(0, 8_000), 0);
    p.queue.flush(null);
    expect(p.queue.isEmpty).toBe(true);
    expect(gate.acked).toBe(0);
  });

  it("settles the ack of a write that throws, then rethrows", () => {
    const gate = new RunnerGate();
    const acks = new RenderAckAccumulator();
    const queue = new AckedWriteQueue(acks, (n) => gate.ack(n));
    queue.stageChunk(chunk(0, 6_000), 0);
    const boom: RenderedWriteSink = {
      write: () => {
        throw new Error("renderer gone");
      },
    };
    expect(() => queue.flush(boom)).toThrow("renderer gone");
    expect(acks.outstanding).toBe(0);
    expect(gate.acked).toBe(6_000);
  });
});

describe("TerminalInstance routes every received byte through the acked queue", () => {
  const source = readFileSync(resolve(__dirname, "TerminalInstance.tsx"), "utf8");

  it("never trims a live chunk outside the queue", () => {
    // A bare `trimReplayedChunk` in the component is exactly the shape that
    // wrote the unreplayed suffix and forgot the trimmed prefix's ack.
    expect(source).not.toMatch(/\btrimReplayedChunk\s*\(/);
  });

  it("stages live, buffered and leftover held chunks with stageChunk", () => {
    expect(source).toContain("ackQueue.stageChunk({ bytes, offset }, replayedThrough)");
    expect(source).toContain("ackQueue.stageChunk(buffered, replayedThrough)");
    expect(source).toContain("ackQueue.stageChunk(c, replayedThrough)");
  });

  it("acks a resync drain by what it consumed, not by what it wrote", () => {
    expect(source).toContain("ackQueue.stage(drained.writable, drained.receivedBytes)");
  });
});
