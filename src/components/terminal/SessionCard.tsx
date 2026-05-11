import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  TerminalSquare,
  Eye,
  Copy,
  AlertTriangle,
  FileWarning,
  Pin,
  Tag,
  GitBranch,
  GitCommit,
  Loader2,
  CheckCircle2,
  Lock,
} from "lucide-react";
import type { UnifiedSession, SessionLiveStatus } from "./useSessionManager";
import type { LockState } from "./useFileLockTracking";
import type { CommandResponse } from "./types";

interface PromoteToWorktreeData {
  worktree_id: string;
  worktree_path: string;
  branch_name: string;
}

interface CommitProgressData {
  /** New HEAD SHA on success; null when no commit was created (empty or no-diff). */
  commit_hash: string | null;
  /** Number of files in the tracker at commit time (may be 0). */
  file_count: number;
  /** Branch HEAD pointed at in the session's cwd at commit time. */
  branch: string;
  /** The auto-generated commit message that was used. */
  message: string;
}

interface SessionCardProps {
  session: UnifiedSession;
  isSelected: boolean;
  isChecked: boolean;
  selectionMode: boolean;
  isPinned: boolean;
  sessionLabel: string | null;
  /** Number of files this session shares with other sessions (0 = no conflicts) */
  fileConflictCount?: number;
  /**
   * Current per-session file-lock state (waiting / holding / idle).
   * When `kind === "waiting"` the card renders a "blocked on …"
   * subtitle. Sourced from `SessionManagerPanel.sessionLockStates`.
   */
  lockState?: LockState;
  onResume: (session: UnifiedSession) => void;
  onViewTranscript: (session: UnifiedSession) => void;
  onCopyId: (session: UnifiedSession) => void;
  onToggleSelect: (sessionId: string) => void;
  onTogglePin: (sessionId: string) => void;
  onSetLabel: (sessionId: string, label: string | null) => void;
  /** Optional: notify parent that the session list should refresh after a promotion. */
  onAfterPromote?: () => void;
}

const STATUS_DOT: Record<SessionLiveStatus, { color: string; pulse: boolean; label: string }> = {
  "active-in-zone": { color: "#9ece6a", pulse: true, label: "Active in zone" },
  "active-external": { color: "#73daca", pulse: true, label: "Active externally" },
  frozen: { color: "#f7768e", pulse: false, label: "Frozen" },
  "needs-input": { color: "#e0af68", pulse: true, label: "Needs input" },
  completed: { color: "#565f89", pulse: false, label: "Completed" },
  error: { color: "#f7768e", pulse: false, label: "Error" },
  dormant: { color: "#414868", pulse: false, label: "Dormant" },
};

const ACCOUNT_BADGE_COLORS: Record<string, { bg: string; text: string }> = {
  gmail: { bg: "bg-[#7aa2f7]/10", text: "text-[#7aa2f7]" },
  hotmail: { bg: "bg-[#bb9af7]/10", text: "text-[#bb9af7]" },
};

function formatTimeAgo(iso: string): string {
  if (!iso) return "";
  try {
    const diff = Date.now() - new Date(iso).getTime();
    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return "just now";
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days === 1) return "yesterday";
    return `${days}d ago`;
  } catch {
    return "";
  }
}

function formatDuration(ms: number | null): string | null {
  if (ms == null || ms <= 0) return null;
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainMin = minutes % 60;
  if (hours < 24) return `${hours}h${remainMin > 0 ? `${remainMin}m` : ""}`;
  const days = Math.floor(hours / 24);
  return `${days}d${hours % 24 > 0 ? `${hours % 24}h` : ""}`;
}

/**
 * Format a "since" epoch-ms as a short relative-age string ("5s",
 * "42s", "5m", "2h"). Mirrors the helper in `TerminalTabBar.tsx` and
 * `formatHoldingAge` in `LaunchMenu.tsx`. Negative ages clamp to "0s";
 * undefined returns the empty string.
 */
function formatLockSince(ms?: number): string {
  if (ms === undefined) return "";
  const deltaSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (deltaSec < 60) return `${deltaSec}s`;
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m`;
  const deltaHr = Math.floor(deltaMin / 60);
  return `${deltaHr}h`;
}

export function SessionCard({
  session,
  isSelected,
  isChecked,
  selectionMode,
  isPinned,
  sessionLabel,
  fileConflictCount = 0,
  lockState,
  onResume,
  onViewTranscript,
  onCopyId,
  onToggleSelect,
  onTogglePin,
  onSetLabel,
  onAfterPromote,
}: SessionCardProps) {
  const statusInfo = STATUS_DOT[session.liveStatus];
  const accountBadge = ACCOUNT_BADGE_COLORS[session.accountLabel];
  const durationLabel = formatDuration(session.durationMs);

  // Context menu
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [labelInput, setLabelInput] = useState("");
  const [showLabelInput, setShowLabelInput] = useState(false);
  const contextRef = useRef<HTMLDivElement>(null);

  // ── Promote to worktree state ────────────────────────────────────────────
  // The promote command takes a few seconds (kill the current PTY, create
  // worktree, respawn). Local state is enough — there's no toast lib in this
  // app; we surface progress and errors inline on the card itself.
  const [promoteState, setPromoteState] = useState<
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "success"; data: PromoteToWorktreeData }
    | { kind: "error"; message: string; busy: boolean; alreadyInWorktree: boolean }
  >({ kind: "idle" });

  // Promotion is only meaningful for sessions that are actually live in this
  // runner — promote_session_to_worktree looks the session up in
  // SessionManager by task_run_id, which equals the claude session id for
  // this runner's live PTYs. External / dormant / completed sessions can't
  // be promoted from this UI (the backend would just 404).
  const canPromote =
    session.liveStatus === "active-in-zone" ||
    session.liveStatus === "needs-input" ||
    session.liveStatus === "frozen";

  // Hide the button entirely once the promotion has succeeded — the parent
  // refreshes session state and the next render will reflect the new
  // (worktree-backed) session. We keep a brief success badge for ~6s so the
  // user sees the outcome.
  const promoted = promoteState.kind === "success";

  const handlePromote = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!canPromote || promoteState.kind === "loading") return;
      setPromoteState({ kind: "loading" });
      try {
        const result = await invoke<CommandResponse>("promote_session_to_worktree", {
          taskRunId: session.sessionId,
        });
        if (result.success && result.data) {
          const data = result.data as PromoteToWorktreeData;
          setPromoteState({ kind: "success", data });
          onAfterPromote?.();
          // Auto-clear the success badge after a few seconds so the card
          // doesn't carry stale UI forever; by then the refreshed session
          // list should reflect the new worktree state directly.
          setTimeout(() => {
            setPromoteState((prev) => (prev.kind === "success" ? { kind: "idle" } : prev));
          }, 6000);
        } else {
          const msg = result.message ?? "Promotion failed";
          const lower = msg.toLowerCase();
          // The backend distinguishes 409 cases via the message text
          // ("already in worktree" vs "busy"). We can't see the HTTP status
          // through `invoke`, so we sniff the message.
          const alreadyInWorktree = lower.includes("already") && lower.includes("worktree");
          const busy = lower.includes("busy") || lower.includes("in use");
          setPromoteState({ kind: "error", message: msg, busy, alreadyInWorktree });
          setTimeout(() => {
            setPromoteState((prev) => (prev.kind === "error" ? { kind: "idle" } : prev));
          }, 8000);
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setPromoteState({
          kind: "error",
          message: msg,
          busy: false,
          alreadyInWorktree: false,
        });
        setTimeout(() => {
          setPromoteState((prev) => (prev.kind === "error" ? { kind: "idle" } : prev));
        }, 8000);
      }
    },
    [canPromote, promoteState.kind, session.sessionId, onAfterPromote],
  );

  // ── Commit progress state ───────────────────────────────────────────────
  // Commits the session's tracked file-set (PG `session_touched_files`) to
  // whatever branch the session's cwd has checked out — worktree branch if
  // promoted, user's current branch if not. No automatic state changes — the
  // session is left running, the parent doesn't refresh.
  const [commitState, setCommitState] = useState<
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "success"; data: CommitProgressData }
    | { kind: "error"; message: string }
  >({ kind: "idle" });

  // Same predicate as canPromote: committing requires a live session in this
  // runner. A non-promoted session committing to the user's current branch is
  // the explicit, correct behavior.
  const canCommit =
    session.liveStatus === "active-in-zone" ||
    session.liveStatus === "needs-input" ||
    session.liveStatus === "frozen";

  const handleCommit = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!canCommit || commitState.kind === "loading") return;
      setCommitState({ kind: "loading" });
      try {
        const result = await invoke<CommandResponse>("commit_session_progress", {
          taskRunId: session.sessionId,
        });
        if (result.success && result.data) {
          const data = result.data as CommitProgressData;
          setCommitState({ kind: "success", data });
          // Auto-clear the success badge after ~6s. Unlike promote we don't
          // need to refresh the parent — the session itself didn't change.
          setTimeout(() => {
            setCommitState((prev) => (prev.kind === "success" ? { kind: "idle" } : prev));
          }, 6000);
        } else {
          const msg = result.message ?? "Commit failed";
          setCommitState({ kind: "error", message: msg });
          setTimeout(() => {
            setCommitState((prev) => (prev.kind === "error" ? { kind: "idle" } : prev));
          }, 8000);
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setCommitState({ kind: "error", message: msg });
        setTimeout(() => {
          setCommitState((prev) => (prev.kind === "error" ? { kind: "idle" } : prev));
        }, 8000);
      }
    },
    [canCommit, commitState.kind, session.sessionId],
  );

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY });
  }, []);

  // Close on click outside
  useEffect(() => {
    if (!contextMenu) return;
    const handler = (e: MouseEvent) => {
      if (contextRef.current && !contextRef.current.contains(e.target as Node)) {
        setContextMenu(null);
        setShowLabelInput(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [contextMenu]);

  return (
    <div
      onContextMenu={handleContextMenu}
      className={`
        group relative border-l-2 transition-colors
        ${
          isSelected
            ? "border-l-[#7aa2f7] bg-[#7aa2f7]/5"
            : session.liveStatus === "frozen"
              ? "border-l-[#f7768e]/50 hover:bg-[#1a1b26]"
              : session.liveStatus === "needs-input"
                ? "border-l-[#e0af68]/50 hover:bg-[#1a1b26]"
                : session.liveStatus === "active-in-zone"
                  ? "border-l-[#9ece6a]/50 hover:bg-[#1a1b26]"
                  : session.liveStatus === "active-external"
                    ? "border-l-[#73daca]/50 hover:bg-[#1a1b26]"
                    : "border-l-transparent hover:bg-[#1a1b26]"
        }
      `}
    >
      <button
        onClick={() => onViewTranscript(session)}
        className="w-full text-left px-3 py-2 pr-20"
        title={`Session ${session.sessionId}\n${session.workSummaryHint || session.displayName}`}
      >
        {/* Row 1: Status dot + Name + Account badge */}
        <div className="flex items-center gap-1.5 mb-0.5">
          {selectionMode && (
            <input
              type="checkbox"
              checked={isChecked}
              onChange={(e) => {
                e.stopPropagation();
                onToggleSelect(session.sessionId);
              }}
              onClick={(e) => e.stopPropagation()}
              className="w-3 h-3 rounded border-[#414868] bg-[#1a1b26] text-[#7aa2f7] shrink-0 cursor-pointer"
            />
          )}
          <span
            className={`w-2 h-2 rounded-full shrink-0 ${statusInfo.pulse ? "animate-pulse" : ""}`}
            style={{ backgroundColor: statusInfo.color }}
            title={statusInfo.label}
          />
          <span className="text-xs text-[#a9b1d6] truncate flex-1">{session.displayName}</span>
          {accountBadge && (
            <span
              className={`px-1 py-0 rounded text-[9px] font-medium shrink-0 ${accountBadge.bg} ${accountBadge.text}`}
            >
              {session.accountLabel}
            </span>
          )}
          {!accountBadge && session.accountLabel !== "default" && (
            <span className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-[#565f89]/10 text-[#565f89]">
              {session.accountLabel}
            </span>
          )}
          {fileConflictCount > 0 && (
            <span
              className="flex items-center gap-0.5 shrink-0"
              title={`${fileConflictCount} file${fileConflictCount !== 1 ? "s" : ""} shared with other sessions`}
            >
              <FileWarning className="w-3 h-3 text-[#e0af68]" />
              <span className="text-[9px] text-[#e0af68]/80">{fileConflictCount}</span>
            </span>
          )}
          {isPinned && <Pin className="w-3 h-3 text-[#e0af68] shrink-0" />}
          {sessionLabel && (
            <span className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-[#73daca]/10 text-[#73daca] truncate max-w-[60px]">
              {sessionLabel}
            </span>
          )}
        </div>

        {/* Row 2: Meta info */}
        <div className="flex items-center gap-1 text-[10px] text-[#414868] ml-3.5">
          <span>{formatTimeAgo(session.lastModified)}</span>
          <span>&middot;</span>
          <span>{session.messageCount} msgs</span>
          {durationLabel && (
            <>
              <span>&middot;</span>
              <span title="Session duration">{durationLabel}</span>
            </>
          )}
          <span>&middot;</span>
          <span className="truncate">{session.projectLabel}</span>
        </div>

        {/* Row 3: Work summary hint (only for frozen/needs-input/active) */}
        {session.workSummaryHint &&
          session.liveStatus !== "dormant" &&
          session.liveStatus !== "completed" && (
            <div className="mt-1 ml-3.5 text-[10px] text-[#565f89] truncate leading-tight">
              {session.lastMessagePreview || session.workSummaryHint}
            </div>
          )}

        {/* Frozen warning badge */}
        {session.liveStatus === "frozen" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#f7768e]/80">
            <AlertTriangle className="w-3 h-3" />
            <span>Session appears frozen — may need resume</span>
          </div>
        )}

        {/* File-lock waiting subtitle — sourced from useFileLockTracking
            via `sessionLockStates`. Same orange color convention as the
            tab-bar lock icon (`text-[#e0af68]`). */}
        {lockState?.kind === "waiting" && (
          <div
            data-testid="session-card-lock-subtitle"
            className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#e0af68]"
          >
            <Lock className="w-3 h-3 shrink-0" />
            <span className="truncate">
              blocked on {lockState.counterpartyName ?? "another session"}
              {lockState.filePath ? `'s ${lockState.filePath}` : ""}
              {lockState.sinceMs ? ` since ${formatLockSince(lockState.sinceMs)}` : ""}
            </span>
          </div>
        )}

        {/* Promote-to-worktree status */}
        {promoteState.kind === "loading" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#7aa2f7]/80">
            <Loader2 className="w-3 h-3 animate-spin" />
            <span>Promoting session to a new worktree…</span>
          </div>
        )}
        {promoteState.kind === "success" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#9ece6a]/90">
            <CheckCircle2 className="w-3 h-3" />
            <span className="truncate">
              Promoted to worktree{" "}
              <span className="font-mono">{promoteState.data.branch_name}</span>
            </span>
          </div>
        )}
        {promoteState.kind === "error" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#f7768e]/90">
            <AlertTriangle className="w-3 h-3 shrink-0" />
            <span className="truncate" title={promoteState.message}>
              {promoteState.alreadyInWorktree
                ? "Already running in a worktree."
                : promoteState.busy
                  ? `Session is busy — try again in a moment. (${promoteState.message})`
                  : `Promote failed: ${promoteState.message}`}
            </span>
          </div>
        )}

        {/* Commit-progress status */}
        {commitState.kind === "loading" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#7aa2f7]/80">
            <Loader2 className="w-3 h-3 animate-spin" />
            <span>Committing session progress…</span>
          </div>
        )}
        {commitState.kind === "success" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#9ece6a]/90">
            <CheckCircle2 className="w-3 h-3 shrink-0" />
            <span className="truncate">
              {commitState.data.commit_hash ? (
                <>
                  Committed {commitState.data.file_count} file
                  {commitState.data.file_count === 1 ? "" : "s"} to{" "}
                  <span className="font-mono">{commitState.data.branch}</span>{" "}
                  <span className="font-mono text-[#7aa2f7]/80">
                    ({commitState.data.commit_hash.slice(0, 7)})
                  </span>
                </>
              ) : (
                "No changes to commit"
              )}
            </span>
          </div>
        )}
        {commitState.kind === "error" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#f7768e]/90">
            <AlertTriangle className="w-3 h-3 shrink-0" />
            <span className="truncate" title={commitState.message}>
              Commit failed: {commitState.message}
            </span>
          </div>
        )}
      </button>

      {/* Quick action buttons — visible on hover */}
      <div className="absolute right-2 top-1/2 -translate-y-1/2 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
        <button
          onClick={(e) => {
            e.stopPropagation();
            onResume(session);
          }}
          className="p-1 rounded text-[#565f89] hover:text-[#9ece6a] hover:bg-[#9ece6a]/10 transition-colors"
          title={`Resume: claude --resume ${session.sessionId.slice(0, 8)}...`}
        >
          <TerminalSquare className="w-3.5 h-3.5" />
        </button>
        {canPromote && !promoted && (
          <button
            onClick={handlePromote}
            disabled={promoteState.kind === "loading"}
            className="p-1 rounded text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            title="Promote this session into an isolated git worktree"
          >
            {promoteState.kind === "loading" ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <GitBranch className="w-3.5 h-3.5" />
            )}
          </button>
        )}
        {canCommit && (
          <button
            onClick={handleCommit}
            disabled={commitState.kind === "loading"}
            className="p-1 rounded text-[#565f89] hover:text-[#9ece6a] hover:bg-[#9ece6a]/10 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            title="Commit Progress (commits this session's edits to the current branch)"
          >
            {commitState.kind === "loading" ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <GitCommit className="w-3.5 h-3.5" />
            )}
          </button>
        )}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onViewTranscript(session);
          }}
          className="p-1 rounded text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors"
          title="View transcript"
        >
          <Eye className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onCopyId(session);
          }}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title={`Copy session ID: ${session.sessionId}`}
        >
          <Copy className="w-3 h-3" />
        </button>
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={contextRef}
          className="fixed z-50 py-1 min-w-[160px] bg-[#1a1b26] border border-[#2a2d3d] rounded-md shadow-xl"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          {showLabelInput ? (
            <div className="px-2 py-1">
              <input
                autoFocus
                value={labelInput}
                onChange={(e) => setLabelInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    onSetLabel(session.sessionId, labelInput.trim() || null);
                    setContextMenu(null);
                    setShowLabelInput(false);
                    setLabelInput("");
                  } else if (e.key === "Escape") {
                    setShowLabelInput(false);
                    setLabelInput("");
                  }
                }}
                placeholder="Enter label..."
                className="w-full px-2 py-1 text-xs bg-[#13141f] border border-[#2a2d3d] rounded text-[#a9b1d6] placeholder-[#414868] focus:outline-none focus:border-[#7aa2f7]/50"
              />
            </div>
          ) : (
            <>
              <button
                onClick={() => {
                  onResume(session);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <TerminalSquare className="w-3 h-3 text-[#9ece6a]" /> Resume
              </button>
              <button
                onClick={() => {
                  onViewTranscript(session);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Eye className="w-3 h-3 text-[#7aa2f7]" /> View Transcript
              </button>
              <button
                onClick={() => {
                  onCopyId(session);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Copy className="w-3 h-3" /> Copy Session ID
              </button>
              {canPromote && !promoted && (
                <button
                  onClick={(e) => {
                    setContextMenu(null);
                    void handlePromote(e);
                  }}
                  disabled={promoteState.kind === "loading"}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors disabled:opacity-50"
                >
                  {promoteState.kind === "loading" ? (
                    <Loader2 className="w-3 h-3 text-[#7aa2f7] animate-spin" />
                  ) : (
                    <GitBranch className="w-3 h-3 text-[#7aa2f7]" />
                  )}
                  Promote to Worktree
                </button>
              )}
              {canCommit && (
                <button
                  onClick={(e) => {
                    setContextMenu(null);
                    void handleCommit(e);
                  }}
                  disabled={commitState.kind === "loading"}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors disabled:opacity-50"
                >
                  {commitState.kind === "loading" ? (
                    <Loader2 className="w-3 h-3 text-[#9ece6a] animate-spin" />
                  ) : (
                    <GitCommit className="w-3 h-3 text-[#9ece6a]" />
                  )}
                  Commit Progress
                </button>
              )}
              <div className="my-1 h-px bg-[#2a2d3d]" />
              <button
                onClick={() => {
                  onTogglePin(session.sessionId);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Pin className={`w-3 h-3 ${isPinned ? "text-[#e0af68]" : ""}`} />
                {isPinned ? "Unpin" : "Pin to Top"}
              </button>
              <button
                onClick={() => {
                  setShowLabelInput(true);
                  setLabelInput(sessionLabel ?? "");
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Tag className="w-3 h-3 text-[#73daca]" />
                {sessionLabel ? "Edit Label" : "Add Label"}
              </button>
              {!selectionMode && (
                <button
                  onClick={() => {
                    onToggleSelect(session.sessionId);
                    setContextMenu(null);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
                >
                  <Copy className="w-3 h-3" /> Select for Bulk
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
