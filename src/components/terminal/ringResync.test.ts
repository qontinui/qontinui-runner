/**
 * `RingResync`: the emission-gap / reveal resync, driven against a fake
 * backend and the shared `RunnerGate` model. It pins the paths a source scan
 * cannot: the bare-reset fallback when no ring read succeeds, the give-up
 * marker's order behind the flushed leftovers, the queued retry, and that a
 * successful pass replays exactly the missed window.
 */

import { describe, it, expect, vi } from "vitest";
import type { ScrollbackRingWindow } from "./backends/types";
import { PaneOutputStream } from "./paneOutputStream";
import { RingResync, type RingResyncHost } from "./ringResync";
import { RESYNC_INCOMPLETE_MARKER } from "./scrollbackReplay";
import { RunnerGate, chunk, paneWriter } from "./__fixtures__/runnerGateModel.testkit";

function ring(start: number, end: number): ScrollbackRingWindow {
  return { bytes: new Uint8Array(end - start).fill(0x42), startOffset: start, endOffset: end };
}

function setup(reads: Array<() => Promise<ScrollbackRingWindow | null>>) {
  const gate = new RunnerGate();
  const writer = paneWriter(gate);
  const stream = new PaneOutputStream(writer.queue, {
    isPaused: () => false,
    hasBackend: () => true,
    noteMissedOutput: vi.fn(),
    requestResync: vi.fn(),
    scheduleFlush: vi.fn(),
  });
  stream.drainPending();
  const log: string[] = [];
  const written: Array<Uint8Array | string> = [];
  let readIndex = 0;
  const state = { disposed: false };
  const host = {
    isDisposed: () => state.disposed,
    backend: () => ({
      readScrollbackRing: async () => {
        log.push("read");
        const next = reads[Math.min(readIndex++, reads.length - 1)];
        const r = await next();
        if (r) gate.reset(); // terminal_get_scrollback resets before it snapshots
        return r;
      },
      write: (data: Uint8Array | string) => {
        log.push(data === RESYNC_INCOMPLETE_MARKER ? "incomplete-marker" : "write");
        written.push(data);
      },
    }),
    noteRingReplay: vi.fn(),
    noteResynced: vi.fn(() => true),
    noteResyncFailed: vi.fn(() => log.push("failed")),
    flushStaged: vi.fn(() => {
      log.push("flush");
      writer.queue.flush(writer.sink);
    }),
    flowReset: vi.fn((why: string) => {
      log.push(`flowReset:${why}`);
      gate.reset();
    }),
    warn: vi.fn(),
  } satisfies RingResyncHost;
  const resync = new RingResync(stream, host, 3);
  return { gate, writer, stream, host, log, written, resync, state };
}

describe("RingResync", () => {
  it("replays exactly the missed window and reopens the gate through the ring read", async () => {
    const h = setup([async () => ring(0, 5_000)]);
    h.stream.receive(chunk(0, 1_000));
    h.writer.settle();
    // The gate pauses while the pane misses 4 000 bytes.
    h.gate.sent += 150_000;
    h.gate.paused = true;
    await h.resync.run();
    expect(h.written).toHaveLength(1);
    expect((h.written[0] as Uint8Array).length).toBe(4_000);
    expect(h.stream.writtenThrough).toBe(5_000);
    expect(h.host.flowReset).not.toHaveBeenCalled();
    expect(h.host.noteResyncFailed).not.toHaveBeenCalled();
    expect(h.gate.gap).toBe(0);
    expect(h.stream.resyncInFlight).toBe(false);
  });

  it("falls back to a bare reset when every ring read fails, so a paused gate reopens", async () => {
    const h = setup([() => Promise.reject(new Error("ipc down"))]);
    h.stream.receive(chunk(0, 1_000));
    h.writer.settle();
    // A paused gate that no ring read will reset (the late attach's case).
    h.gate.sent += 150_000;
    h.gate.paused = true;
    h.stream.resyncInFlight = false;
    // A chunk past a hole is held while the pass runs.
    h.stream.receive(chunk(9_000, 500));

    await h.resync.run();

    expect(h.log.filter((l) => l === "read")).toHaveLength(3);
    expect(h.host.flowReset).toHaveBeenCalledTimes(1);
    expect(h.host.flowReset).toHaveBeenCalledWith("resync fallback");
    // Leftovers were released and flushed before the in-band notice.
    expect(h.log.indexOf("flush")).toBeLessThan(h.log.indexOf("incomplete-marker"));
    expect(h.host.noteResyncFailed).toHaveBeenCalledTimes(1);
    expect(h.stream.writtenThrough).toBe(9_500);
    expect(h.gate.gap).toBe(0);
    expect(h.gate.tryEmit(100)).toBe(true);
  });

  it("does not fall back once a ring read has reset the counters", async () => {
    const h = setup([async () => ring(0, 1_000)]);
    h.host.noteResynced.mockReturnValue(false); // never caught up: retries run out
    await h.resync.run();
    expect(h.log.filter((l) => l === "read")).toHaveLength(3);
    expect(h.host.noteResyncFailed).toHaveBeenCalledTimes(1);
    expect(h.host.flowReset).not.toHaveBeenCalled();
  });

  it("does nothing more for a pane that was disposed mid-pass", async () => {
    const h = setup([() => Promise.reject(new Error("gone"))]);
    h.state.disposed = true;
    await h.resync.run();
    expect(h.host.flowReset).not.toHaveBeenCalled();
    expect(h.host.noteResyncFailed).not.toHaveBeenCalled();
  });

  it("queues a request that arrives mid-pass and runs exactly one more", async () => {
    let release: (r: ScrollbackRingWindow) => void = () => {};
    const h = setup([
      () => new Promise((resolve) => (release = resolve)),
      async () => ring(0, 2_000),
    ]);
    const first = h.resync.run();
    await Promise.resolve();
    void h.resync.run(); // arrives while the first read is in flight
    void h.resync.run(); // coalesced with the one above
    release(ring(0, 1_000));
    await first;
    await vi.waitFor(() => expect(h.log.filter((l) => l === "read")).toHaveLength(2));
    await vi.waitFor(() => expect(h.stream.resyncInFlight).toBe(false));
    expect(h.stream.writtenThrough).toBe(2_000);
  });
});
