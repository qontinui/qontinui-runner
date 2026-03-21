import { useCallback, useMemo, useReducer, useRef, useEffect } from "react";
import { Maximize2, Check, X, Send, AlertTriangle, Copy, Filter, RotateCcw } from "lucide-react";
import type { TerminalTab } from "../useTerminalManager";
import type { ZoneAssignments, SessionState } from "../useZoneLayout";
import { STATE_BORDER_COLORS, STATE_BG_COLORS, STATE_LABELS, TREND_ICONS } from "./constants";
import { isActionableLine, isYesNoPrompt, computeOutputTrend } from "./utils";
import { ActivitySparkline } from "./ActivitySparkline";
import { HighlightedText } from "./HighlightedText";

interface CompactCardState {
  commandText: string;
  isHovered: boolean;
  editingLabel: boolean;
  labelValue: string;
  editingNote: boolean;
  noteValue: string;
  copied: boolean;
  outputFilter: string;
  showFilter: boolean;
  showSwitch: boolean;
}

type CompactCardAction =
  | { type: "SET_COMMAND_TEXT"; value: string }
  | { type: "SET_HOVERED"; value: boolean }
  | { type: "START_EDIT_LABEL"; value: string }
  | { type: "CANCEL_EDIT_LABEL"; originalValue: string }
  | { type: "FINISH_EDIT_LABEL" }
  | { type: "SET_LABEL_VALUE"; value: string }
  | { type: "START_EDIT_NOTE"; value: string }
  | { type: "CANCEL_EDIT_NOTE"; originalValue: string }
  | { type: "FINISH_EDIT_NOTE" }
  | { type: "SET_NOTE_VALUE"; value: string }
  | { type: "SET_COPIED"; value: boolean }
  | { type: "SET_OUTPUT_FILTER"; value: string }
  | { type: "TOGGLE_FILTER" }
  | { type: "SET_SHOW_SWITCH"; value: boolean };

function compactCardReducer(state: CompactCardState, action: CompactCardAction): CompactCardState {
  switch (action.type) {
    case "SET_COMMAND_TEXT":
      return { ...state, commandText: action.value };
    case "SET_HOVERED":
      return { ...state, isHovered: action.value };
    case "START_EDIT_LABEL":
      return { ...state, editingLabel: true, labelValue: action.value };
    case "CANCEL_EDIT_LABEL":
      return { ...state, editingLabel: false, labelValue: action.originalValue };
    case "FINISH_EDIT_LABEL":
      return { ...state, editingLabel: false };
    case "SET_LABEL_VALUE":
      return { ...state, labelValue: action.value };
    case "START_EDIT_NOTE":
      return { ...state, editingNote: true, noteValue: action.value };
    case "CANCEL_EDIT_NOTE":
      return { ...state, editingNote: false, noteValue: action.originalValue };
    case "FINISH_EDIT_NOTE":
      return { ...state, editingNote: false };
    case "SET_NOTE_VALUE":
      return { ...state, noteValue: action.value };
    case "SET_COPIED":
      return { ...state, copied: action.value };
    case "SET_OUTPUT_FILTER":
      return { ...state, outputFilter: action.value };
    case "TOGGLE_FILTER":
      return {
        ...state,
        showFilter: !state.showFilter,
        outputFilter: state.showFilter ? "" : state.outputFilter,
      };
    case "SET_SHOW_SWITCH":
      return { ...state, showSwitch: action.value };
  }
}

function createInitialState(zoneLabel?: string, note?: string): CompactCardState {
  return {
    commandText: "",
    isHovered: false,
    editingLabel: false,
    labelValue: zoneLabel ?? "",
    editingNote: false,
    noteValue: note ?? "",
    copied: false,
    outputFilter: "",
    showFilter: false,
    showSwitch: false,
  };
}

export function CompactZoneCard({
  tab,
  state,
  zoneIndex,
  lastLines,
  onQuickApprove,
  onQuickReject,
  onSendCommand,
  duration,
  isStale,
  searchQuery,
  activity,
  zoneLabel,
  onSetZoneLabel,
  onRestart,
  groupColor,
  uptime,
  lastCommand,
  note,
  onSetNote,
  allTabs,
  assignments,
  sessionStates,
  onAssignTab,
  tagColor,
}: {
  tab: TerminalTab;
  state: SessionState;
  zoneIndex: number;
  lastLines: string[];
  onQuickApprove?: () => void;
  onQuickReject?: () => void;
  onSendCommand?: (text: string) => void;
  duration?: string;
  isStale?: boolean;
  searchQuery?: string;
  activity?: number[];
  zoneLabel?: string;
  onSetZoneLabel?: (label: string) => void;
  onRestart?: () => void;
  groupColor?: string;
  uptime?: string;
  lastCommand?: string;
  note?: string;
  onSetNote?: (note: string) => void;
  allTabs?: TerminalTab[];
  assignments?: ZoneAssignments;
  sessionStates?: Record<string, SessionState>;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  tagColor?: string;
}) {
  const needsInput = state === "needs-input";
  const [cardState, dispatch] = useReducer(
    compactCardReducer,
    { zoneLabel, note },
    ({ zoneLabel: zl, note: n }) => createInitialState(zl, n),
  );

  const switchRef = useRef<HTMLDivElement>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const yesNo = needsInput && isYesNoPrompt(lastLines);
  const outputRef = useRef<HTMLDivElement>(null);

  const spinSpeed = useMemo(() => {
    if (state !== "working" || !activity || activity.length < 2) return "2s";
    const recent = activity.slice(-3);
    const avg = recent.reduce((a, b) => a + b, 0) / recent.length;
    if (avg > 100) return "0.5s";
    if (avg > 50) return "1s";
    if (avg > 10) return "1.5s";
    return "2.5s";
  }, [state, activity]);

  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [lastLines]);

  useEffect(() => {
    if (!cardState.showSwitch) return;
    const handleMouseDown = (e: MouseEvent) => {
      if (switchRef.current && !switchRef.current.contains(e.target as Node)) {
        dispatch({ type: "SET_SHOW_SWITCH", value: false });
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") dispatch({ type: "SET_SHOW_SWITCH", value: false });
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [cardState.showSwitch]);

  const handleMouseEnter = useCallback(() => {
    hoverTimerRef.current = setTimeout(() => dispatch({ type: "SET_HOVERED", value: true }), 300);
  }, []);

  const handleMouseLeave = useCallback(() => {
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    dispatch({ type: "SET_HOVERED", value: false });
  }, []);

  const searchMatchCount =
    searchQuery && searchQuery.length >= 2
      ? lastLines.filter((l) => l.toLowerCase().includes(searchQuery.toLowerCase())).length
      : 0;

  return (
    <div
      className="h-full w-full flex flex-col p-2 gap-1.5 select-none cursor-pointer relative group"
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      <div
        className="h-[3px] w-full rounded-t shrink-0"
        style={{ backgroundColor: tagColor ? `${tagColor}40` : "#2a2d3d20" }}
      />
      {lastLines.length > 0 && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            if (lastLines.length === 0) return;
            navigator.clipboard.writeText(lastLines.join("\n")).then(() => {
              dispatch({ type: "SET_COPIED", value: true });
              setTimeout(() => dispatch({ type: "SET_COPIED", value: false }), 1500);
            });
          }}
          className={`absolute top-1 right-1 p-0.5 rounded transition-all z-10 ${
            cardState.copied
              ? "text-[#9ece6a] opacity-100"
              : "text-[#565f89] opacity-0 group-hover:opacity-100 hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
          }`}
          title={cardState.copied ? "Copied!" : "Copy output to clipboard"}
        >
          {cardState.copied ? <Check className="w-3 h-3" /> : <Copy className="w-3 h-3" />}
        </button>
      )}
      {searchQuery && searchMatchCount > 0 && (
        <span className="absolute top-1 right-6 text-[8px] font-mono bg-[#e0af68]/20 text-[#e0af68] rounded px-1 z-10">
          {searchMatchCount}
        </span>
      )}
      <div className="flex items-center gap-1.5 shrink-0">
        <div className="relative shrink-0" ref={switchRef}>
          <span
            className="text-[9px] text-[#565f89] font-mono w-3 shrink-0 cursor-grab active:cursor-grabbing hover:text-[#a9b1d6] transition-colors"
            role="button"
            tabIndex={0}
            draggable
            onDragStart={(e) => {
              e.dataTransfer.setData("text/zone-index", String(zoneIndex));
              e.dataTransfer.effectAllowed = "move";
              e.stopPropagation();
            }}
            onClick={(e) => {
              e.stopPropagation();
              dispatch({ type: "SET_SHOW_SWITCH", value: !cardState.showSwitch });
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.stopPropagation();
                dispatch({ type: "SET_SHOW_SWITCH", value: !cardState.showSwitch });
              }
            }}
            title="Click to quick-switch session, drag to swap zones"
          >
            {zoneIndex + 1}
          </span>
          {cardState.showSwitch && allTabs && assignments && sessionStates && onAssignTab && (
            <QuickSwitchDropdown
              zoneIndex={zoneIndex}
              allTabs={allTabs}
              assignments={assignments}
              sessionStates={sessionStates}
              onAssignTab={onAssignTab}
              onClose={() => dispatch({ type: "SET_SHOW_SWITCH", value: false })}
            />
          )}
        </div>
        {state === "working" ? (
          <svg width={12} height={12} className="shrink-0" viewBox="0 0 12 12">
            <circle
              cx={6}
              cy={6}
              r={4.5}
              fill="none"
              stroke="#7aa2f7"
              strokeWidth={1.5}
              opacity={0.2}
            />
            <circle
              cx={6}
              cy={6}
              r={4.5}
              fill="none"
              stroke="#7aa2f7"
              strokeWidth={1.5}
              strokeDasharray="14 14"
              strokeLinecap="round"
              className="animate-spin"
              style={{ transformOrigin: "center", animationDuration: spinSpeed }}
            />
          </svg>
        ) : (
          <div
            className={`w-2 h-2 rounded-full shrink-0 ${state === "needs-input" ? "animate-pulse" : ""}`}
            style={{ backgroundColor: STATE_BORDER_COLORS[state] }}
          />
        )}
        <span className="text-[11px] text-[#c0caf5] font-medium truncate flex-1">{tab.title}</span>
        {lastLines.length > 0 && (
          <button
            className={`p-0.5 rounded transition-colors shrink-0 ${
              cardState.showFilter || cardState.outputFilter
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
            }`}
            title={
              cardState.outputFilter
                ? `Filter: ${cardState.outputFilter}`
                : "Filter output by regex"
            }
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              dispatch({ type: "TOGGLE_FILTER" });
            }}
          >
            <Filter className="w-2.5 h-2.5" />
          </button>
        )}
        {isStale && (
          <span
            className="flex items-center gap-0.5 text-[9px] text-[#e0af68] shrink-0"
            title="No output for 60s+ — may be stalled"
          >
            <AlertTriangle className="w-3 h-3" />
            stalled?
          </span>
        )}
        {activity && activity.length > 1 && <ActivitySparkline data={activity} />}
        {(() => {
          const trend = computeOutputTrend(activity);
          if (!trend || state !== "working") return null;
          const icon = TREND_ICONS[trend.trend];
          return (
            <span
              className="text-[8px] shrink-0"
              style={{ color: icon.color }}
              title={`Output trend: ${trend.trend}`}
            >
              {icon.symbol}
            </span>
          );
        })()}
        <span
          className={`text-[9px] px-1.5 py-0.5 rounded-full font-medium shrink-0 ${STATE_BG_COLORS[state]}`}
          style={{ color: STATE_BORDER_COLORS[state] }}
        >
          {STATE_LABELS[state]}
          {duration && <span className="ml-1 opacity-70">{duration}</span>}
          {uptime && (
            <span className="ml-1 opacity-50">
              {"\u23F1"}
              {uptime}
            </span>
          )}
        </span>
      </div>

      {(zoneLabel || onSetZoneLabel) && (
        <div className="shrink-0">
          {cardState.editingLabel ? (
            <input
              autoFocus
              value={cardState.labelValue}
              onChange={(e) => dispatch({ type: "SET_LABEL_VALUE", value: e.target.value })}
              onBlur={() => {
                dispatch({ type: "FINISH_EDIT_LABEL" });
                onSetZoneLabel?.(cardState.labelValue.trim());
              }}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  dispatch({ type: "FINISH_EDIT_LABEL" });
                  onSetZoneLabel?.(cardState.labelValue.trim());
                }
                if (e.key === "Escape") {
                  dispatch({ type: "CANCEL_EDIT_LABEL", originalValue: zoneLabel ?? "" });
                }
              }}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
              className="bg-transparent border-b border-[#565f89] text-[9px] text-[#bb9af7] outline-hidden w-20"
              placeholder="Group label..."
            />
          ) : zoneLabel ? (
            <span
              className="text-[9px] px-1.5 py-0.5 rounded cursor-pointer transition-colors"
              style={{
                color: groupColor ?? "#bb9af7",
                backgroundColor: `${groupColor ?? "#bb9af7"}15`,
              }}
              onDoubleClick={(e) => {
                e.stopPropagation();
                dispatch({ type: "START_EDIT_LABEL", value: zoneLabel });
              }}
              title="Double-click to edit group label"
            >
              {zoneLabel}
            </span>
          ) : (
            <span
              className="text-[9px] text-[#565f89]/40 cursor-pointer hover:text-[#565f89] transition-colors"
              onDoubleClick={(e) => {
                e.stopPropagation();
                dispatch({ type: "START_EDIT_LABEL", value: "" });
              }}
              title="Double-click to add group label"
            >
              + label
            </span>
          )}
        </div>
      )}

      {(note || onSetNote) && (
        <div className="shrink-0">
          {cardState.editingNote ? (
            <input
              autoFocus
              value={cardState.noteValue}
              onChange={(e) =>
                dispatch({ type: "SET_NOTE_VALUE", value: e.target.value.slice(0, 100) })
              }
              onBlur={() => {
                dispatch({ type: "FINISH_EDIT_NOTE" });
                onSetNote?.(cardState.noteValue.trim());
              }}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  dispatch({ type: "FINISH_EDIT_NOTE" });
                  onSetNote?.(cardState.noteValue.trim());
                }
                if (e.key === "Escape") {
                  dispatch({ type: "CANCEL_EDIT_NOTE", originalValue: note ?? "" });
                }
              }}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => e.stopPropagation()}
              className="bg-transparent border-b border-[#565f89] text-[9px] text-[#a9b1d6] outline-hidden w-full"
              placeholder="Add a note..."
              maxLength={100}
            />
          ) : note ? (
            <span
              className="text-[9px] text-[#565f89] italic truncate block cursor-pointer hover:text-[#a9b1d6] transition-colors"
              onDoubleClick={(e) => {
                e.stopPropagation();
                dispatch({ type: "START_EDIT_NOTE", value: note });
              }}
              title={`${note}\n(double-click to edit)`}
            >
              {note}
            </span>
          ) : (
            <span
              className="text-[9px] text-[#565f89]/30 cursor-pointer hover:text-[#565f89] transition-colors"
              onDoubleClick={(e) => {
                e.stopPropagation();
                dispatch({ type: "START_EDIT_NOTE", value: "" });
              }}
              title="Double-click to add note"
            >
              + note
            </span>
          )}
        </div>
      )}

      {cardState.isHovered && lastLines.length > 6 && (
        <div
          className="absolute left-0 right-0 bottom-full mb-1 z-40 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl p-2 max-h-[240px] overflow-y-auto scrollbar-dark"
          onMouseEnter={() => dispatch({ type: "SET_HOVERED", value: true })}
          onMouseLeave={handleMouseLeave}
        >
          <div className="text-[9px] text-[#565f89] mb-1 font-medium">
            Output preview — {lastLines.length} lines
          </div>
          <div className="font-mono text-[10px] leading-relaxed">
            {lastLines.map((line, i) => {
              const actionable = needsInput && isActionableLine(line);
              return (
                <div
                  key={`preview-${tab.id}-${i}`}
                  className={`truncate ${
                    actionable
                      ? "text-[#e0af68] font-semibold bg-[#e0af68]/10 rounded px-0.5 -mx-0.5"
                      : "text-[#a9b1d6]/70"
                  }`}
                >
                  {line ? <HighlightedText text={line} query={searchQuery} /> : "\u00A0"}
                </div>
              );
            })}
          </div>
        </div>
      )}

      {(tab.workingDir || lastCommand) && (
        <div className="shrink-0">
          {tab.workingDir && (
            <div className="text-[9px] text-[#565f89] truncate">
              {tab.workingDir.split(/[/\\]/).slice(-2).join("/")}
            </div>
          )}
          {lastCommand && (
            <div className="text-[9px] text-[#7aa2f7]/50 font-mono truncate" title={lastCommand}>
              $ {lastCommand}
            </div>
          )}
        </div>
      )}

      {cardState.showFilter && (
        <CompactFilterBar
          outputFilter={cardState.outputFilter}
          lastLines={lastLines}
          onFilterChange={(value) => dispatch({ type: "SET_OUTPUT_FILTER", value })}
          onClose={() => {
            dispatch({ type: "TOGGLE_FILTER" });
          }}
        />
      )}

      <CompactOutputLines
        lastLines={lastLines}
        needsInput={needsInput}
        outputFilter={cardState.outputFilter}
        searchQuery={searchQuery}
        outputRef={outputRef}
      />

      {needsInput && onQuickApprove ? (
        <CompactInputActions
          yesNo={yesNo}
          commandText={cardState.commandText}
          onCommandTextChange={(value) => dispatch({ type: "SET_COMMAND_TEXT", value })}
          onQuickApprove={onQuickApprove}
          onQuickReject={onQuickReject}
          onSendCommand={onSendCommand}
        />
      ) : (state === "completed" || state === "error") && onRestart ? (
        <div className="flex items-center gap-1.5 shrink-0">
          <button
            className="flex-1 flex items-center justify-center gap-1 text-[10px] font-medium
              py-1 rounded bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/25
              transition-colors"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onRestart();
            }}
          >
            <RotateCcw className="w-3 h-3" />
            Restart
          </button>
        </div>
      ) : (
        <div className="flex items-center gap-1 text-[9px] text-[#565f89]/60 shrink-0">
          <Maximize2 className="w-2.5 h-2.5" />
          <span>Click to focus, double-click to maximize</span>
        </div>
      )}

      {state === "working" &&
        (() => {
          const trend = computeOutputTrend(activity);
          if (!trend || trend.peak === 0) return null;
          const pct = Math.min(100, (trend.rate / trend.peak) * 100);
          return (
            <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-[#2a2d3d]/50">
              <div
                className="h-full transition-all duration-1000 ease-out"
                style={{
                  width: `${pct}%`,
                  backgroundColor: TREND_ICONS[trend.trend].color,
                  opacity: 0.6,
                }}
              />
            </div>
          );
        })()}
    </div>
  );
}

function QuickSwitchDropdown({
  zoneIndex,
  allTabs,
  assignments,
  sessionStates,
  onAssignTab,
  onClose,
}: {
  zoneIndex: number;
  allTabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  onAssignTab: (zoneIndex: number, tabId: string) => void;
  onClose: () => void;
}) {
  const currentTabId = assignments[zoneIndex];
  const otherTabs = allTabs.filter((t) => t.id !== currentTabId);

  return (
    <div
      role="presentation"
      className="absolute top-full left-0 mt-1 min-w-[140px] max-h-[150px] overflow-y-auto bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50"
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
    >
      {otherTabs.length === 0 ? (
        <div className="px-2 py-1.5 text-[10px] text-[#565f89]">No other sessions</div>
      ) : (
        otherTabs.map((t) => {
          const tState = sessionStates[t.id] ?? "idle";
          return (
            <button
              key={t.id}
              className="flex items-center gap-1.5 w-full px-2 py-1 text-left hover:bg-[#2a2d3d] transition-colors"
              onClick={(e) => {
                e.stopPropagation();
                onAssignTab(zoneIndex, t.id);
                onClose();
              }}
            >
              <div
                className="w-1.5 h-1.5 rounded-full shrink-0"
                style={{ backgroundColor: STATE_BORDER_COLORS[tState] }}
              />
              <span className="text-[10px] text-[#c0caf5] truncate">{t.title}</span>
            </button>
          );
        })
      )}
    </div>
  );
}

function CompactFilterBar({
  outputFilter,
  lastLines,
  onFilterChange,
  onClose,
}: {
  outputFilter: string;
  lastLines: string[];
  onFilterChange: (value: string) => void;
  onClose: () => void;
}) {
  return (
    <div className="flex items-center gap-1 shrink-0">
      <input
        autoFocus
        value={outputFilter}
        onChange={(e) => onFilterChange(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") {
            onClose();
          }
        }}
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
        placeholder="Filter regex..."
        className="flex-1 bg-[#13141f] border border-[#2a2d3d] rounded px-1.5 py-0.5 text-[9px] font-mono text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors"
      />
      {outputFilter &&
        (() => {
          let matchCount = 0;
          try {
            const re = new RegExp(outputFilter, "i");
            matchCount = lastLines.filter((l) => re.test(l)).length;
          } catch {
            // intentionally empty
          }
          return (
            <span
              className={`text-[9px] shrink-0 ${matchCount > 0 ? "text-[#9ece6a]" : "text-[#565f89]"}`}
            >
              {matchCount}/{lastLines.length}
            </span>
          );
        })()}
    </div>
  );
}

function CompactOutputLines({
  lastLines,
  needsInput,
  outputFilter,
  searchQuery,
  outputRef,
}: {
  lastLines: string[];
  needsInput: boolean;
  outputFilter: string;
  searchQuery?: string;
  outputRef: React.RefObject<HTMLDivElement | null>;
}) {
  let filteredLines = lastLines;
  let filterRegex: RegExp | null = null;
  if (outputFilter) {
    try {
      filterRegex = new RegExp(outputFilter, "i");
      filteredLines = lastLines.filter((l) => filterRegex!.test(l));
    } catch {
      // Invalid regex — show all lines
    }
  }

  return (
    <div
      role="presentation"
      ref={outputRef}
      className="flex-1 min-h-0 overflow-y-auto scrollbar-dark"
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
    >
      <div className="font-mono text-[10px] leading-relaxed">
        {filteredLines.length > 0 ? (
          filteredLines.map((line, i) => {
            const actionable = needsInput && isActionableLine(line);
            return (
              <div
                key={`output-${i}`}
                className={`truncate ${
                  actionable
                    ? "text-[#e0af68] font-semibold bg-[#e0af68]/10 rounded px-0.5 -mx-0.5"
                    : "text-[#a9b1d6]/70"
                }`}
              >
                {line ? (
                  <HighlightedText text={line} query={filterRegex ? outputFilter : searchQuery} />
                ) : (
                  "\u00A0"
                )}
              </div>
            );
          })
        ) : (
          <div className="text-[#565f89] italic">
            {outputFilter ? "No matching lines" : "No output yet"}
          </div>
        )}
      </div>
    </div>
  );
}

function CompactInputActions({
  yesNo,
  commandText,
  onCommandTextChange,
  onQuickApprove,
  onQuickReject,
  onSendCommand,
}: {
  yesNo: boolean;
  commandText: string;
  onCommandTextChange: (value: string) => void;
  onQuickApprove: () => void;
  onQuickReject?: () => void;
  onSendCommand?: (text: string) => void;
}) {
  return (
    <div className="flex flex-col gap-1 shrink-0">
      {yesNo && (
        <div className="flex items-center gap-1.5">
          <button
            className="flex-1 flex items-center justify-center gap-1 text-[10px] font-medium
              py-1 rounded bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25
              transition-colors"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onQuickApprove();
            }}
          >
            <Check className="w-3 h-3" />
            Approve
          </button>
          {onQuickReject && (
            <button
              className="flex-1 flex items-center justify-center gap-1 text-[10px] font-medium
                py-1 rounded bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25
                transition-colors"
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                onQuickReject();
              }}
            >
              <X className="w-3 h-3" />
              Reject
            </button>
          )}
        </div>
      )}
      {onSendCommand && (
        <div className="flex items-center gap-1">
          <input
            type="text"
            value={commandText}
            onChange={(e) => onCommandTextChange(e.target.value)}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter" && commandText.trim()) {
                onSendCommand(commandText);
                onCommandTextChange("");
              }
            }}
            placeholder={yesNo ? "or type..." : "Type response..."}
            className={`flex-1 min-w-0 bg-[#2a2d3d]/50 border border-[#3b3d57] rounded px-1.5 py-0.5
              text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden
              focus:border-[#7aa2f7] transition-colors ${!yesNo ? "border-[#e0af68]/50" : ""}`}
          />
          <button
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              if (commandText.trim()) {
                onSendCommand(commandText);
                onCommandTextChange("");
              }
            }}
            disabled={!commandText.trim()}
            className="p-1 rounded text-[#565f89] hover:text-[#7aa2f7] disabled:opacity-30 transition-colors"
          >
            <Send className="w-3 h-3" />
          </button>
        </div>
      )}
    </div>
  );
}
