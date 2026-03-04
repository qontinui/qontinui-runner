import { useState, useRef, useEffect } from "react";
import { Terminal, X, Plus, List } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  onRename: (id: string, title: string) => void;
}

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
}: TerminalTabBarProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [showDropdown, setShowDropdown] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

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

  // Close dropdown when clicking outside
  useEffect(() => {
    if (!showDropdown) return;
    const handleClick = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setShowDropdown(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showDropdown]);

  return (
    <div className="flex items-center gap-0.5 bg-[#13141f] border-b border-[#2a2d3d] px-1 h-9 shrink-0 overflow-x-auto scrollbar-none">
      {tabs.map((tab) => {
        const isActive = tab.id === activeId;
        const isDead = !tab.isAlive;
        const isEditing = editingId === tab.id;

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
            className={`
              flex items-center gap-1.5 px-3 py-1 rounded-t text-xs font-medium
              transition-colors whitespace-nowrap max-w-[200px] group
              ${
                isActive
                  ? "bg-[#1a1b26] text-[#c0caf5] border-t border-x border-[#2a2d3d] -mb-px"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]/50"
              }
              ${isDead ? "opacity-60" : ""}
            `}
          >
            <Terminal className="w-3 h-3 shrink-0" />
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
                            isDead ? "bg-[#565f89]" : "bg-[#9ece6a]"
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
