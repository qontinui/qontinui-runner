import { memo, useState, useEffect, useRef, useCallback, type DragEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIElement } from "@qontinui/ui-bridge";
import {
  TerminalSquare,
  Eye,
  Copy,
  AlertTriangle,
  FileWarning,
  Pin,
  BellOff,
  Clock,
  GitBranch,
  GitCommit,
  Loader2,
  CheckCircle2,
  Lock,
  RotateCcw,
  FolderGit2,
  Monitor,
  X,
} from "lucide-react";
import type { UnifiedSession } from "./useSessionManager";
import type { LockState } from "./useFileLockTracking";
import type { CommandResponse } from "./types";
import { isInjectedSession } from "./syntheticTabs";
import { DURABLE_TOOLTIP } from "./sessionDurability";
import { useNow1Hz } from "./useNow1Hz";
import { describeThrown } from "@/lib/utils";
import type { MemberOrg, SessionGroup, SessionOrgOp } from "./sessionOrgTypes";
import { sessionOrgControlAttrs } from "./sessionOrgControls";
import { SessionOrgMenuItems, type MenuPage } from "./SessionOrgMenus";
import {
  ACTIVITY_DOT,
  activityText,
  isLiveInThisRunner,
  isPossiblyLive,
  locationText,
  SESSION_STATUS_LABELS,
  type SessionActivity,
  type SessionLocation,
  type SessionStatus,
} from "./sessionStatusModel";

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
  /**
   * The row's organization (pin, tags, mute, snooze) from the runner's
   * session-org store; `null` when it has none or the store is not known.
   */
  orgMember: MemberOrg | null;
  /**
   * The left bar's color: the session's GROUP color as a CSS value (D4 —
   * status lives in the dot and its word). `null` = not in a group.
   */
  groupColor?: string | null;
  /** The Pinned section's compact row: name and status words only. */
  compact?: boolean;
  /** The row's organization-store key — what the org menu's ops name. */
  sessionKey?: string;
  /**
   * What the context menu's organization items need (group, pin, page,
   * mute, snooze, tags). One object shared by every card, so the memo holds.
   * Absent when the org is not known — the menu then offers none.
   */
  orgMenu?: SessionCardOrgMenu;
  /** Native drag wiring, shared by every card (reorder, groups, page tabs). */
  drag?: SessionCardDrag;
  /** A drop here would land ABOVE this row — draw the insertion line. */
  dropBefore?: boolean;
  /** Number of files this session shares with other sessions (0 = no conflicts) */
  fileConflictCount?: number;
  /**
   * Current per-session file-lock state (waiting / holding / idle).
   * When `kind === "waiting"` the card renders a "blocked on …"
   * subtitle. Sourced from `SessionManagerPanel.sessionLockStates`.
   */
  lockState?: LockState;
  onResume: (session: UnifiedSession) => void;
  /** Card-body click: go to the open session's terminal, else its transcript. */
  onOpen: (session: UnifiedSession) => void;
  onViewTranscript: (session: UnifiedSession) => void;
  onCopyId: (session: UnifiedSession) => void;
  onToggleSelect: (sessionId: string) => void;
  /**
   * The row click with its modifiers (Phase 5 multi-select): Ctrl/⌘ toggles,
   * Shift selects a range, a plain click opens. Absent = a plain click opens.
   */
  onRowClick?: (session: UnifiedSession, mods: { toggle: boolean; range: boolean }) => void;
  /** The session list's keyboard cursor is on this row. */
  isCursor?: boolean;
  /** Optional: notify parent that the session list should refresh after a promotion. */
  onAfterPromote?: () => void;
  /**
   * Cross-link into the Worktrees cleanup panel (plan
   * `2026-07-19-worktree-cleanup-lifecycle-tracking` Phase 4). Called with
   * the session's worktree path so the panel opens focused on that row —
   * this card's worktree metadata is the only other worktree surface in the
   * runner, so it is where the link belongs.
   */
  onOpenWorktrees?: (worktreePath: string) => void;
  /**
   * tmux work mode: this row's session is the one on screen. Marked distinctly
   * so the operator can tell the shown session from the hidden ones, whose
   * state chips stay as they are (plan `2026-10-11-runner-terminal-tmux-work-mode`).
   */
  isOnScreen?: boolean;
  /**
   * Close this row's terminal tab — the page's close path, the same one the
   * zone's close button takes. Absent when the row has no live tab in this
   * window (a transcript-only row has nothing to close), and the card then
   * offers no close.
   */
  onClose?: (session: UnifiedSession) => void;
}

/** The panel's drag handlers; each names the row it fires on. */
export interface SessionCardDrag {
  start: (session: UnifiedSession, e: DragEvent<HTMLDivElement>) => void;
  end: () => void;
  over: (session: UnifiedSession, e: DragEvent<HTMLDivElement>) => void;
  drop: (session: UnifiedSession, e: DragEvent<HTMLDivElement>) => void;
}

/** The organization menu's shared inputs. */
export interface SessionCardOrgMenu {
  groups: readonly SessionGroup[];
  /** Pages other than this one. */
  pages: readonly MenuPage[];
  apply: (ops: SessionOrgOp[]) => void;
  moveToPage: (session: UnifiedSession, pageId: string) => void;
}

/** "Snoozed until 14:05" when `until` is still ahead of `now`; else null. */
export function snoozeLabel(until: number | null, now: number = Date.now()): string | null {
  if (until === null || until <= now) return null;
  const d = new Date(until);
  const sameDay = new Date(now).toDateString() === d.toDateString();
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return sameDay
    ? `Snoozed until ${time}`
    : `Snoozed until ${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })} ${time}`;
}

/**
 * The activity word. Only `No output 12m (mid-tool)` carries a duration that
 * moves, so only that one subscribes to the shared 1 Hz tick — a list of
 * hundreds of cards does not re-render every second.
 */
function ActivityWord({ status }: { status: SessionStatus }) {
  return status.activity === "no-output-mid-tool" ? (
    <TickingActivityWord status={status} />
  ) : (
    <>{activityText(status, 0)}</>
  );
}

function TickingActivityWord({ status }: { status: SessionStatus }) {
  return <>{activityText(status, useNow1Hz())}</>;
}

/** What a card shows for its status: two words, a dot, and why. */
export interface SessionCardStatusDisplay {
  location: SessionLocation;
  activity: SessionActivity;
  locationLabel: string;
  activityLabel: string;
  color: string;
  pulse: boolean;
  /** Which source decided each axis — the words' tooltip. */
  reason: string;
}

/**
 * The status a card shows: the activity word and the location word, each its
 * own `data-session-*` attribute, plus the dot's color. The word is the
 * signal; the color only repeats it (D2: never color alone).
 *
 * `activityLabel` carries an exit code but not the moving `No output` duration
 * — the rendered word ticks that itself ({@link ActivityWord}).
 */
export function sessionCardStatusDisplay(status: SessionStatus): SessionCardStatusDisplay {
  return {
    location: status.location,
    activity: status.activity,
    locationLabel: locationText(status),
    activityLabel:
      status.activity === "no-output-mid-tool"
        ? SESSION_STATUS_LABELS.activity["no-output-mid-tool"]
        : activityText(status, 0),
    ...ACTIVITY_DOT[status.activity],
    reason: status.reason,
  };
}

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

/**
 * Does this path look like an isolated AGENT worktree (rather than a
 * canonical repo checkout)? Matches the three shapes the runner + coord
 * materialize: the sibling `<repo>-wt-*` convention, the external
 * `qontinui-worktrees/` root, and the per-repo `.claude/worktrees/` dir.
 *
 * Advisory only — it decides whether to OFFER the cross-link. The panel
 * itself is authoritative about what exists and what is reclaimable.
 */
function isWorktreePath(path: string | undefined | null): boolean {
  if (!path) return false;
  const p = path.replace(/\\/g, "/").toLowerCase();
  return (
    p.includes("-wt-") || p.includes("/qontinui-worktrees/") || p.includes("/.claude/worktrees/")
  );
}

/**
 * Card headline: the string Claude Code is showing in this session's own window
 * when we have it, else today's transcript-derived chain.
 *
 * `registryName` is null unless the session is LIVE and its registry row is
 * operator-named, so a `derived` cwd slug or a closed session falls straight
 * through to the previous chain untouched.
 */
export function sessionCardName(
  session: Pick<UnifiedSession, "registryName" | "resumeName" | "displayName">,
): string {
  return session.registryName || session.resumeName || session.displayName;
}

/**
 * Card tooltip: session id, headline, and — only when the registry name
 * displaced a DIFFERENT transcript name — that transcript name too.
 *
 * Without the last line, promoting the registry name would make `resumeName` /
 * `workSummaryHint` unreachable from the UI; they disagreed with the window
 * name in 22 of 33 measured cases (2026-07-23), so "which one is this?" is a
 * question the operator will actually have.
 */
export function sessionCardTooltip(
  session: Pick<
    UnifiedSession,
    "sessionId" | "registryName" | "resumeName" | "workSummaryHint" | "displayName"
  > &
    Partial<Pick<UnifiedSession, "source">>,
): string {
  const headline = sessionCardName(session);
  // A tab-sourced row is keyed by its tab id — calling that a session id would
  // invite pasting it into `claude --resume`.
  const base =
    session.source === "tab"
      ? `Terminal ${session.sessionId}\n${headline}`
      : `Session ${session.sessionId}\n${headline}`;
  const transcriptName = session.resumeName || session.workSummaryHint || session.displayName;
  if (transcriptName && transcriptName !== headline) {
    return `${base}\ntranscript name: ${transcriptName}`;
  }
  return base;
}

/** Which of a card's actions apply to its row. */
export interface SessionCardActions {
  resume: boolean;
  viewTranscript: boolean;
  copySessionId: boolean;
  promote: boolean;
  commit: boolean;
}

/**
 * The card's action set. A tab-sourced row (a plain shell, a session before its
 * first message, a remote session tab) has no transcript, so view transcript,
 * copy session id and resume do not apply, and nothing backs
 * promote or commit (both key on a Claude session id the row does not carry).
 * Promote and commit additionally need a session that is live in this runner
 * (`On screen` / `Hidden here`, not exited).
 */
export function sessionCardActions(
  session: Pick<UnifiedSession, "source" | "status">,
): SessionCardActions {
  const transcript = session.source === "transcript";
  const liveHere = isLiveInThisRunner(session.status);
  return {
    resume: transcript,
    viewTranscript: transcript,
    copySessionId: transcript,
    promote: transcript && liveHere,
    commit: transcript && liveHere,
  };
}

/**
 * Author-controlled UI-Bridge control id for one session card.
 *
 * Same convention as `SESSION_MANAGER_TOGGLE_ID`
 * (`terminal.session-manager-toggle`) and `ZoneHoverActions`'
 * `terminal.close-session-<id>`. The stamp is load-bearing rather than
 * decorative: `useAutoRegister` preserves an existing stamp verbatim, skips
 * collision-suffixing for stamped ids and early-returns on re-registration,
 * whereas the auto-derived id is minted from the element's TEXT — and a card's
 * text is the session title, which changes whenever Claude re-titles the
 * session or the operator runs `/rename`. An auto id would therefore go stale
 * mid-session and silently strand any driver holding it.
 */
export function sessionCardElementId(sessionId: string): string {
  return `terminal.session-card-${sessionId}`;
}

/**
 * UI-Bridge id of a card's close button. Stamped for the same reason as
 * {@link sessionCardElementId}, and distinct from the zone close button's
 * `terminal.close-session-<tabId>`: both can be on screen at once for the
 * same session, and the stamp must name one element.
 */
export function sessionCardCloseElementId(sessionId: string): string {
  return `terminal.session-card-close-${sessionId}`;
}

/**
 * Memoized: the session manager renders one card per session, so an unrelated
 * re-render of the panel used to walk every card (plan Phase 1).
 */
function SessionCardInner({
  session,
  isSelected,
  isChecked,
  selectionMode,
  orgMember,
  groupColor = null,
  compact = false,
  sessionKey,
  orgMenu,
  drag,
  dropBefore = false,
  fileConflictCount = 0,
  lockState,
  onResume,
  onOpen,
  onViewTranscript,
  onCopyId,
  onToggleSelect,
  onRowClick,
  isCursor = false,
  onAfterPromote,
  onOpenWorktrees,
  isOnScreen = false,
  onClose,
}: SessionCardProps) {
  const actions = sessionCardActions(session);
  const statusInfo = sessionCardStatusDisplay(session.status);
  // "Still running somewhere" for the live-only affordances below: a process
  // that may be up (an UNKNOWN placement included) and has not exited.
  const mayBeRunning =
    isPossiblyLive(session.status.location) &&
    session.status.activity !== "exited" &&
    session.status.activity !== "exited-error";
  const accountBadge = ACCOUNT_BADGE_COLORS[session.accountLabel];
  const durationLabel = formatDuration(session.durationMs);

  // Context menu
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const contextRef = useRef<HTMLDivElement>(null);
  const closeMenu = useCallback(() => setContextMenu(null), []);
  const isPinned = orgMember?.pinned ?? false;
  const tags = orgMember?.tags ?? [];
  const snoozed = snoozeLabel(orgMember?.snoozedUntil ?? null);

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
  // this runner's live PTYs. Outside-runner / not-running / exited sessions
  // can't be promoted from this UI (the backend would just 404).
  const canPromote = actions.promote;

  // Hide the button entirely once the promotion has succeeded — the parent
  // refreshes session state and the next render will reflect the new
  // (worktree-backed) session. We keep a brief success badge for ~6s so the
  // user sees the outcome.
  const promoted = promoteState.kind === "success";

  const handlePromote = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!canPromote || promoteState.kind === "loading") return;
      // F1 inertness: a debug-injected fake has no real ClaudeSession in the
      // manager — `promote_session_to_worktree` would 404. No-op it (and don't
      // flip the card into a misleading loading/error state).
      if (isInjectedSession(session)) {
        console.warn(
          `[test-fixtures] ignoring promote on synthetic tab ${session.zoneTabId ?? session.sessionId}`,
        );
        return;
      }
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
        const msg = describeThrown(err, "Failed to promote session to worktree");
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
    [canPromote, promoteState.kind, session, onAfterPromote],
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
  const canCommit = actions.commit;

  const handleCommit = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!canCommit || commitState.kind === "loading") return;
      // F1 inertness: a debug-injected fake has no real ClaudeSession —
      // `commit_session_progress` would 404. No-op it.
      if (isInjectedSession(session)) {
        console.warn(
          `[test-fixtures] ignoring commit on synthetic tab ${session.zoneTabId ?? session.sessionId}`,
        );
        return;
      }
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
        const msg = describeThrown(err, "Failed to commit session progress");
        setCommitState({ kind: "error", message: msg });
        setTimeout(() => {
          setCommitState((prev) => (prev.kind === "error" ? { kind: "idle" } : prev));
        }, 8000);
      }
    },
    [canCommit, commitState.kind, session],
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
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [contextMenu]);

  const cardName = sessionCardName(session);
  // Registered on the card's main BUTTON, not the wrapper `<div>`: the button
  // is what actually acts on a click — it focuses the session's open terminal
  // tab when it has one, else opens the transcript (a click dispatched at the wrapper
  // would not reach the child handler), and it spans the whole visible card
  // row, so its rect is the card's rect.
  //
  // The display title travels as a `data-*` attribute rather than as
  // `data-ui-bridge-content` on the title `<span>`. Both would remove the
  // `span.text-xs.truncate.flex-1` + `page/evaluate` scrape, but the content
  // attribute registers a SECOND element per card whose value the driver still
  // has to read as text — two ids and two reads for one card, and the text of
  // that span is the only field it can ever carry. `ElementState.dataset`
  // projects EVERY `data-*` attribute of the element the driver already
  // addresses, so one `GET /control/element/terminal.session-card-<id>`
  // returns the title, the live status and the account together, each as its
  // own typed field — and none of them polluted by the meta row, the badges
  // or the promote/commit status lines that share the button's `textContent`.
  const { ref: cardBridgeRef } = useUIElement({
    id: sessionCardElementId(session.sessionId),
    type: "button",
    label: cardName,
  });

  return (
    <div
      onContextMenu={handleContextMenu}
      draggable={drag !== undefined}
      onDragStart={drag ? (e) => drag.start(session, e) : undefined}
      onDragEnd={drag?.end}
      onDragOver={drag ? (e) => drag.over(session, e) : undefined}
      onDrop={drag ? (e) => drag.drop(session, e) : undefined}
      data-session-group-color={groupColor ?? "none"}
      // The left bar is the session's GROUP color (D4); status is the dot and
      // its word. Selection and on-screen keep their background tint.
      style={{ borderLeftColor: groupColor ?? "transparent" }}
      className={`
        group relative border-l-[3px] transition-colors
        ${dropBefore ? "border-t-2 border-t-brand-primary" : ""}
        ${isChecked ? "bg-[#7aa2f7]/15" : isOnScreen ? "bg-brand-primary/10" : isSelected ? "bg-[#7aa2f7]/5" : "hover:bg-[#1a1b26]"}
      `}
    >
      <button
        ref={cardBridgeRef}
        data-ui-bridge-id={sessionCardElementId(session.sessionId)}
        // Projected verbatim into `ElementState.dataset` (camelCased), so a
        // driver reads the card's fields off one element instead of scraping
        // its rendered text.
        data-session-id={session.sessionId}
        data-session-title={cardName}
        data-session-location={statusInfo.location}
        data-session-activity={statusInfo.activity}
        data-session-account={session.accountLabel}
        data-session-source={session.source}
        data-session-on-screen={isOnScreen ? "true" : "false"}
        data-session-checked={isChecked ? "true" : "false"}
        data-session-cursor={isCursor ? "true" : "false"}
        aria-selected={isChecked}
        aria-current={isOnScreen ? "true" : undefined}
        // `accessibleName` and `ai/find` resolve on this. `title` carries the
        // rich multi-line tooltip, which makes a poor name, so the headline is
        // stated separately here; the tooltip is unchanged visually.
        aria-label={cardName}
        onClick={(e) =>
          onRowClick
            ? onRowClick(session, { toggle: e.ctrlKey || e.metaKey, range: e.shiftKey })
            : onOpen(session)
        }
        // The list owns keyboard focus (↑/↓/Enter/Space); rows are reached by it.
        tabIndex={onRowClick ? -1 : undefined}
        className={`w-full text-left px-3 pr-20 ${compact ? "py-1" : "py-2"} ${
          isCursor ? "outline outline-1 -outline-offset-1 outline-brand-primary/60" : ""
        }`}
        title={sessionCardTooltip(session)}
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
            title={`${statusInfo.activityLabel} · ${statusInfo.locationLabel}`}
          />
          <span className="text-xs text-[#a9b1d6] truncate flex-1">{sessionCardName(session)}</span>
          {isOnScreen && (
            <span
              data-testid="session-card-on-screen"
              className="flex items-center gap-0.5 px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-brand-primary/15 text-brand-primary"
              title="This session is the one on screen"
            >
              <Monitor className="w-2.5 h-2.5" />
              on screen
            </span>
          )}
          {session.remoteDeviceLabel && (
            <span
              className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-surface-raised text-text-muted truncate max-w-[80px]"
              title={`Remote session on ${session.remoteDeviceLabel}`}
            >
              {session.remoteDeviceLabel}
            </span>
          )}
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
          {/* Durable AI-session affordance (Phase 5 honesty label). A
              transcript row is a transcript-backed AI/chat session, which
              resumes automatically on a runner restart (and is listed by the
              startup recovery banner). We surface a subtle, calm cue ONLY for
              live sessions — historical dormant/completed transcripts don't
              need the reassurance and it would clutter the long list. This is
              the deliberate counterpart to the "ephemeral" marker on
              non-durable interactive PTY tabs — which is why a tab-sourced
              row (a plain shell, nothing to resume) never carries it. */}
          {actions.resume && mayBeRunning && (
            <span
              className="shrink-0 inline-flex"
              title={DURABLE_TOOLTIP}
              aria-label={`Durable session: ${DURABLE_TOOLTIP}`}
            >
              <RotateCcw className="w-3 h-3 text-[#565f89]" />
            </span>
          )}
          {isPinned && (
            <span className="shrink-0 inline-flex" title="Pinned">
              <Pin className="w-3 h-3 text-[#e0af68]" aria-label="Pinned" />
            </span>
          )}
          {orgMember?.muted && (
            <span className="shrink-0 inline-flex" title="Muted: no needs-input toast or sound">
              <BellOff className="w-3 h-3 text-[#565f89]" aria-label="Muted" />
            </span>
          )}
          {snoozed && (
            <span className="shrink-0 inline-flex" title={snoozed}>
              <Clock className="w-3 h-3 text-[#565f89]" aria-label={snoozed} />
            </span>
          )}
          {tags.slice(0, 2).map((tag) => (
            <span
              key={tag}
              data-session-tag={tag}
              className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-[#73daca]/10 text-[#73daca] truncate max-w-[60px]"
              title={`Tag: ${tag}`}
            >
              {tag}
            </span>
          ))}
          {tags.length > 2 && (
            <span className="text-[9px] text-[#565f89] shrink-0" title={`Tags: ${tags.join(", ")}`}>
              +{tags.length - 2}
            </span>
          )}
        </div>

        {/* Row 2: Status words — activity, then location. Each carries the
            source that decided it in its tooltip. */}
        <div
          data-testid="session-card-status"
          className="flex items-center gap-1 text-[10px] ml-3.5 min-w-0"
          title={statusInfo.reason}
        >
          <span className="shrink-0 font-medium" style={{ color: statusInfo.color }}>
            <ActivityWord status={session.status} />
          </span>
          <span className="text-[#414868]">&middot;</span>
          <span className="truncate text-[#565f89]">{statusInfo.locationLabel}</span>
        </div>

        {/* Row 3: Meta info (not on a compact pinned row) */}
        {!compact && (
          <div className="flex items-center gap-1 text-[10px] text-[#414868] ml-3.5">
            <span>{formatTimeAgo(session.lastModified)}</span>
            <span>&middot;</span>
            {/* A tab-sourced row has no transcript, so no message count. */}
            <span>{session.source === "tab" ? "terminal" : `${session.messageCount} msgs`}</span>
            {durationLabel && (
              <>
                <span>&middot;</span>
                <span title="Session duration">{durationLabel}</span>
              </>
            )}
            <span>&middot;</span>
            <span className="truncate">{session.projectLabel}</span>
          </div>
        )}

        {/* Row 4: Work summary hint (only while the session may be running) */}
        {!compact && session.workSummaryHint && mayBeRunning && (
          <div className="mt-1 ml-3.5 text-[10px] text-[#565f89] truncate leading-tight">
            {session.lastMessagePreview || session.workSummaryHint}
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
            {onOpenWorktrees && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onOpenWorktrees(promoteState.data.worktree_path);
                }}
                className="shrink-0 underline text-[#7aa2f7]/90 hover:text-[#7aa2f7]"
                title="Show this worktree in the Worktrees cleanup panel"
              >
                manage
              </button>
            )}
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
        {actions.resume && (
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
        )}
        {/*
          Cross-link into the Worktrees cleanup panel for a session that is
          ALREADY running in an isolated worktree (the promote affordance
          below is for sessions that aren't). Same panel, focused on this
          session's worktree row.
        */}
        {onOpenWorktrees && isWorktreePath(session.projectPath) && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onOpenWorktrees(session.projectPath);
            }}
            className="p-1 rounded text-[#565f89] hover:text-[#bb9af7] hover:bg-[#bb9af7]/10 transition-colors"
            title={`Show this session's worktree in the Worktrees cleanup panel: ${session.projectPath}`}
          >
            <FolderGit2 className="w-3.5 h-3.5" />
          </button>
        )}
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
        {actions.viewTranscript && (
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
        )}
        {actions.copySessionId && (
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
        )}
        {onClose && (
          <button
            data-ui-bridge-id={sessionCardCloseElementId(session.sessionId)}
            onClick={(e) => {
              e.stopPropagation();
              onClose(session);
            }}
            className="p-1 rounded text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 transition-colors"
            title="Close this session's terminal"
            aria-label="Close session"
          >
            <X className="w-3.5 h-3.5" />
          </button>
        )}
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={contextRef}
          className="fixed z-50 py-1 min-w-[160px] bg-[#1a1b26] border border-[#2a2d3d] rounded-md shadow-xl"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <>
            {actions.resume && (
              <button
                onClick={() => {
                  onResume(session);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <TerminalSquare className="w-3 h-3 text-[#9ece6a]" /> Resume
              </button>
            )}
            {actions.viewTranscript && (
              <button
                onClick={() => {
                  onViewTranscript(session);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Eye className="w-3 h-3 text-[#7aa2f7]" /> View Transcript
              </button>
            )}
            {actions.copySessionId && (
              <button
                onClick={() => {
                  onCopyId(session);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Copy className="w-3 h-3" /> Copy Session ID
              </button>
            )}
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
            {orgMenu && sessionKey && (
              <>
                <div className="my-1 h-px bg-[#2a2d3d]" />
                <SessionOrgMenuItems
                  sessionId={session.sessionId}
                  sessionKey={sessionKey}
                  member={orgMember}
                  groups={orgMenu.groups}
                  pages={orgMenu.pages}
                  hasLiveTab={onClose !== undefined}
                  onApply={orgMenu.apply}
                  onMoveToPage={(pageId) => orgMenu.moveToPage(session, pageId)}
                  close={closeMenu}
                />
              </>
            )}
            {!selectionMode && (
              <button
                onClick={() => {
                  onToggleSelect(session.sessionId);
                  setContextMenu(null);
                }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Copy className="w-3 h-3" /> Select (Ctrl-click)
              </button>
            )}
            {onClose && (
              <>
                <div className="my-1 h-px bg-[#2a2d3d]" />
                <button
                  {...sessionOrgControlAttrs("sessionMenuClose", session.sessionId)}
                  onClick={() => {
                    setContextMenu(null);
                    onClose(session);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#f7768e] hover:bg-[#2a2d3d] transition-colors"
                >
                  <X className="w-3 h-3" /> Close…
                </button>
              </>
            )}
          </>
        </div>
      )}
    </div>
  );
}

export const SessionCard = memo(SessionCardInner);
