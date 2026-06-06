/**
 * Centralized API base URL configuration for the runner frontend.
 *
 * At startup, the actual port is read from the Tauri backend via the
 * `get_api_port` command. All API calls should use these getters instead
 * of hardcoding "http://localhost:9876".
 *
 * Components that need the *current* port reactively (so a late-arriving
 * `setApiPort` causes a re-render) should call `useApiBase()` /
 * `useApiPort()` instead of the bare getters — the getters return whatever
 * value happened to be set when they were called and will not update if
 * the port changes later in the same component.
 */
import { useEffect, useState } from "react";

/**
 * Synchronous port-resolution fast-path. Every webview (the main window AND
 * each pop-out terminal window) is launched with an init script that sets
 * `window.__QONTINUI_PORT__` to the runner's bound API port *before* this
 * bundle evaluates (`main.rs` for main, `commands/terminal_windows.rs` for
 * pop-outs). Seeding `_apiPort` from it here means every realm addresses the
 * correct port from its very first call.
 *
 * Why this matters: each webview is a separate JS realm with its own
 * `_apiPort`. The main window also self-corrects via the `api-ready` Tauri
 * event (it exists when that one-shot event fires at startup), but a pop-out
 * window opened *later* misses that event — so without this seed it stayed on
 * the `9876` default and sent its UI Bridge responses/pongs to the wrong
 * runner (e.g. the primary on 9876 instead of a temp runner on 9877),
 * silently breaking per-window targeting on any non-9876 runner.
 * `useApiReady`/`get_api_port` still override this asynchronously.
 */
function injectedApiPort(): number {
  if (typeof window !== "undefined") {
    const injected = (window as unknown as Record<string, unknown>).__QONTINUI_PORT__;
    const n = Number(injected);
    if (Number.isFinite(n) && n > 0) return n;
  }
  return 9876;
}

let _apiPort: number = injectedApiPort();
let _apiBase: string = `http://localhost:${_apiPort}`;
let _wsBase: string = `ws://localhost:${_apiPort}`;

const portChangeListeners = new Set<() => void>();

export function setApiPort(port: number) {
  if (port === _apiPort) return;
  _apiPort = port;
  _apiBase = `http://localhost:${port}`;
  _wsBase = `ws://localhost:${port}`;
  for (const listener of portChangeListeners) {
    try {
      listener();
    } catch {
      /* swallow — one bad subscriber must not block the rest */
    }
  }
}

export function getApiPort(): number {
  return _apiPort;
}

export function getApiBase(): string {
  return _apiBase;
}

export function getWsBase(): string {
  return _wsBase;
}

/**
 * Resolve the runner HTTP port for local-control-surface traffic
 * (`/file-registry/...`, `/file-locks/...`, `/file-activity/...`, etc.).
 *
 * Resolution order:
 *   1. `window.__QONTINUI_PORT__` if explicitly set — manual-test override
 *      and the fast-path the runner now injects at webview boot (PR #90,
 *      `WebviewWindowBuilder::initialization_script`). On temp/secondary
 *      runners this resolves to the correct port before any page JS runs.
 *   2. `getApiPort()` — the async-populated centralized port from
 *      `useApiReady` via Tauri's `api-ready` event and the `get_api_port`
 *      IPC. Remains the source of truth if the bound port ever differs
 *      from the intended one (port-fallback rare case).
 *
 * The hardcoded `9876` fallback lives inside `getApiPort()`'s default
 * state so it's a single source of truth for the entire frontend.
 */
export function resolvePort(): number {
  if (typeof window === "undefined") return getApiPort();
  const fromGlobal = (window as unknown as Record<string, unknown>).__QONTINUI_PORT__;
  if (fromGlobal) return Number(fromGlobal);
  return getApiPort();
}

/**
 * React hook that returns the current API base URL and re-renders the
 * component whenever `setApiPort` runs. Use this in components whose props
 * (e.g. a polling URL passed to a hook) must track the resolved port —
 * `getApiBase()` alone returns the value at *render* time and won't trigger
 * a re-render when the port resolves asynchronously at startup.
 */
export function useApiBase(): string {
  const [base, setBase] = useState<string>(_apiBase);
  useEffect(() => {
    // Sync immediately in case `setApiPort` already ran between this hook's
    // initial render and effect mount. React bails out on same-value setState,
    // so no conditional check is needed.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional sync on effect mount to catch missed setApiBase calls
    setBase(_apiBase);
    const listener = () => setBase(_apiBase);
    portChangeListeners.add(listener);
    return () => {
      portChangeListeners.delete(listener);
    };
  }, []);
  return base;
}

/**
 * React hook that returns the current API port and re-renders on change.
 * See `useApiBase` for rationale.
 */
export function useApiPort(): number {
  const [port, setPort] = useState<number>(_apiPort);
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional sync on effect mount to catch missed setApiPort calls
    setPort(_apiPort);
    const listener = () => setPort(_apiPort);
    portChangeListeners.add(listener);
    return () => {
      portChangeListeners.delete(listener);
    };
  }, []);
  return port;
}

export { tracedFetch } from "./traced-fetch";

import { tracedFetch } from "./traced-fetch";
import type { BreakpointSnapshot } from "../components/active-dashboard/types";

/**
 * Fetch all breakpoint snapshots for a task run.
 */
export async function fetchBreakpoints(taskRunId: string): Promise<BreakpointSnapshot[]> {
  const response = await tracedFetch(
    `${getApiBase()}/task-runs/${encodeURIComponent(taskRunId)}/breakpoints`,
  );
  if (!response.ok) {
    throw new Error(`Failed to fetch breakpoints: ${response.status}`);
  }
  const data = await response.json();
  return data.snapshots ?? [];
}

/**
 * Fetch a single breakpoint snapshot by ID.
 */
export async function fetchBreakpointDetail(
  taskRunId: string,
  snapshotId: string,
): Promise<BreakpointSnapshot> {
  const response = await tracedFetch(
    `${getApiBase()}/task-runs/${encodeURIComponent(taskRunId)}/breakpoints/${encodeURIComponent(snapshotId)}`,
  );
  if (!response.ok) {
    throw new Error(`Failed to fetch breakpoint detail: ${response.status}`);
  }
  return response.json();
}

/**
 * Resume a paused breakpoint, allowing execution to continue.
 */
export async function resumeBreakpoint(taskRunId: string, snapshotId: string): Promise<void> {
  const response = await tracedFetch(
    `${getApiBase()}/task-runs/${encodeURIComponent(taskRunId)}/breakpoints/${encodeURIComponent(snapshotId)}/resume`,
    { method: "POST" },
  );
  if (!response.ok) {
    throw new Error(`Failed to resume breakpoint: ${response.status}`);
  }
}
