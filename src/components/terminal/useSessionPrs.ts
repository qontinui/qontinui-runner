import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Per-session PR merged/unmerged status for the zone-header dropdown.
 *
 * Backed by the `session_prs_get` Tauri command, which proxies coord's
 * `GET /pr-merge/session/:session_id/prs` (agent_worktrees → repo_branches
 * join) with the device JWT. The command is fail-soft: unpaired device,
 * unreachable coord, or a coord build predating the endpoint all come back
 * as `{ available: false }` — the dropdown hides instead of erroring.
 */

export interface SessionPr {
  repo: string;
  pr_number: number;
  branch: string;
  pr_state: string | null;
  merged: boolean;
  merged_at: string | null;
}

export interface SessionPrsState {
  available: boolean;
  prs: SessionPr[];
  allMerged: boolean;
}

const EMPTY: SessionPrsState = { available: false, prs: [], allMerged: true };

/** Unmerged first (the actionable rows), newest PR first within each group.
 * Coord already orders this way; re-sorting keeps the UI honest if the
 * payload ever arrives unsorted. Pure — unit-tested. */
export function sortPrsUnmergedFirst(prs: SessionPr[]): SessionPr[] {
  return [...prs].sort((a, b) => {
    if (a.merged !== b.merged) return a.merged ? 1 : -1;
    if (a.pr_number !== b.pr_number) return b.pr_number - a.pr_number;
    return a.repo.localeCompare(b.repo);
  });
}

/** Roll-up for the trigger symbol: green ⇔ every PR merged. Pure. */
export function summarizePrs(prs: SessionPr[]): {
  total: number;
  unmerged: number;
  allMerged: boolean;
} {
  const unmerged = prs.filter((p) => !p.merged).length;
  return { total: prs.length, unmerged, allMerged: unmerged === 0 };
}

/** GitHub URL when the repo is the webhook's `owner/name`; coord can also
 * surface a bare checkout name (repo-format skew) — no owner, no link. */
export function prGithubUrl(pr: Pick<SessionPr, "repo" | "pr_number">): string | null {
  return pr.repo.includes("/") ? `https://github.com/${pr.repo}/pull/${pr.pr_number}` : null;
}

/** Compact display label: bare repo name + PR number (`runner#663`). */
export function prLabel(pr: Pick<SessionPr, "repo" | "pr_number">): string {
  const bare = pr.repo.split("/").pop() ?? pr.repo;
  return `${bare}#${pr.pr_number}`;
}

function normalize(raw: unknown): SessionPrsState {
  if (!raw || typeof raw !== "object") return EMPTY;
  const v = raw as { available?: boolean; prs?: SessionPr[]; allMerged?: boolean };
  if (!v.available || !Array.isArray(v.prs)) return EMPTY;
  const prs = sortPrsUnmergedFirst(v.prs);
  return { available: true, prs, allMerged: summarizePrs(prs).allMerged };
}

/**
 * Poll the session's PR list while mounted. `claudeSessionId` is undefined
 * for plain PTY tabs and not-yet-reconciled spawns — no fetch, empty state.
 */
export function useSessionPrs(
  claudeSessionId: string | undefined,
  pollMs = 60_000,
): SessionPrsState & { refresh: () => void } {
  // Cache keyed by session id: when the zone is reassigned to another
  // session, render falls back to EMPTY until that session's fetch lands —
  // no setState-in-effect reset, no flash of the previous session's PRs.
  const [cached, setCached] = useState<{ sid: string; data: SessionPrsState } | null>(null);
  const inFlight = useRef(false);

  const fetchPrs = useCallback(async () => {
    if (!claudeSessionId || inFlight.current) return;
    inFlight.current = true;
    try {
      const raw = await invoke<unknown>("session_prs_get", {
        claudeSessionId,
      });
      setCached({ sid: claudeSessionId, data: normalize(raw) });
    } catch (err) {
      // Hard Err = malformed session id (caller bug) — log once per poll,
      // keep the last known state rather than flapping the indicator.
      console.error("session_prs_get failed:", err);
    } finally {
      inFlight.current = false;
    }
  }, [claudeSessionId]);

  useEffect(() => {
    if (!claudeSessionId) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- fetchPrs is async; setCached fires after the IPC await, never synchronously in the effect body
    void fetchPrs();
    const timer = setInterval(() => void fetchPrs(), pollMs);
    return () => clearInterval(timer);
  }, [claudeSessionId, fetchPrs, pollMs]);

  const state = claudeSessionId && cached?.sid === claudeSessionId ? cached.data : EMPTY;
  return { ...state, refresh: () => void fetchPrs() };
}
