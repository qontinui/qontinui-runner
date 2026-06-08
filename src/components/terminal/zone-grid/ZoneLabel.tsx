import { useState, useRef, useEffect } from "react";
import { ChevronDown, Pin, Filter, ArrowDownToLine, Zap } from "lucide-react";
import type { TerminalTab } from "../useTerminalManager";
import type { ZoneAssignments, SessionState } from "../useZoneLayout";
import { STATE_BORDER_COLORS } from "./constants";
import { isNonDurablePty, NON_DURABLE_TOOLTIP, NON_DURABLE_LABEL } from "../sessionDurability";
import { useTerminalWindowActions } from "../useTerminalWindowActions";

export function ZoneLabel({
  tab,
  state,
  zoneIndex,
  allTabs,
  assignments,
  sessionStates,
  onAssignTab,
  isPinned,
  onTogglePin,
  zoneLabel,
  onSetZoneLabel,
  onScrollToBottom,
  outputLineCount,
  outputByteSize,
  onToggleFilter,
  filterActive,
}: {
  tab: TerminalTab;
  state: SessionState;
  zoneIndex: number;
  allTabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  isPinned?: boolean;
  onTogglePin?: () => void;
  zoneLabel?: string;
  onSetZoneLabel?: (label: string) => void;
  onScrollToBottom?: () => void;
  outputLineCount?: number;
  outputByteSize?: number;
  onToggleFilter?: () => void;
  filterActive?: boolean;
}) {
  const [showSelector, setShowSelector] = useState(false);
  const [editingLabel, setEditingLabel] = useState(false);
  const [labelValue, setLabelValue] = useState(zoneLabel ?? "");
  const selectorRef = useRef<HTMLDivElement>(null);
  const { popOutTab } = useTerminalWindowActions();

  useEffect(() => {
    if (!showSelector) return;
    const handleClick = (e: MouseEvent) => {
      if (selectorRef.current && !selectorRef.current.contains(e.target as Node)) {
        setShowSelector(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showSelector]);

  return (
    <div
      className="absolute top-0 left-0 right-0 flex items-center gap-1.5 px-2 py-0.5 bg-[#13141f]/80 backdrop-blur-sm z-10 cursor-grab active:cursor-grabbing"
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData("text/tab-id", tab.id);
        e.dataTransfer.effectAllowed = "move";
      }}
      onDragEnd={(e) => {
        // Drag-tab-out: if the header was dropped OUTSIDE this window's
        // viewport and no in-window zone accepted it (dropEffect "none"),
        // pop the terminal into its own new window. HTML5 DnD can't cross
        // OS windows, so a release over another runner window also lands
        // here as "none" with an out-of-bounds pointer — treated the same
        // (new window). A drop onto a zone sets dropEffect "move" → skip.
        if (e.dataTransfer.dropEffect !== "none") return;
        const out =
          e.clientX <= 0 ||
          e.clientY <= 0 ||
          e.clientX >= window.innerWidth ||
          e.clientY >= window.innerHeight;
        if (!out) return;
        void popOutTab(tab.id).catch((err) =>
          console.error("Failed to pop out terminal (drag-out):", err),
        );
      }}
    >
      <div
        className={`w-1.5 h-1.5 rounded-full shrink-0 ${
          state === "needs-input" ? "animate-pulse" : ""
        }`}
        style={{ backgroundColor: STATE_BORDER_COLORS[state] }}
      />
      <span className="text-[10px] text-[#a9b1d6] truncate font-medium">{tab.title}</span>

      {/* Honesty label (Phase 5): interactive PTY shells are non-durable — a
          runner restart ends them. Mirrors the compact-card marker so the
          operator sees it in the full-terminal view too, not only in the
          compact multi-zone overview. */}
      {isNonDurablePty(tab) && (
        <span
          className="flex items-center gap-0.5 shrink-0 text-[8px] text-[#565f89] bg-[#565f89]/15 px-1 py-0 rounded"
          title={NON_DURABLE_TOOLTIP}
          aria-label={`Ephemeral session: ${NON_DURABLE_TOOLTIP}`}
        >
          <Zap className="w-2.5 h-2.5" />
          {NON_DURABLE_LABEL}
        </span>
      )}

      {outputLineCount !== undefined && outputLineCount > 0 && (
        <span className="text-[9px] text-[#565f89] font-mono ml-1 shrink-0">
          {outputLineCount > 999 ? `${(outputLineCount / 1000).toFixed(1)}k` : outputLineCount}{" "}
          lines
          {outputByteSize !== undefined && outputByteSize > 1024 && (
            <span className="ml-1">
              (
              {outputByteSize > 1048576
                ? `${(outputByteSize / 1048576).toFixed(1)}MB`
                : `${(outputByteSize / 1024).toFixed(0)}KB`}
              )
            </span>
          )}
        </span>
      )}

      {onSetZoneLabel &&
        (editingLabel ? (
          <input
            autoFocus
            value={labelValue}
            onChange={(e) => setLabelValue(e.target.value)}
            onBlur={() => {
              setEditingLabel(false);
              onSetZoneLabel(labelValue.trim());
            }}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") {
                setEditingLabel(false);
                onSetZoneLabel(labelValue.trim());
              }
              if (e.key === "Escape") {
                setEditingLabel(false);
                setLabelValue(zoneLabel ?? "");
              }
            }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            className="bg-transparent border-b border-[#565f89] text-[9px] text-[#bb9af7] outline-hidden w-16 shrink-0"
            placeholder="Label..."
          />
        ) : zoneLabel ? (
          <span
            className="text-[9px] text-[#bb9af7] bg-[#bb9af7]/10 px-1 py-0 rounded cursor-pointer hover:bg-[#bb9af7]/20 transition-colors shrink-0"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setLabelValue(zoneLabel);
              setEditingLabel(true);
            }}
            title="Double-click to edit"
          >
            {zoneLabel}
          </span>
        ) : (
          <span
            className="text-[9px] text-[#565f89]/30 cursor-pointer hover:text-[#565f89] transition-colors shrink-0"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setLabelValue("");
              setEditingLabel(true);
            }}
            title="Double-click to add label"
          >
            +
          </span>
        ))}

      {onAssignTab && allTabs.length > 1 && (
        <div className="relative shrink-0" ref={selectorRef}>
          <button
            onClick={(e) => {
              e.stopPropagation();
              setShowSelector(!showSelector);
            }}
            className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
            title="Switch session in this zone"
          >
            <ChevronDown className="w-2.5 h-2.5" />
          </button>

          {showSelector && (
            <div className="absolute left-0 top-full mt-0.5 w-48 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
              <div className="max-h-48 overflow-y-auto scrollbar-dark">
                {allTabs.map((t) => {
                  const isCurrent = t.id === tab.id;
                  const assignedZone = Object.entries(assignments).find(([, id]) => id === t.id);
                  const tabState = sessionStates[t.id] ?? "idle";

                  return (
                    <button
                      key={t.id}
                      onClick={(e) => {
                        e.stopPropagation();
                        onAssignTab(zoneIndex, t.id);
                        setShowSelector(false);
                      }}
                      className={`w-full flex items-center gap-1.5 px-2 py-1 text-left transition-colors ${
                        isCurrent
                          ? "bg-[#7aa2f7]/10 text-[#7aa2f7]"
                          : "text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                      }`}
                    >
                      <div
                        className={`w-1.5 h-1.5 rounded-full shrink-0 ${tabState === "needs-input" ? "animate-pulse" : ""}`}
                        style={{ backgroundColor: STATE_BORDER_COLORS[tabState] }}
                      />
                      <span className="text-[10px] truncate flex-1">{t.title}</span>
                      {assignedZone && !isCurrent && (
                        <span className="text-[8px] text-[#565f89]">
                          Z{Number(assignedZone[0]) + 1}
                        </span>
                      )}
                      {!assignedZone && (
                        <span className="text-[8px] text-[#565f89] italic">hidden</span>
                      )}
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      )}

      {onTogglePin && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onTogglePin();
          }}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            isPinned
              ? "text-[#7aa2f7] bg-[#7aa2f7]/15"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
          }`}
          title={
            isPinned
              ? "Unpin zone (show compact in auto mode)"
              : "Pin zone (keep full terminal in auto mode)"
          }
        >
          <Pin className="w-2.5 h-2.5" />
        </button>
      )}

      {onToggleFilter && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleFilter();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            filterActive
              ? "text-[#e0af68] bg-[#e0af68]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title="Filter output"
        >
          <Filter className="w-2.5 h-2.5" />
        </button>
      )}

      {onScrollToBottom && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onScrollToBottom();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
          title="Scroll to bottom"
        >
          <ArrowDownToLine className="w-2.5 h-2.5" />
        </button>
      )}

      {tab.workingDir && (
        <span className="text-[9px] text-[#565f89] truncate ml-auto pointer-events-none">
          {tab.workingDir.split(/[/\\]/).pop()}
        </span>
      )}
    </div>
  );
}
