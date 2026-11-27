/**
 * The runner's scan for `claude` processes it does not own
 * (`transcript_find_external_processes`), as one webview-wide store.
 *
 * The Session Manager used to keep only the COUNT of these processes and let
 * that count turn any recently-touched transcript "Active (External)". The
 * per-session match now comes from Claude Code's registry
 * (`useLiveClaudeSessions`); this scan answers two narrower questions:
 *
 *  - **Could a row with no registry match still be running?** Only if the scan
 *    could not look. A `degraded` (or not yet answered) scan places such a row
 *    `Unknown`, never `Not running`.
 *  - **How many unmanaged `claude` processes have no registry row at all?**
 *    Shown as "N unidentified Claude processes", never attributed to a session
 *    (`countUnidentified`).
 *
 * One ref-counted poller for every mounted page scope, in the shape of
 * `useLiveClaudeSessions`: on Windows each scan is a `Get-CimInstance
 * Win32_Process` spawn, so N per-scope pollers would be N spawns per period.
 */

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { CommandResponse } from "./types";
import type { ExternalScanState } from "./sessionStatusModel";

const SCAN_POLL_MS = 30_000;

/** `transcript_find_external_processes`' `data` on success. */
interface ExternalProcessesData {
  processes: { pid: number; working_directory: string | null }[];
  /**
   * The runner skipped the enumeration under memory pressure and returned its
   * last COMPLETE result — a labelled previous answer, never an empty list.
   */
  stale: boolean;
}

export interface ExternalClaudeScan {
  state: ExternalScanState;
  /** Unmanaged `claude` pids from the last answer (empty while pending). */
  pids: readonly number[];
}

const PENDING: ExternalClaudeScan = { state: "pending", pids: [] };

let current: ExternalClaudeScan = PENDING;
const subscribers = new Set<() => void>();
let refCount = 0;
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

function publish(next: ExternalClaudeScan): void {
  const samePids =
    next.pids.length === current.pids.length && next.pids.every((p, i) => p === current.pids[i]);
  if (next.state === current.state && samePids) return;
  current = { state: next.state, pids: samePids ? current.pids : next.pids };
  for (const notify of subscribers) notify();
}

/**
 * Interpret one command response. Exported for tests.
 *
 * `success: false` is the backend saying the enumeration DEGRADED (its probe
 * was killed at its budget, or it was shed with no recent answer) — UNKNOWN,
 * not zero. A shape not understood is the same.
 */
export function readExternalScan(res: CommandResponse | null | undefined): ExternalClaudeScan {
  if (!res?.success) return { state: "degraded", pids: [] };
  const data = res.data as Partial<ExternalProcessesData> | null | undefined;
  if (!data || !Array.isArray(data.processes)) return { state: "degraded", pids: [] };
  const pids = data.processes
    .map((p) => p?.pid)
    .filter((p): p is number => typeof p === "number")
    .sort((a, b) => a - b);
  return { state: data.stale ? "stale" : "ok", pids };
}

/** Re-run the scan now and publish the result if it changed. */
export async function refreshExternalClaudeScan(): Promise<void> {
  // A scan can outlive the interval on a loaded box; an overlapping pair
  // could land out of order.
  if (inFlight) return;
  inFlight = true;
  try {
    publish(readExternalScan(await invoke<CommandResponse>("transcript_find_external_processes")));
  } catch {
    // Transport failure: nothing is known about external processes.
    publish({ state: "degraded", pids: [] });
  } finally {
    inFlight = false;
  }
}

function subscribe(onStoreChange: () => void): () => void {
  subscribers.add(onStoreChange);
  if (++refCount === 1) {
    void refreshExternalClaudeScan();
    timer = setInterval(() => void refreshExternalClaudeScan(), SCAN_POLL_MS);
  }
  return () => {
    subscribers.delete(onStoreChange);
    if (--refCount === 0 && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

function getSnapshot(): ExternalClaudeScan {
  return current;
}

/** The latest external-process scan. Identity is stable while unchanged. */
export function useExternalClaudeScan(): ExternalClaudeScan {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
