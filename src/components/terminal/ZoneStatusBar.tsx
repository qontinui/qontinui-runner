import { useMemo, useState, useRef, useEffect } from "react";
import {
  ArrowRight,
  ArrowUpDown,
  BarChart3,
  Bell,
  BellRing,
  Check,
  ChevronDown,
  ChevronUp,
  Clock,
  Download,
  Eye,
  Focus,
  Keyboard,
  Pin,
  PinOff,
  RefreshCw,
  Shield,
  Tag,
  Volume2,
  VolumeOff,
} from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";

interface ZoneStatusBarProps {
  tabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  focusedZone: number;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onJumpToNeedsInput: () => void;
  onFocusZone: (zoneIndex: number) => void;
  onShowShortcuts?: () => void;
  autoFocus?: boolean;
  onToggleAutoFocus?: () => void;
  soundEnabled?: boolean;
  onToggleSound?: () => void;
  desktopNotify?: boolean;
  onToggleDesktopNotify?: () => void;
  stateDurations?: Record<string, string>;
  onSelectByState?: (state: SessionState) => void;
  pinnedZones?: Set<number>;
  staleTabs?: Set<string>;
  metrics?: {
    totalApprovals: number;
    totalRejections: number;
    totalBroadcasts: number;
    sessionsCreated: number;
  };
  zoneLabels?: Record<number, string>;
  onSetZoneLabel?: (zoneIndex: number, label: string) => void;
  flashingTabs?: Set<string>;
  onExport?: () => void;
  onSortZones?: () => void;
  eventHistory?: { time: number; type: string; session: string; zone?: number; color: string }[];
  labelColorMap?: Record<string, string>;
  focusMode?: boolean;
  onToggleFocusMode?: () => void;
  autoApprovePatterns?: string[];
  onSetAutoApprovePatterns?: (patterns: string[]) => void;
  autoApproveCount?: number;
  stateTimeAccum?: Record<SessionState, number>;
  autoRestart?: boolean;
  onToggleAutoRestart?: () => void;
  autoRestartCount?: number;
  onApproveTab?: (tabId: string) => void;
  onRestartZone?: (zoneIdx: number) => void;
  onTogglePin?: (zoneIdx: number) => void;
  activeTagFilters?: Set<string>;
  onSetActiveTagFilters?: (filters: Set<string>) => void;
  allTags?: string[];
  activityData?: Record<string, number[]>;
  sessionStartTimes?: Record<string, number>;
  lastOutputLines?: Record<string, string[]>;
  unreadTabs?: Set<string>;
}

const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

const STATE_LABELS: Record<SessionState, string> = {
  idle: "idle",
  working: "working",
  "needs-input": "waiting",
  completed: "done",
  error: "error",
};

export function ZoneStatusBar({
  tabs,
  assignments,
  sessionStates,
  focusedZone,
  collapsed,
  onToggleCollapsed,
  onJumpToNeedsInput,
  onFocusZone,
  onShowShortcuts,
  autoFocus,
  onToggleAutoFocus,
  soundEnabled,
  onToggleSound,
  desktopNotify,
  onToggleDesktopNotify,
  stateDurations,
  onSelectByState,
  pinnedZones,
  staleTabs,
  metrics,
  zoneLabels,
  onSetZoneLabel,
  flashingTabs,
  onExport,
  onSortZones,
  eventHistory,
  labelColorMap,
  focusMode,
  onToggleFocusMode,
  autoApprovePatterns,
  onSetAutoApprovePatterns,
  autoApproveCount,
  stateTimeAccum,
  autoRestart,
  onToggleAutoRestart,
  autoRestartCount,
  onApproveTab,
  onRestartZone,
  onTogglePin,
  activeTagFilters: externalActiveTagFilters,
  onSetActiveTagFilters: externalSetActiveTagFilters,
  allTags: externalAllTags,
  activityData,
  sessionStartTimes,
  lastOutputLines,
  unreadTabs,
}: ZoneStatusBarProps) {
  const stateCounts = useMemo(() => {
    const counts: Record<SessionState, number> = {
      idle: 0,
      working: 0,
      "needs-input": 0,
      completed: 0,
      error: 0,
    };
    for (const tab of tabs) {
      const state = sessionStates[tab.id] ?? "idle";
      counts[state]++;
    }
    return counts;
  }, [tabs, sessionStates]);

  const needsInputCount = stateCounts["needs-input"];
  const hasActionNeeded = needsInputCount > 0 || stateCounts.error > 0;

  // Parse tags from zone labels (comma-separated)
  const zoneTags = useMemo(() => {
    const map: Record<number, string[]> = {};
    if (!zoneLabels) return map;
    for (const [zStr, label] of Object.entries(zoneLabels)) {
      if (!label) continue;
      map[Number(zStr)] = label
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean);
    }
    return map;
  }, [zoneLabels]);

  // All unique tags across all zones (use external if provided, otherwise compute locally)
  const localAllTags = useMemo(() => {
    const tagSet = new Set<string>();
    for (const tags of Object.values(zoneTags)) {
      for (const t of tags) tagSet.add(t);
    }
    return [...tagSet].sort();
  }, [zoneTags]);
  const allTags = externalAllTags ?? localAllTags;

  // Hover preview popover state
  const [hoveredDot, setHoveredDot] = useState<number | null>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Inline tag editor state (double-click on zone dot)
  const [editingDotZone, setEditingDotZone] = useState<number | null>(null);
  const [editingDotValue, setEditingDotValue] = useState("");

  // Active tag filters (OR logic — show zones matching ANY active tag)
  // Use external state if provided, otherwise keep local
  const [localActiveTagFilters, setLocalActiveTagFilters] = useState<Set<string>>(new Set());
  const activeTagFilters = externalActiveTagFilters ?? localActiveTagFilters;
  const setActiveTagFilters = externalSetActiveTagFilters ?? setLocalActiveTagFilters;

  const toggleTagFilter = (tag: string) => {
    setActiveTagFilters(
      (() => {
        const next = new Set(activeTagFilters);
        if (next.has(tag)) next.delete(tag);
        else next.add(tag);
        return next;
      })(),
    );
  };

  // Build ordered zone→tab mapping for the dot display
  const zoneDots = useMemo(() => {
    const dots: { zoneIndex: number; tabId: string; state: SessionState; title: string }[] = [];
    const assignmentEntries = Object.entries(assignments)
      .map(([z, tabId]) => ({ zoneIndex: Number(z), tabId }))
      .sort((a, b) => a.zoneIndex - b.zoneIndex);

    for (const { zoneIndex, tabId } of assignmentEntries) {
      const tab = tabs.find((t) => t.id === tabId);
      if (tab) {
        dots.push({
          zoneIndex,
          tabId,
          state: sessionStates[tabId] ?? "idle",
          title: tab.title,
        });
      }
    }
    return dots;
  }, [assignments, tabs, sessionStates]);

  // Filter dots by active tag filters
  const filteredDots = useMemo(() => {
    if (activeTagFilters.size === 0) return zoneDots;
    return zoneDots.filter((dot) => {
      const tags = zoneTags[dot.zoneIndex];
      if (!tags || tags.length === 0) return false;
      return tags.some((t) => activeTagFilters.has(t));
    });
  }, [zoneDots, activeTagFilters, zoneTags]);

  // Auto-scroll to newly flashing zone dot
  const dotRefsMap = useRef<Map<number, HTMLButtonElement>>(new Map());
  useEffect(() => {
    if (!flashingTabs || flashingTabs.size === 0) return;
    // Find the first flashing zone dot
    for (const dot of zoneDots) {
      if (flashingTabs.has(dot.tabId)) {
        const el = dotRefsMap.current.get(dot.zoneIndex);
        el?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "nearest" });
        break;
      }
    }
  }, [flashingTabs, zoneDots]);

  // Tick counter to force re-render for live elapsed time on working zones
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const interval = setInterval(() => setTick((t) => t + 1), 30000);
    return () => clearInterval(interval);
  }, []);

  // Format elapsed ms into compact time string
  const formatElapsed = (ms: number): string => {
    const totalMin = Math.floor(ms / 60000);
    if (totalMin < 1) return "<1m";
    const hr = Math.floor(totalMin / 60);
    const min = totalMin % 60;
    if (hr > 0) return `${hr}h ${min}m`;
    return `${totalMin}m`;
  };

  if (collapsed) {
    return (
      <button
        onClick={onToggleCollapsed}
        className="flex items-center gap-2 px-3 h-6 bg-[#13141f] border-b border-[#2a2d3d] text-[10px] text-[#565f89] hover:text-[#a9b1d6] transition-colors shrink-0"
      >
        <ChevronDown className="w-3 h-3" />
        <span>{tabs.length} sessions</span>
        {/* Mini dot strip */}
        <div className="flex items-center gap-0.5 ml-1">
          {zoneDots.map((dot) => (
            <div
              key={dot.zoneIndex}
              className={`w-1.5 h-1.5 rounded-full ${dot.state === "needs-input" ? "animate-pulse" : ""}`}
              style={{ backgroundColor: STATE_COLORS[dot.state] }}
            />
          ))}
        </div>
        {needsInputCount > 0 && (
          <span className="text-[#e0af68] ml-1">{needsInputCount} waiting</span>
        )}
      </button>
    );
  }

  return (
    <div className="flex items-center gap-3 px-3 h-8 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
      {/* Collapse button */}
      <button
        onClick={onToggleCollapsed}
        className="text-[#565f89] hover:text-[#a9b1d6] transition-colors shrink-0"
        title="Collapse status bar"
      >
        <ChevronUp className="w-3 h-3" />
      </button>

      {/* Session count */}
      <span className="text-[10px] text-[#565f89] shrink-0">
        {tabs.length} session{tabs.length !== 1 ? "s" : ""}
      </span>

      {/* State breakdown — clickable to select zones by state */}
      <div className="flex items-center gap-2 text-[10px] shrink-0">
        {(["needs-input", "working", "completed", "error", "idle"] as SessionState[]).map(
          (state) => {
            const count = stateCounts[state];
            if (count === 0) return null;
            return (
              <button
                key={state}
                className="flex items-center gap-1 rounded px-1 py-0.5 -mx-1 hover:bg-white/5 transition-colors"
                style={{ color: STATE_COLORS[state] }}
                onClick={() => onSelectByState?.(state)}
                title={`Select all ${STATE_LABELS[state]} zones`}
              >
                <span
                  className={`w-1.5 h-1.5 rounded-full ${state === "needs-input" ? "animate-pulse" : ""}`}
                  style={{ backgroundColor: STATE_COLORS[state] }}
                />
                {count} {STATE_LABELS[state]}
              </button>
            );
          },
        )}
      </div>

      {/* Separator */}
      <div className="w-px h-3 bg-[#2a2d3d] shrink-0" />

      {/* Tag filter pills */}
      {allTags.length > 0 && (
        <>
          <div className="flex items-center gap-1 shrink-0">
            <Tag className="w-3 h-3 text-[#565f89]" />
            {activeTagFilters.size > 0 && (
              <button
                onClick={() => setActiveTagFilters(new Set())}
                className="text-[9px] px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#a9b1d6] hover:bg-[#3b3d57] transition-colors"
              >
                All
              </button>
            )}
            {allTags.map((tag) => {
              const isActive = activeTagFilters.has(tag);
              const color = labelColorMap?.[tag] ?? "#bb9af7";
              return (
                <button
                  key={tag}
                  onClick={() => toggleTagFilter(tag)}
                  className={`text-[9px] px-1.5 py-0.5 rounded transition-colors ${
                    isActive ? "ring-1" : "opacity-60 hover:opacity-100"
                  }`}
                  style={{
                    color,
                    backgroundColor: `${color}${isActive ? "25" : "10"}`,
                    ...(isActive ? { boxShadow: `0 0 0 1px ${color}` } : {}),
                  }}
                >
                  {tag}
                </button>
              );
            })}
          </div>
          <div className="w-px h-3 bg-[#2a2d3d] shrink-0" />
        </>
      )}

      {/* Zone dots — clickable, filtered by active tags */}
      <div className="flex items-center gap-1 flex-1 min-w-0 overflow-x-auto scrollbar-none">
        {filteredDots.map((dot) => {
          const isFocused = dot.zoneIndex === focusedZone;
          const duration = stateDurations?.[dot.tabId];
          const showDuration = duration && (dot.state === "needs-input" || dot.state === "error");
          const isPinned = pinnedZones?.has(dot.zoneIndex);
          const isStale = staleTabs?.has(dot.tabId);
          const tags = zoneTags[dot.zoneIndex];
          const outputLines = lastOutputLines?.[dot.tabId] ?? [];
          const previewLines = outputLines.slice(-5);
          const isHovered = hoveredDot === dot.zoneIndex;
          return (
            <div key={dot.zoneIndex} className="relative shrink-0">
              <button
                ref={(el) => {
                  if (el) dotRefsMap.current.set(dot.zoneIndex, el);
                }}
                onClick={() => onFocusZone(dot.zoneIndex)}
                onDoubleClick={(e) => {
                  e.preventDefault();
                  if (!onSetZoneLabel) return;
                  setEditingDotZone(dot.zoneIndex);
                  setEditingDotValue(zoneLabels?.[dot.zoneIndex] ?? "");
                }}
                onMouseEnter={() => {
                  hoverTimerRef.current = setTimeout(() => setHoveredDot(dot.zoneIndex), 500);
                }}
                onMouseLeave={() => {
                  if (hoverTimerRef.current) {
                    clearTimeout(hoverTimerRef.current);
                    hoverTimerRef.current = null;
                  }
                  setHoveredDot(null);
                }}
                className={`flex items-center gap-1 px-1.5 py-0.5 rounded transition-colors shrink-0 ${
                  isFocused ? "bg-[#7aa2f7]/15 ring-1 ring-[#7aa2f7]/30" : "hover:bg-[#2a2d3d]/50"
                }`}
                title={`Zone ${dot.zoneIndex + 1}: ${dot.title} (${STATE_LABELS[dot.state]}${duration ? ` ${duration}` : ""}${isPinned ? " 📌" : ""}${isStale ? " ⚠ stalled" : ""}${tags?.length ? ` [${tags.join(", ")}]` : ""})`}
              >
                {editingDotZone === dot.zoneIndex ? (
                  <input
                    autoFocus
                    value={editingDotValue}
                    onChange={(e) => setEditingDotValue(e.target.value)}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Enter") {
                        onSetZoneLabel?.(dot.zoneIndex, editingDotValue);
                        setEditingDotZone(null);
                      }
                      if (e.key === "Escape") {
                        setEditingDotZone(null);
                      }
                    }}
                    onBlur={() => {
                      onSetZoneLabel?.(dot.zoneIndex, editingDotValue);
                      setEditingDotZone(null);
                    }}
                    onClick={(e) => e.stopPropagation()}
                    placeholder="tag1, tag2..."
                    className="w-24 bg-[#13141f] border border-[#7aa2f7] rounded px-1 py-0.5 text-[10px] text-[#c0caf5] outline-none"
                  />
                ) : (
                  <>
                    <div
                      className={`w-2 h-2 rounded-full shrink-0 ${dot.state === "needs-input" ? "animate-pulse" : ""} ${isStale ? "ring-1 ring-[#e0af68]" : ""}`}
                      style={{ backgroundColor: STATE_COLORS[dot.state] }}
                    />
                    {unreadTabs?.has(dot.tabId) && (
                      <span className="text-[8px] font-bold text-[#7aa2f7] shrink-0">+</span>
                    )}
                    {isPinned && <Pin className="w-2 h-2 text-[#7aa2f7] shrink-0" />}
                    {tags &&
                      tags.length > 0 &&
                      tags.map((tag) => (
                        <span
                          key={tag}
                          className="text-[9px] px-1 rounded shrink-0"
                          style={{
                            color: labelColorMap?.[tag] ?? "#bb9af7",
                            backgroundColor: `${labelColorMap?.[tag] ?? "#bb9af7"}15`,
                          }}
                        >
                          {tag}
                        </span>
                      ))}
                    <span
                      className={`text-[10px] truncate max-w-[60px] ${
                        isFocused ? "text-[#c0caf5]" : isStale ? "text-[#e0af68]" : "text-[#565f89]"
                      }`}
                    >
                      {dot.title}
                    </span>
                    {(() => {
                      const points = activityData?.[dot.tabId];
                      if (!points || points.length < 3) return null;
                      const recent = points.slice(-8);
                      const peak = Math.max(...recent, 1);
                      const barColor = STATE_COLORS[dot.state];
                      return (
                        <svg width={24} height={8} className="shrink-0" aria-hidden="true">
                          {recent.map((v, i) => {
                            const h = Math.max(1, (v / peak) * 8);
                            return (
                              <rect
                                key={i}
                                x={i * 3}
                                y={8 - h}
                                width={2}
                                height={h}
                                fill={barColor}
                              />
                            );
                          })}
                        </svg>
                      );
                    })()}
                    {showDuration && (
                      <span
                        className="text-[9px] opacity-60"
                        style={{ color: STATE_COLORS[dot.state] }}
                      >
                        {duration}
                      </span>
                    )}
                    {dot.state === "working" &&
                      sessionStartTimes?.[dot.tabId] &&
                      (() => {
                        const elapsed = Date.now() - sessionStartTimes[dot.tabId];
                        void tick; // force dependency on tick for re-render
                        return (
                          <span
                            className="text-[9px] opacity-60"
                            style={{ color: STATE_COLORS.working }}
                          >
                            {formatElapsed(elapsed)}
                          </span>
                        );
                      })()}
                  </>
                )}
              </button>
              {isHovered && (
                <div className="absolute left-1/2 -translate-x-1/2 bottom-full mb-2 w-[280px] bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
                  {/* Header: zone name + state */}
                  <div className="px-3 py-1.5 border-b border-[#2a2d3d] flex items-center justify-between">
                    <span className="text-[10px] text-[#c0caf5] font-medium truncate">
                      Zone {dot.zoneIndex + 1}: {dot.title}
                    </span>
                    <span
                      className="text-[9px] px-1.5 py-0.5 rounded shrink-0 ml-2"
                      style={{
                        color: STATE_COLORS[dot.state],
                        backgroundColor: `${STATE_COLORS[dot.state]}20`,
                      }}
                    >
                      {STATE_LABELS[dot.state]}
                      {duration ? ` ${duration}` : ""}
                    </span>
                  </div>
                  {/* Output rate area chart */}
                  {(() => {
                    const points = activityData?.[dot.tabId];
                    if (!points || points.length < 3) return null;
                    const data = points.slice(-30);
                    const peak = Math.max(...data, 1);
                    const w = 200;
                    const h = 30;
                    const step = w / (data.length - 1);
                    const color = STATE_COLORS[dot.state];
                    const linePoints = data
                      .map((v, i) => `${i * step},${h - (v / peak) * h}`)
                      .join(" L");
                    const areaPath = `M0,${h} L${linePoints} L${w},${h} Z`;
                    const linePath = `M${linePoints}`;
                    return (
                      <div className="px-3 py-1.5 border-b border-[#2a2d3d]">
                        <div className="text-[9px] text-[#565f89] mb-1">Output Rate</div>
                        <svg width={w} height={h} className="w-full">
                          <path d={areaPath} fill={`${color}15`} />
                          <path
                            d={linePath}
                            fill="none"
                            stroke={color}
                            strokeWidth={1.5}
                            strokeLinejoin="round"
                          />
                        </svg>
                      </div>
                    );
                  })()}
                  {/* Output preview */}
                  <div className="px-3 py-2">
                    {previewLines.length > 0 ? (
                      <div className="space-y-0.5">
                        {previewLines.map((line, i) => (
                          <div
                            key={i}
                            className="text-[9px] font-mono text-[#a9b1d6] truncate leading-tight"
                          >
                            {line || "\u00A0"}
                          </div>
                        ))}
                      </div>
                    ) : (
                      <div className="text-[9px] text-[#565f89] italic">No output yet</div>
                    )}
                  </div>
                  {/* Footer: line count + tags */}
                  <div className="px-3 py-1.5 border-t border-[#2a2d3d] flex items-center justify-between">
                    <span className="text-[9px] text-[#565f89] font-mono">
                      {outputLines.length} line{outputLines.length !== 1 ? "s" : ""}
                    </span>
                    {tags && tags.length > 0 && (
                      <div className="flex items-center gap-1">
                        {tags.map((tag) => (
                          <span
                            key={tag}
                            className="text-[8px] px-1 rounded"
                            style={{
                              color: labelColorMap?.[tag] ?? "#bb9af7",
                              backgroundColor: `${labelColorMap?.[tag] ?? "#bb9af7"}15`,
                            }}
                          >
                            {tag}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* Bulk tag operations bar */}
      {activeTagFilters.size > 0 && (onApproveTab || onRestartZone || onTogglePin) && (
        <BulkTagOpsBar
          activeTagFilters={activeTagFilters}
          filteredDots={filteredDots}
          pinnedZones={pinnedZones}
          onApproveTab={onApproveTab}
          onRestartZone={onRestartZone}
          onTogglePin={onTogglePin}
          labelColorMap={labelColorMap}
        />
      )}

      {/* Jump to next action button */}
      {hasActionNeeded && (
        <button
          onClick={onJumpToNeedsInput}
          className="flex items-center gap-1.5 px-2.5 py-1 rounded bg-[#e0af68]/15 text-[#e0af68] text-[10px] font-medium hover:bg-[#e0af68]/25 transition-colors shrink-0"
          title="Jump to next session needing input (Ctrl+Shift+N)"
        >
          <ArrowRight className="w-3 h-3" />
          Next Action
          {needsInputCount > 0 && (
            <span className="px-1 py-0.5 rounded bg-[#e0af68]/20 text-[9px]">
              {needsInputCount}
            </span>
          )}
        </button>
      )}

      {/* Auto-focus toggle */}
      {onToggleAutoFocus && (
        <button
          onClick={onToggleAutoFocus}
          className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors shrink-0 ${
            autoFocus
              ? "bg-[#e0af68]/15 text-[#e0af68]"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={`Auto-focus on needs-input: ${autoFocus ? "ON" : "OFF"}`}
        >
          <Focus className="w-3 h-3" />
          {autoFocus && <span>Auto</span>}
        </button>
      )}

      {/* Sound notification toggle */}
      {onToggleSound && (
        <button
          onClick={onToggleSound}
          className={`p-1 rounded transition-colors shrink-0 ${
            soundEnabled
              ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={`Sound notification: ${soundEnabled ? "ON" : "OFF"}`}
        >
          {soundEnabled ? (
            <Volume2 className="w-3.5 h-3.5" />
          ) : (
            <VolumeOff className="w-3.5 h-3.5" />
          )}
        </button>
      )}

      {/* Desktop notification toggle */}
      {onToggleDesktopNotify && (
        <button
          onClick={onToggleDesktopNotify}
          className={`p-1 rounded transition-colors shrink-0 ${
            desktopNotify
              ? "text-[#e0af68] bg-[#e0af68]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={`Desktop notifications: ${desktopNotify ? "ON" : "OFF"}`}
        >
          {desktopNotify ? <BellRing className="w-3.5 h-3.5" /> : <Bell className="w-3.5 h-3.5" />}
        </button>
      )}

      {/* Focus mode toggle */}
      {onToggleFocusMode && (
        <button
          onClick={onToggleFocusMode}
          className={`p-1 rounded transition-colors shrink-0 ${
            focusMode
              ? "text-[#e0af68] bg-[#e0af68]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={`Focus mode: ${focusMode ? "ON" : "OFF"} (Ctrl+Shift+D)`}
        >
          <Eye className="w-3.5 h-3.5" />
        </button>
      )}

      {/* Auto-restart toggle */}
      {onToggleAutoRestart && (
        <button
          onClick={onToggleAutoRestart}
          className={`flex items-center gap-1 p-1 rounded transition-colors shrink-0 ${
            autoRestart
              ? "text-[#7dcfff] bg-[#7dcfff]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={`Auto-restart completed sessions: ${autoRestart ? "ON" : "OFF"}${autoRestartCount ? ` (${autoRestartCount} restarted)` : ""}`}
        >
          <RefreshCw className="w-3.5 h-3.5" />
          {autoRestart && autoRestartCount !== undefined && autoRestartCount > 0 && (
            <span className="text-[9px] font-mono">{autoRestartCount}</span>
          )}
        </button>
      )}

      {/* Auto-approve config popover */}
      {onSetAutoApprovePatterns && (
        <AutoApprovePopover
          patterns={autoApprovePatterns ?? []}
          onSetPatterns={onSetAutoApprovePatterns}
          approveCount={autoApproveCount ?? 0}
        />
      )}

      {/* Event history popover */}
      {eventHistory && eventHistory.length > 0 && <HistoryPopover entries={eventHistory} />}

      {/* Sort zones button */}
      {onSortZones && (
        <button
          onClick={onSortZones}
          className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
          title="Sort zones by state (needs-input first)"
        >
          <ArrowUpDown className="w-3.5 h-3.5" />
        </button>
      )}

      {/* Export button */}
      {onExport && (
        <button
          onClick={onExport}
          className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
          title="Export all session output to file"
        >
          <Download className="w-3.5 h-3.5" />
        </button>
      )}

      {/* Metrics button + popover */}
      {metrics && (
        <MetricsPopover
          metrics={metrics}
          stateCounts={stateCounts}
          autoApproveCount={autoApproveCount ?? 0}
          autoRestartCount={autoRestartCount ?? 0}
          stateTimeAccum={stateTimeAccum}
          lastOutputLines={lastOutputLines}
          tabs={tabs}
          assignments={assignments}
          sessionStates={sessionStates}
          zoneLabels={zoneLabels}
          stateDurations={stateDurations}
        />
      )}

      {/* Keyboard shortcuts help button */}
      {onShowShortcuts && (
        <button
          onClick={onShowShortcuts}
          className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
          title="Keyboard shortcuts (Ctrl+Shift+?)"
        >
          <Keyboard className="w-3.5 h-3.5" />
        </button>
      )}
    </div>
  );
}

function BulkTagOpsBar({
  activeTagFilters,
  filteredDots,
  pinnedZones,
  onApproveTab,
  onRestartZone,
  onTogglePin,
  labelColorMap,
}: {
  activeTagFilters: Set<string>;
  filteredDots: { zoneIndex: number; tabId: string; state: SessionState; title: string }[];
  pinnedZones?: Set<number>;
  onApproveTab?: (tabId: string) => void;
  onRestartZone?: (zoneIdx: number) => void;
  onTogglePin?: (zoneIdx: number) => void;
  labelColorMap?: Record<string, string>;
}) {
  const needsInputDots = filteredDots.filter((d) => d.state === "needs-input");
  const restartableDots = filteredDots.filter(
    (d) => d.state === "completed" || d.state === "error",
  );
  const allPinned =
    filteredDots.length > 0 && filteredDots.every((d) => pinnedZones?.has(d.zoneIndex));

  const tagList = [...activeTagFilters];

  return (
    <>
      <div className="w-px h-3 bg-[#2a2d3d] shrink-0" />
      <div className="flex items-center gap-1.5 shrink-0">
        {/* Active tag label */}
        <span className="text-[9px] text-[#565f89] shrink-0">
          {tagList.map((tag, i) => (
            <span key={tag}>
              {i > 0 && <span>, </span>}
              <span style={{ color: labelColorMap?.[tag] ?? "#bb9af7" }}>{tag}</span>
            </span>
          ))}
          <span className="ml-1">({filteredDots.length})</span>
        </span>

        {/* Approve all filtered needs-input zones */}
        {onApproveTab && needsInputDots.length > 0 && (
          <button
            onClick={() => {
              for (const dot of needsInputDots) {
                onApproveTab(dot.tabId);
              }
            }}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[9px] bg-[#9ece6a]/10 text-[#9ece6a] hover:bg-[#9ece6a]/20 transition-colors"
            title={`Approve all ${needsInputDots.length} waiting zone(s) matching active tags`}
          >
            <Check className="w-2.5 h-2.5" />
            Approve {needsInputDots.length}
          </button>
        )}

        {/* Restart all filtered completed/error zones */}
        {onRestartZone && restartableDots.length > 0 && (
          <button
            onClick={() => {
              for (const dot of restartableDots) {
                onRestartZone(dot.zoneIndex);
              }
            }}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[9px] bg-[#7dcfff]/10 text-[#7dcfff] hover:bg-[#7dcfff]/20 transition-colors"
            title={`Restart all ${restartableDots.length} completed/errored zone(s) matching active tags`}
          >
            <RefreshCw className="w-2.5 h-2.5" />
            Restart {restartableDots.length}
          </button>
        )}

        {/* Pin/Unpin all filtered zones */}
        {onTogglePin && filteredDots.length > 0 && (
          <button
            onClick={() => {
              for (const dot of filteredDots) {
                onTogglePin(dot.zoneIndex);
              }
            }}
            className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[9px] transition-colors ${
              allPinned
                ? "bg-[#7aa2f7]/10 text-[#7aa2f7] hover:bg-[#7aa2f7]/20"
                : "bg-[#565f89]/10 text-[#565f89] hover:bg-[#565f89]/20 hover:text-[#a9b1d6]"
            }`}
            title={allPinned ? "Unpin all filtered zones" : "Pin all filtered zones"}
          >
            {allPinned ? <PinOff className="w-2.5 h-2.5" /> : <Pin className="w-2.5 h-2.5" />}
            {allPinned ? "Unpin" : "Pin"} all
          </button>
        )}
      </div>
    </>
  );
}

function MetricsPopover({
  metrics,
  stateCounts,
  autoApproveCount,
  autoRestartCount,
  stateTimeAccum,
  lastOutputLines,
  tabs,
  assignments,
  sessionStates,
  zoneLabels,
  stateDurations,
}: {
  metrics: {
    totalApprovals: number;
    totalRejections: number;
    totalBroadcasts: number;
    sessionsCreated: number;
  };
  stateCounts: Record<SessionState, number>;
  autoApproveCount: number;
  autoRestartCount: number;
  stateTimeAccum?: Record<SessionState, number>;
  lastOutputLines?: Record<string, string[]>;
  tabs?: TerminalTab[];
  assignments?: ZoneAssignments;
  sessionStates?: Record<string, SessionState>;
  zoneLabels?: Record<number, string>;
  stateDurations?: Record<string, string>;
}) {
  const [open, setOpen] = useState(false);
  const [summCopied, setSummCopied] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const topKeywords = useMemo(() => {
    if (!lastOutputLines) return [];
    const stopWords = new Set([
      "the",
      "a",
      "an",
      "is",
      "are",
      "was",
      "were",
      "be",
      "been",
      "being",
      "have",
      "has",
      "had",
      "do",
      "does",
      "did",
      "will",
      "would",
      "could",
      "should",
      "may",
      "might",
      "shall",
      "can",
      "need",
      "dare",
      "ought",
      "used",
      "to",
      "of",
      "in",
      "for",
      "on",
      "with",
      "at",
      "by",
      "from",
      "as",
      "into",
      "through",
      "during",
      "before",
      "after",
      "above",
      "below",
      "between",
      "out",
      "off",
      "over",
      "under",
      "again",
      "further",
      "then",
      "once",
      "here",
      "there",
      "when",
      "where",
      "why",
      "how",
      "all",
      "each",
      "every",
      "both",
      "few",
      "more",
      "most",
      "other",
      "some",
      "such",
      "no",
      "nor",
      "not",
      "only",
      "own",
      "same",
      "so",
      "than",
      "too",
      "very",
      "just",
      "because",
      "but",
      "and",
      "or",
      "if",
      "while",
      "that",
      "this",
      "these",
      "those",
      "it",
      "its",
    ]);

    const freq: Record<string, number> = {};
    for (const lines of Object.values(lastOutputLines)) {
      for (const line of lines) {
        const words = line
          .toLowerCase()
          .replace(/[^a-z0-9\s]/g, " ")
          .split(/\s+/);
        for (const w of words) {
          if (w.length >= 3 && !stopWords.has(w) && !/^\d+$/.test(w)) {
            freq[w] = (freq[w] ?? 0) + 1;
          }
        }
      }
    }
    return Object.entries(freq)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 5);
  }, [lastOutputLines]);

  const handleCopySummary = () => {
    if (!tabs || !assignments) return;
    const rows: string[] = [];
    rows.push("| Zone | Title | State | Duration | Lines | Tags |");
    rows.push("|------|-------|-------|----------|-------|------|");

    const sortedEntries = Object.entries(assignments)
      .map(([z, tabId]) => ({ zone: Number(z), tabId }))
      .sort((a, b) => a.zone - b.zone);

    for (const { zone, tabId } of sortedEntries) {
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) continue;
      const state = sessionStates?.[tabId] ?? "idle";
      const duration = stateDurations?.[tabId] ?? "-";
      const lineCount = lastOutputLines?.[tabId]?.length ?? 0;
      const tags = zoneLabels?.[zone] ?? "-";
      rows.push(`| ${zone + 1} | ${tab.title} | ${state} | ${duration} | ${lineCount} | ${tags} |`);
    }

    navigator.clipboard.writeText(rows.join("\n")).then(() => {
      setSummCopied(true);
      setTimeout(() => setSummCopied(false), 1500);
    });
  };

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        className={`p-1 rounded transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title="Session metrics"
      >
        <BarChart3 className="w-3.5 h-3.5" />
      </button>
      {open && (
        <div className="absolute right-0 bottom-full mb-2 w-52 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            Session Metrics
          </div>
          <div className="p-3 space-y-1.5 text-[11px]">
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Sessions created</span>
              <span className="text-[#c0caf5] font-mono">{metrics.sessionsCreated}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#9ece6a]">Approvals sent</span>
              <span className="text-[#c0caf5] font-mono">{metrics.totalApprovals}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#f7768e]">Rejections sent</span>
              <span className="text-[#c0caf5] font-mono">{metrics.totalRejections}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#7aa2f7]">Broadcasts sent</span>
              <span className="text-[#c0caf5] font-mono">{metrics.totalBroadcasts}</span>
            </div>
            {autoApproveCount > 0 && (
              <div className="flex justify-between">
                <span className="text-[#9ece6a]">Auto-approved</span>
                <span className="text-[#c0caf5] font-mono">{autoApproveCount}</span>
              </div>
            )}
            {autoRestartCount > 0 && (
              <div className="flex justify-between">
                <span className="text-[#7dcfff]">Auto-restarted</span>
                <span className="text-[#c0caf5] font-mono">{autoRestartCount}</span>
              </div>
            )}
            <div className="h-px bg-[#2a2d3d] my-1" />
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Currently working</span>
              <span className="text-[#7aa2f7] font-mono">{stateCounts.working}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Currently waiting</span>
              <span className="text-[#e0af68] font-mono">{stateCounts["needs-input"]}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Completed</span>
              <span className="text-[#9ece6a] font-mono">{stateCounts.completed}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Errors</span>
              <span className="text-[#f7768e] font-mono">{stateCounts.error}</span>
            </div>
            {stateTimeAccum &&
              (stateTimeAccum.working > 0 || stateTimeAccum["needs-input"] > 0) && (
                <>
                  <div className="h-px bg-[#2a2d3d] my-1" />
                  <div className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1">
                    Time in state
                  </div>
                  {(["working", "needs-input", "idle", "completed", "error"] as SessionState[]).map(
                    (s) => {
                      const ms = stateTimeAccum[s];
                      if (ms < 1000) return null;
                      const sec = Math.floor(ms / 1000);
                      const min = Math.floor(sec / 60);
                      const hr = Math.floor(min / 60);
                      const label =
                        hr > 0 ? `${hr}h${min % 60}m` : min > 0 ? `${min}m${sec % 60}s` : `${sec}s`;
                      return (
                        <div key={s} className="flex justify-between">
                          <span style={{ color: STATE_COLORS[s] }}>{STATE_LABELS[s]}</span>
                          <span className="text-[#c0caf5] font-mono">{label}</span>
                        </div>
                      );
                    },
                  )}
                </>
              )}
            {topKeywords.length > 0 && (
              <>
                <div className="h-px bg-[#2a2d3d] my-1" />
                <div className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1">
                  Top Keywords
                </div>
                {topKeywords.map(([word, count]) => (
                  <div key={word} className="flex justify-between">
                    <span className="text-[#a9b1d6] truncate">{word}</span>
                    <span className="text-[#c0caf5] font-mono">{count}</span>
                  </div>
                ))}
              </>
            )}
          </div>
          <div className="px-3 py-2 border-t border-[#2a2d3d]">
            <button
              onClick={handleCopySummary}
              className={`w-full text-[10px] py-1 rounded transition-colors ${
                summCopied
                  ? "text-[#9ece6a] bg-[#9ece6a]/10"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
              }`}
            >
              {summCopied ? "Copied!" : "Copy summary as Markdown"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function AutoApprovePopover({
  patterns,
  onSetPatterns,
  approveCount,
}: {
  patterns: string[];
  onSetPatterns: (patterns: string[]) => void;
  approveCount: number;
}) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState(() => patterns.join("\n"));
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        // Save on close
        const newPatterns = text
          .split("\n")
          .map((l) => l.trim())
          .filter(Boolean);
        onSetPatterns(newPatterns);
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open, text, onSetPatterns]);

  // Sync text when patterns change externally
  useEffect(() => {
    if (!open) setText(patterns.join("\n"));
  }, [patterns, open]);

  const hasPatterns = patterns.length > 0;

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        className={`flex items-center gap-1 p-1 rounded transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : hasPatterns
              ? "text-[#9ece6a] bg-[#9ece6a]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title={`Auto-approve rules: ${hasPatterns ? `${patterns.length} pattern(s), ${approveCount} auto-approved` : "none configured"}`}
      >
        <Shield className="w-3.5 h-3.5" />
        {hasPatterns && <span className="text-[9px] font-mono">{approveCount}</span>}
      </button>
      {open && (
        <div className="absolute right-0 bottom-full mb-2 w-72 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            Auto-Approve Rules
          </div>
          <div className="p-3 space-y-2">
            <p className="text-[10px] text-[#565f89]">
              Regex patterns (one per line). When a session enters &quot;needs-input&quot; and its
              last output matches a pattern, &quot;y&quot; is sent automatically.
            </p>
            <textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => e.stopPropagation()}
              placeholder={"Do you want to proceed\\?\nApply changes\\?\nContinue\\?"}
              rows={5}
              className="w-full bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1.5 text-[11px] font-mono text-[#c0caf5] placeholder-[#565f89]/50 outline-none focus:border-[#7aa2f7] transition-colors resize-none"
            />
            <div className="flex items-center justify-between text-[10px]">
              <span className="text-[#565f89]">
                {patterns.length} pattern{patterns.length !== 1 ? "s" : ""}
              </span>
              <span className="text-[#9ece6a]">{approveCount} auto-approved</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function HistoryPopover({
  entries,
}: {
  entries: { time: number; type: string; session: string; zone?: number; color: string }[];
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const formatTime = (ts: number) => {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  };

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        className={`p-1 rounded transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title="Event history"
      >
        <Clock className="w-3.5 h-3.5" />
      </button>
      {open && (
        <div className="absolute right-0 bottom-full mb-2 w-72 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            Event History ({entries.length})
          </div>
          <div className="max-h-64 overflow-y-auto scrollbar-dark">
            {entries.length === 0 ? (
              <div className="px-3 py-4 text-center text-[10px] text-[#565f89]">No events yet</div>
            ) : (
              [...entries].reverse().map((entry, i) => (
                <div
                  key={i}
                  className="flex items-start gap-2 px-3 py-1.5 hover:bg-[#2a2d3d]/30 transition-colors"
                >
                  <span className="text-[9px] text-[#565f89] font-mono shrink-0 mt-0.5">
                    {formatTime(entry.time)}
                  </span>
                  <div className="flex-1 min-w-0">
                    <span className="text-[10px] font-medium" style={{ color: entry.color }}>
                      {entry.type}
                    </span>
                    <span className="text-[10px] text-[#a9b1d6] ml-1 truncate">
                      {entry.session}
                    </span>
                  </div>
                  {entry.zone !== undefined && (
                    <span className="text-[9px] text-[#565f89] font-mono shrink-0">
                      Z{entry.zone + 1}
                    </span>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}
