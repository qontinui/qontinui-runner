import { useState, useRef, useEffect } from "react";
import { Plus, Shuffle, SquareArrowOutUpRight, BrushCleaning } from "lucide-react";
import type { TerminalPageConfig } from "./useTerminalPages";
import { PageRenameInput, PageTab, PinnedPageTitle } from "./PageTab";
import { NAME_IN_USE } from "./pageNaming";
import { pruneNoticeText, type PageRename } from "./pagePrune";

/**
 * Sentinel `dragOverId` value for the drop zone AFTER the last tab (the
 * "move to end" gesture) — distinct from any real page id.
 */
const END_ZONE_ID = "__terminal-tab-end-zone__";

interface TerminalPageTabBarProps {
  pages: TerminalPageConfig[];
  activePageId: string;
  onSelectPage: (id: string) => void;
  /**
   * Add a page. Called with NO name: the page hook names it against the full
   * roster (detached pages included), which this strip cannot see.
   */
  onAddPage: () => void;
  onRemovePage: (id: string) => void;
  /**
   * Rename a page. Returning `false` means the name was refused (another page
   * uses it, or it is the default page's reserved "Terminal"): the input stays
   * open and shows {@link NAME_IN_USE}.
   */
  onRenamePage: (id: string, name: string) => boolean | void;
  /**
   * Drag-to-reorder (and its Alt+Arrow keyboard equivalent — WCAG 2.5.7
   * requires a non-dragging alternative to a drag-only gesture): move
   * `sourceId` to sit immediately before `targetId`, or to the end of the
   * list when `targetId` is `null`.
   */
  onReorderPage?: (sourceId: string, targetId: string | null) => void;
  onReorganize?: () => void;
  /** Close every page that hosts no terminals. Resolves to how many were closed. */
  onCloseEmptyPages?: () => Promise<number | null>;
  /** Last-known count of tabs the button would close; 0 disables it. Omitted = unknown, stays enabled. */
  emptyPageCount?: number;
  /**
   * The last auto-prune of empty auto-created pages and/or the one-time name
   * repair, while its Undo window is open (the page hook times it out). Shown
   * in the note slot; `null`/omitted shows nothing.
   */
  pruneNotice?: { removed: readonly unknown[]; renames: readonly PageRename[] } | null;
  /** Put back the pages `pruneNotice` removed. */
  onUndoPrune?: () => void;
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
  onReorderPage,
  onReorganize,
  onCloseEmptyPages,
  emptyPageCount,
  pruneNotice,
  onUndoPrune,
  onPopOut,
  onPopOutPage,
  isPinned,
}: TerminalPageTabBarProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [renameError, setRenameError] = useState<string | null>(null);
  const [closingEmpty, setClosingEmpty] = useState(false);
  const [closeEmptyNote, setCloseEmptyNote] = useState<string | null>(null);

  const handleCloseEmpty = async () => {
    if (!onCloseEmptyPages || closingEmpty) return;
    setClosingEmpty(true);
    try {
      const n = await onCloseEmptyPages();
      // null = occupancy could not be fully read (a source failed, or coord's
      // drain withheld the restorable set): UNKNOWN, never "no empty tabs".
      setCloseEmptyNote(
        n === null
          ? "Couldn't verify which tabs are empty"
          : n > 0
            ? `Closed ${n} empty`
            : "No empty tabs",
      );
    } catch {
      setCloseEmptyNote("Couldn't close empty tabs");
    } finally {
      setClosingEmpty(false);
      setTimeout(() => setCloseEmptyNote(null), 3000);
    }
  };

  // Drag-to-reorder state: the tab currently being dragged, and the tab it's
  // currently hovering over (drop target). Both are cleared on drop/dragend
  // so a stale highlight never survives a completed or abandoned drag.
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [dragOverId, setDragOverId] = useState<string | null>(null);

  // The tab list scrolls horizontally once the pages outgrow the strip (the
  // controls after it stay pinned), so keep the active tab in view.
  // `scrollIntoView` is optional-called because jsdom does not implement it.
  // Keyed on the active tab's POSITION (not the whole order), so moving the
  // active tab re-scrolls but reordering some other tab does not yank the strip
  // back to the active one; the Alt+Arrow handler scrolls the moved tab itself.
  const tabListRef = useRef<HTMLDivElement>(null);
  const activeIndex = pages.findIndex((p) => p.id === activePageId);
  useEffect(() => {
    const active = tabListRef.current?.querySelector<HTMLElement>('[aria-selected="true"]');
    active?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [activePageId, activeIndex]);

  const startRename = (id: string, currentName: string) => {
    setEditingId(id);
    setEditValue(currentName);
    setRenameError(null);
  };

  const cancelRename = () => {
    setEditingId(null);
    setEditValue("");
    setRenameError(null);
  };

  /**
   * Commit the edit. A refused name never commits a second tab with the same
   * label: on Enter the input stays open with the reason; on blur (the
   * operator clicked away) the edit is dropped and the old name kept, so the
   * input cannot trap focus-less on the strip.
   */
  const commitRename = (onRefused: "keep-open" | "revert") => {
    if (editingId && editValue.trim()) {
      if (onRenamePage(editingId, editValue.trim()) === false && onRefused === "keep-open") {
        setRenameError(NAME_IN_USE);
        return;
      }
    }
    cancelRename();
  };

  // No name: the page hook owns naming (see `onAddPage`).
  const handleAdd = () => onAddPage();

  // Page-pinned pop-out window: one fixed page, no tab strip — just a label.
  if (isPinned) {
    const page = pages[0];
    return (
      <div className="flex items-center gap-2 px-3 py-1 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
        <SquareArrowOutUpRight className="w-3 h-3 text-[#7aa2f7]" />
        <PinnedPageTitle name={page?.name ?? "Terminal"} />
        <span className="text-[9px] text-[#565f89] uppercase tracking-wider">detached page</span>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-0.5 px-2 py-0.5 bg-[#13141f] border-b border-[#2a2d3d] shrink-0 min-w-0">
      {/* Tabs scroll; the controls below are pinned. Without this split the
          row simply overflowed, and past ~17 pages every control (add,
          reorganize, close-empty, pop-out) rendered beyond the viewport. */}
      <div
        ref={tabListRef}
        data-testid="terminal-page-tab-list"
        className="flex items-center gap-0.5 min-w-0 overflow-x-auto overflow-y-hidden p-0.5"
      >
        {pages.map((page) => {
          const isActive = page.id === activePageId;

          // Render a <div> when editing (an input inside a button is invalid HTML)
          if (editingId === page.id) {
            return (
              <div
                key={page.id}
                className={`shrink-0 flex items-center gap-1 px-2.5 py-1 rounded text-[11px] ${
                  isActive ? "bg-[#2a2d3d] text-[#c0caf5]" : "text-[#565f89]"
                }`}
              >
                <PageRenameInput
                  value={editValue}
                  error={renameError}
                  onChange={(value) => {
                    setEditValue(value);
                    setRenameError(null);
                  }}
                  onCommit={commitRename}
                  onCancel={cancelRename}
                />
              </div>
            );
          }

          return (
            <PageTab
              key={page.id}
              page={page}
              pages={pages}
              isActive={isActive}
              draggedId={draggedId}
              dragOverId={dragOverId}
              setDraggedId={setDraggedId}
              setDragOverId={setDragOverId}
              onSelect={onSelectPage}
              onStartRename={startRename}
              onReorder={onReorderPage}
              onPopOut={onPopOutPage}
              onRemove={pages.length > 1 ? onRemovePage : undefined}
            />
          );
        })}
        {onReorderPage && draggedId && (
          // "Move to end" drop zone: only a per-tab `onDrop` exists otherwise,
          // so a tab could never become the LAST tab (there's nothing after it
          // to drop "before"). Rendered only mid-drag so it costs no layout
          // space the rest of the time.
          <div
            onDragOver={(e) => {
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              if (dragOverId !== END_ZONE_ID) setDragOverId(END_ZONE_ID);
            }}
            onDragLeave={() => {
              setDragOverId((current) => (current === END_ZONE_ID ? null : current));
            }}
            onDrop={(e) => {
              e.preventDefault();
              const sourceId = draggedId ?? e.dataTransfer.getData("text/plain");
              setDraggedId(null);
              setDragOverId(null);
              if (sourceId) onReorderPage(sourceId, null);
            }}
            className={`self-stretch shrink-0 w-2 rounded transition-colors ${
              dragOverId === END_ZONE_ID ? "bg-[#7aa2f7]/30 ring-1 ring-inset ring-[#7aa2f7]" : ""
            }`}
            aria-hidden="true"
          />
        )}
      </div>
      <div data-testid="terminal-page-tab-controls" className="flex items-center gap-0.5 shrink-0">
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
        {pages.length >= 2 && onCloseEmptyPages && (
          <button
            onClick={() => void handleCloseEmpty()}
            disabled={closingEmpty || emptyPageCount === 0}
            aria-label="Close empty tabs"
            className="flex items-center gap-0.5 px-1.5 py-1 rounded text-[10px] text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 transition-colors disabled:opacity-50"
            title={
              emptyPageCount === 0
                ? "No empty tabs to close"
                : emptyPageCount === undefined
                  ? "Close empty tabs (tabs that have no terminals open)"
                  : `Close ${emptyPageCount} empty tab${emptyPageCount === 1 ? "" : "s"} (no terminals open)`
            }
          >
            <BrushCleaning className="w-3 h-3" />
          </button>
        )}
        {closeEmptyNote && (
          <span role="status" className="text-[10px] text-[#565f89] px-1">
            {closeEmptyNote}
          </span>
        )}
        {pruneNotice && (pruneNotice.removed.length > 0 || pruneNotice.renames.length > 0) && (
          <span
            role="status"
            data-testid="terminal-page-prune-notice"
            className="flex items-center gap-1 text-[10px] text-[#a9b1d6] px-1 min-w-0"
          >
            <span
              className="truncate max-w-[360px]"
              title={pruneNoticeText(pruneNotice.removed.length, pruneNotice.renames)}
            >
              {pruneNoticeText(pruneNotice.removed.length, pruneNotice.renames)}
            </span>
            {pruneNotice.removed.length > 0 && onUndoPrune && (
              <button
                onClick={onUndoPrune}
                className="shrink-0 px-1 rounded text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors"
                title="Put the removed pages back"
              >
                Undo
              </button>
            )}
          </span>
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
    </div>
  );
}
