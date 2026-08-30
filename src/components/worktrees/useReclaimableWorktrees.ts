/**
 * Data hook for the on-demand agent-worktree cleanup surface
 * (plan `2026-07-19-worktree-cleanup-lifecycle-tracking`, Phase 4).
 *
 * Talks to the runner's own `:9876` API:
 *   - `GET  /agent-worktrees/reclaimable` — coord's reap decision for THIS
 *     device, joined with the local disk census.
 *   - `POST /agent-worktrees/reclaim`     — remove only the cleared set.
 *
 * The exact same endpoints an agent in a runner terminal drives, so the
 * button and the agent path can never diverge.
 */

import { useCallback, useEffect, useState } from "react";
import { apiFetch } from "@/hooks/useApiHelpers";

/** Why a worktree may not be reclaimed. Mirrors Rust `SkipReason::as_str()`. */
export type SkipReason =
  | "dirty"
  /**
   * G1 refused on a tree nobody could READ. `git status --porcelain` did not
   * answer (timed out, failed to spawn, or exited non-zero against a `.git`
   * that is present or undeterminable), so `is_dirty` is `true` only because
   * "not provably clean" fails closed.
   *
   * The refusal is identical to `dirty`; the SENTENCE is not. This row is not
   * a claim that the tree holds uncommitted work — telling the operator to
   * "commit or stash it first" here hands them an instruction that fails for
   * the very reason the probe did (a dead mount, a permission wall, a wedged
   * git).
   */
  | "dirtiness-unknown"
  | "pinned"
  | "session-live"
  | "building"
  | "not-landed"
  | "main-merge"
  | "grace"
  | "not-a-candidate"
  | "not-cleared"
  | "coord-unreachable"
  | "absent"
  | "not-reapable"
  | "error";

export interface WorktreeSurveyItem {
  /** Stable id for the POST — the normalized worktree path. */
  id: string;
  worktree_path: string;
  repo: string;
  branch: string | null;
  status: "reapable" | "blocked";
  reason: SkipReason | null;
  reason_detail: string | null;
  is_dirty: boolean;
  /**
   * Whether `is_dirty` is a MEASUREMENT or the fail-closed default an
   * UNREADABLE tree gets. `false` ⇒ `is_dirty` is `true` only because the
   * probe degraded, and NOTHING here established that work exists.
   *
   * The panel renders `reason` / `reason_detail`, which the backend already
   * splits by this field (`dirty` vs `dirtiness-unknown`); it is carried on
   * the item too so a consumer can tell the two apart without string-matching
   * a reason token.
   */
  is_dirty_known: boolean;
  building: boolean;
  pinned: boolean;
  landed_in_main: boolean | null;
  attributable_bytes: number;
  junctioned_paths: string[];
  coord_reason: string | null;
}

/**
 * Freshness of the on-disk census the survey is derived from.
 *
 * The disk census is a MULTI-MINUTE walk on a real machine, so the endpoint
 * serves the snapshot published by the runner's periodic census task rather
 * than rebuilding one per request. That makes the age load-bearing: the panel
 * must say "as of Nm ago" and must never render `pending` as an authoritative
 * "nothing to clean up".
 */
export type CensusStatus = "pending" | "fresh" | "stale";

export interface WorktreeSurvey {
  device_id: string | null;
  coord_reachable: boolean;
  coord_error: string | null;
  remove_armed: boolean;
  rejunction_armed: boolean;
  canonical_excluded: number;
  items: WorktreeSurveyItem[];
  summary: { reapable: number; blocked: number; reclaimable_bytes: number };
  /** `pending` = no walk has completed yet — the list is UNKNOWN, not empty. */
  census_status: CensusStatus;
  census_taken_at: string | null;
  census_age_secs: number | null;
  census_build_ms: number | null;
  /** A census walk is running right now. */
  census_refreshing: boolean;
  census_note: string;
}

export interface ReclaimOutcome {
  removed: Array<{
    id: string;
    worktree_path: string;
    repo: string;
    freed_bytes: number;
    dry_run: boolean;
  }>;
  skipped: Array<{
    id: string;
    worktree_path: string;
    reason: SkipReason;
    detail: string;
  }>;
  dry_run: boolean;
  /** Freshness of the CANDIDATE set. Guards are always re-checked live. */
  census_status: CensusStatus;
  census_age_secs: number | null;
}

interface ApiEnvelope<T> {
  success: boolean;
  data?: T;
  error?: string;
}

/** Human-readable byte size. */
export function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** `95` → `"1m ago"`. The "as of" label — honest about snapshot age. */
export function formatAge(secs: number | null): string {
  if (secs === null || secs < 0) return "unknown";
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ${mins % 60}m ago`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h ago`;
}

/** Short label for a blocked reason — the badge text. */
export const REASON_LABEL: Record<SkipReason, string> = {
  dirty: "uncommitted work",
  // NOT "uncommitted work" — nothing measured any. The badge says what
  // actually happened, so the operator inspects the mount instead of trying
  // to commit in a tree whose git did not answer.
  "dirtiness-unknown": "unreadable",
  pinned: "pinned",
  "session-live": "session live",
  building: "building",
  "not-landed": "not landed",
  "main-merge": "main merge",
  grace: "grace period",
  "not-a-candidate": "in use",
  "not-cleared": "not cleared",
  "coord-unreachable": "coord offline",
  absent: "gone",
  "not-reapable": "not reapable",
  error: "error",
};

export interface UseReclaimableWorktrees {
  survey: WorktreeSurvey | null;
  loading: boolean;
  error: string | null;
  /** Result of the last reclaim call, for the inline receipt. */
  lastOutcome: ReclaimOutcome | null;
  reclaiming: boolean;
  /**
   * Re-read the survey. `rescan` additionally asks the runner to start a fresh
   * disk walk in the BACKGROUND (`?refresh=1`) — the response still comes back
   * promptly from the cached snapshot with `census_refreshing: true`.
   */
  refresh: (rescan?: boolean) => Promise<void>;
  /** `ids` omitted = every currently-reapable worktree. */
  reclaim: (ids?: string[], dryRun?: boolean) => Promise<void>;
}

export function useReclaimableWorktrees(enabled: boolean): UseReclaimableWorktrees {
  const [survey, setSurvey] = useState<WorktreeSurvey | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastOutcome, setLastOutcome] = useState<ReclaimOutcome | null>(null);
  const [reclaiming, setReclaiming] = useState(false);

  const refresh = useCallback(async (rescan = false) => {
    setLoading(true);
    setError(null);
    try {
      const res = await apiFetch<ApiEnvelope<WorktreeSurvey>>(
        `/agent-worktrees/reclaimable${rescan ? "?refresh=1" : ""}`,
      );
      if (!res.success || !res.data) throw new Error(res.error ?? "survey failed");
      setSurvey(res.data);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const reclaim = useCallback(
    async (ids?: string[], dryRun = false) => {
      setReclaiming(true);
      setError(null);
      try {
        const res = await apiFetch<ApiEnvelope<ReclaimOutcome>>("/agent-worktrees/reclaim", {
          method: "POST",
          body: JSON.stringify({ ids: ids ?? null, dryRun }),
        });
        if (!res.success || !res.data) throw new Error(res.error ?? "reclaim failed");
        setLastOutcome(res.data);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setReclaiming(false);
        // Always re-survey: a partial failure still changed disk.
        await refresh();
      }
    },
    [refresh],
  );

  // Survey on open (and on re-open). Deferred a tick so the effect body never
  // calls setState synchronously — that would cascade a render before the
  // panel has painted.
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      if (!cancelled) void refresh();
    }, 0);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [enabled, refresh]);

  // While a census walk is in flight the snapshot WILL change under us, so
  // re-poll (cheaply — the endpoint just reads the cache) until it settles.
  // Without this a cold-start panel would sit on "pending" forever until the
  // operator manually refreshed.
  const settling =
    enabled && survey !== null && (survey.census_refreshing || survey.census_status === "pending");
  useEffect(() => {
    if (!settling) return;
    const timer = setInterval(() => void refresh(), 20_000);
    return () => clearInterval(timer);
  }, [settling, refresh]);

  return { survey, loading, error, lastOutcome, reclaiming, refresh, reclaim };
}
