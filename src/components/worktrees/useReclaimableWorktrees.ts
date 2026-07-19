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
  building: boolean;
  pinned: boolean;
  landed_in_main: boolean | null;
  attributable_bytes: number;
  junctioned_paths: string[];
  coord_reason: string | null;
}

export interface WorktreeSurvey {
  device_id: string | null;
  coord_reachable: boolean;
  coord_error: string | null;
  remove_armed: boolean;
  rejunction_armed: boolean;
  canonical_excluded: number;
  items: WorktreeSurveyItem[];
  summary: { reapable: number; blocked: number; reclaimable_bytes: number };
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

/** Short label for a blocked reason — the badge text. */
export const REASON_LABEL: Record<SkipReason, string> = {
  dirty: "uncommitted work",
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
  refresh: () => Promise<void>;
  /** `ids` omitted = every currently-reapable worktree. */
  reclaim: (ids?: string[], dryRun?: boolean) => Promise<void>;
}

export function useReclaimableWorktrees(enabled: boolean): UseReclaimableWorktrees {
  const [survey, setSurvey] = useState<WorktreeSurvey | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastOutcome, setLastOutcome] = useState<ReclaimOutcome | null>(null);
  const [reclaiming, setReclaiming] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await apiFetch<ApiEnvelope<WorktreeSurvey>>("/agent-worktrees/reclaimable");
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

  return { survey, loading, error, lastOutcome, reclaiming, refresh, reclaim };
}
