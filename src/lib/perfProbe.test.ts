/**
 * The bundled perf-harness probe (`perfProbe.ts`) — plan
 * `2026-10-14-perf-harness-frontend-probe-evaluates-strings-the-csp-refuses`.
 *
 * Driven over an injected env (a manual rAF queue, a fake PerformanceObserver,
 * a fake `listen`), so it runs under vitest's node environment. Every shape
 * asserted here is one `scripts/perf-harness-lib.mjs` folds: a drift here is a
 * fold that silently reads nothing.
 */

import { describe, expect, it, vi } from "vitest";

import {
  buildPerfProbeActions,
  createPerfProbe,
  PERF_PROBE_RING_CAP,
  type PerfProbeEnv,
} from "./perfProbe";
import type { TransportStats } from "@/components/terminal/transportStats";

function harness(over: Partial<PerfProbeEnv> = {}) {
  let clock = 1000;
  const frames: Array<() => void> = [];
  let observerCb: ((list: { getEntries(): Array<{ startTime: number; duration: number }> }) => void) | null =
    null;
  const disconnect = vi.fn();
  const observe = vi.fn();
  class FakeObserver {
    constructor(cb: typeof observerCb) {
      observerCb = cb;
    }
    observe = observe;
    disconnect = disconnect;
  }
  let outputHandler: (() => void) | null = null;
  const unlisten = vi.fn();
  const listen = vi.fn(async (_event: string, handler: () => void) => {
    outputHandler = handler;
    return unlisten;
  });
  const con = { warn: vi.fn() };
  const origWarn = con.warn;
  const ipc = { postMessage: vi.fn() as (...args: unknown[]) => unknown };
  const origPost = ipc.postMessage;
  const cancel = vi.fn();
  const env: PerfProbeEnv = {
    now: () => clock,
    requestAnimationFrame: (cb) => {
      frames.push(cb);
      return frames.length;
    },
    cancelAnimationFrame: cancel,
    PerformanceObserver: FakeObserver as unknown as typeof PerformanceObserver,
    console: con,
    getIpc: () => ipc,
    listen,
    getTransportStats: () => undefined,
    ...over,
  };
  const probe = createPerfProbe(() => env);
  /** Advance the clock by `ms` and run the one queued frame. */
  const frame = (ms: number) => {
    clock += ms;
    const cb = frames.shift();
    if (!cb) throw new Error("no frame queued");
    cb();
  };
  return {
    probe,
    frame,
    frames,
    advance: (ms: number) => (clock += ms),
    longTask: (startTime: number, duration: number) =>
      observerCb?.({ getEntries: () => [{ startTime, duration }] }),
    output: () => outputHandler?.(),
    con,
    origWarn,
    ipc,
    origPost,
    observe,
    disconnect,
    unlisten,
    cancel,
    listen,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("perfProbe", () => {
  it("does nothing until install", () => {
    const h = harness();
    expect(h.probe.sample()).toEqual({ error: "probe-not-installed" });
    expect(h.probe.reset()).toEqual({ error: "probe-not-installed" });
    expect(h.probe.stop()).toEqual({ stopped: false });
    expect(h.frames).toHaveLength(0);
    expect(h.listen).not.toHaveBeenCalled();
  });

  it("install answers the evaluated probe's shape", () => {
    const h = harness();
    expect(h.probe.install()).toEqual({ installed: true, longTaskObserver: true, errors: [] });
    expect(h.observe).toHaveBeenCalledWith({ entryTypes: ["longtask"] });
    expect(h.listen).toHaveBeenCalledWith("terminal-output", expect.any(Function));
  });

  it("a second install tears the first down: one rAF chain, console and ipc restored once on stop", async () => {
    const h = harness();
    h.probe.install();
    await flush();
    h.probe.install();
    await flush();
    // The first install's listener is released by the second; each queued
    // frame belongs to a live chain, and a stale chain does not re-arm.
    expect(h.unlisten).toHaveBeenCalledTimes(1);
    h.frame(16);
    h.frame(16);
    expect(h.frames).toHaveLength(1);
    expect(h.probe.stop()).toEqual({ stopped: true });
    expect(h.unlisten).toHaveBeenCalledTimes(2);
    expect(h.con.warn).toBe(h.origWarn);
    expect(h.ipc.postMessage).toBe(h.origPost);
  });

  it("records rAF frame gaps", () => {
    const h = harness();
    h.probe.install();
    h.frame(16);
    h.frame(17);
    h.frame(50);
    const s = h.probe.sample();
    expect("error" in s).toBe(false);
    if ("error" in s) return;
    expect(s.frameGapsMs).toEqual([16, 17, 50]);
  });

  it("records long tasks, terminal-output deliveries and the IPC fallback", async () => {
    const h = harness();
    h.probe.install();
    await flush();
    h.longTask(5, 60);
    h.longTask(80, 120);
    h.output();
    h.output();
    h.con.warn("IPC custom protocol failed, Tauri will now use the postMessage interface instead");
    h.ipc.postMessage("x");
    h.advance(250);
    expect(h.probe.sample()).toEqual({
      elapsedMs: 250,
      frameGapsMs: [],
      longTasks: [
        { start: 5, duration: 60 },
        { start: 80, duration: 120 },
      ],
      webviewOutputEvents: 2,
      webviewHooked: true,
      errors: [],
      ipc: { fallbackWarned: true, postMessageHooked: true, postMessageCalls: 1 },
    });
    // The page's own logging still reaches the original console.warn.
    expect(h.origWarn).toHaveBeenCalledTimes(1);
    expect(h.origPost).toHaveBeenCalledWith("x");
  });

  it("caps each ring at the evaluated probe's 20000", () => {
    const h = harness();
    h.probe.install();
    for (let i = 0; i < PERF_PROBE_RING_CAP + 5; i += 1) h.longTask(i, 51);
    const s = h.probe.sample();
    if ("error" in s) throw new Error(s.error);
    expect(s.longTasks).toHaveLength(PERF_PROBE_RING_CAP);
    expect(s.longTasks[0].start).toBe(5);
  });

  it("reset restarts the window without reinstalling", async () => {
    const h = harness();
    h.probe.install();
    await flush();
    h.frame(20);
    h.longTask(1, 70);
    h.output();
    h.advance(100);
    expect(h.probe.reset()).toEqual({ reset: true });
    const s = h.probe.sample();
    if ("error" in s) throw new Error(s.error);
    expect(s).toMatchObject({ elapsedMs: 0, frameGapsMs: [], longTasks: [], webviewOutputEvents: 0 });
    // The first gap after a reset is measured from the reset, not the last frame.
    h.frame(9);
    const t = h.probe.sample();
    if ("error" in t) throw new Error(t.error);
    expect(t.frameGapsMs).toEqual([9]);
    expect(h.listen).toHaveBeenCalledTimes(1);
  });

  it("stop removes every observer and hook", async () => {
    const h = harness();
    h.probe.install();
    await flush();
    expect(h.con.warn).not.toBe(h.origWarn);
    expect(h.probe.stop()).toEqual({ stopped: true });
    expect(h.disconnect).toHaveBeenCalled();
    expect(h.unlisten).toHaveBeenCalled();
    expect(h.cancel).toHaveBeenCalled();
    expect(h.con.warn).toBe(h.origWarn);
    expect(h.ipc.postMessage).toBe(h.origPost);
    // The queued frame no longer re-arms the loop.
    h.frame(16);
    expect(h.frames).toHaveLength(0);
    expect(h.probe.sample()).toEqual({ error: "probe-not-installed" });
  });

  it("a stop before listen settles still unlistens", async () => {
    const h = harness();
    h.probe.install();
    h.probe.stop();
    await flush();
    expect(h.unlisten).toHaveBeenCalled();
  });

  it("reports an engine without PerformanceObserver instead of throwing", () => {
    const h = harness({ PerformanceObserver: undefined });
    const r = h.probe.install();
    expect(r.longTaskObserver).toBe(false);
    expect(r.errors[0]).toMatch(/^longtask-observer: /);
  });

  it("records a refused listen as an error and leaves webviewHooked false", async () => {
    const h = harness({ listen: () => Promise.reject(new Error("no tauri")) });
    h.probe.install();
    await flush();
    const s = h.probe.sample();
    if ("error" in s) throw new Error(s.error);
    expect(s.webviewHooked).toBe(false);
    expect(s.errors).toEqual(["listen: Error: no tauri"]);
  });

  it("transport actions answer {available:false} on a build without the counters", () => {
    const h = harness();
    expect(h.probe.transportReset()).toEqual({ available: false });
    expect(h.probe.transportSample()).toEqual({ available: false });
  });

  it("transport actions reset and snapshot window.__qontinuiTransportStats as plain JSON", () => {
    let clock = 500;
    const stats: TransportStats = {
      enabled: true,
      resetAt: 100,
      decodeNs: { pane: 1, tap: 2 },
      decodeCalls: { pane: 3, tap: 4 },
      decodeBytes: { pane: 5, tap: 6 },
      writeToRenderNs: 7,
      writeToRenderCount: 8,
      writeToRenderBytes: 9,
      eventsDelivered: 10,
      eventsForeign: 11,
      ringReplay: { fetches: 1, bytesFetched: 2, bytesWritten: 3 },
      rawIpc: null,
      reset: vi.fn(),
    };
    const h = harness({ getTransportStats: () => stats, now: () => clock });
    expect(h.probe.transportReset()).toEqual({ available: true });
    expect(stats.reset).toHaveBeenCalledTimes(1);
    clock = 1500;
    const snap = h.probe.transportSample();
    expect(snap).toEqual({
      available: true,
      stats: {
        enabled: true,
        sinceResetMs: 1400,
        decodeNs: { pane: 1, tap: 2 },
        decodeCalls: { pane: 3, tap: 4 },
        decodeBytes: { pane: 5, tap: 6 },
        writeToRenderNs: 7,
        writeToRenderCount: 8,
        writeToRenderBytes: 9,
        eventsDelivered: 10,
        eventsForeign: 11,
        ringReplay: { fetches: 1, bytesFetched: 2, bytesWritten: 3 },
        rawIpc: null,
      },
    });
    // A copy, not the live object: a later count does not move the snapshot.
    stats.decodeNs.pane = 99;
    if (!snap.available) throw new Error("unavailable");
    expect(snap.stats.decodeNs.pane).toBe(1);
  });
});

describe("buildPerfProbeActions", () => {
  it("registers the six actions perf-harness drives, each with an effect", async () => {
    const h = harness();
    const actions = buildPerfProbeActions(h.probe);
    expect(actions.map((a) => a.id)).toEqual([
      "install",
      "reset",
      "sample",
      "stop",
      "transport-reset",
      "transport-sample",
    ]);
    const effects = Object.fromEntries(actions.map((a) => [a.id, a.effect]));
    expect(effects).toEqual({
      install: "write",
      reset: "write",
      sample: "read",
      stop: "write",
      "transport-reset": "write",
      "transport-sample": "read",
    });
    for (const a of actions) expect(a.handler.length).toBe(0);
  });

  it("each action answers its probe method's shape", async () => {
    const h = harness();
    const byId = Object.fromEntries(buildPerfProbeActions(h.probe).map((a) => [a.id, a]));
    expect(await byId.sample.handler()).toEqual({ error: "probe-not-installed" });
    expect(await byId.install.handler()).toEqual({ installed: true, longTaskObserver: true, errors: [] });
    expect(await byId.reset.handler()).toEqual({ reset: true });
    expect(await byId.sample.handler()).toMatchObject({ elapsedMs: 0, frameGapsMs: [], longTasks: [] });
    expect(await byId["transport-reset"].handler()).toEqual({ available: false });
    expect(await byId["transport-sample"].handler()).toEqual({ available: false });
    expect(await byId.stop.handler()).toEqual({ stopped: true });
  });
});
