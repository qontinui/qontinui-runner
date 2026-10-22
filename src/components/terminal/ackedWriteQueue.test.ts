/**
 * The defect: a visible pane can stop repainting while its PTY keeps flowing,
 * at a large repaint burst after a remount or resync. (It is the suspected
 * cause of termhost-bench CI run 38351621760, qontinui-runner PR #2181, whose
 * runner tiled arm recorded too few output-latency samples to gate on; that
 * log does not show the freeze directly.)
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
 * `RunnerGate` (`__fixtures__/runnerGateModel.testkit.ts`) mirrors the Rust gate. The
 * scenarios drive `AckedWriteQueue` the way `PaneOutputStream` does. They
 * assert two properties: once the pane has rendered everything it received,
 * the gate can reopen; and the acks it sends for bytes the runner already
 * wrote off (sent before a reset, or never counted) are clamped away, leaving
 * no surplus. The routing, the reconnect path and the no-compounding cycles
 * are covered in `paneOutputStream.test.ts`.
 */

import { describe, it, expect } from "vitest";
import { AckedWriteQueue, type RenderedWriteSink } from "./ackedWriteQueue";
import { RenderAckAccumulator } from "./flowControl";
import { drainHeldChunks } from "./scrollbackReplay";
import {
  FLOW_HIGH_WATERMARK,
  FLOW_LOW_WATERMARK,
  RunnerGate,
  chunk,
  emitUntilPause,
  paneWriter,
} from "./__fixtures__/runnerGateModel.testkit";

/** `BACKGROUND_HOLD_BYTE_CAP` (visibility.rs). */
const BACKGROUND_HOLD_BYTE_CAP = 256 * 1024;

/**
 * A repaint burst, as the pane sees it: each chunk the gate admits is staged
 * and flushed, but the renderer lags, so nothing renders until the burst ends.
 * That is what drives the gap past the High watermark.
 */
function burst(gate: RunnerGate, p: ReturnType<typeof paneWriter>, from: number, chunks: number) {
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
    const p = paneWriter(gate);

    // The new pane mounts (maximize or restore-zone). Its ring read resets the
    // counters and snapshots the ring through offset 1_000_000.
    gate.reset();
    const replayedThrough = 1_000_000;
    // Emitted AFTER the reset: one coalesced sync frame wholly tee'd into the
    // ring before the snapshot, then a chunk past it. Both arrive before the
    // backend is ready and are buffered.
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
    const p = paneWriter(gate);

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
    const p = paneWriter(gate);
    gate.tryEmit(12_000);
    p.queue.stageChunk(chunk(0, 12_000), 50_000);
    expect(p.queue.isEmpty).toBe(false);
    p.queue.flush(p.sink);
    expect(p.sink.written).toEqual([]);
    expect(gate.gap).toBe(0); // 12_000 >= AckSize, acked at once
  });
});

describe("AckedWriteQueue — acks the runner already wrote off are clamped away", () => {
  /** Receive a chunk the way `PaneOutputStream` does: stage it, owe all of it. */
  function receive(p: ReturnType<typeof paneWriter>, offset: number, len: number) {
    p.queue.stageChunk(chunk(offset, len), 0);
  }

  it("chunks received before a reset and acked after it leave acked == sent", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);

    // Before the reset: the gate admits chunks until it pauses. The pane
    // received them but has not written them (buffered, or held by a resync).
    emitUntilPause(gate, 40_000, (offset) => receive(p, offset, 40_000));
    expect(gate.paused).toBe(true);

    // A ring read resets the counters; the pane writes and acks the chunks
    // after it. The runner's clamp discards those acks: no surplus.
    gate.reset();
    p.settle();
    expect(gate.acked).toBe(gate.sent);

    // Backpressure re-engages at High plus one chunk, as if nothing happened.
    const admitted = emitUntilPause(gate, 40_000);
    expect(gate.paused).toBe(true);
    expect(admitted).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + 40_000);
  });

  it("an ack for a byte sent after the reset still counts", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);
    gate.reset();
    expect(gate.tryEmit(30_000)).toBe(true);
    receive(p, 0, 30_000);
    p.queue.flush(p.sink);
    expect(gate.gap).toBe(30_000);
    p.sink.renderAll();
    expect(gate.gap).toBe(0);
    expect(gate.acked).toBe(gate.sent);
  });

  it("a background window sent uncounted and acked by a now-visible pane leaves no surplus", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);

    // The pane comes back to `focused`. The runner flushes the background hold
    // on the tier edge, and that send is not counted. The pane is visible
    // again, so it writes and acks the window. Uncounted: `gate.sent` stays 0.
    p.queue.stageChunk(chunk(0, BACKGROUND_HOLD_BYTE_CAP), 0);
    p.settle();
    expect(gate.acked).toBe(gate.sent);

    // Backpressure engages at High plus one chunk, not a hold window later.
    const admitted = emitUntilPause(gate, 40_000);
    expect(gate.paused).toBe(true);
    expect(admitted).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + 40_000);
  });
});

describe("AckedWriteQueue — coalescing", () => {
  it("writes everything staged as one write and acks the received total once", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);
    gate.tryEmit(7_000);
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
    const p = paneWriter(gate);
    p.queue.stageChunk(chunk(0, 8_000), 0);
    p.queue.flush(null);
    expect(p.queue.isEmpty).toBe(true);
    expect(gate.acked).toBe(0);
  });

  it("settles the ack of a write that throws, then rethrows", () => {
    const gate = new RunnerGate();
    const acks = new RenderAckAccumulator();
    const queue = new AckedWriteQueue(acks, (n) => gate.ack(n));
    gate.tryEmit(6_000);
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

  it("settles a write once when its sink renders synchronously and then throws", () => {
    const gate = new RunnerGate();
    const acks = new RenderAckAccumulator();
    const queue = new AckedWriteQueue(acks, (n) => gate.ack(n));
    gate.tryEmit(6_000);
    queue.stageChunk(chunk(0, 6_000), 0);
    const renderThenThrow: RenderedWriteSink = {
      write: (_data, onRendered) => {
        onRendered();
        throw new Error("renderer gone");
      },
    };
    expect(() => queue.flush(renderThenThrow)).toThrow("renderer gone");
    expect(gate.acked).toBe(6_000);
    expect(acks.outstanding).toBe(0);
    expect(acks.pending).toBe(0);
  });

  it("acks a settled-without-write chunk behind the write before it, not ahead of it", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);
    // Written, not yet rendered.
    gate.tryEmit(6_000);
    p.queue.stageChunk(chunk(1_000_000, 6_000), 0);
    p.queue.flush(p.sink);
    // Wholly inside a ring replay: nothing to write.
    gate.tryEmit(6_000);
    p.queue.stageChunk(chunk(0, 6_000), 1_000_000);
    p.queue.flush(p.sink);
    expect(p.sink.written).toEqual([6_000]);
    expect(gate.acked).toBe(0); // the later chunk did not overtake the write

    p.sink.renderAll();
    expect(gate.acked).toBe(12_000);
  });
});

describe("RunnerGate mirrors the runner's reset (fetch_max, then re-clamp)", () => {
  it("keeps an ack that lands while the reset runs; a plain store would erase it", () => {
    for (const mode of ["fetch_max", "store"] as const) {
      const gate = new RunnerGate();
      gate.tryEmit(50_000);
      gate.resetAround(() => {
        gate.tryEmit(8_192); // counted after the reset read `sent`
        gate.ack(50_000 + 8_192); // and the pane acks it
      }, mode);
      expect(gate.gap).toBe(mode === "fetch_max" ? 0 : 8_192);
    }
  });

  it("leaves no surplus when a failed emit is un-counted while the reset runs", () => {
    const gate = new RunnerGate();
    gate.tryEmit(50_000);
    gate.ack(50_000);
    gate.tryEmit(8_192); // counted; its emit fails
    gate.resetAround(() => gate.uncount(8_192));
    expect(gate.sent).toBe(50_000);
    expect(gate.acked).toBe(gate.sent);
  });
});
