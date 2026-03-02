import { useState } from "react";
import { Terminal, X, Plus } from "lucide-react";
import type { TerminalTab } from "./useTerminalManager";

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  onRename: (id: string, title: string) => void;
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
    </div>
  );
}
