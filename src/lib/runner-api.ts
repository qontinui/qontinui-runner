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

let _apiPort: number = 9876;
let _apiBase: string = "http://localhost:9876";
let _wsBase: string = "ws://localhost:9876";

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
