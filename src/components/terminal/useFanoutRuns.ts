/**
 * Live fan-out run list for the status strip.
 *
 * Two sources, deliberately both: the runner's `fanout-changed` Tauri event
 * (a full `RunView` per change — local only) and a `GET /fanout` poll as the
 * fallback the event cannot be, because an event missed while the listener was
 * registering, or a run changed by another window, would otherwise never
 * arrive. A poll that fails moves the state to UNKNOWN (with the error) — it
 * never leaves a stale list looking current, and never reads as "no runs".
 *
 * All derivation lives in `fanoutStripModel.ts`; this hook only moves data.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { createLogger } from "@/lib/logger";

import {
  FANOUT_CHANGED_EVENT,
  cancelFanoutQueued,
  isFanoutRunView,
  listFanoutRuns,
  releaseFanoutMember,
  setFanoutMaxConcurrent,
  type FanoutCapOutcome,
  type FanoutResult,
  type FanoutRunView,
} from "./fanoutApi";
import {
  createFreshnessGate,
  mergeRunUpdate,
  readStateFromResult,
  type FanoutReadState,
} from "./fanoutStripModel";

const logger = createLogger("FanoutRuns");

/** Fallback poll cadence. The event is the fast path; this bounds staleness. */
export const FANOUT_POLL_MS = 5_000;

export interface FanoutRunsApi {
  state: FanoutReadState;
  refresh: () => Promise<void>;
  cancelQueued: (runId: string) => Promise<FanoutResult<FanoutRunView>>;
  release: (runId: string, index: number) => Promise<FanoutResult<FanoutRunView>>;
  setCap: (runId: string, maxConcurrent: number) => Promise<FanoutResult<FanoutCapOutcome>>;
}

export function useFanoutRuns(): FanoutRunsApi {
  const [state, setState] = useState<FanoutReadState>({ kind: "loading" });
  const mountedRef = useRef(true);
  const gateRef = useRef(createFreshnessGate());

  /** Merge one changed run; stamp the state only when it actually changed. */
  const applyRunUpdate = useCallback((run: FanoutRunView) => {
    setState((prev) => {
      const next = mergeRunUpdate(prev, run);
      if (next !== prev) gateRef.current.markApplied();
      return next;
    });
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    const stamp = gateRef.current.begin();
    const result = await listFanoutRuns();
    if (!mountedRef.current) return;
    // Read before a change the state already shows: newer state wins.
    if (!gateRef.current.acceptPoll(stamp)) return;
    setState(readStateFromResult(result));
  }, []);

  // Fallback poll (also the initial read).
  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), FANOUT_POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  // Fast path: one changed run per event.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    listen<unknown>(FANOUT_CHANGED_EVENT, (event) => {
      if (!isFanoutRunView(event.payload)) {
        logger.warn("fanout-changed: payload had an unexpected shape; waiting for the poll");
        return;
      }
      applyRunUpdate(event.payload);
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((err) => {
        logger.warn(`Failed to subscribe to ${FANOUT_CHANGED_EVENT}:`, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [applyRunUpdate]);

  const applyRun = useCallback(
    <T>(result: FanoutResult<T>, pick: (data: T) => FanoutRunView) => {
      if (result.ok && mountedRef.current) applyRunUpdate(pick(result.data));
      return result;
    },
    [applyRunUpdate],
  );

  const cancelQueued = useCallback(
    async (runId: string) => applyRun(await cancelFanoutQueued(runId), (r) => r),
    [applyRun],
  );
  const release = useCallback(
    async (runId: string, index: number) =>
      applyRun(await releaseFanoutMember(runId, index), (r) => r),
    [applyRun],
  );
  const setCap = useCallback(
    async (runId: string, maxConcurrent: number) =>
      applyRun(await setFanoutMaxConcurrent(runId, maxConcurrent), (o) => o.run),
    [applyRun],
  );

  return { state, refresh, cancelQueued, release, setCap };
}
