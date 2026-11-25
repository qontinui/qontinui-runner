import { useEffect, useRef, useCallback, type Dispatch, type SetStateAction } from "react";
import { X, SquareArrowOutUpRight } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";
import { keyboardReorderTarget, type TerminalPageConfig } from "./useTerminalPages";

/**
 * One page tab of the terminal page strip (plan
 * `2026-11-23-runner-terminal-page-popout-ui-bridge-and-resync-lag`, D1).
 *
 * The tab is a `presentation` wrapper holding three SIBLING buttons — the
 * `role="tab"` button, the pop-out button and the close button. The pop-out and
 * close controls used to be `<span role="button">` nested INSIDE the tab
 * `<button>`: invalid content, and a declared role that neither assistive
 * technology nor the UI Bridge could reliably reach as a button.
 *
 * Every button registers with the UI Bridge under an id keyed on the PAGE ID,
 * never on position or name, so an address survives a reorder and a rename:
 * {@link pageTabElementId}, {@link pageTabPopOutElementId},
 * {@link pageTabCloseElementId}. Each also carries `data-testid` equal to its id,
 * the same insurance `SessionInfoDropdown`'s `InfoRow` uses against losing the
 * registry entry and falling back to a positional id.
 *
 * Its own component so the registrations are plain unconditional hook calls
 * (hooks cannot run inside the strip's `pages.map`). A registration only lands
 * when its ref attaches, so an action button that does not render (no
 * `onPopOut`, or the last page's close) registers nothing.
 */

export const pageTabElementId = (pageId: string) => `terminal-page-tab-${pageId}`;
export const pageTabPopOutElementId = (pageId: string) => `terminal-page-tab-popout-${pageId}`;
export const pageTabCloseElementId = (pageId: string) => `terminal-page-tab-close-${pageId}`;
export const PINNED_PAGE_TITLE_ELEMENT_ID = "terminal-page-pinned-title";
export const PAGE_RENAME_INPUT_ELEMENT_ID = "terminal-page-rename-input";

/**
 * The action buttons rest invisible (so the strip looks unchanged to a mouse
 * user) and show on hover, on focus anywhere in the tab, and on their own
 * keyboard focus — a keyboard user and a focused automation target can see
 * what they are about to activate.
 */
export const PAGE_TAB_ACTION_VISIBILITY =
  "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100";

export interface PageTabProps {
  page: TerminalPageConfig;
  /** The full strip order, for the Alt+Arrow `keyboardReorderTarget`. */
  pages: TerminalPageConfig[];
  isActive: boolean;
  /** The tab currently being dragged, and the tab hovered as a drop target. */
  draggedId: string | null;
  dragOverId: string | null;
  setDraggedId: Dispatch<SetStateAction<string | null>>;
  setDragOverId: Dispatch<SetStateAction<string | null>>;
  onSelect: (id: string) => void;
  onStartRename: (id: string, currentName: string) => void;
  /** Move `sourceId` before `targetId` (`null` = to the end). Absent = no reordering. */
  onReorder?: (sourceId: string, targetId: string | null) => void;
  /** Detach this page into its own window. Absent = no pop-out button. */
  onPopOut?: (pageId: string) => void;
  /** Close this page. Absent = no close button (the strip omits it for the last page). */
  onRemove?: (pageId: string) => void;
}

export function PageTab({
  page,
  pages,
  isActive,
  draggedId,
  dragOverId,
  setDraggedId,
  setDragOverId,
  onSelect,
  onStartRename,
  onReorder,
  onPopOut,
  onRemove,
}: PageTabProps) {
  const tabId = pageTabElementId(page.id);
  const popOutId = pageTabPopOutElementId(page.id);
  const closeId = pageTabCloseElementId(page.id);
  const { ref: tabRef } = useUIElement({ id: tabId, type: "tab", label: page.name });
  const { ref: popOutRef } = useUIElement({
    id: popOutId,
    type: "button",
    label: `Pop out ${page.name}`,
  });
  const { ref: closeRef } = useUIElement({
    id: closeId,
    type: "button",
    label: `Close ${page.name}`,
  });

  const isDragging = draggedId === page.id;
  // Only the tab under the pointer — not the one being dragged — shows the
  // drop-target highlight (dragging a tab over itself is a no-op).
  const isDropTarget = !!onReorder && dragOverId === page.id && draggedId !== page.id;
  const hasActions = !!onPopOut || !!onRemove;

  // The wrapper is the drop target and carries the visual tab state, so a drop
  // anywhere on the tab — the name or either action button — reorders.
  const wrapperClasses = `group shrink-0 flex items-center gap-0.5 rounded text-[11px] transition-colors ${
    hasActions ? "pr-1" : ""
  } ${isActive ? "bg-[#2a2d3d]" : "hover:bg-[#1e1f2e]"} ${isDragging ? "opacity-40" : ""} ${
    isDropTarget ? "ring-1 ring-inset ring-[#7aa2f7]" : ""
  }`;
  const tabClasses = `shrink-0 flex items-center py-1 pl-2.5 ${
    hasActions ? "pr-0.5" : "pr-2.5"
  } rounded cursor-pointer transition-colors focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-[#7aa2f7] ${
    isActive ? "text-[#c0caf5]" : "text-[#565f89] group-hover:text-[#a9b1d6]"
  }`;

  return (
    <div
      role="presentation"
      className={wrapperClasses}
      onDragOver={(e) => {
        if (!onReorder || !draggedId || draggedId === page.id) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        if (dragOverId !== page.id) setDragOverId(page.id);
      }}
      onDragLeave={() => {
        setDragOverId((current) => (current === page.id ? null : current));
      }}
      onDrop={(e) => {
        e.preventDefault();
        const sourceId = draggedId ?? e.dataTransfer.getData("text/plain");
        setDraggedId(null);
        setDragOverId(null);
        if (onReorder && sourceId && sourceId !== page.id) {
          onReorder(sourceId, page.id);
        }
      }}
    >
      <button
        ref={tabRef}
        data-testid={tabId}
        role="tab"
        aria-selected={isActive}
        className={tabClasses}
        draggable={!!onReorder}
        onClick={() => onSelect(page.id)}
        onDoubleClick={() => onStartRename(page.id, page.name)}
        onDragStart={(e) => {
          setDraggedId(page.id);
          e.dataTransfer.effectAllowed = "move";
          // Firefox requires data to be set for the drag to start at all.
          e.dataTransfer.setData("text/plain", page.id);
        }}
        onDragEnd={() => {
          setDraggedId(null);
          setDragOverId(null);
        }}
        onKeyDown={(e) => {
          // Alt+Arrow reorder: the non-dragging alternative WCAG 2.5.7
          // requires for a drag-only interaction. Swaps the focused tab
          // with its immediate neighbor; focus follows it (React moves
          // rather than remounts a same-key element across a reorder).
          // The index arithmetic lives in `keyboardReorderTarget`
          // (unit-tested directly — see its doc comment for why).
          if (!onReorder || !e.altKey) return;
          if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
          e.preventDefault();
          const target = keyboardReorderTarget(
            pages,
            page.id,
            e.key === "ArrowLeft" ? "left" : "right",
          );
          if (target !== undefined) {
            onReorder(page.id, target);
            // React moves (not remounts) this element, so after the
            // reorder commits it is still the focused tab: keep it in view.
            const moved = e.currentTarget;
            requestAnimationFrame(() =>
              moved.scrollIntoView?.({ block: "nearest", inline: "nearest" }),
            );
          }
        }}
        title={`Switch to ${page.name}${onReorder ? " (drag, or Alt+←/→, to reorder)" : ""}`}
      >
        <span className="truncate max-w-[120px]">{page.name}</span>
      </button>
      {onPopOut && (
        <button
          ref={popOutRef}
          data-testid={popOutId}
          type="button"
          onClick={() => onPopOut(page.id)}
          aria-label={`Pop out ${page.name} into its own window`}
          title="Pop out this page into its own window"
          className={`p-0.5 rounded ${PAGE_TAB_ACTION_VISIBILITY} text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-[#7aa2f7] transition-all`}
        >
          <SquareArrowOutUpRight className="w-3 h-3" />
        </button>
      )}
      {onRemove && (
        <button
          ref={closeRef}
          data-testid={closeId}
          type="button"
          onClick={() => onRemove(page.id)}
          aria-label={`Close ${page.name}`}
          title="Close page"
          className={`p-0.5 rounded ${PAGE_TAB_ACTION_VISIBILITY} text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-[#f7768e] transition-all`}
        >
          <X className="w-3 h-3" />
        </button>
      )}
    </div>
  );
}

/**
 * The page-pinned pop-out window's title: the only render of the page name in
 * that window, so it is the observable for "a rename reached the pinned
 * window". Registered as {@link PINNED_PAGE_TITLE_ELEMENT_ID} with the name as
 * its label.
 */
export function PinnedPageTitle({ name }: { name: string }) {
  const { ref } = useUIElement({ id: PINNED_PAGE_TITLE_ELEMENT_ID, type: "generic", label: name });
  return (
    <span
      ref={ref}
      data-testid={PINNED_PAGE_TITLE_ELEMENT_ID}
      className="text-[11px] text-[#c0caf5] truncate"
    >
      {name}
    </span>
  );
}

export interface PageRenameInputProps {
  value: string;
  error: string | null;
  onChange: (value: string) => void;
  /** Enter commits and keeps the input open on a refused name; blur reverts. */
  onCommit: (onRefused: "keep-open" | "revert") => void;
  onCancel: () => void;
}

/**
 * The inline rename input for the tab being edited, registered as
 * {@link PAGE_RENAME_INPUT_ELEMENT_ID} so an agent can type a new name through
 * the UI Bridge. At most one renders at a time. Focuses and selects its text on
 * mount.
 */
export function PageRenameInput({
  value,
  error,
  onChange,
  onCommit,
  onCancel,
}: PageRenameInputProps) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const { ref: bridgeRef } = useUIElement({
    id: PAGE_RENAME_INPUT_ELEMENT_ID,
    type: "input",
    label: "Page name",
  });
  // Stable merged ref: an inline callback would detach and re-attach (and so
  // unregister and re-register) on every keystroke.
  const setRefs = useCallback(
    (el: HTMLInputElement | null) => {
      inputRef.current = el;
      bridgeRef(el);
    },
    [bridgeRef],
  );

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  return (
    <>
      <input
        ref={setRefs}
        data-testid={PAGE_RENAME_INPUT_ELEMENT_ID}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => onCommit("revert")}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") onCommit("keep-open");
          if (e.key === "Escape") onCancel();
        }}
        aria-invalid={error !== null}
        aria-label="Page name"
        className={`bg-[#13141f] border rounded px-1 py-0 text-[11px] text-[#c0caf5] outline-hidden w-24 ${
          error ? "border-[#f7768e]" : "border-[#7aa2f7]"
        }`}
        maxLength={30}
      />
      {error && (
        <span role="alert" className="text-[10px] text-[#f7768e] whitespace-nowrap">
          {error}
        </span>
      )}
    </>
  );
}
