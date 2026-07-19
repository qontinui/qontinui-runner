/**
 * Worktrees panel — the PRIMARY, human-consented trigger for agent-worktree
 * cleanup (plan `2026-07-19-worktree-cleanup-lifecycle-tracking`, Phase 4).
 *
 * Renders as a collapsible section at the foot of the session-manager
 * sidebar, i.e. a sibling of the terminal session list. There was no
 * pre-existing worktree list to extend — the only worktree UI in the runner
 * is the per-session inline metadata on `SessionCard`, which cross-links
 * here.
 *
 * The whole point of this panel is VISIBILITY BEFORE CONSENT: the silent
 * background reclaim poller already runs on every runner but has no per-item
 * consent and no visibility, which is exactly why its destructive half is
 * kept disarmed. Here the operator sees:
 *   - every worktree that WILL be removed, with the disk it frees, and
 *   - every worktree that will NOT be, with the specific guard that blocked
 *     it (G1 dirty / pin / G3 session-live / G6 in-flight build / G2 not
 *     landed / G4 main-merge / G5 grace),
 * and only then presses "Clean up safe worktrees".
 */

import { useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  FolderGit2,
  HardDrive,
  Loader2,
  RefreshCw,
  Trash2,
  AlertTriangle,
  Link2,
  CheckCircle2,
  Clock,
} from "lucide-react";
import {
  useReclaimableWorktrees,
  formatBytes,
  formatAge,
  REASON_LABEL,
  type WorktreeSurvey,
  type WorktreeSurveyItem,
} from "./useReclaimableWorktrees";

interface WorktreesPanelProps {
  /** Start expanded (e.g. when a SessionCard cross-link opened it). */
  defaultOpen?: boolean;
  /** Path to scroll into view / highlight — the SessionCard cross-link. */
  highlightPath?: string | null;
}

/** Colour per blocked reason: red = your data is at stake, amber = wait. */
function reasonTone(reason: string | null): string {
  switch (reason) {
    case "dirty":
      return "text-[#f7768e] bg-[#f7768e]/10";
    case "pinned":
      return "text-[#bb9af7] bg-[#bb9af7]/10";
    case "building":
    case "session-live":
      return "text-[#e0af68] bg-[#e0af68]/10";
    case "coord-unreachable":
    case "error":
      return "text-[#f7768e] bg-[#f7768e]/10";
    default:
      return "text-[#565f89] bg-[#565f89]/10";
  }
}

/**
 * The "as of" line — UX honesty about snapshot age.
 *
 * The disk census is a multi-minute walk, so this list is a SNAPSHOT, never
 * live. Rendering it without its age would present stale data as current; a
 * `pending` census must read as "not known yet", never as "nothing to clean".
 */
function CensusFreshness({ survey }: { survey: WorktreeSurvey }) {
  const pending = survey.census_status === "pending";
  const stale = survey.census_status === "stale";
  const tone = pending
    ? "bg-[#e0af68]/10 text-[#e0af68]"
    : stale
      ? "bg-[#e0af68]/10 text-[#e0af68]/90"
      : "bg-[#1a1b26] text-[#565f89]";

  return (
    <div
      className={`mx-3 mb-2 px-2 py-1 rounded text-[10px] flex items-start gap-1 shrink-0 ${tone}`}
      data-testid="worktrees-census-freshness"
      data-census-status={survey.census_status}
      data-census-refreshing={survey.census_refreshing ? "true" : "false"}
      title={survey.census_note}
    >
      {survey.census_refreshing ? (
        <Loader2 className="w-3 h-3 mt-px shrink-0 animate-spin" />
      ) : (
        <Clock className="w-3 h-3 mt-px shrink-0" />
      )}
      <span>
        {pending ? (
          <>
            Disk scan not finished yet — this list is <strong>not known yet</strong>, not
            &ldquo;nothing to clean up&rdquo;.
            {survey.census_refreshing ? " Scanning now…" : ""}
          </>
        ) : (
          <>
            Disk state as of {formatAge(survey.census_age_secs)}
            {stale ? " (stale — the scan is overdue)" : ""}
            {survey.census_refreshing ? " · rescanning now…" : ""}
          </>
        )}
      </span>
    </div>
  );
}

function WorktreeRow({ item, highlighted }: { item: WorktreeSurveyItem; highlighted: boolean }) {
  const reapable = item.status === "reapable";
  return (
    <div
      className={`px-3 py-1.5 border-b border-[#1e2030] ${highlighted ? "bg-[#7aa2f7]/10" : ""}`}
      data-testid="worktree-row"
      data-worktree-status={item.status}
      data-worktree-reason={item.reason ?? ""}
    >
      <div className="flex items-center gap-1.5">
        <FolderGit2
          className={`w-3 h-3 shrink-0 ${reapable ? "text-[#9ece6a]" : "text-[#565f89]"}`}
        />
        <span className="text-[11px] text-[#c0caf5] truncate font-mono" title={item.worktree_path}>
          {item.worktree_path.split(/[\\/]/).pop()}
        </span>
        <div className="flex-1" />
        {reapable ? (
          <span className="text-[10px] text-[#9ece6a] whitespace-nowrap">
            {formatBytes(item.attributable_bytes)}
          </span>
        ) : (
          <span
            className={`text-[9px] px-1 py-px rounded whitespace-nowrap ${reasonTone(item.reason)}`}
          >
            {item.reason ? (REASON_LABEL[item.reason] ?? item.reason) : "blocked"}
          </span>
        )}
      </div>

      <div className="mt-0.5 ml-4.5 flex items-center gap-2 text-[10px] text-[#565f89]">
        <span className="truncate">{item.repo}</span>
        {item.branch && <span className="truncate font-mono">{item.branch}</span>}
        {item.junctioned_paths.length > 0 && (
          <span
            className="flex items-center gap-0.5 shrink-0"
            title={`Junctioned (shared with the canonical checkout, unlinked not deleted): ${item.junctioned_paths.join(", ")}`}
          >
            <Link2 className="w-2.5 h-2.5" />
            {item.junctioned_paths.length}
          </span>
        )}
      </div>

      {/* The "why" — this is what makes the refusal non-silent. */}
      {!reapable && item.reason_detail && (
        <div className="mt-0.5 ml-4.5 text-[10px] text-[#565f89]/90 leading-tight">
          {item.reason_detail}
        </div>
      )}
      {reapable && item.coord_reason && (
        <div className="mt-0.5 ml-4.5 text-[10px] text-[#565f89]/70 font-mono truncate">
          {item.coord_reason}
        </div>
      )}
    </div>
  );
}

export function WorktreesPanel({ defaultOpen = false, highlightPath = null }: WorktreesPanelProps) {
  const [open, setOpen] = useState(defaultOpen);
  const [showBlocked, setShowBlocked] = useState(true);
  const { survey, loading, error, lastOutcome, reclaiming, refresh, reclaim } =
    useReclaimableWorktrees(open);

  const reapable = survey?.items.filter((i) => i.status === "reapable") ?? [];
  const blocked = survey?.items.filter((i) => i.status === "blocked") ?? [];
  const highlightId = highlightPath ? highlightPath.replace(/\\/g, "/").toLowerCase() : null;

  return (
    <div
      className="border-t border-[#2a2d3d] bg-[#13141f] shrink-0 max-h-[45%] flex flex-col"
      data-testid="worktrees-panel"
    >
      {/* Header */}
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1.5 px-3 py-2 w-full text-left hover:bg-[#1a1b26] transition-colors shrink-0"
        title="Agent worktrees on this machine — what can be cleaned up, and why the rest can't"
      >
        {open ? (
          <ChevronDown className="w-3 h-3 text-[#565f89]" />
        ) : (
          <ChevronRight className="w-3 h-3 text-[#565f89]" />
        )}
        <HardDrive className="w-3.5 h-3.5 text-[#7aa2f7]" />
        <span className="text-xs font-medium text-[#c0caf5]">Worktrees</span>
        {survey &&
          (survey.census_status === "pending" ? (
            // "0 safe to clean" before the first scan would be a lie — we do
            // not know yet.
            <span className="text-[10px] text-[#e0af68]">scanning disk…</span>
          ) : (
            <span className="text-[10px] text-[#565f89]">
              {survey.summary.reapable} safe to clean
              {survey.summary.reclaimable_bytes > 0 &&
                ` · ${formatBytes(survey.summary.reclaimable_bytes)}`}
              {survey.census_status === "stale" && " · stale"}
            </span>
          ))}
        <div className="flex-1" />
        {loading && <Loader2 className="w-3 h-3 text-[#565f89] animate-spin" />}
      </button>

      {open && (
        <>
          {/* Action bar */}
          <div className="flex items-center gap-2 px-3 pb-2 shrink-0">
            <button
              onClick={() => void reclaim()}
              disabled={reclaiming || reapable.length === 0}
              className="flex items-center gap-1 px-2 py-1 rounded text-[10px] font-medium bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
              title={
                reapable.length === 0
                  ? "Nothing is currently safe to clean up"
                  : `Remove ${reapable.length} worktree(s) coord has cleared — dirty, pinned, session-live and building trees are never touched`
              }
              data-testid="worktrees-cleanup-button"
            >
              {reclaiming ? (
                <Loader2 className="w-3 h-3 animate-spin" />
              ) : (
                <Trash2 className="w-3 h-3" />
              )}
              Clean up safe worktrees{reapable.length > 0 ? ` (${reapable.length})` : ""}
            </button>
            <button
              onClick={() => void refresh(true)}
              disabled={loading}
              className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
              title="Re-check with coord and start a fresh disk scan in the background (the scan takes several minutes on a big workspace — the list updates when it lands)"
              data-testid="worktrees-rescan-button"
            >
              <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
            </button>
          </div>

          {/* Snapshot age — never present stale data as live. */}
          {survey && <CensusFreshness survey={survey} />}

          {/* coord-offline / error banners */}
          {survey && !survey.coord_reachable && (
            <div className="mx-3 mb-2 px-2 py-1 rounded bg-[#f7768e]/10 text-[10px] text-[#f7768e] flex items-start gap-1 shrink-0">
              <AlertTriangle className="w-3 h-3 mt-px shrink-0" />
              <span>
                coord is unreachable — nothing can be cleaned up without its decision.
                {survey.coord_error ? ` (${survey.coord_error})` : ""}
              </span>
            </div>
          )}
          {error && (
            <div className="mx-3 mb-2 px-2 py-1 rounded bg-[#f7768e]/10 text-[10px] text-[#f7768e] shrink-0">
              {error}
            </div>
          )}

          {/* Receipt for the last cleanup */}
          {lastOutcome && (
            <div className="mx-3 mb-2 px-2 py-1 rounded bg-[#1a1b26] text-[10px] shrink-0">
              <div className="flex items-center gap-1 text-[#9ece6a]">
                <CheckCircle2 className="w-3 h-3" />
                Removed {lastOutcome.removed.length}
                {lastOutcome.dry_run ? " (dry run)" : ""}
                {lastOutcome.removed.length > 0 &&
                  ` · ${formatBytes(lastOutcome.removed.reduce((s, r) => s + r.freed_bytes, 0))} freed`}
              </div>
              {lastOutcome.skipped.map((s) => (
                <div key={s.id} className="text-[#e0af68]/90 truncate" title={s.detail}>
                  skipped {s.worktree_path.split(/[\\/]/).pop()} — {s.reason}
                </div>
              ))}
            </div>
          )}

          {/* Lists */}
          <div className="overflow-y-auto scrollbar-dark">
            {reapable.length === 0 && blocked.length === 0 && !loading && (
              <div className="px-3 py-4 text-center text-[10px] text-[#565f89]">
                {/*
                  Three DIFFERENT empty states, because they mean different
                  things. Only the last one is a claim about the disk; the
                  other two are "we don't know", and saying "No agent
                  worktrees" for them would be a lie.
                */}
                {survey === null
                  ? "Couldn't read the worktree survey — the list below is unknown, not empty."
                  : survey.census_status === "pending"
                    ? "Waiting for the first disk scan of this session…"
                    : "No agent worktrees on this machine."}
              </div>
            )}

            {reapable.length > 0 && (
              <div className="px-3 pt-1 pb-0.5 text-[9px] uppercase tracking-wide text-[#9ece6a]/70">
                Safe to clean ({reapable.length})
              </div>
            )}
            {reapable.map((i) => (
              <WorktreeRow key={i.id} item={i} highlighted={i.id === highlightId} />
            ))}

            {blocked.length > 0 && (
              <button
                onClick={() => setShowBlocked((v) => !v)}
                className="w-full flex items-center gap-1 px-3 pt-2 pb-0.5 text-[9px] uppercase tracking-wide text-[#565f89] hover:text-[#c0caf5] transition-colors"
              >
                {showBlocked ? (
                  <ChevronDown className="w-2.5 h-2.5" />
                ) : (
                  <ChevronRight className="w-2.5 h-2.5" />
                )}
                Kept ({blocked.length})
              </button>
            )}
            {showBlocked &&
              blocked.map((i) => (
                <WorktreeRow key={i.id} item={i} highlighted={i.id === highlightId} />
              ))}

            {survey && survey.canonical_excluded > 0 && (
              <div className="px-3 py-1.5 text-[10px] text-[#565f89]/70">
                {survey.canonical_excluded} canonical checkout
                {survey.canonical_excluded === 1 ? "" : "s"} not listed (never reclaimable).
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
