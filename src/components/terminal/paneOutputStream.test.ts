/**
 * `PaneOutputStream`: routing for every received chunk, and the reconnect path
 * the review of the termhost-bench freeze fix found leaking (SF1).
 *
 * On reconnect, `TerminalInstance` reads the mount ring, which resets the
 * runner's flow-control counters. It registers its output handler only after
 * the bootstrap paint (`terminal_resize`, a 50 ms wait, `terminal_get_grid`).
 * A full-TUI SIGWINCH redraw sent in that window is counted as sent and reaches
 * no handler. Before the fix, `nextExpectedOffset` was still null, so no resync
 * fired and nothing acked those bytes. A window past the High watermark left
 * the gate paused before the handler existed, so no chunk could ever arrive.
 *
 * The runner side is the shared `RunnerGate` model. The last suite pins the
 * wiring. `TerminalInstance` cannot be imported under `environment: "node"`,
 * so its source is scanned.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it, expect, vi } from "vitest";
import { PaneOutputStream, decodeOutputPayload, type PaneOutputHooks } from "./paneOutputStream";
import { FLOW_LOW_WATERMARK, RunnerGate, chunk, paneWriter } from "./__fixtures__/runnerGateModel";

function harness(gate: RunnerGate, opts: { paused?: boolean } = {}) {
  const writer = paneWriter(gate);
  const state = { paused: opts.paused ?? false };
  const hooks = {
    isPaused: () => state.paused,
    hasBackend: () => true,
    noteMissedOutput: vi.fn(),
    requestResync: vi.fn(),
    scheduleFlush: vi.fn(),
    resetFlowControl: vi.fn(() => gate.reset()),
  } satisfies PaneOutputHooks;
  const stream = new PaneOutputStream(writer.queue, hooks);
  return { ...writer, stream, hooks, state };
}

describe("PaneOutputStream — routing", () => {
  it("buffers before the backend is ready, then drains behind the mount ring with a full ack", () => {
    const gate = new RunnerGate();
    const h = harness(gate);
    gate.reset();
    const c = chunk(900, 300); // 100 bytes already in the ring through 1000
    gate.tryEmit(300);
    expect(h.stream.receive(c)).toBe("buffer");
    h.stream.applyMountRing(1_000);
    expect(h.stream.drainPending()).toBe(300);
    expect(h.stream.backendReady).toBe(true);
    h.settle();
    expect(h.sink.written).toEqual([200]);
    expect(gate.gap).toBe(0);
  });

  it("writes a contiguous live chunk through the queue", () => {
    const h = harness(new RunnerGate());
    h.stream.drainPending();
    expect(h.stream.receive(chunk(0, 50))).toBe("write");
    expect(h.hooks.scheduleFlush).toHaveBeenCalled();
    expect(h.stream.writtenThrough).toBe(50);
  });

  it("holds a chunk past a gap and asks for a resync", () => {
    const h = harness(new RunnerGate());
    h.stream.drainPending();
    h.stream.receive(chunk(0, 50));
    expect(h.stream.receive(chunk(500, 50))).toBe("hold");
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
    h.stream.noteRingReplayed(500);
    expect(h.stream.drainHeld()).toBe(true);
    expect(h.stream.writtenThrough).toBe(550);
  });

  it("discards a chunk when the pane has no backend", () => {
    const gate = new RunnerGate();
    const writer = paneWriter(gate);
    const stream = new PaneOutputStream(writer.queue, {
      isPaused: () => false,
      hasBackend: () => false,
      noteMissedOutput: vi.fn(),
      requestResync: vi.fn(),
      scheduleFlush: vi.fn(),
      resetFlowControl: vi.fn(),
    });
    stream.drainPending();
    expect(stream.receive(chunk(0, 10))).toBe("discard");
    expect(writer.queue.isEmpty).toBe(true);
  });

  it("decodes a base64 payload with its offset", () => {
    expect(decodeOutputPayload({ data: btoa("hi"), offset: 7 })).toEqual({
      bytes: new Uint8Array([104, 105]),
      offset: 7,
    });
  });
});

describe("PaneOutputStream — a hidden pane acks nothing the runner did not count", () => {
  it("drops background-tier sends without staging or acking them", () => {
    const gate = new RunnerGate();
    const h = harness(gate, { paused: true });
    h.stream.drainPending();
    // Hidden for a long time: the runner holds and flushes background windows,
    // which it never counts as sent. Acking them would over-ack without bound.
    let offset = 0;
    for (let i = 0; i < 50; i++) {
      gate.sendUncounted(40_000);
      expect(h.stream.receive(chunk(offset, 40_000))).toBe("drop");
      offset += 40_000;
    }
    h.settle();
    expect(gate.acked).toBe(0);
    expect(gate.sent).toBe(0);
    expect(h.hooks.noteMissedOutput).toHaveBeenLastCalledWith(offset);
  });
});

describe("PaneOutputStream — the reconnect path's late attach (SF1)", () => {
  /** The reconnect mount: ring read (a reset) and the drain, with no handler attached yet. */
  function reconnectMount(gate: RunnerGate, ringEnd: number) {
    const h = harness(gate);
    gate.reset(); // terminal_get_scrollback
    h.stream.applyMountRing(ringEnd);
    h.stream.drainPending(); // nothing buffered: no handler was registered
    return h;
  }

  it("a hole sent before the attach reads as a gap, so the pane resyncs", () => {
    const gate = new RunnerGate();
    const ringEnd = 1_000_000;
    const h = reconnectMount(gate, ringEnd);
    // The SIGWINCH redraw after terminal_resize: sent, counted, seen by nobody.
    expect(gate.tryEmit(30_000)).toBe(true);

    h.stream.attachedLate();
    expect(h.hooks.resetFlowControl).toHaveBeenCalledTimes(1);

    // The first live chunk starts past the ring end: the hole is detected.
    expect(gate.tryEmit(2_000)).toBe(true);
    expect(h.stream.receive(chunk(ringEnd + 30_000, 2_000))).toBe("hold");
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
  });

  it("a window past the High watermark cannot leave the gate paused before the attach", () => {
    const gate = new RunnerGate();
    const ringEnd = 1_000_000;
    const h = reconnectMount(gate, ringEnd);
    // A big redraw while no handler exists: the gate pauses, and no chunk can
    // reach the pane to reveal it.
    let sentInWindow = 0;
    while (gate.tryEmit(40_000)) sentInWindow += 40_000;
    expect(gate.paused).toBe(true);

    h.stream.attachedLate();

    // The reset reopened the gate; the next chunk flows and reads as a gap.
    expect(gate.tryEmit(2_000)).toBe(true);
    expect(h.stream.receive(chunk(ringEnd + sentInWindow + 40_000, 2_000))).toBe("hold");
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
    // Simulate the resync (its ring read resets too) and confirm the pane can
    // keep the gate open from here.
    gate.reset();
    h.stream.noteRingReplayed(ringEnd + sentInWindow + 40_000);
    h.stream.drainHeld();
    h.settle();
    expect(gate.gap).toBeLessThanOrEqual(FLOW_LOW_WATERMARK);
    expect(gate.tryEmit(100)).toBe(true);
  });
});

describe("TerminalInstance routes every received byte through PaneOutputStream", () => {
  const source = readFileSync(resolve(__dirname, "TerminalInstance.tsx"), "utf8");
  // Comments mention `backend.write()` in prose; scan code only.
  const code = source
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\*|\/\*)/.test(l))
    .join("\n");

  it("never stages, trims or decodes output itself", () => {
    expect(code).not.toMatch(/\bstageChunk\s*\(/);
    expect(code).not.toMatch(/\.stage\s*\(/);
    expect(code).not.toMatch(/\btrimReplayedChunk\s*\(/);
    expect(code).not.toMatch(/\batob\s*\(/);
  });

  it("writes to the backend directly only from the allowlisted non-output sources", () => {
    // The first argument of every `.write(` in the component. A live chunk
    // written here would bypass the acked queue, which is the defect class.
    const args = [...code.matchAll(/\.write\(\s*([^\s,)]+)/g)].map((m) => m[1]);
    const allowed = [
      /^["`]/, // literal notices (input refused, process exited)
      /^data$/, // writeToDisplay: text a parent component paints, never PTY output
      /^formatLostOutputMarker\(/, // resync: in-band "bytes lost" notice
      /^slice$/, // resync: the ring slice (`ringBytes.subarray`), counted by the ring read's reset
      /^RESYNC_INCOMPLETE_MARKER$/,
      /^ring\.bytes$/, // mount ring replay and remote earlier-output re-render
      /^detail\.bytes$/, // remote earlier-output history
    ];
    const unexpected = args.filter((a) => !allowed.some((re) => re.test(a)));
    expect(unexpected).toEqual([]);
    expect(code).toMatch(/const slice = ringBytes\.subarray\(/);
  });

  it("the output handler only decodes and hands the chunk to the stream", () => {
    const start = code.indexOf("const handleOutputPayload");
    const end = code.indexOf("};", start);
    const body = code.slice(start, end);
    expect(body).toMatch(/stream\.receive\(/);
    expect(body).not.toMatch(/\.write\(/);
  });

  it("the reconnect path's late registration forgives the window it missed", () => {
    expect(code).toMatch(
      /isReconnecting\s*&&\s*!outputUnsub\)\s*\{\s*outputUnsub\s*=\s*registerTerminalOutputHandler\([^)]*\);\s*stream\.attachedLate\(\);/,
    );
    expect(code).toMatch(/stream\.applyMountRing\(ring\.endOffset\)/);
  });

  it("flushes staged leftovers before writing the resync-incomplete marker", () => {
    expect(code).toMatch(
      /flushCoalesced\(\);\s*backendRef\.current\?\.write\(RESYNC_INCOMPLETE_MARKER\)/,
    );
  });
});
