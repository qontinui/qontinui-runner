/**
 * OverlappingIntentsPanel — Coordination Phase 1B (§4.10), the L2
 * visibility surface.
 *
 * Lists active agent pairs whose declared_overlap_paths intersect, with both
 * agents' free-text intents and the overlapping path set. Read-only and
 * informational: the visibility layer is not authoritative, and an agent
 * reading it decides for itself whether to continue, pivot or coordinate.
 *
 * Extracted out of the deleted `CoordinatorDashboard` by Phase 4 of plan
 * `2026-09-12-consolidate-local-orchestration-onto-conductor`, so the one
 * coord read worth keeping on the Productivity page no longer needs the
 * board around it.
 *
 * Three facts this panel keeps distinct, because they are distinct:
 *
 *   - ISOLATED — the runner has no coord configured. `coord.agent_worktrees`
 *     is written only by coord replication, so the answer is structurally
 *     empty forever; rendering "no overlap" would be a false statement about
 *     a fleet that does not exist. The panel disables itself and says why.
 *   - UNKNOWN — the read has not succeeded: not asked yet, or the last ask
 *     failed. An empty list here would be a lie, so the panel says UNKNOWN
 *     and shows the failure text verbatim (see `describeThrown`).
 *   - EMPTY — a read SUCCEEDED and returned no pairs. Only then does the
 *     panel say there is no L2 contention right now.
 */

import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, HelpCircle, RefreshCw } from "lucide-react";

import { coordCallsReady, useCoordMode, type CoordGating } from "@/contexts/CoordModeContext";
import {
  CoordConnectionRequired,
  coordDisabledCopy,
} from "@/components/shared/CoordConnectionRequired";
import {
  describeThrown,
  listOverlappingIntents,
  type OverlappingIntentPair,
} from "./overlappingIntentsApi";

/** Operator-facing name of this surface, shared by the isolated notice and
 *  the disabled control's tooltip so both read from one `coordDisabledCopy`. */
export const OVERLAP_SURFACE = "Overlapping intents";

/** Poll cadence. Bounded by `limit` on the Tauri side and informational, so
 *  a comfortable 30s. */
const POLL_MS = 30_000;

// ---------------------------------------------------------------------------
// View derivation — pure, so it is testable under the runner's
// `environment: "node"` vitest config (no jsdom; FileActivityPanel.test.tsx
// has the same constraint).
// ---------------------------------------------------------------------------

/** What the panel has learned so far from the read. */
export interface OverlapReadState {
  /** Rows from the most recent SUCCESSFUL read. */
  rows: OverlappingIntentPair[];
  /** True once at least one read has succeeded. */
  loaded: boolean;
  /** Description of the most recent failure, or `null` when the most recent
   *  read succeeded (or none has run). */
  error: string | null;
}

export type OverlapViewMode = "isolated" | "unknown" | "empty" | "rows";

export interface OverlapView {
  mode: OverlapViewMode;
  /** Rows to render. Non-empty ONLY in `rows` mode: a failed read after a
   *  successful one does not keep showing the stale pairs as current. */
  rows: OverlappingIntentPair[];
  /** Header count. A number only when a read has established one. */
  countLabel: string;
  /** The failure text, in `unknown` mode after a failed read. */
  error: string | null;
}

/**
 * Map coord gating + read state onto what the panel shows.
 *
 * Order matters: isolated wins over everything (there is nothing to read);
 * a failed or not-yet-run read is UNKNOWN regardless of what an earlier
 * success returned; only a clean read decides between empty and rows.
 */
export function deriveOverlapView(coord: CoordGating, read: OverlapReadState): OverlapView {
  if (coord.isolated) {
    return { mode: "isolated", rows: [], countLabel: "off", error: null };
  }
  if (read.error !== null || !read.loaded) {
    return { mode: "unknown", rows: [], countLabel: "unknown", error: read.error };
  }
  if (read.rows.length === 0) {
    return { mode: "empty", rows: [], countLabel: "0 active", error: null };
  }
  return { mode: "rows", rows: read.rows, countLabel: `${read.rows.length} active`, error: null };
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

export function OverlappingIntentsPanel() {
  const coord = useCoordMode();
  const [read, setRead] = useState<OverlapReadState>({ rows: [], loaded: false, error: null });
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const rows = await listOverlappingIntents();
      setRead({ rows, loaded: true, error: null });
    } catch (err) {
      // `invoke()` rejects with a plain STRING, so `describeThrown` rather
      // than an `instanceof Error` arm that would discard the cause.
      setRead((prev) => ({
        ...prev,
        error: describeThrown(err, "Failed to read overlapping intents"),
      }));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // `coordCallsReady` holds the first request until `get_coord_mode` has
    // answered and skips the poll entirely on an isolated runner — the panel
    // states the reason instead of firing at a coord that does not exist.
    if (!coordCallsReady(coord)) return;
    void load();
    const id = setInterval(() => {
      void load();
    }, POLL_MS);
    return () => clearInterval(id);
  }, [load, coord]);

  const view = deriveOverlapView(coord, read);
  const disabled = view.mode === "isolated";
  const disabledTooltip = disabled
    ? coordDisabledCopy(coord.source, OVERLAP_SURFACE).tooltip
    : undefined;

  return (
    <section
      role="region"
      aria-labelledby="productivity-coord-overlap-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-overlapping-intents"
      data-overlap-mode={view.mode}
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <AlertTriangle className="w-4 h-4 text-sky-400" aria-hidden="true" />
          <h2
            id="productivity-coord-overlap-heading"
            className="text-sm font-semibold text-foreground"
          >
            {OVERLAP_SURFACE}
          </h2>
          <span
            className="text-xs text-muted-foreground"
            data-ui-bridge-id="productivity.coord-overlapping-intents-count"
          >
            {view.countLabel}
          </span>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={loading || disabled}
          title={disabledTooltip}
          data-ui-bridge-id="productivity.coord-overlapping-intents-refresh"
          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} aria-hidden="true" />
          Refresh
        </button>
      </header>

      {view.mode === "isolated" ? (
        <CoordConnectionRequired
          source={coord.source}
          surface={OVERLAP_SURFACE}
          uiBridgeId="productivity.coord-overlapping-intents-isolated"
        />
      ) : view.mode === "unknown" ? (
        <div
          role="status"
          className="flex flex-col gap-1 rounded-md border border-amber-500/30 bg-amber-500/10 p-3 text-xs"
          data-ui-bridge-id="productivity.coord-overlapping-intents-unknown"
        >
          <div className="flex items-center gap-1.5 font-semibold text-amber-300">
            <HelpCircle className="w-3.5 h-3.5" aria-hidden="true" />
            UNKNOWN
          </div>
          <p className="text-muted-foreground">
            {view.error
              ? "The last read of overlapping intents failed, so whether any agents overlap right now is not known."
              : loading
                ? "Reading overlapping intents from coord…"
                : "Overlapping intents have not been read yet."}
          </p>
          {view.error ? (
            <p className="font-mono text-red-400 break-words" data-overlap-error>
              {view.error}
            </p>
          ) : null}
        </div>
      ) : view.mode === "empty" ? (
        <div
          className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground"
          data-ui-bridge-id="productivity.coord-overlapping-intents-empty"
        >
          No overlapping agent intents detected. Agents declare paths at allocation; coord flags
          pairs whose declared sets intersect. Empty here = no L2 contention right now.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {view.rows.map((row) => (
            <li
              key={`${row.agentA}-${row.agentB}`}
              className="rounded-md border border-sky-500/30 bg-sky-500/5 p-3"
              data-ui-bridge-id="productivity.coord-overlapping-intent-card"
              data-agent-a={row.agentA}
              data-agent-b={row.agentB}
            >
              <div className="flex flex-col gap-2 min-w-0">
                <div className="grid grid-cols-2 gap-3">
                  <IntentCell agent={row.agentA} intent={row.intentA} />
                  <IntentCell agent={row.agentB} intent={row.intentB} />
                </div>
                <div className="border-t border-border/40 pt-2">
                  <div className="text-[11px] text-muted-foreground mb-1">
                    Overlapping paths ({row.overlappingPaths.length})
                  </div>
                  <div className="flex flex-wrap gap-1">
                    {row.overlappingPaths.map((p) => (
                      <code
                        key={p}
                        className="rounded border border-sky-500/30 bg-sky-500/10 px-1.5 py-0.5 text-[11px] font-mono text-sky-300"
                      >
                        {p}
                      </code>
                    ))}
                  </div>
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function IntentCell({ agent, intent }: { agent: string; intent: string | null }) {
  return (
    <div className="flex flex-col gap-0.5 min-w-0">
      <div className="text-[11px] text-muted-foreground font-mono truncate">{agent}</div>
      <p className="text-xs text-foreground/90 whitespace-pre-wrap break-words">
        {intent || <span className="text-muted-foreground italic">no intent declared</span>}
      </p>
    </div>
  );
}

export default OverlappingIntentsPanel;
