import { useState, useRef, useEffect, type ReactNode } from "react";
import { Terminal, X, Plus, List, ChevronDown } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState } from "./useZoneLayout";

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  onRename: (id: string, title: string) => void;
  sessionStates?: Record<string, SessionState>;
  layoutPicker?: ReactNode;
  statusSummary?: { needsInput: number; working: number; errors: number; unseen?: number };
  onQuickLaunch?: (count: number, autoCommand?: string) => void;
}

const STATE_DOT_COLORS: Record<SessionState, string> = {
  idle: "bg-[#565f89]",
  working: "bg-[#7aa2f7]",
  "needs-input": "bg-[#e0af68]",
  completed: "bg-[#9ece6a]",
  error: "bg-[#f7768e]",
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

export function TerminalTabBar({
  tabs,
  activeId,
  onSelect,
  onClose,
  onCreate,
  onRename,
  sessionStates,
  layoutPicker,
  statusSummary,
  onQuickLaunch,
}: TerminalTabBarProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [showDropdown, setShowDropdown] = useState(false);
  const [showQuickLaunch, setShowQuickLaunch] = useState(false);
  const [quickLaunchCmd, setQuickLaunchCmd] = useState("");
  const dropdownRef = useRef<HTMLDivElement>(null);
  const quickLaunchRef = useRef<HTMLDivElement>(null);

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
    if (!showDropdown && !showQuickLaunch) return;
    const handleClick = (e: MouseEvent) => {
      if (showDropdown && dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setShowDropdown(false);
      }
      if (
        showQuickLaunch &&
        quickLaunchRef.current &&
        !quickLaunchRef.current.contains(e.target as Node)
      ) {
        setShowQuickLaunch(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showDropdown, showQuickLaunch]);

  return (
    <div className="flex items-center gap-0.5 bg-[#13141f] border-b border-[#2a2d3d] px-1 h-9 shrink-0 overflow-x-auto scrollbar-none">
      {/* Layout picker */}
      {layoutPicker && <div className="shrink-0 mr-1">{layoutPicker}</div>}

      {tabs.map((tab) => {
        const isActive = tab.id === activeId;
        const isDead = !tab.isAlive;
        const isEditing = editingId === tab.id;
        const state = sessionStates?.[tab.id] ?? "idle";

        return (
          <button
            key={tab.id}
            onClick={() => onSelect(tab.id)}
            onDoubleClick={() => startEditing(tab)}
            onAuxClick={(e) => {
              // Middle-click to close
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
              transition-colors whitespace-nowrap max-w-[200px] group cursor-grab active:cursor-grabbing
              ${
                isActive
                  ? "bg-[#1a1b26] text-[#c0caf5] border-t border-x border-[#2a2d3d] -mb-px"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
              }
              ${isDead ? "opacity-60" : ""}
            `}
          >
            {/* Session state dot (replaces plain terminal icon in multi-zone) */}
            {sessionStates ? (
              <div
                className={`w-2 h-2 rounded-full shrink-0 ${STATE_DOT_COLORS[state]} ${
                  state === "needs-input" ? "animate-pulse" : ""
                }`}
              />
            ) : (
              <Terminal className="w-3 h-3 shrink-0" />
            )}
            <span className="flex flex-col items-start min-w-0">
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
                  className="bg-transparent border-b border-[#565f89] text-[#c0caf5] text-xs w-20 outline-none"
                />
              ) : (
                <span className="truncate">{tab.title}</span>
              )}
              {isActive && tab.workingDir && (
                <span className="text-[9px] text-[#565f89] truncate max-w-[140px]">
                  {tab.workingDir.split(/[/\\]/).slice(-2).join("/")}
                </span>
              )}
            </span>
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

      <button
        onClick={onCreate}
        className="flex items-center justify-center w-6 h-6 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50 transition-colors ml-1 shrink-0"
        title="New terminal (Ctrl+Shift+T)"
      >
        <Plus className="w-3.5 h-3.5" />
      </button>

      {/* Quick-launch dropdown */}
      {onQuickLaunch && (
        <div className="relative shrink-0" ref={quickLaunchRef}>
          <button
            onClick={() => setShowQuickLaunch(!showQuickLaunch)}
            className={`flex items-center justify-center w-5 h-6 rounded transition-colors ${
              showQuickLaunch
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
            }`}
            title="Launch multiple terminals"
          >
            <ChevronDown className="w-3 h-3" />
          </button>
          {showQuickLaunch && (
            <div className="absolute left-0 top-full mt-1 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden min-w-[200px]">
              <div className="px-3 py-1.5 text-[9px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
                Quick Launch
              </div>
              <div className="px-3 py-1.5 border-b border-[#2a2d3d]">
                <input
                  value={quickLaunchCmd}
                  onChange={(e) => setQuickLaunchCmd(e.target.value)}
                  onKeyDown={(e) => e.stopPropagation()}
                  placeholder="Auto-run command (e.g. claude)"
                  className="w-full bg-[#13141f] border border-[#2a2d3d] rounded px-2 py-1 text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-none focus:border-[#7aa2f7] transition-colors"
                />
              </div>
              {[2, 4, 6, 9].map((count) => (
                <button
                  key={count}
                  onClick={() => {
                    onQuickLaunch(count, quickLaunchCmd.trim() || undefined);
                    setShowQuickLaunch(false);
                  }}
                  className="w-full text-left px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#7aa2f7]/10 transition-colors"
                >
                  Launch {count} terminals
                  {quickLaunchCmd.trim() ? ` + "${quickLaunchCmd.trim()}"` : ""}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Status summary (multi-zone only) */}
      {statusSummary && (statusSummary.needsInput > 0 || statusSummary.errors > 0) && (
        <div className="flex items-center gap-2 ml-2 shrink-0 text-[10px]">
          {statusSummary.needsInput > 0 && (
            <span className="flex items-center gap-1 text-[#e0af68]">
              <span className="w-1.5 h-1.5 rounded-full bg-[#e0af68] animate-pulse" />
              {statusSummary.needsInput} waiting
              {statusSummary.unseen && statusSummary.unseen > 0 ? (
                <span className="px-1 py-0 rounded-full bg-[#f7768e] text-[#1a1b26] text-[9px] font-bold leading-tight min-w-[14px] text-center">
                  {statusSummary.unseen}
                </span>
              ) : null}
            </span>
          )}
          {statusSummary.errors > 0 && (
            <span className="flex items-center gap-1 text-[#f7768e]">
              <span className="w-1.5 h-1.5 rounded-full bg-[#f7768e]" />
              {statusSummary.errors} error{statusSummary.errors !== 1 ? "s" : ""}
            </span>
          )}
        </div>
      )}

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
            <div className="absolute right-0 top-full mt-1 w-72 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
              <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
                Sessions ({tabs.length})
              </div>
              <div className="max-h-64 overflow-y-auto scrollbar-dark">
                {tabs.map((tab) => {
                  const isActive = tab.id === activeId;
                  const isDead = !tab.isAlive;
                  const state = sessionStates?.[tab.id] ?? "idle";
                  return (
                    <div
                      key={tab.id}
                      onClick={() => {
                        onSelect(tab.id);
                        setShowDropdown(false);
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
  );
}
