import { AlertTriangle, X, FileWarning, ChevronDown, ChevronUp, GitBranch } from "lucide-react";
import { useState } from "react";
import type { FileConflict } from "./useFileConflicts";

interface FileConflictBannerProps {
  /** Files with active conflicts (multiple sessions, scoped per worktree) */
  conflicts: FileConflict[];
  /** Recent alert from real-time conflict detection */
  recentAlert: {
    file_path: string;
    holder_name: string;
    worktree_id?: string | null;
  } | null;
  /** Dismiss the recent alert */
  onDismissAlert: () => void;
}

/**
 * Render a short, copy-paste-friendly worktree label for inline use.
 * Currently the registry only carries the worktree_id; if/when the runner
 * surfaces the branch name on the registry payload we can swap this to show
 * the branch with the id as fallback.
 */
function shortWorktreeLabel(worktreeId: string): string {
  if (worktreeId.length <= 12) return worktreeId;
  return `${worktreeId.slice(0, 10)}…`;
}

/** Inline chip showing the worktree id (or "main" for null/main-tree). */
function WorktreeChip({ worktreeId }: { worktreeId: string | null | undefined }) {
  if (!worktreeId) {
    return null;
  }
  return (
    <span
      className="inline-flex items-center gap-0.5 px-1 py-0 rounded text-[9px] font-medium bg-[#7aa2f7]/10 text-[#7aa2f7] shrink-0 align-middle"
      title={`Worktree ${worktreeId}`}
    >
      <GitBranch className="w-2.5 h-2.5" />
      {shortWorktreeLabel(worktreeId)}
    </span>
  );
}

/**
 * Banner component that shows file conflict warnings in the Terminal page.
 *
 * Displays two types of warnings:
 * 1. Toast-style alert when a conflict is detected in real-time
 * 2. Persistent summary of all files with multiple active editors
 *
 * Worktree-scoped conflicts (worktree_id != null) get a small chip showing
 * the worktree id; main-tree conflicts (worktree_id == null) display the
 * existing layout unchanged.
 */
export function FileConflictBanner({
  conflicts,
  recentAlert,
  onDismissAlert,
}: FileConflictBannerProps) {
  const [expanded, setExpanded] = useState(false);

  if (conflicts.length === 0 && !recentAlert) return null;

  return (
    <div className="shrink-0">
      {/* Real-time conflict alert (toast-style) */}
      {recentAlert && (
        <div className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium bg-[#e0af68]/15 text-[#e0af68] border-b border-[#e0af68]/20 animate-in slide-in-from-top-1">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0" />
          <span className="flex-1 truncate">
            Conflict: <strong>{recentAlert.file_path}</strong>
            {recentAlert.worktree_id ? (
              <>
                {" "}
                in worktree <WorktreeChip worktreeId={recentAlert.worktree_id} /> is also being
                edited by {recentAlert.holder_name}
              </>
            ) : (
              <> is also being edited by {recentAlert.holder_name}</>
            )}
          </span>
          <button
            onClick={onDismissAlert}
            className="p-0.5 rounded hover:bg-white/10 transition-colors"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      )}

      {/* Persistent conflict summary */}
      {conflicts.length > 0 && (
        <div className="border-b border-[#e0af68]/20">
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex items-center gap-2 px-3 py-1 text-xs text-[#e0af68]/80 hover:text-[#e0af68] w-full transition-colors"
          >
            <FileWarning className="w-3 h-3 shrink-0" />
            <span>
              {conflicts.length} file{conflicts.length !== 1 ? "s" : ""} under concurrent
              development
            </span>
            {expanded ? (
              <ChevronUp className="w-3 h-3 ml-auto" />
            ) : (
              <ChevronDown className="w-3 h-3 ml-auto" />
            )}
          </button>

          {expanded && (
            <div className="px-3 pb-2 text-xs text-[#a9b1d6]/70 space-y-1">
              {conflicts.map((c) => {
                // (worktree_id, path) is the unique conflict identity now
                const key = `${c.worktree_id ?? ""}::${c.file_path}`;
                return (
                  <div key={key} className="flex items-start gap-1.5">
                    <span className="text-[#e0af68]/60 mt-px">-</span>
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="text-[#a9b1d6]/90 font-mono">{c.file_path}</span>
                      {c.worktree_id && <WorktreeChip worktreeId={c.worktree_id} />}
                      <span className="text-[#a9b1d6]/50">
                        ({c.other_holders.map((h) => h.holder_name).join(", ")})
                      </span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
