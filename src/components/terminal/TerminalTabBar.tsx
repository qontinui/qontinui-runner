import { useState, useRef, useEffect, useMemo, useCallback, type ReactNode } from "react";
import { Terminal, X, Plus, List, ChevronDown, FileText, Lock } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";
import type { AccountUsageInfo } from "./useSessionManager";
import { LaunchMenu } from "./LaunchMenu";
import { useUIComponent, UIBridgeComponentScope } from "@qontinui/ui-bridge";
import { WrapperToolsBadge } from "@/components/wrappers/WrapperToolsBadge";
import { ReviewBadge } from "./ReviewBadge";
import { CommitTrafficLight } from "./CommitTrafficLight";
import type { CommitState } from "./useCommitState";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { RefObject } from "react";

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  onRename: (id: string, title: string) => void;
  sessionStates?: Record<string, SessionState>;
  layoutPicker?: ReactNode;
  onQuickLaunch?: (count: number, autoCommand?: string) => void;
  onLaunchAiSession?: (count: number, configDir: string, context?: string) => void;
  onLaunchMultiAiSessions?: (configDirs: string[], context?: string) => void;
  accountUsage?: AccountUsageInfo[];
  launchCommands?: Record<string, string>;
  fileLocks?: import("./useSessionManager").FileLockInfo[];
  fileLockStates?: Record<string, import("./useFileLockTracking").LockState>;
  /** Per-tab commit-readiness state (populated by `useCommitState`). */
  commitStates?: Record<string, CommitState>;
  /**
   * Map of tab.id → ref to the live TerminalInstance handle. Sourced
   * from `terminalRefs.current` in TerminalPage; the commit traffic
   * light reads `.current?.writeToTerminal` from the matching ref to
   * inject the commit prompt into the PTY. Optional — when omitted the
   * SDK-chat injection path runs instead, but no SDK chat tab type
   * currently routes through this component.
   */
  terminalRefs?: Map<string, RefObject<TerminalInstanceHandle | null>>;
  // Zone monitoring enrichments
  activityData?: Record<string, number[]>;
  stateDurations?: Record<string, string>;
  lastOutputLines?: Record<string, string[]>;
  unreadTabs?: Set<string>;
  staleTabs?: Set<string>;
  zoneLabels?: Record<number, string>;
  labelColorMap?: Record<string, string>;
  assignments?: ZoneAssignments;
}

const STATE_DOT_COLORS: Record<SessionState, string> = {
  idle: "bg-[#565f89]",
  working: "bg-[#7aa2f7]",
  "needs-input": "bg-[#e0af68]",
  completed: "bg-[#9ece6a]",
  error: "bg-[#f7768e]",
};

const STATE_BAR_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

function formatTime(value?: string | number): string {
  if (value === undefined || value === null) return "";
  try {
    const d = new Date(value);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

/**
 * Format a "since" epoch-ms as a short relative-age string ("5s",
 * "42s", "5m", "2h"). Mirrors `formatHoldingAge` from `LaunchMenu.tsx`
 * but takes the "since" timestamp directly. Negative ages clamp to
 * "0s". Returns the empty string when `ms` is undefined.
 */
function formatSince(ms?: number): string {
  if (ms === undefined) return "";
  const deltaSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (deltaSec < 60) return `${deltaSec}s`;
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m`;
  const deltaHr = Math.floor(deltaMin / 60);
  return `${deltaHr}h`;
}

// ── Activity Sparkline ─────────────────────────────────────────────────────

function ActivitySparkline({ data, color }: { data: number[]; color: string }) {
  const points = data.slice(-8);
  if (points.length === 0) return null;
  const max = Math.max(...points, 1);
  const barWidth = 30 / points.length;

  return (
    <svg width={30} height={10} viewBox="0 0 30 10" className="shrink-0 opacity-80">
      {points.map((v, i) => {
        const h = Math.max((v / max) * 9, 0.5);
        return (
          <rect
            key={`bar-${i}`}
            x={i * barWidth + 0.5}
            y={10 - h}
            width={Math.max(barWidth - 1, 1)}
            height={h}
            fill={color}
            rx={0.5}
          />
        );
      })}
    </svg>
  );
}

// ── Tag Badges ─────────────────────────────────────────────────────────────

function TagBadges({
  labels,
  colorMap,
  max = 2,
}: {
  labels: string;
  colorMap?: Record<string, string>;
  max?: number;
}) {
  const tags = labels
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  if (tags.length === 0) return null;

  const visible = tags.slice(0, max);
  const overflow = tags.length - max;

  return (
    <span className="flex items-center gap-0.5 flex-wrap">
      {visible.map((tag) => (
        <span
          key={tag}
          className="px-1 py-0 rounded text-[8px] leading-[14px] font-medium"
          style={{
            backgroundColor: (colorMap?.[tag] ?? "#565f89") + "22",
            color: colorMap?.[tag] ?? "#a9b1d6",
          }}
        >
          {tag}
        </span>
      ))}
      {overflow > 0 && <span className="text-[8px] text-[#565f89]">+{overflow}</span>}
    </span>
  );
}

// ── Duration Badge ─────────────────────────────────────────────────────────

function DurationBadge({ duration, state }: { duration: string; state: SessionState }) {
  if (!duration) return null;
  const show = state === "needs-input" || state === "error" || state === "working";
  if (!show) return null;

  const color =
    state === "error"
      ? "text-[#f7768e]"
      : state === "needs-input"
        ? "text-[#e0af68]"
        : "text-[#7aa2f7]";

  return <span className={`text-[9px] ${color} opacity-80 shrink-0`}>{duration}</span>;
}

// ── Main Component ─────────────────────────────────────────────────────────

export function TerminalTabBar({
  tabs,
  activeId,
  onSelect,
  onClose,
  onCreate,
  onRename,
  sessionStates,
  layoutPicker,
  onQuickLaunch,
  onLaunchAiSession,
  onLaunchMultiAiSessions,
  accountUsage,
  launchCommands,
  fileLocks,
  fileLockStates,
  commitStates,
  terminalRefs,
  activityData,
  stateDurations,
  unreadTabs,
  staleTabs,
  zoneLabels,
  labelColorMap,
  assignments,
}: TerminalTabBarProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [showDropdown, setShowDropdown] = useState(false);
  const [showQuickLaunch, setShowQuickLaunch] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Live elapsed-time tick (30s interval), only in multi-zone mode
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!assignments) return;
    const id = setInterval(() => setTick((t) => t + 1), 30_000);
    return () => clearInterval(id);
  }, [assignments]);

  // Reverse lookup: tab ID -> zone index
  const tabIdToZone = useMemo<Record<string, number>>(() => {
    if (!assignments) return {};
    const map: Record<string, number> = {};
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      map[tabId] = Number(zoneStr);
    }
    return map;
  }, [assignments]);

  const isMultiZone = !!assignments;

  const startEditing = (tab: TerminalTab) => {
    setEditingId(tab.id);
    setEditValue(tab.title);
  };

  const commitEdit = () => {
    if (editingId && editValue.trim()) {
      onRename(editingId, editValue.trim());
    }
    setEditingId(null);
  };

  // Close dropdowns when clicking outside
  useEffect(() => {
    if (!showDropdown) return;
    const handleClick = (e: MouseEvent) => {
      if (showDropdown && dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setShowDropdown(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showDropdown]);

  // ── Helpers for per-tab enrichments ──────────────────────────────────────

  function getTabZoneIndex(tabId: string): number | undefined {
    return tabIdToZone[tabId];
  }

  function getTabLabels(tabId: string): string | undefined {
    const zone = getTabZoneIndex(tabId);
    if (zone === undefined) return undefined;
    return zoneLabels?.[zone];
  }

  function isTabAssigned(tabId: string): boolean {
    return tabId in tabIdToZone;
  }

  // ── UI Bridge: terminal-launch-menu registration ─────────────────────────
  // LaunchMenu only mounts while showQuickLaunch=true, so we register from
  // TerminalTabBar (always mounted) to ensure agents can invoke actions at
  // any time.

  const sortedAccountsForBridge = useMemo(
    () => [...(accountUsage ?? [])].sort((a, b) => (a.utilization ?? 0) - (b.utilization ?? 0)),
    [accountUsage],
  );

  const openLaunchMenu = useCallback(() => setShowQuickLaunch(true), []);
  const closeLaunchMenu = useCallback(() => setShowQuickLaunch(false), []);

  useUIComponent({
    id: "terminal-launch-menu",
    name: "Terminal Launch Menu",
    description:
      "Creates plain terminals and AI sessions. Use create-plain, create-ai-session, " +
      "create-best-account, or create-with-command to launch terminals programmatically.",
    actions: [
      {
        id: "open",
        label: "Open launch menu",
        handler: openLaunchMenu,
      },
      {
        id: "close",
        label: "Close launch menu",
        handler: closeLaunchMenu,
      },
      {
        id: "create-plain",
        label: "Create Plain Terminal",
        description: "Spawn N blank terminals using the user's default shell.",
        paramSchema: { count: "number (>= 1, defaults to 1)" },
        handler: (params?: unknown) => {
          const { count = 1 } = (params ?? {}) as { count?: number };
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-plain requires { count: number } where count >= 1");
          }
          onQuickLaunch?.(count);
          closeLaunchMenu();
        },
      },
      {
        id: "create-ai-session",
        label: "Create AI Session",
        description:
          "Spawn N terminals pre-configured to launch `claude` under the given CLAUDE_CONFIG_DIR, optionally pre-typing a context prompt.",
        paramSchema: {
          count: "number (>= 1, defaults to 1)",
          configDir: "string (absolute path to a Claude Code config dir, required)",
          context: "string (optional initial prompt auto-typed after claude starts)",
        },
        handler: (params?: unknown) => {
          const {
            count = 1,
            configDir,
            context,
          } = (params ?? {}) as {
            count?: number;
            configDir?: string;
            context?: string;
          };
          if (!configDir)
            throw new Error(
              "create-ai-session requires { count?: number, configDir: string, context?: string }",
            );
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-ai-session: count must be a positive number");
          }
          onLaunchAiSession?.(count, configDir, context);
          closeLaunchMenu();
        },
      },
      {
        id: "create-best-account",
        label: "Create AI Session with Best Account",
        description:
          "Like create-ai-session, but picks the AI account with the lowest current utilization. Fails if no accounts are configured.",
        paramSchema: {
          count: "number (>= 1, defaults to 1)",
          context: "string (optional initial prompt auto-typed after claude starts)",
        },
        handler: (params?: unknown) => {
          const { count = 1, context } = (params ?? {}) as {
            count?: number;
            context?: string;
          };
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-best-account: count must be a positive number");
          }
          const best = sortedAccountsForBridge[0];
          if (!best) throw new Error("No AI accounts available");
          onLaunchAiSession?.(count, best.config_dir, context);
          closeLaunchMenu();
        },
      },
      {
        id: "create-with-command",
        label: "Create Terminal with Command",
        description:
          "Spawn N terminals and auto-type the given shell command into each after the prompt renders.",
        paramSchema: {
          count: "number (>= 1, defaults to 1)",
          command: "string (the shell command to type + Enter, required)",
        },
        handler: (params?: unknown) => {
          const { count = 1, command } = (params ?? {}) as {
            count?: number;
            command?: string;
          };
          if (!command)
            throw new Error("create-with-command requires { count?: number, command: string }");
          if (typeof count !== "number" || count < 1) {
            throw new Error("create-with-command: count must be a positive number");
          }
          onQuickLaunch?.(count, command);
          closeLaunchMenu();
        },
      },
    ],
  });

  // ── Render ───────────────────────────────────────────────────────────────

  return (
    <div
      role="tablist"
      aria-label="Terminal sessions"
      className="flex items-center bg-[#13141f] border-b border-[#2a2d3d] h-9 shrink-0"
    >
      {/* Aria-live indicator that reflects current terminal-tab count for accessibility / spec snapshots. */}
      <span
        role="status"
        aria-live="polite"
        aria-label={tabs.length === 0 ? "No terminals open" : `${tabs.length} terminal${tabs.length === 1 ? "" : "s"} open`}
        className="sr-only"
      >
        {tabs.length === 0 ? "No terminals open" : `${tabs.length} terminal${tabs.length === 1 ? "" : "s"} open`}
      </span>
      {/* Pinned left: layout picker + terminal creation buttons */}
      <div className="flex items-center gap-0.5 px-1 shrink-0">
        {layoutPicker && <div className="shrink-0 mr-1">{layoutPicker}</div>}

        <button
          onClick={onCreate}
          className="flex items-center justify-center w-6 h-6 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50 transition-colors shrink-0"
          title="New terminal (Ctrl+Shift+T)"
        >
          <Plus className="w-3.5 h-3.5" />
        </button>

        {/* Launch menu dropdown */}
        {onQuickLaunch && (
          <UIBridgeComponentScope componentId="terminal-launch-menu">
            <div className="relative shrink-0">
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => setShowQuickLaunch(!showQuickLaunch)}
                className={`flex items-center justify-center w-5 h-6 rounded transition-colors ${
                  showQuickLaunch
                    ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
                }`}
                title="Launch menu"
              >
                <ChevronDown className="w-3 h-3" />
              </button>
              {showQuickLaunch && (
                <LaunchMenu
                  onCreatePlain={(count) => onQuickLaunch(count)}
                  onCreateAiSession={(count, configDir, context) =>
                    onLaunchAiSession?.(count, configDir, context)
                  }
                  onCreateMultiAiSessions={(configDirs, context) =>
                    onLaunchMultiAiSessions?.(configDirs, context)
                  }
                  onCreateWithCommand={(count, command) => onQuickLaunch(count, command)}
                  accountUsage={accountUsage ?? []}
                  launchCommands={launchCommands}
                  fileLocks={fileLocks}
                  onJumpToHolder={(holderName) => {
                    // Resolve holder_name → tab id with the same lookup as
                    // `findTabByHolderName` in `useFileLockTracking.ts:44-49`
                    // (tab.title === holder_name). Reusing the parent's
                    // `onSelect` channel rather than calling `setActiveId`
                    // directly so zone focus / layout side-effects in
                    // `TerminalPage`'s onSelect handler still run. No-op
                    // when no tab matches — the holder may be a workflow
                    // session, not a terminal tab.
                    const tab = tabs.find((t) => t.title === holderName);
                    if (tab) onSelect(tab.id);
                  }}
                  resolveSessionName={(taskRunId) => {
                    // PG `recent_editors` rows carry only `task_run_id`.
                    // Try claudeSessionId first (the canonical mapping for
                    // live AI tabs); fall back to tab.title when the
                    // upstream id happens to match an explicit title.
                    const byClaude = tabs.find((t) => t.claudeSessionId === taskRunId);
                    if (byClaude) return byClaude.title;
                    const byTitle = tabs.find((t) => t.title === taskRunId);
                    return byTitle?.title;
                  }}
                  onClose={() => setShowQuickLaunch(false)}
                />
              )}
            </div>
          </UIBridgeComponentScope>
        )}
      </div>

      {/* Scrollable tab area */}
      <div className="flex items-center gap-0.5 px-1 overflow-x-auto scrollbar-none flex-1 min-w-0 h-full">
        {tabs.map((tab) => {
          const isActive = tab.id === activeId;
          const isDead = !tab.isAlive;
          const isEditing = editingId === tab.id;
          const state = sessionStates?.[tab.id] ?? "idle";
          const hasUnread = unreadTabs?.has(tab.id) ?? false;
          const isStale = staleTabs?.has(tab.id) ?? false;
          const tabActivity = activityData?.[tab.id];
          const tabDuration = stateDurations?.[tab.id];
          const tabLabels = getTabLabels(tab.id);
          const showSparkline =
            isMultiZone && isTabAssigned(tab.id) && tabActivity && tabActivity.length > 0;
          const lockState = fileLockStates?.[tab.id];
          const commitState = commitStates?.[tab.id];

          return (
            <button
              key={tab.id}
              onClick={() => onSelect(tab.id)}
              onDoubleClick={() => startEditing(tab)}
              onAuxClick={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  onClose(tab.id);
                }
              }}
              draggable
              onDragStart={(e) => {
                e.dataTransfer.setData("text/tab-id", tab.id);
                e.dataTransfer.effectAllowed = "move";
              }}
              className={`
              flex items-center gap-1.5 px-3 py-1 rounded-t text-xs font-medium
              transition-colors whitespace-nowrap max-w-[260px] group cursor-grab active:cursor-grabbing
              ${
                isActive
                  ? "bg-[#1a1b26] text-[#c0caf5] border-t border-x border-[#2a2d3d] -mb-px"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
              }
              ${isDead ? "opacity-60" : ""}
              ${isStale ? "border-dashed border-[#565f89]/40" : ""}
            `}
            >
              {/* Session state dot / plan icon */}
              {tab.type === "plan" ? (
                <FileText className="w-3 h-3 shrink-0 text-[#7dcfff]" />
              ) : sessionStates ? (
                <div
                  className={`w-2 h-2 rounded-full shrink-0 ${STATE_DOT_COLORS[state]} ${
                    state === "needs-input" ? "animate-pulse" : ""
                  }`}
                />
              ) : (
                <Terminal className="w-3 h-3 shrink-0" />
              )}

              {/* File lock indicator. Wrapped in a span so the native
                  `title` tooltip reaches the user — Lucide icons render
                  as <svg> and don't accept the React title attribute
                  directly. */}
              {lockState?.kind === "waiting" && (
                <span
                  data-testid="tab-lock-waiting"
                  className="shrink-0 inline-flex"
                  aria-label="waiting on file lock"
                  title={`waiting on ${lockState.counterpartyName ?? "another session"}'s ${
                    lockState.filePath ?? "edit"
                  }${lockState.sinceMs ? ` since ${formatSince(lockState.sinceMs)}` : ""}`}
                >
                  <Lock className="w-2.5 h-2.5 text-[#e0af68] animate-pulse" />
                </span>
              )}
              {lockState?.kind === "holding" && (
                <span
                  data-testid="tab-lock-holding"
                  className="shrink-0 inline-flex"
                  aria-label="holding file lock"
                  title={`holding ${lockState.filePath ?? "file"} — ${lockState.waiterCount ?? 0} session${
                    lockState.waiterCount === 1 ? "" : "s"
                  } waiting`}
                >
                  <Lock className="w-2.5 h-2.5 text-[#7aa2f7] opacity-60" />
                </span>
              )}

              {/*
                Commit-readiness traffic light. Renders nothing visible when
                state.status is "unknown" but always slots in the GitCommit
                button (disabled) so the tab layout stays stable.

                isPtyTab decision: every tab that reaches `TerminalTabBar`
                today is a PTY-launched terminal. SDK chat sessions live in
                a separate component path (`useAiSession.ts` →
                `MessagesPanel`-style surfaces) and never render here. We
                pass `isPtyTab={true}` unconditionally; if a future SDK-chat
                surface starts routing through this component the toggle
                can flip per-tab. Tracked at plan §0 caveat.
              */}
              {tab.type !== "plan" && (
                <span onClick={(e) => e.stopPropagation()}>
                  <CommitTrafficLight
                    state={commitState}
                    sessionId={tab.claudeSessionId}
                    isPtyTab={true}
                    onWriteToTerminal={(text) => {
                      const ref = terminalRefs?.get(tab.id);
                      ref?.current?.writeToTerminal(text);
                    }}
                  />
                </span>
              )}

              <span className="flex flex-col items-start min-w-0">
                {/* Title row */}
                <span className="flex items-center gap-1 min-w-0">
                  {isEditing ? (
                    <input
                      autoFocus
                      value={editValue}
                      onChange={(e) => setEditValue(e.target.value)}
                      onBlur={commitEdit}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") commitEdit();
                        if (e.key === "Escape") setEditingId(null);
                        e.stopPropagation();
                      }}
                      onClick={(e) => e.stopPropagation()}
                      className="bg-transparent border-b border-[#565f89] text-[#c0caf5] text-xs w-20 outline-hidden"
                    />
                  ) : (
                    <span className="truncate">{tab.title}</span>
                  )}

                  {/* Inline sparkline + duration (active tab only) */}
                  {isActive && showSparkline && (
                    <ActivitySparkline data={tabActivity!} color={STATE_BAR_COLORS[state]} />
                  )}
                  {isActive && tabDuration && (
                    <DurationBadge duration={tabDuration} state={state} />
                  )}
                </span>

                {/* Tags row (active tab only, multi-zone) */}
                {isActive && tabLabels && isMultiZone && (
                  <TagBadges labels={tabLabels} colorMap={labelColorMap} />
                )}

                {/* Working dir (active tab only) */}
                {isActive && tab.workingDir && (
                  <span className="text-[9px] text-[#565f89] truncate max-w-[140px]">
                    {tab.workingDir.split(/[/\\]/).slice(-2).join("/")}
                  </span>
                )}
              </span>

              {/* Stale indicator (inactive tabs) */}
              {!isActive && isStale && (
                <span className="text-[8px] text-[#565f89] italic shrink-0">stalled?</span>
              )}

              {/* Duration badge on inactive tabs for attention states */}
              {!isActive && tabDuration && <DurationBadge duration={tabDuration} state={state} />}

              {/* Exit code badge */}
              {isDead && tab.exitCode !== null && (
                <span
                  className={`text-[10px] px-1 rounded ${
                    tab.exitCode === 0
                      ? "bg-green-900/30 text-green-400"
                      : "bg-red-900/30 text-red-400"
                  }`}
                >
                  {tab.exitCode}
                </span>
              )}

              {/* Unread indicator */}
              {hasUnread && <span className="w-1.5 h-1.5 rounded-full bg-[#f7768e] shrink-0" />}

              {/* Phase 3 review verdict badge — next to the existing per-tab
                  controls cluster. Renders nothing when no review row
                  exists for the session. */}
              <span
                className="shrink-0"
                data-testid="terminal-tab-controls"
                onClick={(e) => e.stopPropagation()}
              >
                <ReviewBadge sessionId={tab.id} />
              </span>

              {/* Close button */}
              <span
                role="button"
                tabIndex={0}
                onClick={(e) => {
                  e.stopPropagation();
                  onClose(tab.id);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.stopPropagation();
                    onClose(tab.id);
                  }
                }}
                className="ml-1 p-0.5 rounded opacity-0 group-hover:opacity-100 hover:bg-[#2a2d3d] transition-opacity"
              >
                <X className="w-3 h-3" />
              </span>
            </button>
          );
        })}
      </div>

      {/* Pinned right: wrapper tools badge + session list dropdown */}
      <div className="flex items-center gap-0.5 px-1 shrink-0">
        {/* Wrapper tools available to AI agents (Phase 4 visibility) */}
        <WrapperToolsBadge />
        {/* Session dropdown */}
        {tabs.length > 0 && (
          <div className="relative ml-auto shrink-0" ref={dropdownRef}>
            <button
              onClick={() => setShowDropdown(!showDropdown)}
              className={`flex items-center justify-center w-6 h-6 rounded transition-colors ${
                showDropdown
                  ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
              }`}
              title="All sessions"
            >
              <List className="w-3.5 h-3.5" />
            </button>

            {showDropdown && (
              <div className="absolute right-0 top-full mt-1 w-80 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
                <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
                  Sessions ({tabs.length})
                </div>
                <div className="max-h-64 overflow-y-auto scrollbar-dark">
                  {tabs.map((tab) => {
                    const isActive = tab.id === activeId;
                    const isDead = !tab.isAlive;
                    const state = sessionStates?.[tab.id] ?? "idle";
                    const tabActivity = activityData?.[tab.id];
                    const tabDuration = stateDurations?.[tab.id];
                    const tabLabels = getTabLabels(tab.id);
                    const hasSparkline =
                      isMultiZone && isTabAssigned(tab.id) && tabActivity && tabActivity.length > 0;

                    return (
                      <div
                        key={tab.id}
                        role="button"
                        tabIndex={0}
                        onClick={() => {
                          onSelect(tab.id);
                          setShowDropdown(false);
                        }}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            onSelect(tab.id);
                            setShowDropdown(false);
                          }
                        }}
                        className={`flex items-start gap-2 px-3 py-2 cursor-pointer transition-colors ${
                          isActive ? "bg-[#7aa2f7]/10" : "hover:bg-[#2a2d3d]/50"
                        }`}
                      >
                        {/* Status dot */}
                        <div className="mt-1 shrink-0">
                          <div
                            className={`w-2 h-2 rounded-full ${
                              sessionStates
                                ? `${STATE_DOT_COLORS[state]} ${state === "needs-input" ? "animate-pulse" : ""}`
                                : isDead
                                  ? "bg-[#565f89]"
                                  : "bg-[#9ece6a]"
                            }`}
                          />
                        </div>

                        {/* Info */}
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-1.5">
                            <span
                              className={`text-xs font-medium truncate ${
                                isActive ? "text-[#7aa2f7]" : "text-[#c0caf5]"
                              }`}
                            >
                              {tab.title}
                            </span>
                            {isDead && tab.exitCode !== null && (
                              <span
                                className={`text-[10px] px-1 rounded ${
                                  tab.exitCode === 0
                                    ? "bg-green-900/30 text-green-400"
                                    : "bg-red-900/30 text-red-400"
                                }`}
                              >
                                exit {tab.exitCode}
                              </span>
                            )}
                            {/* Sparkline in dropdown */}
                            {hasSparkline && (
                              <ActivitySparkline
                                data={tabActivity!}
                                color={STATE_BAR_COLORS[state]}
                              />
                            )}
                            {/* Duration in dropdown */}
                            {tabDuration && <DurationBadge duration={tabDuration} state={state} />}
                            {/* Phase 3 review verdict badge — same data
                                surface as the per-tab header. */}
                            <ReviewBadge sessionId={tab.id} />
                          </div>
                          <div className="flex items-center gap-2 text-[10px] text-[#565f89] mt-0.5">
                            {tab.pid && <span>PID {tab.pid}</span>}
                            {tab.workingDir && (
                              <span className="truncate" title={tab.workingDir}>
                                {tab.workingDir.split(/[/\\]/).pop()}
                              </span>
                            )}
                            {tab.createdAt && <span>{formatTime(tab.createdAt)}</span>}
                          </div>
                          {/* Tag badges in dropdown */}
                          {tabLabels && isMultiZone && (
                            <div className="mt-0.5">
                              <TagBadges labels={tabLabels} colorMap={labelColorMap} max={3} />
                            </div>
                          )}
                        </div>

                        {/* Close button */}
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            onClose(tab.id);
                          }}
                          className="mt-0.5 p-0.5 rounded text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 transition-colors shrink-0"
                        >
                          <X className="w-3 h-3" />
                        </button>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
