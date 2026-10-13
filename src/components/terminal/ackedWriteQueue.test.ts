/**
 * The termhost-bench freeze (CI run 38351621760, qontinui-runner PR #2181): a
 * visible pane stopped repainting while its PTY kept flowing. It froze at a
 * large repaint burst, and froze again after `restore-zone` remounted it.
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
 * `RunnerGate` (`__fixtures__/runnerGateModel.ts`) mirrors the Rust gate. The
 * scenarios drive `AckedWriteQueue` the way `PaneOutputStream` does. They
 * assert two properties: once the pane has rendered everything it received,
 * the gate can reopen; and the over-ack this queue deliberately does across a
 * reset stays inside the bound `reset_flow_control` documents. The routing and
 * the reconnect path are covered in `paneOutputStream.test.ts`.
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
  paneWriter,
} from "./__fixtures__/runnerGateModel";

/** `SYNC_FLUSH_BYTE_CAP` (session.rs) plus the 8 KiB read that can tip a frame over it. */
const MAX_EMISSION = 256 * 1024 + 8 * 1024;
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

/** Bytes the gate admits (counted sends, nothing rendered) before it pauses again. */
function admittedUntilPause(gate: RunnerGate, chunkLen: number): number {
  let admitted = 0;
  for (let i = 0; i < 1_000 && gate.tryEmit(chunkLen); i++) admitted += chunkLen;
  return admitted;
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

describe("AckedWriteQueue — the over-ack across a reset is bounded", () => {
  it("a chunk sent before a reset and written after it over-acks by at most the pre-reset gap", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);

    // Before the reset: the gate admits chunks while the gap is <= High. The
    // pane received them, but they are still buffered/held, so nothing is acked.
    let offset = 0;
    while (gate.tryEmit(40_000)) {
      p.queue.stageChunk(chunk(offset, 40_000), 0);
      offset += 40_000;
    }
    const preResetGap = gate.gap;
    expect(preResetGap).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + MAX_EMISSION);

    // A ring read resets the counters; the pane writes and acks those chunks after it.
    gate.reset();
    p.settle();
    const overAck = gate.acked - gate.sent;
    expect(overAck).toBeGreaterThan(0); // acked passed sent: the documented skew
    expect(overAck).toBeLessThanOrEqual(preResetGap);

    // Backpressure re-engages within High + the over-ack + one chunk.
    const admitted = admittedUntilPause(gate, 40_000);
    expect(gate.paused).toBe(true);
    expect(admitted).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + overAck + 40_000);
  });

  it("a background window sent uncounted and acked by a now-visible pane stays inside one hold window", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);

    // The pane comes back to `focused`. The runner flushes the background hold
    // on the tier edge, and that send is not counted. The pane is visible again,
    // so it writes and acks the window.
    const window = chunk(0, 200_000);
    expect(gate.sendUncounted(window.bytes.length)).toBe(true);
    p.queue.stageChunk(window, 0);
    p.settle();
    const overAck = gate.acked - gate.sent;
    expect(overAck).toBe(window.bytes.length);
    expect(overAck).toBeLessThanOrEqual(BACKGROUND_HOLD_BYTE_CAP);

    // Gap saturates at 0 and backpressure still re-engages, within the bound.
    expect(gate.gap).toBe(0);
    const admitted = admittedUntilPause(gate, 40_000);
    expect(gate.paused).toBe(true);
    expect(admitted).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + overAck + 40_000);
  });
});

describe("AckedWriteQueue — coalescing", () => {
  it("writes everything staged as one write and acks the received total once", () => {
    const gate = new RunnerGate();
    const p = paneWriter(gate);
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
