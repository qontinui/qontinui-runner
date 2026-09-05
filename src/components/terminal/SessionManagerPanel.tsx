import { useMemo, useState } from "react";
import { TerminalSquare, X, CheckSquare, History, Radio, Server } from "lucide-react";
import { SessionManagerHeader } from "./SessionManagerHeader";
import { StewardControl } from "./StewardControl";
import { SessionCard } from "./SessionCard";
import { WorktreesPanel } from "../worktrees/WorktreesPanel";
import { PastSessionsView } from "./PastSessionsView";
import { FleetSessionPicker } from "./FleetSessionPicker";
import type { PastSession } from "./usePastSessions";
import type {
  UseSessionManagerReturn,
  UnifiedSession,
  SessionLiveStatus,
} from "./useSessionManager";
import type { LockState } from "./useFileLockTracking";

interface SessionManagerPanelProps {
  manager: UseSessionManagerReturn;
  selectedSessionId: string | null;
  /** Map of session task_run_id → number of conflicting files */
  sessionConflictCounts?: Map<string, number>;
  /**
   * Map of session id → current {@link LockState}. Sessions in
   * `kind: "waiting"` get a "blocked on …" subtitle in their card.
   * Key is `session.sessionId` (= the Claude transcript session id,
   * which equals `tab.claudeSessionId` for live PTY tabs); the parent
   * derives this from the per-tab `fileLockStates` keyed on `tab.id`.
   */
  sessionLockStates?: Map<string, LockState>;
  /**
   * Resume a past (historical) session by id. Wired in `TerminalPage` to the
   * real terminal resume path (`shellIntegration.handleResumeSession`) — it
   * creates a tab and queues `claude --resume <id>` with the session's config
   * dir. Consumed only by the "Previous" view; undefined ⇒ Resume falls back to
   * copying the command.
   */
  onResumePastSession?: (session: PastSession) => void;
}

/** Human-readable status group labels. */
const STATUS_GROUP_LABELS: Record<SessionLiveStatus, string> = {
  frozen: "Frozen",
  "needs-input": "Needs Input",
  "active-in-zone": "Active",
  "active-external": "Active (External)",
  error: "Error",
  completed: "Completed",
  dormant: "Recent",
};

/** Group sessions by the current groupBy mode. Returns [groupKey, sessions][] */
function groupSessions(
  sessions: UnifiedSession[],
  groupBy: "status" | "account" | "project" | "date",
): [string, UnifiedSession[]][] {
  const groups = new Map<string, UnifiedSession[]>();

  for (const session of sessions) {
    let key: string;
    switch (groupBy) {
      case "status":
        key = STATUS_GROUP_LABELS[session.liveStatus] || session.liveStatus;
        break;
      case "account":
        key = session.accountLabel;
        break;
      case "project":
        key = session.projectLabel || "Unknown";
        break;
      case "date":
        key = formatDateGroupKey(session.lastModified);
        break;
      default:
        key = "Other";
    }

    const list = groups.get(key);
    if (list) {
      list.push(session);
    } else {
      groups.set(key, [session]);
    }
  }

  return Array.from(groups.entries());
}

function formatDateGroupKey(iso: string): string {
  if (!iso) return "Unknown";
  try {
    const d = new Date(iso);
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const target = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    const diff = (today.getTime() - target.getTime()) / 86400000;

    if (diff === 0) return "Today";
    if (diff === 1) return "Yesterday";
    if (diff < 7) return "This Week";
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  } catch {
    return "Unknown";
  }
}

export function SessionManagerPanel({
  manager,
  selectedSessionId,
  sessionConflictCounts,
  sessionLockStates,
  onResumePastSession,
}: SessionManagerPanelProps) {
  const [view, setView] = useState<"live" | "previous" | "fleet">("live");
  const {
    sessions,
    loading,
    accountUsage,
    searchQuery,
    setSearchQuery,
    statusFilter,
    setStatusFilter,
    accountFilter,
    setAccountFilter,
    groupBy,
    setGroupBy,
    sortBy,
    setSortBy,
    refresh,
    resumeSession,
    viewTranscript,
    copySessionId,
    selectedIds,
    toggleSelect,
    selectAllFrozen,
    clearSelection,
    bulkResume,
    pinnedIds,
    togglePin,
    sessionLabels,
    setSessionLabel,
    allAccounts,
    frozenCount,
    needsInputCount,
    activeCount,
  } = manager;

  const selectionMode = selectedIds.size > 0;

  const groups = useMemo(() => groupSessions(sessions, groupBy), [sessions, groupBy]);

  /**
   * Cross-link target from `SessionCard`: the worktree path of the session
   * whose "manage worktree" affordance was clicked. Opens + highlights the
   * row in the Worktrees panel below.
   */
  const [worktreeFocus, setWorktreeFocus] = useState<string | null>(null);

  return (
    <div className="w-[340px] h-full flex flex-col border-r border-[#2a2d3d] bg-[#13141f] shrink-0">
      {/*
        Live | Previous view toggle.

        Both buttons carry an author-controlled `data-ui-bridge-id`: the SDK's
        auto-derived ids would slugify from the visible text (`button-live` /
        `button-previous`), which is neither namespaced nor stable across the
        page. With these stamps a driver can switch the panel deterministically
        — `POST /ui-bridge/control/element/terminal.session-manager-view-previous/action
        {"action":"click"}` — which is the only way to reach the
        `past-sessions-view` surface, since the panel always mounts on "live".

        This row sits ABOVE the `view === "previous" ? … : <>…</>` conditional
        so it renders in both views; see `SessionManagerPanel.test.ts` for the
        arm invariant that guards the panels below.
      */}
      <div className="flex items-center gap-1 px-2 py-1.5 border-b border-[#2a2d3d]">
        <button
          data-ui-bridge-id="terminal.session-manager-view-live"
          onClick={() => setView("live")}
          className={`flex-1 flex items-center justify-center gap-1 px-2 py-1 rounded text-[10px] font-medium transition-colors ${
            view === "live"
              ? "bg-[#7aa2f7]/15 text-[#7aa2f7]"
              : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
          }`}
          title="Live sessions currently on this runner"
        >
          <Radio className="w-3 h-3" />
          Live
        </button>
        <button
          data-ui-bridge-id="terminal.session-manager-view-previous"
          onClick={() => setView("previous")}
          className={`flex-1 flex items-center justify-center gap-1 px-2 py-1 rounded text-[10px] font-medium transition-colors ${
            view === "previous"
              ? "bg-[#7aa2f7]/15 text-[#7aa2f7]"
              : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
          }`}
          title="Previous sessions — every session previously on this runner; resume by name"
        >
          <History className="w-3 h-3" />
          Previous
        </button>
        <button
          data-ui-bridge-id="terminal.session-manager-view-fleet"
          onClick={() => setView("fleet")}
          className={`flex-1 flex items-center justify-center gap-1 px-2 py-1 rounded text-[10px] font-medium transition-colors ${
            view === "fleet"
              ? "bg-[#7aa2f7]/15 text-[#7aa2f7]"
              : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
          }`}
          title="Fleet sessions — every session on every machine in this tenant, read-only"
        >
          <Server className="w-3 h-3" />
          Fleet
        </button>
      </div>

      {view === "fleet" ? (
        <FleetSessionPicker />
      ) : view === "previous" ? (
        <PastSessionsView onResumePastSession={onResumePastSession} />
      ) : (
        <>
      <StewardControl />
      <SessionManagerHeader
        loading={loading}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        statusFilter={statusFilter}
        onStatusFilterChange={setStatusFilter}
        accountFilter={accountFilter}
        onAccountFilterChange={setAccountFilter}
        groupBy={groupBy}
        onGroupByChange={setGroupBy}
        sortBy={sortBy}
        onSortByChange={setSortBy}
        accountUsage={accountUsage}
        accounts={allAccounts}
        onRefresh={refresh}
        frozenCount={frozenCount}
        needsInputCount={needsInputCount}
        activeCount={activeCount}
        totalCount={sessions.length}
      />

      {/* Bulk action bar */}
      {selectionMode && (
        <div className="flex items-center gap-2 px-3 py-1.5 bg-[#7aa2f7]/5 border-b border-[#2a2d3d]">
          <span className="text-[10px] text-[#7aa2f7] font-medium">
            {selectedIds.size} selected
          </span>
          <div className="flex-1" />
          <button
            onClick={bulkResume}
            className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25 transition-colors"
            title="Resume all selected sessions"
          >
            <TerminalSquare className="w-3 h-3" />
            Resume {selectedIds.size}
          </button>
          <button
            onClick={clearSelection}
            className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
            title="Clear selection"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      )}

      {/* Select all frozen shortcut */}
      {!selectionMode && frozenCount > 1 && (
        <div className="flex items-center px-3 py-1 border-b border-[#2a2d3d]">
          <button
            onClick={selectAllFrozen}
            className="flex items-center gap-1 text-[10px] text-[#f7768e]/70 hover:text-[#f7768e] transition-colors"
          >
            <CheckSquare className="w-3 h-3" />
            Select all {frozenCount} frozen sessions
          </button>
        </div>
      )}

      {/* Session list */}
      <div className="flex-1 overflow-y-auto scrollbar-dark">
        {loading && sessions.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            <div className="w-3 h-3 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
            Loading sessions...
          </div>
        ) : sessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#565f89] text-xs">
            {searchQuery || statusFilter !== "all" || accountFilter !== "all"
              ? "No sessions match filters"
              : "No sessions found"}
          </div>
        ) : (
          groups.map(([groupKey, groupSessions]) => (
            <div key={groupKey}>
              {/* Group header */}
              {groups.length > 1 && (
                <div className="flex items-center gap-2 px-3 pt-3 pb-1">
                  <div className="flex-1 h-px bg-[#2a2d3d]" />
                  <span className="text-[10px] font-medium text-[#565f89] whitespace-nowrap">
                    {groupKey}
                    <span className="ml-1 text-[#414868]">({groupSessions.length})</span>
                  </span>
                  <div className="flex-1 h-px bg-[#2a2d3d]" />
                </div>
              )}

              {/* Session cards */}
              {groupSessions.map((session) => (
                <SessionCard
                  key={session.sessionId}
                  session={session}
                  isSelected={session.sessionId === selectedSessionId}
                  isChecked={selectedIds.has(session.sessionId)}
                  selectionMode={selectionMode}
                  isPinned={pinnedIds.has(session.sessionId)}
                  sessionLabel={sessionLabels[session.sessionId] ?? null}
                  fileConflictCount={sessionConflictCounts?.get(session.sessionId) ?? 0}
                  lockState={sessionLockStates?.get(session.sessionId)}
                  onResume={resumeSession}
                  onViewTranscript={viewTranscript}
                  onCopyId={copySessionId}
                  onToggleSelect={toggleSelect}
                  onTogglePin={togglePin}
                  onSetLabel={setSessionLabel}
                  onAfterPromote={refresh}
                  onOpenWorktrees={setWorktreeFocus}
                />
              ))}
            </div>
          ))
        )}
      </div>

      {/*
        Worktrees panel — sibling of the session list. The on-demand,
        human-consented cleanup trigger (plan
        `2026-07-19-worktree-cleanup-lifecycle-tracking` Phase 4). Collapsed
        by default so it costs nothing until asked for; `SessionCard`'s
        worktree affordance opens it focused on that session's worktree.
      */}
      <WorktreesPanel defaultOpen={worktreeFocus !== null} highlightPath={worktreeFocus} />
        </>
      )}
    </div>
  );
}
