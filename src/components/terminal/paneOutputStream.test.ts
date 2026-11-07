/**
 * `PaneOutputStream`: routing for every received chunk, the no-surplus cycles
 * against the runner's clamped ack, and the reconnect path's late attach.
 *
 * On reconnect, `TerminalInstance` reads the mount ring, which resets the
 * runner's flow-control counters, and defers its output handler past the
 * bootstrap resize. A full-TUI SIGWINCH redraw sent in that window is counted
 * as sent and reaches no handler. The grid paint that follows already shows
 * it, so replaying it as well would apply relative output twice. The handler
 * is therefore registered before the grid snapshot is requested, and the
 * snapshot's `stream_offset` decides what is shown, acked and replayed.
 *
 * The runner side is the shared `RunnerGate` model. The last suite pins the
 * wiring. `TerminalInstance` cannot be imported under `environment: "node"`,
 * so its source is scanned.
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it, expect, vi } from "vitest";
import { PaneOutputStream, decodeOutputPayload, type PaneOutputHooks } from "./paneOutputStream";
import {
  FLOW_HIGH_WATERMARK,
  FLOW_LOW_WATERMARK,
  RunnerGate,
  chunk,
  paneWriter,
} from "./__fixtures__/runnerGateModel.testkit";

function harness(gate: RunnerGate, opts: { paused?: boolean } = {}) {
  const writer = paneWriter(gate);
  const state = { paused: opts.paused ?? false };
  const hooks = {
    isPaused: () => state.paused,
    hasBackend: () => true,
    noteMissedOutput: vi.fn(),
    requestResync: vi.fn(),
    scheduleFlush: vi.fn(),
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
    h.stream.applyResyncRing(500);
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

describe("PaneOutputStream — a hidden pane acks nothing it did not render", () => {
  it("drops a counted chunk that races the hide, and the reveal resync recovers the gate", () => {
    const gate = new RunnerGate();
    const h = harness(gate);
    h.stream.drainPending();
    expect(gate.tryEmit(1_000)).toBe(true);
    h.stream.receive(chunk(0, 1_000));
    h.settle();

    // The pane hides, but the runner has not yet seen its `background`
    // declaration: it still sends at the focused tier and COUNTS the chunk.
    // The pane drops it unacked, since it never parsed it.
    h.state.paused = true;
    expect(gate.tryEmit(40_000)).toBe(true);
    expect(h.stream.receive(chunk(1_000, 40_000))).toBe("drop");
    h.settle();
    expect(gate.gap).toBe(40_000);
    expect(h.hooks.noteMissedOutput).toHaveBeenLastCalledWith(41_000);
    expect(h.stream.writtenThrough).toBe(1_000);

    // Revealed: the visibility service resyncs. Its ring read resets the
    // counters, and the replay covers the dropped window from `writtenThrough`.
    h.state.paused = false;
    gate.reset();
    h.stream.applyResyncRing(41_000);
    expect(h.stream.drainHeld()).toBe(true);
    h.settle();
    expect(gate.gap).toBe(0);
    expect(gate.tryEmit(100)).toBe(true);
    expect(h.stream.receive(chunk(41_000, 100))).toBe("write");
  });
});

describe("PaneOutputStream — no surplus builds up across resets", () => {
  /**
   * Reveal resyncs under sustained output while event delivery lags (the
   * second review's `surplus.test.ts`): the runner emits until it pauses, the
   * pane issues a ring read before any of it is delivered, and the backlog
   * arrives after the reset. Before the runner clamped its acks, each cycle's
   * late acks stood as a surplus the next cycle's burst ran into, and the
   * surplus grew every cycle. With the clamp acked never passes sent, so each
   * burst pauses at High plus one chunk.
   */
  it("keeps acked == sent and every burst at High plus one chunk when each reset meets a backlog", () => {
    const gate = new RunnerGate();
    const h = harness(gate);
    h.stream.drainPending();
    let offset = 0;
    const C = 8_192;
    for (let cycle = 0; cycle < 30; cycle++) {
      const pipeline: number[] = [];
      while (gate.tryEmit(C)) {
        pipeline.push(offset);
        offset += C;
      }
      expect(pipeline.length * C).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + C);
      // The ring read: the runner resets, then the backlog is delivered.
      h.stream.resyncInFlight = true;
      gate.reset();
      for (const o of pipeline) h.stream.receive(chunk(o, C));
      h.stream.applyResyncRing(pipeline[0] ?? offset);
      h.stream.drainHeld();
      h.stream.resyncInFlight = false;
      h.settle();
      expect(gate.acked).toBe(gate.sent);
    }
  });

  /**
   * The first review's scenario A: the renderer lags, catches up only until the
   * resume marker, and two chunks are held across each resync's ring read.
   * Unclamped, the surplus grew 16 KiB a cycle (24 576 after the first, 737 280
   * after 30, as the second review measured).
   */
  it("keeps acked == sent after every burst, reopen and resync cycle", () => {
    const gate = new RunnerGate();
    const h = harness(gate);
    h.stream.drainPending();
    let offset = 0;
    for (let cycle = 0; cycle < 30; cycle++) {
      // Burst: every admitted chunk is written; nothing renders yet.
      let admitted = 0;
      while (gate.tryEmit(8_192)) {
        h.stream.receive(chunk(offset, 8_192));
        offset += 8_192;
        admitted += 8_192;
        h.queue.flush(h.sink);
      }
      expect(admitted).toBeLessThanOrEqual(FLOW_HIGH_WATERMARK + 8_192);
      gate.tryEmit(8_192); // skipped: the runner flags `emission_skipped`
      const markers = gate.resumeMarkers;
      while (gate.resumeMarkers === markers && h.sink.renderOne()) {
        /* render one write */
      }
      expect(gate.resumeMarkers).toBe(markers + 1);
      // The resume marker reads as a gap: the pane holds while it resyncs.
      h.stream.resyncInFlight = true;
      for (let k = 0; k < 2; k++) {
        expect(gate.tryEmit(8_192)).toBe(true);
        expect(h.stream.receive(chunk(offset, 8_192))).toBe("hold");
        offset += 8_192;
      }
      gate.reset(); // the ring read
      h.stream.applyResyncRing(offset - 2 * 8_192);
      expect(h.stream.drainHeld()).toBe(true);
      h.stream.resyncInFlight = false;
      h.settle();
      expect(gate.acked).toBe(gate.sent);
    }
  });
});

describe("PaneOutputStream — an empty mount ring still anchors the stream", () => {
  it("a chunk past a hole after an empty ring reads as a gap", () => {
    const h = harness(new RunnerGate());
    h.stream.applyMountRing(0); // `{ bytes: [], startOffset: 0, endOffset: 0 }`
    h.stream.drainPending();
    expect(h.stream.nextExpectedOffset).toBe(0);
    expect(h.stream.receive(chunk(30_000, 100))).toBe("hold");
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
  });
});

describe("PaneOutputStream — the reconnect path's late attach", () => {
  /**
   * The reconnect mount, in the runner's real order: whatever was produced
   * before is in the ring; the ring read resets and then snapshots it; the
   * mount drain finds nothing (no handler yet).
   */
  function reconnect(gate: RunnerGate) {
    const h = harness(gate);
    const ringEnd = gate.readRing(); // terminal_get_scrollback
    h.stream.applyMountRing(ringEnd);
    h.stream.drainPending();
    return { ...h, ringEnd };
  }

  /** The handler registers (holding until the paint), then the grid is fetched. */
  function attachAndSnapshot(gate: RunnerGate, h: ReturnType<typeof harness>, reset: boolean) {
    h.stream.beginLateAttach();
    gate.attach((c) => h.stream.receive(c));
    return gate.snapshotGrid(reset);
  }

  /** Output keeps flowing; the pane renders and acks everything it gets. */
  function sustainedOutput(gate: RunnerGate, h: ReturnType<typeof harness>) {
    for (let i = 0; i < 40; i++) {
      gate.produce(8_192);
      gate.flush();
      h.settle();
    }
  }

  /**
   * A burst while the renderer lags: the gate pauses (the rest is skipped),
   * then the pane renders and acks everything it received.
   */
  function laggingBurst(gate: RunnerGate, h: ReturnType<typeof harness>) {
    for (let i = 0; i < 20; i++) gate.produce(8_192);
    gate.flush();
    expect(gate.paused).toBe(true);
    h.settle();
  }

  // Review 3's repro: a DEC-2026 frame held in the coalescer (60 KB), or a
  // single 8 KiB read caught between the ring write and its emit.
  for (const inFlight of [60_000, 8_192]) {
    it(`acks a ${inFlight}-byte read tee'd into the ring but counted after its reset`, () => {
      const gate = new RunnerGate();
      gate.produce(inFlight); // in the ring, not yet emitted
      const h = reconnect(gate);
      gate.flush(); // counted after the ring read's reset, emitted to no handler
      const snapshot = attachAndSnapshot(gate, h, true);
      expect(h.stream.lateAttachSettled(true, snapshot)).toBe(0);
      h.settle();
      expect(gate.gap).toBe(0);
      sustainedOutput(gate, h);
      laggingBurst(gate, h);
      // Everything the pane got is acked, so the gap is back under Low, the
      // resume marker went out, and the gate reopens.
      expect(gate.gap).toBeLessThanOrEqual(FLOW_LOW_WATERMARK);
      expect(gate.resumeMarkers).toBe(1);
      expect(gate.tryEmit(100)).toBe(true);
    });
  }

  it("with the reset left at the ring read, that same read wedges the gate", () => {
    const gate = new RunnerGate();
    gate.produce(60_000);
    const h = reconnect(gate);
    gate.flush();
    const snapshot = attachAndSnapshot(gate, h, false);
    h.stream.lateAttachSettled(true, snapshot);
    sustainedOutput(gate, h);
    laggingBurst(gate, h);
    // The pane acked everything it received, and the gap still sits at the
    // frame no handler saw: above Low, so no resume marker, and frozen.
    expect(gate.gap).toBe(60_000);
    expect(gate.resumeMarkers).toBe(0);
    expect(gate.tryEmit(100)).toBe(false);
  });

  it("does not replay what the grid paint already shows", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    gate.produce(30_000); // the SIGWINCH redraw after the bootstrap resize
    gate.flush(); // to no handler
    h.stream.beginLateAttach();
    gate.attach((c) => h.stream.receive(c));
    gate.produce(5_000); // absorbed before the snapshot, emitted after it
    const snapshot = gate.snapshotGrid(true);
    gate.produce(2_000); // past the snapshot
    gate.flush();
    expect(h.hooks.requestResync).not.toHaveBeenCalled();

    expect(h.stream.lateAttachSettled(true, snapshot)).toBe(0);

    // No ring read: the paint covers everything up to the snapshot.
    expect(h.hooks.requestResync).not.toHaveBeenCalled();
    h.settle();
    // Only bytes past the snapshot were written, so nothing is applied twice.
    expect(h.sink.written).toEqual([2_000]);
    expect(h.stream.writtenThrough).toBe(h.ringEnd + 37_000);
    expect(gate.gap).toBe(0);
    gate.produce(100);
    gate.flush();
    expect(h.stream.writtenThrough).toBe(h.ringEnd + 37_100);
  });

  it("a redraw past High while unattached cannot wedge the gate", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    for (let i = 0; i < 6; i++) gate.produce(40_000);
    gate.flush(); // counted until the gate pauses, the rest skipped; all to no handler
    expect(gate.paused).toBe(true);
    expect(gate.emissionSkipped).toBe(true);
    const snapshot = attachAndSnapshot(gate, h, true); // the grid absorbed all of it
    h.stream.lateAttachSettled(true, snapshot);
    h.settle();
    expect(gate.gap).toBe(0);
    expect(h.hooks.requestResync).not.toHaveBeenCalled();
    sustainedOutput(gate, h);
    expect(gate.gap).toBeLessThanOrEqual(FLOW_LOW_WATERMARK);
    expect(h.stream.writtenThrough).toBe(gate.produced);
  });

  it("replays from the mount ring's end when it could not paint, even if the TUI goes idle", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    for (let i = 0; i < 4; i++) gate.produce(40_000);
    gate.flush();
    const snapshot = attachAndSnapshot(gate, h, true);
    // Alt screen, or a runner that sends no stream offset: nothing painted.
    h.stream.lateAttachSettled(false, snapshot);
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
    // The resync's slice starts where the mount ring ended.
    expect(h.stream.writtenThrough).toBe(h.ringEnd);
    h.stream.lateAttachSettled(false, undefined);
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(2);
  });

  it("defers a reveal resync that arrives before the paint, then runs it from the snapshot", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    gate.produce(30_000); // the redraw no handler saw
    gate.flush();
    h.stream.beginLateAttach();
    expect(h.stream.mayResyncNow()).toBe(false); // the pane was revealed meanwhile
    gate.attach((c) => h.stream.receive(c));
    h.stream.lateAttachSettled(true, gate.snapshotGrid(true));
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
    // The deferred resync replays from the snapshot, not the mount ring's end.
    expect(h.stream.writtenThrough).toBe(h.ringEnd + 30_000);
    expect(h.stream.mayResyncNow()).toBe(true);
  });

  it("skips the ring read when the snapshot proves nothing was produced", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    const snapshot = attachAndSnapshot(gate, h, true);
    h.stream.lateAttachSettled(false, snapshot);
    expect(h.hooks.requestResync).not.toHaveBeenCalled();
    h.stream.lateAttachSettled(true, snapshot);
    expect(h.hooks.requestResync).not.toHaveBeenCalled();
    expect(gate.gap).toBe(0);
  });

  it("anchors at a snapshot behind the mount ring and replays from it", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    h.stream.beginLateAttach();
    expect(h.stream.lateAttachSettled(true, h.ringEnd - 500)).toBe(500);
    // Set to the snapshot, not left at the ring's end: the resync replays the
    // 500 bytes the paint does not show.
    expect(h.stream.writtenThrough).toBe(h.ringEnd - 500);
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
  });

  /**
   * Review 4's repro: the mount ring ends inside an SGR a read boundary cut,
   * so the grid's offset (5) is behind the ring's end (12). The paint's own
   * escapes abort that open sequence in xterm, so the live remainder must not
   * be written from the ring's end, or xterm prints `196m` as text.
   */
  it("never writes the tail of a sequence the mount ring cut, after a paint", () => {
    const enc = (t: string) => new TextEncoder().encode(t);
    const dec = (b: Uint8Array) => new TextDecoder().decode(b);
    const gate = new RunnerGate();
    const h = harness(gate);
    const ring = "hello\x1b[38;5;";
    gate.produce(ring.length);
    gate.gridBoundary = 5; // the boundary tracker stopped before the ESC
    h.stream.applyMountRing(gate.readRing());
    h.stream.drainPending();
    h.stream.beginLateAttach();
    gate.attach((c) => h.stream.receive(c));
    const snapshot = gate.snapshotGrid(true);
    expect(snapshot).toBe(5);
    // The rest of the SGR arrives live.
    expect(h.stream.receive({ bytes: enc("196mworld"), offset: ring.length })).toBe("hold");

    expect(h.stream.lateAttachSettled(true, snapshot)).toBe(7);

    const writes: string[] = [];
    h.queue.flush({
      write: (d, cb) => {
        writes.push(dec(d));
        cb();
      },
    });
    expect(writes).toEqual([]);
    // It reads as a hole, so the resync replays from the boundary (the ESC).
    expect(h.hooks.requestResync).toHaveBeenCalledTimes(1);
    expect(h.stream.writtenThrough).toBe(5);
    // The resync's ring slice starts at the ESC and covers the live chunk.
    h.stream.applyResyncRing(ring.length + 9);
    expect(h.stream.drainHeld()).toBe(true);
    h.queue.flush({
      write: (d, cb) => {
        writes.push(dec(d));
        cb();
      },
    });
    expect(writes).toEqual([]); // wholly inside the replayed slice: acked, not rewritten
  });

  /**
   * Tauri's `listen()` resolves after the JS handler is registered. A
   * snapshot reset issued before it would count bytes that reach no listener.
   */
  it("does not request the snapshot until the runner-side listener is live", async () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    const order: string[] = [];
    let resolveListen: () => void = () => {};
    const listen = new Promise<void>((resolve) => (resolveListen = resolve));
    const attached = h.stream.lateAttach({
      register: () => {
        order.push("register");
        gate.attach((c) => h.stream.receive(c), { live: false });
      },
      listening: () =>
        listen.then(() => {
          order.push("live");
          gate.goLive();
        }),
      isCancelled: () => false,
      snapshot: async () => {
        order.push("snapshot");
        return gate.snapshotGrid(true);
      },
    });
    for (let i = 0; i < 5; i++) await Promise.resolve();
    expect(order).toEqual(["register"]);
    // A redraw past High while the listener is still coming up: counted,
    // delivered to nobody, and the gate pauses.
    gate.produce(60_000);
    gate.produce(60_000);
    gate.flush();
    expect(gate.gap).toBe(120_000);
    resolveListen();
    const snapshot = await attached;
    expect(order).toEqual(["register", "live", "snapshot"]);
    h.stream.lateAttachSettled(true, snapshot);
    h.settle();
    expect(gate.gap).toBe(0);
    sustainedOutput(gate, h);
    laggingBurst(gate, h);
    expect(gate.gap).toBeLessThanOrEqual(FLOW_LOW_WATERMARK);
    expect(gate.tryEmit(100)).toBe(true);
  });

  it("skips the resetting snapshot when the pane was disposed while it waited", async () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    let disposed = false;
    let resolveListen: () => void = () => {};
    const listen = new Promise<void>((resolve) => (resolveListen = resolve));
    const snapshot = vi.fn(async () => gate.snapshotGrid(true));
    const attached = h.stream.lateAttach({
      register: () => gate.attach((c) => h.stream.receive(c)),
      listening: () => listen,
      isCancelled: () => disposed,
      snapshot,
    });
    // Another pane's gate state that a stray reset would clear.
    gate.emissionSkipped = true;
    disposed = true;
    resolveListen();
    expect(await attached).toBeUndefined();
    expect(snapshot).not.toHaveBeenCalled();
    expect(gate.emissionSkipped).toBe(true);
  });

  it("a snapshot reset issued before the listener is live leaves the gate frozen", () => {
    const gate = new RunnerGate();
    const h = reconnect(gate);
    h.stream.beginLateAttach();
    gate.attach((c) => h.stream.receive(c), { live: false });
    const snapshot = gate.snapshotGrid(true); // too early
    // A redraw past High, counted after the reset and emitted before the
    // listener existed: the gate pauses with nobody to ack it.
    gate.produce(60_000);
    gate.produce(60_000);
    gate.flush();
    gate.goLive();
    h.stream.lateAttachSettled(true, snapshot);
    h.settle();
    // The TUI goes idle after one more line: skipped, so nothing arrives to
    // reveal a gap, and no ack can bring the gap under Low. Frozen.
    gate.produce(100);
    gate.flush();
    h.settle();
    expect(gate.gap).toBe(120_000);
    expect(gate.emissionSkipped).toBe(true);
    expect(gate.resumeMarkers).toBe(0);
    expect(h.hooks.requestResync).not.toHaveBeenCalled();
  });
});

describe("TerminalInstance routes every received byte through PaneOutputStream", () => {
  const source = readFileSync(resolve(__dirname, "TerminalInstance.tsx"), "utf8");
  // Comments mention `backend.write()` in prose; scan code only.
  const code = source
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\*|\/\*)/.test(l))
    .join("\n");

  /** Index of the bracket matching the opener at `open`. */
  function matching(open: number): number {
    const pairs: Record<string, string> = { "{": "}", "(": ")" };
    const close = pairs[code[open]];
    expect(close).toBeDefined();
    let depth = 0;
    for (let i = open; i < code.length; i++) {
      if (code[i] === code[open]) depth++;
      else if (code[i] === close && --depth === 0) return i;
    }
    throw new Error("unbalanced brackets");
  }

  /** The text from `open` (an index of `{` or `(`) through its match. */
  function block(open: number): string {
    return code.slice(open, matching(open) + 1);
  }

  /** The body of the arrow function bound to `const name = (...) => {`. */
  function arrowBody(name: string): string {
    const at = code.indexOf(`const ${name} = `);
    expect(at).toBeGreaterThanOrEqual(0);
    return block(code.indexOf("=> {", at) + 3);
  }

  it("never stages, trims or decodes output itself", () => {
    expect(code).not.toMatch(/\bstageChunk\s*\(/);
    expect(code).not.toMatch(/\.stage\s*\(/);
    expect(code).not.toMatch(/\btrimReplayedChunk\s*\(/);
    expect(code).not.toMatch(/\batob\s*\(/);
  });

  it("writes to the backend directly only from these exact call sites", () => {
    // Every `.write(` in the component, as `receiver.write(first argument`.
    // A live chunk written here would bypass the acked queue, which is the
    // defect class, so a new call site has to be added here on purpose.
    const sites = [...code.matchAll(/([\w$.?]+?)\.write\(\s*([^,)]*)/g)]
      .map((m) => {
        const arg = /^["'`]/.test(m[2]) ? "<literal>" : m[2].trim();
        return `${m[1]}.write(${arg}`;
      })
      .sort();
    expect(sites).toEqual(
      [
        "backendRef.current?.write(<literal>", // input refused: the process exited
        "backendRef.current?.write(data", // writeToDisplay: a parent's text, never PTY output
        "b.write(coalesced", // flushCoalesced: the acked queue's own write
        "b.write(data", // the resync's backend adapter (ringResync.ts)
        "b.write(detail.bytes", // remote earlier-output history
        "b.write(ring.bytes", // remote earlier-output re-render from the local ring
        "backend.write(ring.bytes", // the mount ring replay
        "backend.write(<literal>", // process-exited notice
      ].sort(),
    );
    expect(code).toMatch(/ackQueue\.flush\(/);
  });

  it("the output handler only decodes and hands the chunk to the stream", () => {
    const body = arrowBody("handleOutputPayload");
    expect(body).toMatch(/stream\.receive\(/);
    expect(body).not.toMatch(/\.write\(/);
    expect(body).not.toMatch(/ackQueue/);
  });

  it("no catch on a flow-control path swallows its error, and no new swallow appears", () => {
    // Every promise `.catch(` and every `try`/`catch` block, classified by
    // what it guards and by whether its handler logs or rethrows.
    const handles = (body: string) => /console\.(warn|error)|\bthrow\b|\breturn\b\s+\S/.test(body);
    const swallows: string[] = [];
    for (const m of code.matchAll(/\.catch\(/g)) {
      const open = m.index + ".catch".length;
      const before = code.slice(Math.max(0, m.index - 400), m.index);
      const cmd = [...before.matchAll(/invoke(?:<[^>]*>)?\(\s*"(\w+)"/g)].map((x) => x[1]).pop();
      if (!handles(block(open))) swallows.push(`invoke ${cmd}`);
    }
    for (const m of code.matchAll(/\bcatch\s*(\([^)]*\))?\s*\{/g)) {
      const open = m.index + m[0].length - 1;
      const tryAt = code.lastIndexOf("try {", m.index);
      const guarded = (code.slice(tryAt + 5, m.index).match(/([\w.?]+)\(/) ?? [])[1];
      if (!handles(block(open))) swallows.push(`try ${guarded}`);
    }
    // Best-effort calls whose failure is deliberately ignored. None of them
    // is an ack, a counter reset or a ring read, whose failure can leave a
    // paused gate shut.
    expect(swallows.sort()).toEqual(
      [
        "invoke terminal_resize",
        "invoke terminal_grid_text",
        "invoke terminal_write",
        "invoke terminal_write",
        "invoke terminal_write",
        "invoke terminal_write",
        "invoke terminal_write",
        "invoke terminal_write",
        "invoke terminal_report_bridge_registration_failure",
        "try b.findNext",
        "try backend.fit",
        "try backendRef.current?.dispose",
      ].sort(),
    );
    // The flow-control calls themselves: one each, both logging on failure.
    expect(code.match(/invoke\("terminal_ack"/g)).toHaveLength(1);
    expect(code.match(/invoke\("terminal_flow_reset"/g)).toHaveLength(1);
    expect(arrowBody("flowReset")).toMatch(/\.catch\(\(e\) => \{\s*console\.warn\(/);
  });

  it("a failed ack resyncs, since the runner still counts those bytes", () => {
    expect(arrowBody("sendAck")).toMatch(
      /\.catch\(\(e\) => \{\s*console\.warn\([^;]*\);\s*if \(!disposed && !exitedRef\.current\) void resyncFromRing\(\);/,
    );
    expect(code).toMatch(
      /const resyncFromRing = async \(\) => \{\s*if \(stream\.mayResyncNow\(\)\) await ringResync\.run\(\);\s*\};/,
    );
    expect(code).toMatch(/requestResync: \(\) => void resyncFromRing\(\)/);
  });

  it("the unmount reset goes through flowReset", () => {
    expect(code).toMatch(/flowReset\("unmount"\);/);
  });

  it("the reconnect path attaches, waits for the listener, then fetches and settles", () => {
    // The snapshot (which resets flow control on reconnect) goes through
    // `lateAttach`, which registers, awaits the runner-side listener, and only
    // then fetches (its order is pinned by behaviour above).
    expect(code).toMatch(
      /"terminal_get_grid", \{\s*terminalId,\s*resetFlowControl: isReconnecting,\s*\}/,
    );
    const attach = code.indexOf("await stream.lateAttach({");
    expect(attach).toBeGreaterThanOrEqual(0);
    expect(block(attach + "await stream.lateAttach(".length)).toMatch(
      /^\{\s*register: \(\) => \{\s*outputUnsub = registerTerminalOutputHandler\(terminalId, handleOutputPayload\);\s*\},\s*listening: \(\) => awaitOutputListener\("reconnect"\),\s*isCancelled: \(\) => disposed,\s*snapshot: fetchGrid,\s*\}$/,
    );
    // No other path registers this pane's handler on reconnect without
    // awaiting the listener before the reset its resync will issue.
    const fallback = code.indexOf("stream.beginLateAttach();");
    expect(fallback).toBeGreaterThan(attach);
    expect(code.slice(fallback, fallback + 300)).toMatch(
      /stream\.beginLateAttach\(\);\s*outputUnsub = registerTerminalOutputHandler\(terminalId, handleOutputPayload\);\s*await awaitOutputListener\("reconnect fallback"\);/,
    );
    const settle = code.indexOf(
      "stream.lateAttachSettled(lateAttachPaint.painted, lateAttachPaint.offset);",
      fallback,
    );
    expect(settle).toBeGreaterThan(fallback);
    // A paint behind the mount ring is logged.
    expect(code.slice(settle, settle + 400)).toMatch(/if \(behind > 0\) \{\s*console\.warn\(/);
    // On reconnect, paint only a snapshot that carries its stream offset.
    expect(code).toMatch(/!isReconnecting \|\| offset !== undefined,/);
    expect(code).toMatch(/lateAttachPaint = \{ painted, offset \};/);
  });

  it("a listener that is not live is logged, never silently dropped", () => {
    const body = arrowBody("awaitOutputListener");
    expect(body).toMatch(
      /const live = await terminalOutputListening\(\);\s*if \(!live\) \{\s*console\.warn\(/,
    );
    // Every wait goes through it.
    expect(code.match(/\bterminalOutputListening\(\)/g)).toHaveLength(1);
  });

  it("the cold mount waits for the listener before its ring read resets the counters", () => {
    const read = code.indexOf("const ring = await backend.readScrollbackRing(terminalId);");
    expect(read).toBeGreaterThanOrEqual(0);
    expect(code.slice(read - 300, read)).toMatch(
      /if \(!isReconnecting\) \{\s*await awaitOutputListener\("cold mount"\);\s*if \(disposed\) return;\s*\}\s*$/,
    );
  });

  it("anchors the stream on every mount ring, an empty one included", () => {
    const at = code.indexOf("stream.applyMountRing(ring.endOffset)");
    expect(at).toBeGreaterThanOrEqual(0);
    // Inside `if (ring) {`, and outside `if (ring.bytes.length > 0) {`.
    const ringGuard = code.lastIndexOf("if (ring) {", at);
    expect(ringGuard).toBeGreaterThanOrEqual(0);
    expect(block(ringGuard + "if (ring) ".length)).toContain(
      "stream.applyMountRing(ring.endOffset)",
    );
    const bytesGuard = code.lastIndexOf("if (ring.bytes.length > 0) {", at);
    expect(bytesGuard).toBeGreaterThan(ringGuard);
    expect(block(bytesGuard + "if (ring.bytes.length > 0) ".length)).not.toContain(
      "applyMountRing",
    );
  });

  it("flushes the mount drain right behind the ring replay", () => {
    expect(code).toMatch(/stream\.drainPending\(\);\s*flushCoalesced\(\);/);
  });
});
