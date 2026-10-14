/**
 * The perf-harness frontend probe, shipped in the bundle (plan
 * `2026-10-14-perf-harness-frontend-probe-evaluates-strings-the-csp-refuses`,
 * Phase 1).
 *
 * `scripts/perf-harness.mjs` measures the Terminal page's frontend: rAF frame
 * gaps, long tasks, webview `terminal-output` delivery counts, the IPC
 * custom-protocol → `postMessage` fallback, and the
 * `window.__qontinuiTransportStats` counters (`transportStats.ts`). It used to
 * send that logic as JavaScript strings to `POST /ui-bridge/control/page/evaluate`
 * — and the runner's CSP (`script-src 'self'`, no `'unsafe-eval'`) refuses
 * every evaluated string wherever the webview enforces it (WebKitGTK on
 * linux-xvfb, qontinui-runner#2217 run 38619704449), so every frontend number
 * was UNKNOWN there.
 *
 * Here the same logic is code, reached through the `perf-probe` UI Bridge
 * component (`usePerfProbeComponent`) over the eval-free component-action door
 * `POST /ui-bridge/control/component/perf-probe/action/{action}`. Each action
 * answers EXACTLY the JSON the evaluated expression answered, so
 * `perf-harness-lib.mjs`'s folds (`foldFrontendSample`,
 * `normalizeFrontendTransport`, `foldTransport`) are unchanged.
 *
 * Nothing runs until `install`: no observer, no rAF loop, no hook. `stop`
 * removes all of them. So the probe ships enabled, with no flag.
 *
 * The probe stores raw samples and does no maths — percentiles are computed in
 * Node by the unit-tested `perf-harness-lib` so there is one implementation.
 */

import { listen as tauriListen } from "@tauri-apps/api/event";

import type { TransportStats } from "@/components/terminal/transportStats";

/** Ring caps, as the evaluated probe had them. */
export const PERF_PROBE_RING_CAP = 20000;

/** The component id perf-harness addresses. */
export const PERF_PROBE_COMPONENT_ID = "perf-probe";

/** The ambient APIs the probe touches, injectable so it is testable in node. */
export interface PerfProbeEnv {
  now: () => number;
  requestAnimationFrame: (cb: () => void) => number;
  cancelAnimationFrame: (handle: number) => void;
  /** `undefined` when the engine has no `PerformanceObserver`. */
  PerformanceObserver?: typeof PerformanceObserver;
  console: { warn: (...args: unknown[]) => void };
  /** Tauri's `window.ipc` (the postMessage transport), when present. */
  getIpc: () => { postMessage?: (...args: unknown[]) => unknown } | undefined;
  listen: (event: string, handler: () => void) => Promise<() => void>;
  getTransportStats: () => TransportStats | undefined;
}

export interface PerfProbeInstallResult {
  installed: true;
  longTaskObserver: boolean;
  errors: string[];
}

export interface PerfProbeSample {
  elapsedMs: number;
  frameGapsMs: number[];
  longTasks: Array<{ start: number; duration: number }>;
  webviewOutputEvents: number;
  webviewHooked: boolean;
  errors: string[];
  ipc: { fallbackWarned: boolean; postMessageHooked: boolean; postMessageCalls: number };
}

export type NotInstalled = { error: "probe-not-installed" };

export interface TransportSample {
  available: true;
  stats: {
    enabled: boolean;
    sinceResetMs: number;
    decodeNs: { pane: number; tap: number };
    decodeCalls: { pane: number; tap: number };
    decodeBytes: { pane: number; tap: number };
    writeToRenderNs: number;
    writeToRenderCount: number;
    writeToRenderBytes: number;
    eventsDelivered: number;
    eventsForeign: number;
    ringReplay: { fetches: number; bytesFetched: number; bytesWritten: number };
    rawIpc: boolean | null;
  };
}

export interface PerfProbe {
  install(): PerfProbeInstallResult;
  reset(): { reset: true } | NotInstalled;
  sample(): PerfProbeSample | NotInstalled;
  stop(): { stopped: boolean };
  transportReset(): { available: boolean };
  transportSample(): TransportSample | { available: false };
}

/** The live webview's ambient APIs. Read lazily so importing this is side-effect free. */
export function browserPerfProbeEnv(): PerfProbeEnv {
  return {
    now: () => performance.now(),
    requestAnimationFrame: (cb) => window.requestAnimationFrame(cb),
    cancelAnimationFrame: (h) => window.cancelAnimationFrame(h),
    PerformanceObserver:
      typeof PerformanceObserver === "undefined" ? undefined : PerformanceObserver,
    console,
    getIpc: () =>
      (window as unknown as { ipc?: { postMessage?: (...args: unknown[]) => unknown } }).ipc,
    listen: (event, handler) => tauriListen(event, handler),
    getTransportStats: () => window.__qontinuiTransportStats,
  };
}

interface ProbeState {
  startedAt: number;
  last: number;
  frameGapsMs: number[];
  longTasks: Array<{ start: number; duration: number }>;
  webviewOutputEvents: number;
  webviewHooked: boolean;
  errors: string[];
  ipc: PerfProbeSample["ipc"];
  raf: number;
  rafLive: boolean;
  obs: PerformanceObserver | null;
  unlisten: (() => void) | null;
  stopped: boolean;
  restore: Array<() => void>;
}

function pushCapped<T>(ring: T[], value: T): void {
  ring.push(value);
  if (ring.length > PERF_PROBE_RING_CAP) ring.shift();
}

/**
 * Build a probe over `env`. The probe holds at most one installed state; a
 * second `install` tears the first down, as the evaluated probe did.
 */
export function createPerfProbe(envFactory: () => PerfProbeEnv = browserPerfProbeEnv): PerfProbe {
  let state: ProbeState | null = null;
  let env: PerfProbeEnv | null = null;
  const getEnv = (): PerfProbeEnv => (env ??= envFactory());

  const teardown = (s: ProbeState, e: PerfProbeEnv): void => {
    s.stopped = true;
    s.rafLive = false;
    try {
      e.cancelAnimationFrame(s.raf);
    } catch (err) {
      s.errors.push("stop-raf: " + String(err));
    }
    try {
      s.obs?.disconnect();
    } catch (err) {
      s.errors.push("stop-observer: " + String(err));
    }
    try {
      s.unlisten?.();
    } catch (err) {
      s.errors.push("stop-unlisten: " + String(err));
    }
    for (const r of s.restore) {
      try {
        r();
      } catch (err) {
        s.errors.push("stop-restore: " + String(err));
      }
    }
    s.restore.length = 0;
  };

  return {
    install() {
      const e = getEnv();
      if (state) teardown(state, e);
      const now = e.now();
      const s: ProbeState = {
        startedAt: now,
        last: now,
        frameGapsMs: [],
        longTasks: [],
        webviewOutputEvents: 0,
        webviewHooked: false,
        errors: [],
        ipc: { fallbackWarned: false, postMessageHooked: false, postMessageCalls: 0 },
        raf: 0,
        rafLive: true,
        obs: null,
        unlisten: null,
        stopped: false,
        restore: [],
      };
      state = s;

      // Tauri keeps its custom-protocol -> postMessage flip in a closure-private
      // flag and only logs it (ipc-protocol.js): watch the warning, and count
      // window.ipc.postMessage calls when the object lets us wrap it.
      try {
        const con = e.console;
        const origWarn = con.warn;
        con.warn = function (this: unknown, ...args: unknown[]) {
          if (typeof args[0] === "string" && args[0].includes("IPC custom protocol failed")) {
            s.ipc.fallbackWarned = true;
          }
          return origWarn.apply(this, args);
        };
        s.restore.push(() => {
          con.warn = origWarn;
        });
      } catch (err) {
        s.errors.push("warn-hook: " + String(err));
      }
      try {
        const ipc = e.getIpc();
        if (ipc && typeof ipc.postMessage === "function") {
          const origPost = ipc.postMessage;
          const wrapped = function (this: unknown, ...args: unknown[]) {
            s.ipc.postMessageCalls += 1;
            return origPost.apply(this, args);
          };
          try {
            ipc.postMessage = wrapped;
          } catch (err) {
            s.errors.push("ipc-hook-frozen: " + String(err));
          }
          if (ipc.postMessage === wrapped) {
            s.ipc.postMessageHooked = true;
            s.restore.push(() => {
              ipc.postMessage = origPost;
            });
          }
        }
      } catch (err) {
        s.errors.push("ipc-hook: " + String(err));
      }
      try {
        if (!e.PerformanceObserver) throw new Error("PerformanceObserver is undefined");
        const obs = new e.PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            pushCapped(s.longTasks, { start: entry.startTime, duration: entry.duration });
          }
        });
        obs.observe({ entryTypes: ["longtask"] });
        s.obs = obs;
      } catch (err) {
        s.errors.push("longtask-observer: " + String(err));
      }
      const tick = (): void => {
        if (!s.rafLive) return;
        const t = e.now();
        pushCapped(s.frameGapsMs, t - s.last);
        s.last = t;
        s.raf = e.requestAnimationFrame(tick);
      };
      s.raf = e.requestAnimationFrame(tick);
      try {
        e.listen("terminal-output", () => {
          s.webviewOutputEvents += 1;
        }).then(
          (un) => {
            // A stop that ran before the listen settled must still unlisten.
            if (s.stopped) {
              un();
              return;
            }
            s.unlisten = un;
            s.webviewHooked = true;
          },
          (err: unknown) => {
            s.errors.push("listen: " + String(err));
          },
        );
      } catch (err) {
        s.errors.push("tauri-hook: " + String(err));
      }
      return { installed: true, longTaskObserver: s.obs !== null, errors: s.errors };
    },

    reset() {
      if (!state) return { error: "probe-not-installed" };
      const t = getEnv().now();
      state.startedAt = t;
      state.last = t;
      state.frameGapsMs.length = 0;
      state.longTasks.length = 0;
      state.webviewOutputEvents = 0;
      state.ipc.postMessageCalls = 0;
      return { reset: true };
    },

    sample() {
      if (!state) return { error: "probe-not-installed" };
      return {
        elapsedMs: getEnv().now() - state.startedAt,
        frameGapsMs: state.frameGapsMs.slice(),
        longTasks: state.longTasks.slice(),
        webviewOutputEvents: state.webviewOutputEvents,
        webviewHooked: state.webviewHooked,
        errors: state.errors.slice(),
        ipc: { ...state.ipc },
      };
    },

    stop() {
      if (!state) return { stopped: false };
      teardown(state, getEnv());
      state = null;
      return { stopped: true };
    },

    transportReset() {
      const t = getEnv().getTransportStats();
      if (!t || typeof t.reset !== "function") return { available: false };
      t.reset();
      return { available: true };
    },

    transportSample() {
      const e = getEnv();
      const t = e.getTransportStats();
      if (!t) return { available: false };
      return {
        available: true,
        stats: {
          enabled: t.enabled,
          sinceResetMs: e.now() - t.resetAt,
          decodeNs: { ...t.decodeNs },
          decodeCalls: { ...t.decodeCalls },
          decodeBytes: { ...t.decodeBytes },
          writeToRenderNs: t.writeToRenderNs,
          writeToRenderCount: t.writeToRenderCount,
          writeToRenderBytes: t.writeToRenderBytes,
          eventsDelivered: t.eventsDelivered,
          eventsForeign: t.eventsForeign,
          ringReplay: { ...t.ringReplay },
          rawIpc: t.rawIpc,
        },
      };
    },
  };
}

/** The action shape `useUIComponent({ actions })` takes, with `effect` required. */
export interface PerfProbeActionDef {
  id: string;
  label: string;
  description: string;
  effect: "read" | "write" | "destructive";
  handler: () => Promise<unknown>;
}

/**
 * The `perf-probe` component's six actions. A list factory (rather than an
 * inline array) so the handlers are testable under vitest's node environment;
 * `action-effect-coverage.test.ts` follows the call to this literal.
 *
 * Every handler is arity-0: none takes params, so no caller bag can reach one
 * (`actionSurfaces.ts`).
 *
 * Effect tiers (`src-tauri/src/mcp/ui_bridge/CONTRACT.md`, "The `effect`
 * classification rubric"): `read` is about PERSISTENCE. The two samples are
 * pure queries. The four that start, zero or tear down a collection window
 * persist nothing, but each DISCARDS in-memory samples (or counters another
 * reader may be mid-window on) that cannot be reconstructed afterwards —
 * dimension 1 — so they are `write`, not `read`.
 */
export function buildPerfProbeActions(probe: PerfProbe): PerfProbeActionDef[] {
  return [
    {
      id: "install",
      label: "Install Perf Probe",
      description:
        "Start collecting rAF frame gaps, long tasks, terminal-output delivery counts and the " +
        "IPC postMessage fallback. Replaces (and discards) a probe already installed. " +
        "Answers {installed, longTaskObserver, errors}.",
      // `write` — in-memory only, but replacing an installed probe discards its samples.
      effect: "write",
      handler: async () => probe.install(),
    },
    {
      id: "reset",
      label: "Reset Perf Probe Window",
      description:
        "Restart the probe's collection window without reinstalling the observers. " +
        "Answers {reset: true}, or {error: 'probe-not-installed'}.",
      // `write` — discards the collected samples; nothing persisted.
      effect: "write",
      handler: async () => probe.reset(),
    },
    {
      id: "sample",
      label: "Sample Perf Probe",
      description:
        "Read the probe's raw samples: {elapsedMs, frameGapsMs, longTasks, webviewOutputEvents, " +
        "webviewHooked, errors, ipc}, or {error: 'probe-not-installed'}.",
      // `read` — a copy of the samples; nothing changes.
      effect: "read",
      handler: async () => probe.sample(),
    },
    {
      id: "stop",
      label: "Stop Perf Probe",
      description:
        "Remove every observer, hook and the rAF loop the probe installed. Answers {stopped}.",
      // `write` — in-memory only, but the collected samples are discarded.
      effect: "write",
      handler: async () => probe.stop(),
    },
    {
      id: "transport-reset",
      label: "Reset Transport Counters",
      description:
        "Zero window.__qontinuiTransportStats and restart its window. Answers {available} " +
        "(false on a build without the object).",
      // `write` — zeroes counters another reader may be measuring with; nothing persisted.
      effect: "write",
      handler: async () => probe.transportReset(),
    },
    {
      id: "transport-sample",
      label: "Sample Transport Counters",
      description:
        "Snapshot window.__qontinuiTransportStats as plain JSON: {available, stats}.",
      // `read` — a snapshot of the counters; nothing changes.
      effect: "read",
      handler: async () => probe.transportSample(),
    },
  ];
}

/** The window's one probe, so a re-render never orphans an installed state. */
export const perfProbe: PerfProbe = createPerfProbe();
