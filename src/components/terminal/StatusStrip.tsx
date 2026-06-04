/**
 * Phase 6 — Top status strip.
 *
 * Thin 24px top-pinned strip that surfaces *only* the conditions
 * requiring attention. Auto-hides entirely when every pill is at zero
 * (`return null`) so the top of the viewport is clean by default —
 * matches the plan's invisible-chrome principle.
 *
 * Pills shipped in this phase (all read from existing contexts; no new
 * state machines):
 *
 *   - **N need input · Tab to cycle** — clickable, focuses the next
 *     needs-input zone via `zoneLayout.focusNextNeedsInput`.
 *   - **N stuck on lock Xm** — count + max age of `kind:"waiting"`
 *     entries in `fileLockStates`. Clicking focuses the longest-stuck
 *     waiter's zone.
 *   - **N errors** — clickable, cycles to the next errored zone (an
 *     inline mirror of `focusNextNeedsInput`'s walk for `state ==="error"`,
 *     since `useZoneLayout` doesn't expose a dedicated cycler).
 *   - **N wrapper tools** — read from `useWrapperTools`; informational
 *     count (no click target until the registry grows a wrappers-page
 *     action). Hides at zero — same self-hide rule the existing
 *     `WrapperToolsBadge` uses.
 *
 * Deferred from the plan's pill list:
 *
 *   - **Profile pill** — active-profile name lives as private React
 *     state inside `ZoneProfilePicker` (line 109) and is only
 *     reachable via an `invoke("setting_get")` round-trip. Surfacing
 *     it here would mean either lifting the state into a context
 *     (cross-cutting, out of Phase-6 scope) or duplicating the fetch
 *     (drift risk). Skip until the profile state is lifted.
 *
 * The existing `WrapperToolsBadge` inside `TerminalTabBar` stays in
 * place during this phase — the plan's "replaces `WrapperToolsBadge`"
 * note belongs to Phase 9 demolition. Operators see both during the
 * transition.
 *
 * 1Hz heartbeat drives the "Xm" duration text so the value advances
 * without external state changes (mirrors `useNow1Hz` from
 * `TerminalTabBar`'s lock-tooltip plumbing; kept local here to avoid
 * an import cycle through the tab bar's deep dependency chain).
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertOctagon,
  ClipboardList,
  Lock,
  Package,
  RefreshCw,
  Sparkles,
  TerminalSquare,
} from "lucide-react";

import type { SessionState } from "./useZoneLayout";

import { useTerminalSession } from "./contexts";
import { useWrapperTools } from "@/hooks/useWrapperTools";

const HEARTBEAT_MS = 1_000;

function formatAge(ms: number): string {
  const sec = Math.max(0, Math.floor(ms / 1000));
  if (sec < 60) return `${sec}s`;
  if (sec < 3_600) return `${Math.floor(sec / 60)}m`;
  return `${Math.floor(sec / 3_600)}h`;
}

interface PillProps {
  icon: React.ReactNode;
  text: string;
  color: string;
  /** When defined, the pill renders as a `<button>` (clickable) and
   *  fires the callback. Otherwise informational — renders as a span. */
  onClick?: () => void;
  title?: string;
}

function Pill({ icon, text, color, onClick, title }: PillProps) {
  const baseClass =
    "flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap";
  if (onClick) {
    return (
      <button
        type="button"
        onClick={onClick}
        className={`${baseClass} hover:bg-white/5 transition-colors`}
        style={{ color }}
        title={title}
      >
        {icon}
        <span>{text}</span>
      </button>
    );
  }
  return (
    <span className={baseClass} style={{ color }} title={title}>
      {icon}
      <span>{text}</span>
    </span>
  );
}

/** Color codes mirror the per-state palette in ZoneStatusBar so the
 *  visual identity stays consistent across the lifted-out pills. */
const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

export function StatusStrip() {
  const session = useTerminalSession();
  const { sessionStates, fileLockStates, zoneLayout, workflowGen, sessionManager } = session;
  const planFileName = workflowGen.planFileName;
  const isPlanLoading = workflowGen.isPlanLoading;
  const {
    tools: wrapperTools,
    routes: wrapperRoutes,
    loading: wrapperLoading,
    error: wrapperError,
    refresh: refreshWrapperTools,
  } = useWrapperTools();

  // Phase 9a — the WrapperToolsBadge popover folded into this pill so
  // operators see one wrapper-tools surface, not two. State + outside-
  // click dismiss mirror the original badge in
  // `components/wrappers/WrapperToolsBadge.tsx`.
  const [showWrapperPopover, setShowWrapperPopover] = useState(false);
  const wrapperPopoverRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!showWrapperPopover) return;
    const handler = (e: MouseEvent) => {
      if (
        wrapperPopoverRef.current &&
        !wrapperPopoverRef.current.contains(e.target as Node)
      ) {
        setShowWrapperPopover(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showWrapperPopover]);

  // 1Hz heartbeat so the "Xm" duration text advances. Stops nothing —
  // the strip auto-hides when there's nothing to show, so the interval
  // only burns cycles while something requires attention.
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((t) => t + 1), HEARTBEAT_MS);
    return () => clearInterval(id);
  }, []);

  // Session-shape counts come from the Claude-session model
  // (`useSessionManager.statusCounts`), scoped to the LIVE / actively-managed
  // set — NOT raw PTY tabs and NOT every historical on-disk transcript.
  // Counting raw tabs inflated the total (18 tabs → "18 sessions"); counting
  // ALL `allSessions` inflated it the other way (hundreds of dormant
  // historical transcripts → "438 sessions / 436 idle"). statusCounts keeps
  // only sessions tied to a live PTY tab in this window (or active-external),
  // so working = active-in-zone + active-external, idle = stale-tab `frozen`
  // still open here, and dormant/orphaned historical transcripts are dropped.
  const {
    sessionCount,
    needsInputCount,
    errorCount,
    workingCount,
    completedCount,
    idleCount,
  } = sessionManager;

  // File-lock "stuck" pill still operates on PTY tabs (a lock is held by
  // a terminal, not a transcript session), so it keeps its tab-scoped
  // derivation here.
  const { stuckLocks, maxStuckMs, longestStuckTabId } = useMemo(() => {
    let stuck = 0;
    let maxMs = 0;
    let longestTabId: string | null = null;
    const now = Date.now();
    for (const [tabId, lockState] of Object.entries(fileLockStates ?? {})) {
      if (lockState.kind !== "waiting") continue;
      stuck++;
      const since = lockState.sinceMs;
      if (typeof since === "number") {
        const elapsed = now - since;
        if (elapsed > maxMs) {
          maxMs = elapsed;
          longestTabId = tabId;
        }
      }
    }
    return {
      stuckLocks: stuck,
      maxStuckMs: maxMs,
      longestStuckTabId: longestTabId,
    };
    // The tick covers cadence-driven re-eval for the maxStuckMs reading;
    // fileLockStates covers state-driven re-eval.
  }, [fileLockStates]);

  const isMultiZone = sessionCount > 1;

  const wrapperCount = wrapperTools.length;

  // Phase 9f — visual indicators lifted from ZoneStatusBar. These
  // surface ambient session-shape info (count, non-attention state
  // breakdown, loaded plan file) so the strip carries the operator
  // context ZSB used to. The attention-pills above keep their bright
  // colors; these new pills render in muted shades so they don't
  // compete visually with "needs attention" signals.
  const hasBreakdown = workingCount > 0 || completedCount > 0 || idleCount > 0;

  // Auto-hide gate — when nothing requires attention AND nothing
  // informational is worth showing either, the strip disappears
  // entirely so the top of viewport is fully clean.
  const hasContent =
    needsInputCount > 0 ||
    errorCount > 0 ||
    stuckLocks > 0 ||
    wrapperCount > 0 ||
    isMultiZone ||
    planFileName !== null ||
    isPlanLoading;

  const focusNextError = useCallback(() => {
    // Inline mirror of `useZoneLayout.focusNextNeedsInput`'s walk,
    // looking for `error` instead. Cycles starting at `focusedZone + 1`
    // and wraps around. Un-maximizes on hit so the operator can see
    // the zone (same posture as the needs-input cycler).
    const zones = zoneLayout.layout.zones.length;
    const start = zoneLayout.focusedZone;
    for (let i = 1; i <= zones; i++) {
      const candidate = (start + i) % zones;
      const tabId = zoneLayout.assignments[candidate];
      if (tabId && sessionStates[tabId] === "error") {
        zoneLayout.setFocusedZone(candidate);
        if (zoneLayout.maximizedZone !== null) zoneLayout.setMaximizedZone(null);
        return;
      }
    }
  }, [zoneLayout, sessionStates]);

  const focusLongestStuck = useCallback(() => {
    if (!longestStuckTabId) return;
    // Locate the zone holding the longest-stuck tab.
    for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
      if (tabId === longestStuckTabId) {
        zoneLayout.setFocusedZone(Number(zoneStr));
        if (zoneLayout.maximizedZone !== null) zoneLayout.setMaximizedZone(null);
        return;
      }
    }
  }, [longestStuckTabId, zoneLayout]);

  const focusNextNeedsInput = useCallback(() => {
    zoneLayout.focusNextNeedsInput(sessionStates);
  }, [zoneLayout, sessionStates]);

  if (!hasContent) return null;

  return (
    <div
      data-page-element="status-strip"
      className="flex items-center gap-1.5 px-2 h-6 bg-[#13141f]/40 backdrop-blur-sm border-b border-[#2a2d3d]/20 shrink-0"
      role="status"
      aria-label="Terminal page status"
    >
      {needsInputCount > 0 && (
        <Pill
          icon={
            <span className="w-1.5 h-1.5 rounded-full bg-[#e0af68] animate-pulse" aria-hidden />
          }
          text={`${needsInputCount} need input · Tab to cycle`}
          color="#e0af68"
          onClick={focusNextNeedsInput}
          title="Click here or press Ctrl+Shift+N to focus the next session waiting on input"
        />
      )}
      {errorCount > 0 && (
        <Pill
          icon={<AlertOctagon className="w-2.5 h-2.5" />}
          text={`${errorCount} error${errorCount === 1 ? "" : "s"}`}
          color="#f7768e"
          onClick={focusNextError}
          title="Focus the next errored session"
        />
      )}
      {stuckLocks > 0 && (
        <Pill
          icon={<Lock className="w-2.5 h-2.5" />}
          text={`${stuckLocks} stuck on lock${maxStuckMs > 0 ? ` ${formatAge(maxStuckMs)}` : ""}`}
          color="#ff9e64"
          onClick={focusLongestStuck}
          title={`Focus the session waiting longest on a file lock${
            maxStuckMs > 0 ? ` (currently ${formatAge(maxStuckMs)})` : ""
          }`}
        />
      )}
      {wrapperCount > 0 && (
        <div className="relative" ref={wrapperPopoverRef}>
          <Pill
            icon={<Package className="w-2.5 h-2.5" />}
            text={`${wrapperCount} wrapper tool${wrapperCount === 1 ? "" : "s"}`}
            color="#7aa2f7"
            onClick={() => setShowWrapperPopover((v) => !v)}
            title={`${wrapperCount} wrapper tool${
              wrapperCount === 1 ? " is" : "s are"
            } available to AI agents this session — click to view`}
          />
          {showWrapperPopover && (
            <div
              role="dialog"
              aria-label="Wrapper tools available"
              className="absolute left-0 top-full mt-1 w-96 max-w-[90vw] bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden"
            >
              <div className="flex items-center justify-between px-3 py-2 border-b border-[#2a2d3d]">
                <span className="text-[10px] uppercase tracking-wider text-[#565f89]">
                  Wrapper tools ({wrapperCount})
                </span>
                <button
                  type="button"
                  onClick={() => void refreshWrapperTools()}
                  disabled={wrapperLoading}
                  className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors disabled:opacity-50"
                  title="Refresh wrapper tool list"
                >
                  <RefreshCw className={`w-3 h-3 ${wrapperLoading ? "animate-spin" : ""}`} />
                  Refresh
                </button>
              </div>
              {wrapperError ? (
                <div className="px-3 py-3 text-[11px] text-[#f7768e]">
                  Failed to load wrappers: {wrapperError}
                </div>
              ) : (
                <div className="max-h-64 overflow-y-auto scrollbar-dark">
                  {wrapperTools.map((tool) => {
                    const route = wrapperRoutes.get(tool.name);
                    const wrapperName =
                      route?.wrapper.manifest?.displayName ?? route?.wrapper.id ?? "unknown";
                    return (
                      <div
                        key={tool.name}
                        className="px-3 py-2 border-b border-[#2a2d3d]/50 last:border-b-0 hover:bg-[#2a2d3d]/30"
                        title={`${wrapperName} — ${tool.description}`}
                      >
                        <div className="flex items-center gap-2">
                          <span className="text-[11px] font-mono text-[#7aa2f7] truncate">
                            {tool.name}
                          </span>
                          <span className="text-[9px] text-[#565f89] shrink-0">
                            {wrapperName}
                          </span>
                        </div>
                        {tool.description && (
                          <p className="text-[10px] text-[#a9b1d6] mt-0.5 line-clamp-2">
                            {tool.description}
                          </p>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
              <div className="px-3 py-1.5 text-[9px] text-[#565f89] bg-[#13141f] border-t border-[#2a2d3d]">
                Tools auto-injected on session start. Restart sessions to pick up newly-installed
                wrappers, or click Refresh.
              </div>
            </div>
          )}
        </div>
      )}
      {/* Phase 9f — session count pill. Multi-zone only; informational. */}
      {isMultiZone && (
        <Pill
          icon={<TerminalSquare className="w-2.5 h-2.5" />}
          text={`${sessionCount} sessions`}
          color="#565f89"
          title={`${sessionCount} Claude sessions tracked on this page`}
        />
      )}

      {/* Phase 9f — state-breakdown pill. Single combined pill listing
          the non-attention states (working / completed / idle); the
          attention-grabbing needs-input / error counts already have
          dedicated pills above. Renders dot-then-count per state in
          its native color so the dense layout stays legible. */}
      {isMultiZone && hasBreakdown && (
        <span
          className="flex items-center gap-2 px-1.5 py-0.5 text-[10px] leading-none whitespace-nowrap text-[#565f89]"
          title="Session state breakdown"
        >
          {workingCount > 0 && (
            <span className="flex items-center gap-1" style={{ color: STATE_COLORS.working }}>
              <span
                className="w-1.5 h-1.5 rounded-full"
                style={{ backgroundColor: STATE_COLORS.working }}
                aria-hidden
              />
              {workingCount} working
            </span>
          )}
          {completedCount > 0 && (
            <span className="flex items-center gap-1" style={{ color: STATE_COLORS.completed }}>
              <span
                className="w-1.5 h-1.5 rounded-full"
                style={{ backgroundColor: STATE_COLORS.completed }}
                aria-hidden
              />
              {completedCount} done
            </span>
          )}
          {idleCount > 0 && (
            <span className="flex items-center gap-1" style={{ color: STATE_COLORS.idle }}>
              <span
                className="w-1.5 h-1.5 rounded-full"
                style={{ backgroundColor: STATE_COLORS.idle }}
                aria-hidden
              />
              {idleCount} idle
            </span>
          )}
        </span>
      )}

      {/* Phase 9f — plan-file pill. Mirrors ZSB's plan-file indicator:
          loaded chip when a PLAN*.md / TODO*.md was found, a muted
          spinner while scanning, and a "no plan" hint otherwise. */}
      {isPlanLoading ? (
        <Pill
          icon={
            <span className="w-2.5 h-2.5 border border-[#565f89] border-t-transparent rounded-full animate-spin" />
          }
          text="scanning plan…"
          color="#565f89"
          title="Looking for PLAN*.md / TODO*.md in the workspace"
        />
      ) : planFileName ? (
        <Pill
          icon={<ClipboardList className="w-2.5 h-2.5" />}
          text={planFileName}
          color="#9ece6a"
          title={`Plan loaded: ${planFileName} — used by /plan-implement, /plan-verify, Architecture, and Plan Progress analyses`}
        />
      ) : (
        <Pill
          icon={<ClipboardList className="w-2.5 h-2.5" />}
          text="no plan"
          color="#414868"
          title="No PLAN*.md / TODO*.md file found in workspace"
        />
      )}

      {/* Trailing decorative spark so the strip's "something present"
          state is visually distinct from a stray border-bottom on a
          blank row, while staying subtle. Aria-hidden — it's purely
          decorative. */}
      <Sparkles className="w-2.5 h-2.5 text-[#565f89]/40 ml-auto" aria-hidden />
    </div>
  );
}
