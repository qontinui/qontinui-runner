/**
 * Tracks per-session commit state for terminal tabs.
 *
 * Mirrors the shape of `useFileLockTracking.ts`:
 *
 *   1. Tauri events (`commit-state-changed`) push fresh `CommitState`
 *      payloads when `auto_register_file` (SDK path), the
 *      transcript-tail watcher (PTY path), or `commit_session_progress_inner`
 *      detects a change. Payload uses snake_case (`task_run_id`, `state`)
 *      explicitly to dodge Tauri's camelCase serde rename trap — see
 *      `proj_tauri_event_payload_camelcase.md` in user memory and the
 *      shipped emitter at `mcp/ai_session.rs::emit_commit_state_for_session`.
 *
 *   2. A 30-second background poll calls
 *      `invoke<CommandResponse>("get_session_commit_state", { taskRunId })`
 *      for every tab carrying a `claudeSessionId`, coalesced via
 *      `Promise.allSettled` so one failure doesn't stall the rest.
 *      This catches external edits (VS Code, manual `git checkout`) the
 *      event surface never sees.
 *
 *   3. Stale-resilience: if a tab's last-known `state.generated_at_ms` is
 *      older than 5 minutes vs `Date.now()`, the rendered status is
 *      pinned to `"unknown"` to avoid showing a misleading green dot
 *      after the runner sleeps. The cap is computed at render time, NOT
 *      mutated into the stored map (so a fresh event/poll snaps it back
 *      automatically).
 *
 * Frontend keys the state map by `tab.id` (so React-side lookups stay
 * stable across `claudeSessionId` changes) but uses `tab.claudeSessionId`
 * for the backend probe and event resolution. PTY tabs only get a
 * `claudeSessionId` once the post-spawn capture hook fills one in
 * (see `useTabSessionIdCapture.ts`); tabs without one are skipped both
 * by polling and event resolution.
 */

import { useState, useEffect, useRef, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type { TerminalTab } from "./useTerminalManager";
import type { CommandResponse } from "./types";
import { createLogger } from "@/lib/logger";

const logger = createLogger("CommitState");

/** Mirror of `git_status_subset::CommitState` with `CommitStateStatus`
 *  rendered as a lowercase string per the Rust serde renaming.
 *
 *  Authoritative declaration lives here and is re-exported from
 *  `CommitTrafficLight.tsx` for downstream consumers. */
export interface CommitState {
  status: "clean" | "dirty" | "empty" | "merging" | "unknown";
  touched_count: number;
  dirty_count: number;
  repo_roots: string[];
  merging_repos: string[];
  generated_at_ms: number;
}

/** How long a `CommitState` is considered fresh before we pin it to
 *  `"unknown"` at render time. 5 minutes matches the plan §3 spec. */
const STALE_CAP_MS = 5 * 60 * 1000;

/** Background poll interval (ms). 30 s matches the plan §3 spec. */
const POLL_INTERVAL_MS = 30_000;

interface CommitStateEvent {
  task_run_id: string;
  state: CommitState;
}

export function useCommitState(tabs: TerminalTab[]): Record<string, CommitState> {
  const [rawStates, setRawStates] = useState<Record<string, CommitState>>({});
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  // Find a tab by its claudeSessionId (the value the backend keys on as
  // `task_run_id`). Returns the React-side tab.id for state-map use.
  const findTabIdBySession = (sessionId: string): string | undefined => {
    for (const tab of tabsRef.current) {
      if (tab.claudeSessionId === sessionId) return tab.id;
    }
    return undefined;
  };

  // ── Listen for `commit-state-changed` events ──────────────────────────────
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    listen<CommitStateEvent>("commit-state-changed", (event) => {
      const { task_run_id, state } = event.payload;
      const tabId = findTabIdBySession(task_run_id);
      if (!tabId) return; // No live tab for this session — drop.
      setRawStates((prev) => ({ ...prev, [tabId]: state }));
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((err) => {
        logger.warn("Failed to subscribe to commit-state-changed:", err);
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // ── 30 s background poll for every tab with a claudeSessionId ────────────
  // The interval is rebuilt on `tabs` changes so newly-spawned tabs start
  // polling immediately and closed tabs stop. We snapshot the tab list
  // inside the closure so the in-flight poll batch sees the same set its
  // caller scheduled.
  useEffect(() => {
    let active = true;

    const pollAll = async () => {
      const snapshot = tabsRef.current.filter((t) => Boolean(t.claudeSessionId));
      if (snapshot.length === 0) return;

      const results = await Promise.allSettled(
        snapshot.map(async (tab) => {
          const taskRunId = tab.claudeSessionId;
          if (!taskRunId) return null;
          const resp = await invoke<CommandResponse>("get_session_commit_state", {
            taskRunId,
          });
          if (!resp.success || !resp.data) return null;
          return { tabId: tab.id, state: resp.data as CommitState };
        }),
      );

      if (!active) return;

      setRawStates((prev) => {
        const next = { ...prev };
        for (const r of results) {
          if (r.status !== "fulfilled" || !r.value) continue;
          next[r.value.tabId] = r.value.state;
        }
        return next;
      });
    };

    // Fire once immediately so we don't wait 30 s for the first probe.
    pollAll();
    const interval = setInterval(pollAll, POLL_INTERVAL_MS);
    return () => {
      active = false;
      clearInterval(interval);
    };
  }, [tabs]);

  // Apply the 5-minute stale cap. The cap is recomputed on a 30 s tick
  // (matched to the poll interval) so a fresh probe can snap the UI back
  // to its real status without us reading `Date.now()` during render —
  // `Date.now()` is impure (react-hooks/purity rule) and would re-fire
  // every render. The tick lives outside the memo so React's render
  // remains pure; the memo just consumes the most recent tick + map.
  const [staleNow, setStaleNow] = useState<number>(() => Date.now());
  useEffect(() => {
    const interval = setInterval(() => setStaleNow(Date.now()), POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, []);

  return useMemo(() => {
    const result: Record<string, CommitState> = {};
    for (const [tabId, state] of Object.entries(rawStates)) {
      if (staleNow - state.generated_at_ms > STALE_CAP_MS) {
        result[tabId] = { ...state, status: "unknown" };
      } else {
        result[tabId] = state;
      }
    }
    return result;
  }, [rawStates, staleNow]);
}
