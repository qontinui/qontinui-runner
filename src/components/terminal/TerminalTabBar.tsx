import { useState, useEffect, useMemo, useCallback, type ReactNode } from "react";
import { Plus, ChevronDown } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";
import type { AccountUsageInfo } from "./useSessionManager";
import { LaunchMenu } from "./LaunchMenu";
import { UIBridgeComponentScope } from "@qontinui/ui-bridge";
import { callRegistry } from "./commands";
import { ReviewBadge } from "./ReviewBadge";
import { CommitTrafficLight } from "./CommitTrafficLight";
import type { CommitState } from "./useCommitState";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { RefObject } from "react";
import { useNow1Hz } from "./useNow1Hz";

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  onRename: (id: string, title: string) => void;
  sessionStates?: Record<string, SessionState>;
  layoutPicker?: ReactNode;
  /**
   * Spawn N plain PTY terminals. Returns the created `tab.id`s in
   * launch order so UI Bridge action handlers can surface them in the
   * synchronous response — see `chore/uib-terminal-sessions-endpoint`.
   * The user-click path through `<LaunchMenu>` ignores the return
   * value (it's a fire-and-forget).
   */
  onQuickLaunch?: (count: number, autoCommand?: string) => Promise<string[]> | void;
  /** Spawn N AI sessions for a single account; returns the new `tab.id`s. */
  onLaunchAiSession?: (
    count: number,
    configDir: string,
    context?: string,
  ) => Promise<string[]> | void;
  /** Spawn one AI session per `configDir`; returns the new `tab.id`s. */
  onLaunchMultiAiSessions?: (
    configDirs: string[],
    context?: string,
  ) => Promise<string[]> | void;
  accountUsage?: AccountUsageInfo[];
  launchCommands?: Record<string, string>;
  fileLocks?: import("./useSessionManager").FileLockInfo[];
  fileLockStates?: Record<string, import("./useFileLockTracking").LockState>;
  /**
   * Per-tab count of files held by OTHER sessions that overlap with
   * this tab's `workingDir`. Sourced from `useRegistryAwareness`.
   * Renders a small yellow pill next to the lock indicator when > 0.
   * Tabs without an entry (or with 0) render nothing.
   */
  registryAwareness?: Record<string, number>;
  /**
   * Active tab's `workingDir`, plumbed through to LaunchMenu's probe so
   * relative-path tokens in the user's session-context prompt can
   * resolve against the launching session's cwd. Undefined when no tab
   * is active (first-tab case); LaunchMenu defaults to "" downstream.
   */
  activeTabCwd?: string;
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

/**
 * Format a "since" epoch-ms as a short relative-age string ("5s",
 * "42s", "5m", "2h"). Mirrors `formatHoldingAge` from `LaunchMenu.tsx`
 * but takes the "since" timestamp directly. Negative ages clamp to
 * "0s". Returns the empty string when `sinceMs` is undefined.
 *
 * Phase 2 of the stuck-session heartbeat plan widened the signature
 * from `(sinceMs?)` to `(sinceMs?, nowMs)` — the helper used to read
 * `Date.now()` internally, which made it invisible to React's render
 * cycle (re-mount didn't tick). Callers now pass `useNow1Hz()` as the
 * second arg so the tooltip text refreshes on next-hover after the
 * shared 1Hz broadcast fires. Native HTML `title` tooltips can't
 * refresh while visibly open — the tick lands at next-hover. See the
 * plan's Problem §1 caveat.
 */
function formatSince(sinceMs: number | undefined, nowMs: number): string {
  if (sinceMs === undefined) return "";
  const deltaSec = Math.max(0, Math.floor((nowMs - sinceMs) / 1000));
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
  registryAwareness = {},
  activeTabCwd,
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
  const [showQuickLaunch, setShowQuickLaunch] = useState(false);

  // Phase 2 (stuck-session heartbeat) — subscribe to the shared 1Hz
  // broadcast so the lock tooltip's "since 5m" text refreshes on next-
  // hover. Native HTML `title` tooltips don't update while visibly open
  // — the tick lands when the user re-hovers. Documented at
  // `formatSince`'s docstring and Problem §1 of the plan.
  const nowMs = useNow1Hz();

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

  // ── UI Bridge: terminal-launch-menu registration HOISTED ─────────────────
  // Phase 9a moved this registration to `TerminalPage.tsx` (next to the
  // `terminal-page` block) so external agents keep getting `create-plain` /
  // `create-ai-session` / `create-best-account` / `create-with-command`
  // even after `TerminalTabBar.tsx` is eventually retired. The local
  // LaunchMenu popover is still rendered here (operator-facing UI); the
  // wire-contract registration is just no longer co-located with it.
  //
  // The hoisted version dropped the `open` / `close` actions — they were
  // popover-toggle UI affordances, not wire contract. If external
  // automation surfaces dependence on them, restore by lifting
  // `showQuickLaunch` state to `TerminalPage` and threading down via props.
  //
  // The local launch-menu popover (chevron + LaunchMenu JSX below) still
  // uses `closeLaunchMenu` for its own onClose handler, so we keep that
  // callback here. `openLaunchMenu` / `sortedAccountsForBridge` were
  // only used by the deleted registration and are removed.
  const closeLaunchMenu = useCallback(() => setShowQuickLaunch(false), []);

  // The original `useUIComponent({id: "terminal-launch-menu", ...})` call
  // and its ~165-line `actions` array previously lived here and have been
  // moved verbatim to TerminalPage.tsx; this comment marks the hoist site
  // for git-blame readability.

  // ── Render ───────────────────────────────────────────────────────────────

  return (
    <div
      role="tablist"
      aria-label="Terminal sessions"
      className="flex items-center bg-[#13141f] border-b border-[#2a2d3d] h-9 shrink-0"
    >
      {/* Aria-live indicator that reflects current terminal-tab count for
          accessibility / spec snapshots.
          NOTE: previously used `role="status"`, which page-health auto-classifies
          as a transient toast (causing the terminal-count line to show up as a
          26h-stale toast in the page-health audit). Switched to a plain
          `aria-live="polite"` span with `data-page-element="status-indicator"`
          so the spec snapshot treats this as a persistent status indicator
          rather than a toast. Screen readers still announce changes via
          `aria-live`; the missing `role="status"` doesn't break a11y because
          aria-live alone is sufficient on a non-interactive span. */}
      <span
        aria-live="polite"
        aria-label={tabs.length === 0 ? "No terminals open" : `${tabs.length} terminal${tabs.length === 1 ? "" : "s"} open`}
        data-page-element="status-indicator"
        data-indicator="terminal-count"
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
                  cwd={activeTabCwd}
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

      <div className="flex-1" />

      {/* Phase 9a — `WrapperToolsBadge` was previously pinned to the
          right of the tab bar; its popover + tool list moved into
          `StatusStrip`'s wrapper-tools pill so there's a single
          surface for wrapper-tools info. The badge component file is
          left in place pending operator decision on whether other
          surfaces should still mount it. */}
    </div>
  );
}
