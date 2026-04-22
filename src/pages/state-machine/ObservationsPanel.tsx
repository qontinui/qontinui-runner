/**
 * ObservationsPanel
 *
 * Per-spec surface for the observation-driven state definition pipeline
 * (see `qontinui-dev-notes/session-prompts/state-definition-observation-pipeline.md`,
 * Step 8). Renders:
 *
 *   1. A drift-score widget showing the most recent fit_score for this spec
 *      (yellow/red below the 0.9 threshold) plus a hint to consider re-derivation.
 *   2. A "Re-derive all" button that POSTs `/state-discovery/derive`.
 *   3. The recent-invalidations log (last 20) with per-row Undo buttons
 *      valid for 24 h.
 *
 * `spec_id` is currently Optional — `StateMachineConfig` rows do not yet
 * carry a `spec_id` column, so when none is available we call the backend
 * without the filter (matching the drift-detector and derivation
 * background-task behaviour of treating NULL spec_id as the default scope).
 */

import { useCallback, useEffect, useState } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import { getApiBase } from "@/lib/runner-api";
import type { ShowToastFn } from "@/hooks/useToast";

// ---------------------------------------------------------------------------
// Types matching the runner backend response shapes
// ---------------------------------------------------------------------------

interface DriftScoreRow {
  id: number;
  spec_id: string | null;
  computed_at: string;
  window_size: number;
  fit_score: number;
  observations_considered: number;
  states_matched: number;
}

interface InvalidationRow {
  id: string;
  spec_id: string | null;
  invalidated_at: string | null;
  invalidated_reason: string | null;
  invalidated_by: string | null;
  invalidation_token: string | null;
  fingerprints_count: number;
}

interface DeriveResponse {
  id?: string;
  spec_id?: string | null;
  derived_at?: string;
  window_days?: number;
  observation_count?: number;
  artifact?: {
    states?: Array<unknown>;
  };
}

// Runner wraps responses in { success, data, error }. `apiFetch` unwraps
// and surfaces backend errors as thrown Errors.
interface ApiEnvelope<T> {
  success: boolean;
  data?: T;
  error?: string;
}

async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(`${getApiBase()}${path}`, init);
  // Parse as envelope even on non-2xx so we can surface the `error` field.
  let json: ApiEnvelope<T> | null = null;
  try {
    json = (await resp.json()) as ApiEnvelope<T>;
  } catch {
    /* body wasn't JSON — fall through to generic error below */
  }
  if (!resp.ok || !json?.success) {
    throw new Error(json?.error ?? `HTTP ${resp.status} ${resp.statusText}`);
  }
  return json.data as T;
}

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

export interface ObservationsPanelProps {
  /**
   * The spec_id to scope the drift score, re-derive, and invalidation log.
   * May be undefined if the current compiled SM doesn't carry one — in that
   * case we call the endpoints without the `spec_id` query param.
   */
  specId?: string;
  showToast: ShowToastFn;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatTimestamp(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString();
}

function hoursSince(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return null;
  return (Date.now() - ms) / 3_600_000;
}

function fitScoreColor(fit: number | null): string {
  if (fit == null) return "text-text-secondary";
  if (fit < 0.75) return "text-red-400";
  if (fit < 0.9) return "text-yellow-400";
  return "text-green-400";
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function ObservationsPanel({ specId, showToast }: ObservationsPanelProps) {
  const [latestDrift, setLatestDrift] = useState<DriftScoreRow | null>(null);
  const [driftLoading, setDriftLoading] = useState(false);
  const [invalidations, setInvalidations] = useState<InvalidationRow[]>([]);
  const [invalidationsLoading, setInvalidationsLoading] = useState(false);
  const [deriving, setDeriving] = useState(false);
  const [invalidatingAll, setInvalidatingAll] = useState(false);

  // Build query strings via URLSearchParams to avoid the classic
  // `?&limit=…` / `&spec_id=…` concatenation bugs when spec_id is absent.
  const buildQuery = useCallback(
    (extra: Record<string, string> = {}): string => {
      const params = new URLSearchParams();
      if (specId) params.set("spec_id", specId);
      for (const [k, v] of Object.entries(extra)) params.set(k, v);
      const s = params.toString();
      return s ? `?${s}` : "";
    },
    [specId],
  );

  // --------------------------------------------------------------------
  // Load drift score
  // --------------------------------------------------------------------
  const loadDrift = useCallback(async () => {
    setDriftLoading(true);
    try {
      const rows = await apiFetch<DriftScoreRow[]>(
        `/state-discovery/drift-scores${buildQuery({ limit: "1" })}`,
      );
      setLatestDrift(rows.length > 0 ? rows[0]! : null);
    } catch (err) {
      console.warn("ObservationsPanel: failed to load drift scores", err);
      setLatestDrift(null);
    } finally {
      setDriftLoading(false);
    }
  }, [buildQuery]);

  // --------------------------------------------------------------------
  // Load invalidation log
  // --------------------------------------------------------------------
  const loadInvalidations = useCallback(async () => {
    setInvalidationsLoading(true);
    try {
      const rows = await apiFetch<InvalidationRow[]>(`/co-occurrence/invalidations${buildQuery()}`);
      setInvalidations(rows.slice(0, 20));
    } catch (err) {
      console.warn("ObservationsPanel: failed to load invalidations", err);
      setInvalidations([]);
    } finally {
      setInvalidationsLoading(false);
    }
  }, [buildQuery]);

  useEffect(() => {
    void loadDrift();
    void loadInvalidations();
  }, [loadDrift, loadInvalidations]);

  // --------------------------------------------------------------------
  // Re-derive (bulk)
  // --------------------------------------------------------------------
  const handleReDerive = useCallback(async () => {
    setDeriving(true);
    try {
      const derivedPath = `/state-discovery/derive${buildQuery({ window_days: "90" })}`;
      const result = await apiFetch<DeriveResponse>(derivedPath, {
        method: "POST",
      });
      const stateCount = result?.artifact?.states?.length ?? 0;
      const obsCount = result?.observation_count ?? 0;
      showToast(`Derived ${stateCount} states from ${obsCount} observations`, "success");
      // Re-derivation can shift the fit-score picture, but scores are
      // computed on the background 10-min tick — refetch for freshness
      // anyway in case one just landed.
      await loadDrift();
    } catch (err) {
      showToast(`Re-derive failed: ${err instanceof Error ? err.message : String(err)}`, "error");
    } finally {
      setDeriving(false);
    }
  }, [buildQuery, showToast, loadDrift]);

  // --------------------------------------------------------------------
  // Invalidate-all (spec-scoped)
  // --------------------------------------------------------------------
  const handleInvalidateAll = useCallback(async () => {
    if (!specId) {
      showToast(
        "Cannot invalidate without a spec_id — operating at the default (NULL) scope is too broad",
        "error",
      );
      return;
    }
    setInvalidatingAll(true);
    try {
      const result = await apiFetch<{ count: number; undo_token: string }>(
        "/co-occurrence/invalidate",
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            spec_id: specId,
            reason: "manual invalidation from State Machine page",
            invalidated_by: "state-machine-page",
          }),
        },
      );
      showToast(
        `Invalidated ${result.count} observation(s). Undo (valid for 24 h) — token: ${result.undo_token.slice(0, 8)}…`,
        "success",
      );
      await loadInvalidations();
    } catch (err) {
      showToast(
        `Invalidation failed: ${err instanceof Error ? err.message : String(err)}`,
        "error",
      );
    } finally {
      setInvalidatingAll(false);
    }
  }, [specId, showToast, loadInvalidations]);

  // --------------------------------------------------------------------
  // Per-row undo
  // --------------------------------------------------------------------
  const handleUndo = useCallback(
    async (token: string) => {
      try {
        const result = await apiFetch<{ count: number }>("/co-occurrence/invalidate/undo", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ undo_token: token }),
        });
        showToast(`Restored ${result.count} observation(s)`, "success");
        await loadInvalidations();
      } catch (err) {
        showToast(`Undo failed: ${err instanceof Error ? err.message : String(err)}`, "error");
      }
    },
    [showToast, loadInvalidations],
  );

  // --------------------------------------------------------------------
  // Render
  // --------------------------------------------------------------------
  const fit = latestDrift?.fit_score ?? null;
  const fitColor = fitScoreColor(fit);
  const fitBelowThreshold = fit != null && fit < 0.9;

  // Group invalidations by undo_token — one display row per token, summing
  // fingerprints_count across observations sharing the token (each
  // invalidate call stamps the same token on every affected row).
  const groupedByToken = (() => {
    const groups = new Map<
      string,
      {
        token: string;
        invalidated_at: string | null;
        reason: string | null;
        invalidated_by: string | null;
        fingerprintsCount: number;
        rowCount: number;
      }
    >();
    for (const row of invalidations) {
      const key = row.invalidation_token ?? `null-${row.id}`;
      const existing = groups.get(key);
      if (existing) {
        existing.fingerprintsCount += row.fingerprints_count;
        existing.rowCount += 1;
      } else {
        groups.set(key, {
          token: row.invalidation_token ?? "",
          invalidated_at: row.invalidated_at,
          reason: row.invalidated_reason,
          invalidated_by: row.invalidated_by,
          fingerprintsCount: row.fingerprints_count,
          rowCount: 1,
        });
      }
    }
    return [...groups.values()].sort((a, b) => {
      const at = a.invalidated_at ? Date.parse(a.invalidated_at) : 0;
      const bt = b.invalidated_at ? Date.parse(b.invalidated_at) : 0;
      return bt - at;
    });
  })();

  return (
    <div className="flex flex-col gap-4 px-6 py-4 border-b border-border-primary bg-surface-primary">
      {/* Drift score row */}
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div className="flex items-center gap-3">
          <span className="text-xs text-text-secondary">Drift fit score</span>
          {driftLoading ? (
            <Loader2 className="size-3 animate-spin text-text-muted" />
          ) : fit == null ? (
            <span className="text-xs text-text-muted">no drift data yet</span>
          ) : (
            <>
              <span className={`text-sm font-semibold ${fitColor}`}>{fit.toFixed(3)}</span>
              <span className="text-[10px] text-text-muted">
                ({latestDrift?.states_matched ?? 0}/{latestDrift?.observations_considered ?? 0}{" "}
                observations, {formatTimestamp(latestDrift?.computed_at)})
              </span>
            </>
          )}
          {fitBelowThreshold && (
            <span className="text-[10px] text-yellow-400">
              observations disagree with compiled SM — consider re-deriving
            </span>
          )}
        </div>

        {/* Bulk actions */}
        <div className="flex items-center gap-2">
          <button
            onClick={handleReDerive}
            disabled={deriving}
            className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium border border-border-primary text-text-primary hover:bg-surface-secondary disabled:opacity-50"
            title="POST /state-discovery/derive (window_days=90)"
          >
            {deriving ? (
              <Loader2 className="size-3 animate-spin" />
            ) : (
              <RefreshCw className="size-3" />
            )}
            Re-derive all
          </button>
          <button
            onClick={handleInvalidateAll}
            disabled={invalidatingAll || !specId}
            className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium border border-yellow-500/40 text-yellow-400 hover:bg-yellow-500/10 disabled:opacity-50"
            title={
              specId
                ? "Invalidate all observations for this spec (24 h undo)"
                : "Spec has no spec_id — invalidation-all is disabled"
            }
          >
            Invalidate spec observations
          </button>
        </div>
      </div>

      {/* Invalidation log */}
      <div>
        <div className="flex items-center justify-between mb-1">
          <h4 className="text-xs font-semibold text-text-secondary">Invalidation log</h4>
          <button
            onClick={() => void loadInvalidations()}
            disabled={invalidationsLoading}
            className="text-[10px] text-brand-primary hover:text-brand-primary/80 disabled:opacity-50"
          >
            Refresh
          </button>
        </div>
        {invalidationsLoading ? (
          <div className="text-xs text-text-muted">Loading…</div>
        ) : groupedByToken.length === 0 ? (
          <div className="text-xs text-text-muted">No invalidations recorded for this spec.</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead>
                <tr className="text-left text-[10px] text-text-muted uppercase tracking-wide">
                  <th className="py-1 pr-3 font-normal">When</th>
                  <th className="py-1 pr-3 font-normal">Reason</th>
                  <th className="py-1 pr-3 font-normal">By</th>
                  <th className="py-1 pr-3 font-normal">Fingerprints</th>
                  <th className="py-1 pr-3 font-normal">Action</th>
                </tr>
              </thead>
              <tbody>
                {groupedByToken.slice(0, 20).map((g) => {
                  const hoursAgo = hoursSince(g.invalidated_at);
                  const withinUndo = hoursAgo != null && hoursAgo < 24;
                  return (
                    <tr
                      key={g.token || `anon-${g.invalidated_at}`}
                      className="border-t border-border-secondary"
                    >
                      <td className="py-1 pr-3 text-text-primary">
                        {formatTimestamp(g.invalidated_at)}
                      </td>
                      <td className="py-1 pr-3 text-text-secondary">{g.reason ?? "—"}</td>
                      <td className="py-1 pr-3 text-text-secondary">{g.invalidated_by ?? "—"}</td>
                      <td className="py-1 pr-3 text-text-primary">{g.fingerprintsCount}</td>
                      <td className="py-1 pr-3">
                        {withinUndo && g.token ? (
                          <button
                            onClick={() => void handleUndo(g.token)}
                            className="text-brand-primary hover:text-brand-primary/80 text-[10px]"
                            title="Valid for 24 h after invalidation"
                          >
                            Undo
                          </button>
                        ) : (
                          <span
                            className="text-[10px] text-text-muted"
                            title={hoursAgo == null ? "no timestamp" : "24 h undo window expired"}
                          >
                            {hoursAgo == null ? "—" : "expired"}
                          </span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
