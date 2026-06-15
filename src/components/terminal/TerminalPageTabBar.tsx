import { useState, useRef, useEffect } from "react";
import { Plus, X, Shuffle, SquareArrowOutUpRight } from "lucide-react";
import type { TerminalPageConfig } from "./useTerminalPages";

interface TerminalPageTabBarProps {
  pages: TerminalPageConfig[];
  activePageId: string;
  onSelectPage: (id: string) => void;
  onAddPage: (name: string) => void;
  onRemovePage: (id: string) => void;
  onRenamePage: (id: string, name: string) => void;
  onReorganize?: () => void;
  /** Open a new pop-out OS window (same process) hosting its own terminals. */
  onPopOut?: () => void;
  /** Detach an entire page (all its terminals + layout) into its own window. */
  onPopOutPage?: (pageId: string) => void;
  /**
   * True in a page-pinned pop-out window — it shows ONE fixed page, so render a
   * minimal static title bar instead of the interactive multi-page tab strip.
   */
  isPinned?: boolean;
}

export function TerminalPageTabBar({
  pages,
  activePageId,
  onSelectPage,
  onAddPage,
  onRemovePage,
  onRenamePage,
  onReorganize,
  onPopOut,
  onPopOutPage,
  isPinned,
}: TerminalPageTabBarProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editingId && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [editingId]);

  const startRename = (id: string, currentName: string) => {
    setEditingId(id);
    setEditValue(currentName);
  };

  const commitRename = () => {
    if (editingId && editValue.trim()) {
      onRenamePage(editingId, editValue.trim());
    }
    setEditingId(null);
    setEditValue("");
  };

  const handleAdd = () => {
    const name = `Page ${pages.length + 1}`;
    onAddPage(name);
  };

  // Page-pinned pop-out window: one fixed page, no tab strip — just a label.
  if (isPinned) {
    const page = pages[0];
    return (
      <div className="flex items-center gap-2 px-3 py-1 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
        <SquareArrowOutUpRight className="w-3 h-3 text-[#7aa2f7]" />
        <span className="text-[11px] text-[#c0caf5] truncate">{page?.name ?? "Terminal"}</span>
        <span className="text-[9px] text-[#565f89] uppercase tracking-wider">detached page</span>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-0.5 px-2 py-1 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
      {pages.map((page) => {
        const isActive = page.id === activePageId;
        const isEditing = editingId === page.id;

        const tabClasses = `group flex items-center gap-1 px-2.5 py-1 rounded text-[11px] cursor-pointer transition-colors ${
          isActive
            ? "bg-[#2a2d3d] text-[#c0caf5]"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1e1f2e]"
        }`;

        // Render a <div> when editing (input inside button is invalid HTML)
        if (isEditing) {
          return (
            <div key={page.id} className={tabClasses}>
              <input
                ref={inputRef}
                value={editValue}
                onChange={(e) => setEditValue(e.target.value)}
                onBlur={commitRename}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") commitRename();
                  if (e.key === "Escape") {
                    setEditingId(null);
                    setEditValue("");
                  }
                }}
                className="bg-[#13141f] border border-[#7aa2f7] rounded px-1 py-0 text-[11px] text-[#c0caf5] outline-hidden w-24"
                maxLength={30}
              />
            </div>
          );
        }

        return (
          <button
            key={page.id}
            role="tab"
            aria-selected={isActive}
            className={tabClasses}
            onClick={() => onSelectPage(page.id)}
            onDoubleClick={() => startRename(page.id, page.name)}
            title={`Switch to ${page.name}`}
          >
            <span className="truncate max-w-[120px]">{page.name}</span>
            {onPopOutPage && (
              <span
                role="button"
                tabIndex={0}
                onClick={(e) => {
                  e.stopPropagation();
                  onPopOutPage(page.id);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.stopPropagation();
                    onPopOutPage(page.id);
                  }
                }}
                className="p-0.5 rounded opacity-0 group-hover:opacity-100 text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-all"
                title="Pop out this page into its own window"
              >
                <SquareArrowOutUpRight className="w-3 h-3" />
              </span>
            )}
            {pages.length > 1 && (
              <span
                role="button"
                tabIndex={0}
                onClick={(e) => {
                  e.stopPropagation();
                  onRemovePage(page.id);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.stopPropagation();
                    onRemovePage(page.id);
                  }
                }}
                className="p-0.5 rounded opacity-0 group-hover:opacity-100 text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 transition-all"
                title="Close page"
              >
                <X className="w-3 h-3" />
              </span>
            )}
          </button>
        );
      })}
      <button
        onClick={handleAdd}
        aria-label="Add terminal page"
        className="flex items-center gap-0.5 px-1.5 py-1 rounded text-[10px] text-[#565f89] hover:text-[#9ece6a] hover:bg-[#9ece6a]/10 transition-colors"
        title="Add terminal page"
      >
        <Plus className="w-3 h-3" />
      </button>
      {pages.length >= 2 && onReorganize && (
        <button
          onClick={onReorganize}
          className="flex items-center gap-0.5 px-1.5 py-1 rounded text-[10px] text-[#565f89] hover:text-[#bb9af7] hover:bg-[#bb9af7]/10 transition-colors"
          title="Reorganize pages by topic (AI)"
        >
          <Shuffle className="w-3 h-3" />
        </button>
      )}
      {onPopOut && (
        <button
          onClick={onPopOut}
          aria-label="Open terminal in a new window"
          className="flex items-center gap-0.5 px-1.5 py-1 rounded text-[10px] text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors"
          title="Open a new pop-out terminal window"
        >
          <SquareArrowOutUpRight className="w-3 h-3" />
        </button>
      )}
    </div>
  );
}
