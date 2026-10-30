/**
 * Phase 5 — Per-zone hover controls.
 *
 * Five-button cluster pinned to a zone cell's top-right corner that
 * fades in on cell hover (with a 150ms enter delay to avoid flicker on
 * mouse-pass-through). Buttons: maximize/restore, restart, label,
 * export, close.
 *
 * In a zone narrower than `ZONE_HOVER_COLLAPSE_BELOW_PX` (a container query on
 * the cell, `@container/zone`) maximize, restart, label, export and
 * send-to-window collapse behind ONE overflow button whose menu holds each of
 * them, so the header toggles beside the cluster stay clickable. Close (and a
 * remote tab's End button) stay visible: Close carries the
 * `terminal.close-session-<id>` UI Bridge id automation clicks, which must not
 * sit on a `display: none` button. See `headerReserve.ts` for the arithmetic.
 *
 * Wired DIRECTLY to context handlers (`zoneLayout.toggleMaximize`,
 * `transitionEffects.handleRestartInZone`, `labelsAndTags.setZoneLabel`,
 * `closeTerminal`) rather than through `callRegistry`. The registry's
 * Phase-1b handlers ALSO target these same context functions, so both
 * surfaces are structurally in lockstep — putting an async network of
 * registry-routed calls on every hover-button click would only add
 * latency + error-handling without changing the source of truth.
 *
 * State-gated restart (per plan's audit §"Per-action success criterion
 * checks" + `TransitionEffectsContext.tsx:44`): button is greyed +
 * tooltip switches when `sessionStates[tabId] ∉ {completed, error}` so
 * the affordance reflects the underlying handler's silent no-op gate
 * rather than appearing broken on a `working` session.
 *
 * Coexists with `SuggestionChip` (Phase 4) in the same corner — both
 * render at `z-30`. When a chip is present its rule was specifically
 * authored to be the more relevant signal (error, stuck-needs-input,
 * layout-mismatch), so the chip visually dominates that moment. Hover
 * actions are still discoverable by mousing into the cell; they sit
 * just below the chip's pixels via document order. Phase 9 retire of
 * the per-tab badge cluster will tighten this layout further.
 */

import { useCallback, useEffect, useId, useRef, useState } from "react";
import {
  AppWindow,
  Download,
  Maximize2,
  Minimize2,
  MoreHorizontal,
  RefreshCw,
  Tag,
  X,
} from "lucide-react";

import { useTerminalSession, useTransitionEffects, useZoneMetadata } from "./contexts";
import { useWindowAssignments } from "./contexts/WindowAssignmentsContext";
import { useTerminalWindowActions, type RunnerWindowRecord } from "./useTerminalWindowActions";
import { RemoteTabEndAction } from "./RemoteSessionEndFlow";
import {
  ZONE_HOVER_COLLAPSED_ONLY_CLASS,
  ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS,
  ZONE_HOVER_FULL_ONLY_CLASS,
  ZONE_HOVER_FULL_ONLY_REMOTE_CLASS,
} from "./zone-grid/headerReserve";

interface ZoneHoverActionsProps {
  zoneIdx: number;
  /**
   * The zone whose label the label button edits (`useZoneLayout().cosmeticZoneFor`):
   * `zoneIdx` in grid mode, the shown session's grid zone in tmux mode.
   * `null` hides the label button — the session has no zone to label.
   */
  labelZone: number | null;
  /** Threaded from `ZoneGrid`'s prop (sourced from `useZoneActions`).
   *  Optional so the export button can degrade to disabled rather than
   *  throwing when the prop isn't wired (unusual; covers test mounts). */
  onExportZone?: (zoneIdx: number, format: "text" | "markdown" | "json") => void;
  /**
   * Render as a static, always-visible cluster for embedding in a header (the
   * maximized view's title bar) instead of the hover-revealed top-right overlay.
   */
  inline?: boolean;
}

/**
 * Width, in CSS px, the overlay cluster spans from the cell's right edge when it
 * shows `buttons` buttons: `right-1` (4) + border (2) + `px-1` (8), and per
 * button `p-1` + a `w-3` icon (20) with a `gap-0.5` (2) between neighbours.
 * `ZoneLabel` reserves at least this much on hover so the cluster never covers
 * the header's cwd label; keep it in step with the classes below.
 */
export function hoverActionsSpanPx(buttons: number): number {
  return 4 + 2 + 8 + buttons * 20 + Math.max(0, buttons - 1) * 2;
}

/**
 * Every `ZoneLabel` row item that holds its width (`shrink-0`), with that
 * width in CSS px, from its classes:
 * - `stateDot`: `w-1.5`.
 * - `sessionInfo`: the `SessionInfoDropdown` trigger — `p-0.5` (4) + `w-2.5`
 *   icon (10) + `gap-0.5` (2) + a one-character `text-[9px]` mono count (~6) +
 *   `gap-0.5` (2) + `w-2` chevron (8).
 * - `labelSlot`: the "+" add-label chip (~6, `shrink-0`); the label chip that
 *   replaces it truncates down to its `px-1` (8), and the edit input to its
 *   `min-w-6` (24) — the slot is budgeted at the widest floor.
 * - the six icon toggles: `p-0.5` + a `w-2.5` icon = 14 each.
 */
export const HEADER_FIXED_ITEMS_PX = {
  stateDot: 6,
  sessionInfo: 32,
  labelSlot: 24,
  switchSession: 14,
  pin: 14,
  filter: 14,
  prompts: 14,
  review: 14,
  scrollToBottom: 14,
} as const;

/**
 * The row items that truncate, with the width each keeps once truncated: its
 * horizontal padding or margin (`nonDurable` and `reviewBadge` `px-1`,
 * `totalOutput` `ml-1`). Each still takes a row gap.
 */
export const HEADER_TRUNCATING_FLOORS_PX = {
  name: 0,
  position: 0,
  nonDurable: 8,
  totalOutput: 4,
  reviewBadge: 8,
  cwd: 0,
} as const;

/** `ZoneLabel`'s row gap, `gap-1.5`. */
export const HEADER_ROW_GAP_PX = 6;

/**
 * Least width, in CSS px, `ZoneLabel`'s row content needs with every item
 * present and every truncating item at its floor: the fixed items, the floors,
 * and a gap between each pair of items. Excludes the row's padding and the
 * hover reserve; `headerReserve.ts` adds those to get the width below which the
 * hover cluster collapses.
 *
 * Not budgeted: `TenantBadge` (multi-tenant devices only) and
 * `RemoteTabControls` (remote tabs only), whose widths follow unbounded text.
 */
export function headerControlsMinPx(): number {
  const widths: number[] = [
    ...Object.values(HEADER_FIXED_ITEMS_PX),
    ...Object.values(HEADER_TRUNCATING_FLOORS_PX),
  ];
  const total = widths.reduce((sum, w) => sum + w, 0);
  return total + Math.max(0, widths.length - 1) * HEADER_ROW_GAP_PX;
}

const EXPORT_FORMATS = ["text", "markdown", "json"] as const;
type ExportFormat = (typeof EXPORT_FORMATS)[number];

const OVERFLOW_ITEM_CLASS =
  "flex w-full items-center gap-1.5 text-left px-2 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#2a2d3d] hover:text-[#c0caf5] focus:bg-[#2a2d3d] focus:outline-hidden transition-colors aria-disabled:opacity-50 aria-disabled:cursor-not-allowed";
const OVERFLOW_SUBITEM_CLASS =
  "block w-full text-left pl-6 pr-2 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#2a2d3d] hover:text-[#c0caf5] focus:bg-[#2a2d3d] focus:outline-hidden transition-colors";
const MENUITEM_SELECTOR = '[role="menuitem"]';

export function ZoneHoverActions({
  zoneIdx,
  labelZone,
  onExportZone,
  inline,
}: ZoneHoverActionsProps) {
  const session = useTerminalSession();
  const { zoneLayout, closeTerminal, sessionStates, tabs } = session;
  const transitionEffects = useTransitionEffects();
  const { labelsAndTags } = useZoneMetadata();
  const { ownerOf } = useWindowAssignments();
  const { popOutTab, moveTabToWindow, listWindows } = useTerminalWindowActions();

  const tabId = zoneLayout.assignments[zoneIdx];
  const isMaximized = zoneLayout.maximizedZone === zoneIdx;
  // tmux mode already shows the one session at full size; maximize does
  // nothing there, so it is not offered.
  const canMaximize = zoneLayout.workMode !== "tmux";
  const state = tabId ? (sessionStates[tabId] ?? "idle") : "idle";
  const canRestart = !!tabId && (state === "completed" || state === "error");
  const hasTab = !!tabId;
  // Set only for a tab mirroring a session on ANOTHER device — it gets the
  // "End on remote session" action beside close.
  const remoteTab = tabId ? tabs.find((t) => t.id === tabId && !!t.remote) : undefined;

  const [showExportMenu, setShowExportMenu] = useState(false);
  const [showLabelInput, setShowLabelInput] = useState(false);
  const [labelDraft, setLabelDraft] = useState("");
  // The popover edits ONE zone's label. When the zone it edits changes under
  // it — a keyboard swap in tmux mode shows another session, whose cosmetics
  // are another grid zone — the draft belongs to the zone it was opened for,
  // and an Enter would write it onto the new session's label. Close it instead
  // (adjusted during render, so no frame shows the popover over the new zone).
  const [labelInputZone, setLabelInputZone] = useState(labelZone);
  if (labelInputZone !== labelZone) {
    setLabelInputZone(labelZone);
    setShowLabelInput(false);
  }
  const [showWindowMenu, setShowWindowMenu] = useState(false);
  const [windowList, setWindowList] = useState<RunnerWindowRecord[]>([]);

  // Narrow zones (below the collapse width, by container query) show ONE
  // overflow button instead of five of the actions; this is its menu. It keeps
  // its own expand state for the export / window lists, so a dropdown opened
  // while the zone was wide never renders a second copy inside the menu.
  const [showOverflowMenu, setShowOverflowMenu] = useState(false);
  const [menuExpand, setMenuExpand] = useState<"export" | "window" | null>(null);
  const [menuMaxHeight, setMenuMaxHeight] = useState<number | null>(null);
  const focusOnOpen = useRef<"first" | "last">("first");
  const overflowMenuId = useId();
  // The overlay collapses in a narrow zone; the maximized header's inline
  // cluster never does (it is not in a zone-sized container).
  const collapsible = !inline;
  const fullOnly = collapsible
    ? ` ${remoteTab ? ZONE_HOVER_FULL_ONLY_REMOTE_CLASS : ZONE_HOVER_FULL_ONLY_CLASS}`
    : "";
  const collapsedOnly = remoteTab
    ? ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS
    : ZONE_HOVER_COLLAPSED_ONLY_CLASS;

  const exportRef = useRef<HTMLDivElement>(null);
  const labelPopoverRef = useRef<HTMLDivElement>(null);
  const labelInputRef = useRef<HTMLInputElement>(null);
  const windowMenuRef = useRef<HTMLDivElement>(null);
  const overflowRef = useRef<HTMLDivElement>(null);
  const overflowButtonRef = useRef<HTMLButtonElement>(null);
  const overflowMenuRef = useRef<HTMLDivElement>(null);

  const closeOverflow = useCallback(() => {
    setShowOverflowMenu(false);
    setMenuExpand(null);
  }, []);

  const openOverflow = useCallback((focus: "first" | "last") => {
    // One open popover at a time, across the collapse boundary too.
    setShowExportMenu(false);
    setShowWindowMenu(false);
    setShowLabelInput(false);
    setMenuExpand(null);
    // The cell clips (`overflow-hidden`), so the menu scrolls rather than
    // running past the cell's bottom edge.
    const anchor = overflowRef.current;
    const cell = anchor?.closest<HTMLElement>(".group");
    if (anchor && cell) {
      const cellRect = cell.getBoundingClientRect();
      const top = anchor.getBoundingClientRect().bottom + 4; // `mt-1`
      setMenuMaxHeight(cellRect.height > 0 ? Math.max(48, cellRect.bottom - top - 4) : null);
    }
    focusOnOpen.current = focus;
    setShowOverflowMenu(true);
  }, []);

  // Outside-click close for the popovers.
  useEffect(() => {
    if (!showExportMenu && !showLabelInput && !showWindowMenu && !showOverflowMenu) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as Node;
      if (showExportMenu && exportRef.current && !exportRef.current.contains(target)) {
        setShowExportMenu(false);
      }
      if (showLabelInput && labelPopoverRef.current && !labelPopoverRef.current.contains(target)) {
        // Commit on outside-click — same convention as the launch-menu
        // close handler so a click-away doesn't silently discard typing.
        if (labelZone !== null) labelsAndTags.setZoneLabel(labelZone, labelDraft.trim());
        setShowLabelInput(false);
      }
      if (showWindowMenu && windowMenuRef.current && !windowMenuRef.current.contains(target)) {
        setShowWindowMenu(false);
      }
      if (showOverflowMenu && overflowRef.current && !overflowRef.current.contains(target)) {
        closeOverflow();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [
    showExportMenu,
    showLabelInput,
    showWindowMenu,
    showOverflowMenu,
    labelDraft,
    labelsAndTags,
    labelZone,
    closeOverflow,
  ]);

  useEffect(() => {
    if (showLabelInput) {
      // Microtask defer so the focus happens after the popover mounts
      // and selection ranges resolve cleanly.
      queueMicrotask(() => labelInputRef.current?.focus());
    }
  }, [showLabelInput]);

  // Opening the overflow menu moves focus to its first (or, from ArrowUp, its
  // last) item, so the keyboard lands in the menu it just opened.
  useEffect(() => {
    if (!showOverflowMenu) return;
    const items = overflowMenuRef.current?.querySelectorAll<HTMLElement>(MENUITEM_SELECTOR);
    if (!items || items.length === 0) return;
    items[focusOnOpen.current === "first" ? 0 : items.length - 1]?.focus();
  }, [showOverflowMenu]);

  const refreshWindows = useCallback(() => {
    // Refresh the target list each open — pop-outs may have come/gone.
    listWindows()
      .then(setWindowList)
      .catch((err) => console.error("list_runner_windows failed:", err));
  }, [listWindows]);

  const handleMaximize = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      zoneLayout.toggleMaximize(zoneIdx);
    },
    [zoneLayout, zoneIdx],
  );

  const handleRestart = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!canRestart) return;
      transitionEffects.handleRestartInZone(zoneIdx);
    },
    [canRestart, transitionEffects, zoneIdx],
  );

  const openLabel = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      // Seed the draft with the current label (if any) so the operator
      // can edit-in-place rather than retyping. Use empty-string when
      // unset rather than undefined to keep the input controlled.
      if (labelZone === null) return;
      setLabelDraft(labelsAndTags.zoneLabels[labelZone] ?? "");
      setShowLabelInput((prev) => !prev);
      setShowExportMenu(false);
      setShowOverflowMenu(false);
    },
    [labelsAndTags.zoneLabels, labelZone],
  );

  const openExportMenu = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!onExportZone) return;
      setShowExportMenu((prev) => !prev);
      setShowLabelInput(false);
      closeOverflow();
    },
    [onExportZone, closeOverflow],
  );

  const handleExport = useCallback(
    (e: React.MouseEvent, format: ExportFormat) => {
      e.stopPropagation();
      if (!onExportZone) return;
      onExportZone(zoneIdx, format);
      setShowExportMenu(false);
    },
    [onExportZone, zoneIdx],
  );

  const handleClose = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!tabId) return;
      closeTerminal(tabId);
    },
    [tabId, closeTerminal],
  );

  const openWindowMenu = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!tabId) return;
      setShowExportMenu(false);
      setShowLabelInput(false);
      closeOverflow();
      setShowWindowMenu((prev) => {
        const next = !prev;
        if (next) refreshWindows();
        return next;
      });
    },
    [tabId, refreshWindows, closeOverflow],
  );

  const handlePopOutNew = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!tabId) return;
      setShowWindowMenu(false);
      void popOutTab(tabId).catch((err) =>
        console.error("Failed to pop out terminal to a new window:", err),
      );
    },
    [tabId, popOutTab],
  );

  const handleMoveTo = useCallback(
    (e: React.MouseEvent, target: string) => {
      e.stopPropagation();
      if (!tabId) return;
      setShowWindowMenu(false);
      void moveTabToWindow(tabId, target).catch((err) =>
        console.error(`Failed to move terminal to ${target}:`, err),
      );
    },
    [tabId, moveTabToWindow],
  );

  const toggleOverflowMenu = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (showOverflowMenu) closeOverflow();
      else openOverflow("first");
    },
    [showOverflowMenu, closeOverflow, openOverflow],
  );

  /** Runs a menu action, then closes the menu; a disabled item does nothing. */
  const fromOverflow =
    (action: (e: React.MouseEvent) => void, disabled = false) =>
    (e: React.MouseEvent): void => {
      e.stopPropagation();
      if (disabled) return;
      action(e);
      closeOverflow();
    };

  const toggleMenuExpand =
    (which: "export" | "window", enabled: boolean) => (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!enabled) return;
      setMenuExpand((prev) => {
        const next = prev === which ? null : which;
        if (next === "window") refreshWindows();
        return next;
      });
    };

  // The menu button: ArrowDown / ArrowUp open it onto its first / last item
  // (Enter and Space open it through the button's own click). Inside the menu:
  // Escape closes it and returns focus to the button, Tab closes it and lets
  // focus move on, Up / Down / Home / End move between items.
  const handleOverflowKeyDown = (e: React.KeyboardEvent) => {
    const fromButton = e.target === overflowButtonRef.current;
    if (!showOverflowMenu) {
      if (fromButton && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
        e.preventDefault();
        e.stopPropagation();
        openOverflow(e.key === "ArrowDown" ? "first" : "last");
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeOverflow();
      overflowButtonRef.current?.focus();
      return;
    }
    if (e.key === "Tab") {
      closeOverflow();
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const items = Array.from(
      overflowMenuRef.current?.querySelectorAll<HTMLElement>(MENUITEM_SELECTOR) ?? [],
    );
    if (items.length === 0) return;
    e.preventDefault();
    e.stopPropagation();
    const at = items.indexOf(document.activeElement as HTMLElement);
    let next: number;
    if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    else if (at < 0) next = e.key === "ArrowDown" ? 0 : items.length - 1;
    else next = (at + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
    items[next]?.focus();
  };

  // Focus leaving the menu for another element closes it. A blur with no
  // `relatedTarget` (a click on something unfocusable — WebKit does not focus
  // a clicked button) is left to the outside-mousedown handler.
  const handleOverflowBlur = (e: React.FocusEvent) => {
    const next = e.relatedTarget as Node | null;
    if (showOverflowMenu && next && !overflowRef.current?.contains(next)) closeOverflow();
  };

  return (
    <div
      // group-hover from the cell parent (`ZoneGrid`'s outer cell div
      // has the `group` class) controls reveal. Base state is
      // pointer-events-none so the invisible chrome doesn't block
      // clicks on the terminal underneath; hover flips it back so
      // buttons are clickable once visible. Enter delay lives on the
      // group-hover variant only — exit transition fires immediately
      // for a snappy "mouse left" feel.
      className={
        inline
          ? "relative shrink-0"
          : "absolute top-1 right-1 z-30 opacity-0 pointer-events-none transition-opacity duration-200 group-hover:opacity-100 group-hover:pointer-events-auto group-hover:delay-150"
      }
      // Inline (maximized header): keep clicks/double-clicks from reaching the
      // header, whose double-click restores the zone.
      onDoubleClick={inline ? (e) => e.stopPropagation() : undefined}
      onMouseDown={inline ? (e) => e.stopPropagation() : undefined}
      data-page-element="zone-hover-actions"
      data-zone-index={zoneIdx}
    >
      <div className="flex items-center gap-0.5 px-1 py-0.5 bg-[#13141f]/90 border border-[#2a2d3d] rounded backdrop-blur-sm shadow-md">
        {/* Maximize / restore */}
        {canMaximize && (
          <button
            type="button"
            onClick={handleMaximize}
            className={`p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]/60 transition-colors${fullOnly}`}
            title={isMaximized ? "Restore zone (Ctrl+Shift+F)" : "Maximize zone (Ctrl+Shift+F)"}
            aria-label={isMaximized ? "Restore zone" : "Maximize zone"}
          >
            {isMaximized ? <Minimize2 className="w-3 h-3" /> : <Maximize2 className="w-3 h-3" />}
          </button>
        )}

        {/* Restart — visually gated */}
        <button
          type="button"
          onClick={handleRestart}
          disabled={!canRestart}
          className={`p-1 rounded transition-colors ${
            canRestart
              ? "text-[#565f89] hover:text-[#7dcfff] hover:bg-[#7dcfff]/10"
              : "text-[#414868] cursor-not-allowed"
          }${fullOnly}`}
          title={
            canRestart
              ? "Restart session (Ctrl+Shift+R)"
              : `Restart available only when session is completed or errored (currently: ${state})`
          }
          aria-label="Restart session"
          aria-disabled={!canRestart}
        >
          <RefreshCw className="w-3 h-3" />
        </button>

        {/* Label */}
        {labelZone !== null && (
          <button
            type="button"
            onClick={openLabel}
            className={`p-1 rounded transition-colors ${
              showLabelInput
                ? "text-[#bb9af7] bg-[#bb9af7]/10"
                : "text-[#565f89] hover:text-[#bb9af7] hover:bg-[#bb9af7]/10"
            }${fullOnly}`}
            title="Edit zone label"
            aria-label="Edit zone label"
            aria-expanded={showLabelInput}
          >
            <Tag className="w-3 h-3" />
          </button>
        )}

        {/* Export */}
        <div className={`relative${fullOnly}`} ref={exportRef}>
          <button
            type="button"
            onClick={openExportMenu}
            disabled={!onExportZone}
            className={`p-1 rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
              showExportMenu
                ? "text-[#9ece6a] bg-[#9ece6a]/10"
                : "text-[#565f89] hover:text-[#9ece6a] hover:bg-[#9ece6a]/10"
            }`}
            title={onExportZone ? "Export zone output" : "Export not available"}
            aria-label="Export zone output"
            aria-expanded={showExportMenu}
          >
            <Download className="w-3 h-3" />
          </button>
          {showExportMenu && onExportZone && (
            <div className="absolute top-full right-0 mt-1 w-24 bg-[#1a1b26] border border-[#2a2d3d] rounded shadow-xl z-50 overflow-hidden">
              {EXPORT_FORMATS.map((format) => (
                <button
                  key={format}
                  type="button"
                  onClick={(e) => handleExport(e, format)}
                  className="block w-full text-left px-2 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#2a2d3d] hover:text-[#c0caf5] transition-colors"
                >
                  {format}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Send to window (pop out to a new window / move to an existing one) */}
        <div className={`relative${fullOnly}`} ref={windowMenuRef}>
          <button
            type="button"
            onClick={openWindowMenu}
            disabled={!hasTab}
            className={`p-1 rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
              showWindowMenu
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10"
            }`}
            title="Send this terminal to a window (or drag its header out)"
            aria-label="Send terminal to a window"
            aria-expanded={showWindowMenu}
          >
            <AppWindow className="w-3 h-3" />
          </button>
          {showWindowMenu && (
            <div className="absolute top-full right-0 mt-1 w-40 bg-[#1a1b26] border border-[#2a2d3d] rounded shadow-xl z-50 overflow-hidden">
              <button
                type="button"
                onClick={handlePopOutNew}
                className="block w-full text-left px-2 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#2a2d3d] hover:text-[#c0caf5] transition-colors"
              >
                New window
              </button>
              {windowList
                .filter((w) => w.label !== ownerOf(tabId ?? ""))
                .map((w) => (
                  <button
                    key={w.label}
                    type="button"
                    onClick={(e) => handleMoveTo(e, w.label)}
                    className="block w-full text-left px-2 py-1 text-[10px] text-[#a9b1d6] hover:bg-[#2a2d3d] hover:text-[#c0caf5] transition-colors"
                  >
                    {w.label === "main" ? "Main window" : w.label}
                  </button>
                ))}
            </div>
          )}
        </div>

        {/* Narrow zone: the five actions above are hidden by container query
            and this one button stands in for them. Each menu item's
            accessible name is its visible text. */}
        {collapsible && (
          <div
            className={`relative ${collapsedOnly}`}
            ref={overflowRef}
            onKeyDown={handleOverflowKeyDown}
            onBlur={handleOverflowBlur}
            data-zone-hover-overflow
          >
            <button
              ref={overflowButtonRef}
              type="button"
              onClick={toggleOverflowMenu}
              className={`p-1 rounded transition-colors ${
                showOverflowMenu
                  ? "text-[#c0caf5] bg-[#2a2d3d]/60"
                  : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]/60"
              }`}
              title="Zone actions"
              aria-label="Zone actions"
              aria-haspopup="menu"
              aria-expanded={showOverflowMenu}
              aria-controls={overflowMenuId}
            >
              <MoreHorizontal className="w-3 h-3" />
            </button>
            <div
              ref={overflowMenuRef}
              id={overflowMenuId}
              role="menu"
              aria-label="Zone actions"
              hidden={!showOverflowMenu}
              className="absolute top-full right-0 mt-1 w-48 bg-[#1a1b26] border border-[#2a2d3d] rounded shadow-xl z-50 overflow-x-hidden overflow-y-auto py-0.5"
              style={menuMaxHeight !== null ? { maxHeight: `${menuMaxHeight}px` } : undefined}
            >
              {canMaximize && (
                <button
                  type="button"
                  role="menuitem"
                  onClick={fromOverflow(handleMaximize)}
                  className={OVERFLOW_ITEM_CLASS}
                  title="Ctrl+Shift+F"
                >
                  {isMaximized ? (
                    <Minimize2 className="w-3 h-3 shrink-0" aria-hidden />
                  ) : (
                    <Maximize2 className="w-3 h-3 shrink-0" aria-hidden />
                  )}
                  {isMaximized ? "Restore zone" : "Maximize zone"}
                </button>
              )}
              <button
                type="button"
                role="menuitem"
                onClick={fromOverflow(handleRestart, !canRestart)}
                aria-disabled={!canRestart}
                className={OVERFLOW_ITEM_CLASS}
                title={
                  canRestart
                    ? "Ctrl+Shift+R"
                    : `Available only when the session is completed or errored (currently: ${state})`
                }
              >
                <RefreshCw className="w-3 h-3 shrink-0" aria-hidden />
                Restart session
              </button>
              {labelZone !== null && (
                <button
                  type="button"
                  role="menuitem"
                  onClick={fromOverflow(openLabel)}
                  className={OVERFLOW_ITEM_CLASS}
                >
                  <Tag className="w-3 h-3 shrink-0" aria-hidden />
                  Edit zone label
                </button>
              )}
              <button
                type="button"
                role="menuitem"
                onClick={toggleMenuExpand("export", !!onExportZone)}
                aria-disabled={!onExportZone}
                aria-haspopup="true"
                aria-expanded={menuExpand === "export"}
                className={OVERFLOW_ITEM_CLASS}
                title={onExportZone ? undefined : "Export not available"}
              >
                <Download className="w-3 h-3 shrink-0" aria-hidden />
                Export zone output
              </button>
              {menuExpand === "export" && onExportZone && (
                <div role="group" aria-label="Export formats">
                  {EXPORT_FORMATS.map((format) => (
                    <button
                      key={format}
                      type="button"
                      role="menuitem"
                      onClick={fromOverflow((e) => handleExport(e, format))}
                      className={OVERFLOW_SUBITEM_CLASS}
                    >
                      {format}
                    </button>
                  ))}
                </div>
              )}
              <button
                type="button"
                role="menuitem"
                onClick={toggleMenuExpand("window", hasTab)}
                aria-disabled={!hasTab}
                aria-haspopup="true"
                aria-expanded={menuExpand === "window"}
                className={OVERFLOW_ITEM_CLASS}
                title="Or drag the zone's header out of the window"
              >
                <AppWindow className="w-3 h-3 shrink-0" aria-hidden />
                Send terminal to a window
              </button>
              {menuExpand === "window" && (
                <div role="group" aria-label="Windows">
                  <button
                    type="button"
                    role="menuitem"
                    onClick={fromOverflow(handlePopOutNew)}
                    className={OVERFLOW_SUBITEM_CLASS}
                  >
                    New window
                  </button>
                  {windowList
                    .filter((w) => w.label !== ownerOf(tabId ?? ""))
                    .map((w) => (
                      <button
                        key={w.label}
                        type="button"
                        role="menuitem"
                        onClick={fromOverflow((e) => handleMoveTo(e, w.label))}
                        className={OVERFLOW_SUBITEM_CLASS}
                      >
                        {w.label === "main" ? "Main window" : w.label}
                      </button>
                    ))}
                </div>
              )}
            </div>
          </div>
        )}

        {/* End on remote — remote tabs only. Close keeps meaning DETACH (plan
            `2026-09-30-close-remote-sessions-from-the-local-runner` D1); this
            is the separate, named, confirmed action that ENDS the session.
            Never collapsed: it owns a confirm dialog. */}
        {remoteTab && <RemoteTabEndAction tab={remoteTab} />}

        {/* Close — also the addressable dismiss for an in-zone exited
            tombstone. Stamped per-tab so automation can dismiss a specific
            dead session (text-independent, stable). Never collapsed, so the
            stamp is always on a visible button. */}
        <button
          type="button"
          onClick={handleClose}
          disabled={!hasTab}
          {...(hasTab && tabId ? { "data-ui-bridge-id": `terminal.close-session-${tabId}` } : {})}
          className="p-1 rounded text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          title="Close session (Ctrl+Shift+W)"
          aria-label="Close session"
        >
          <X className="w-3 h-3" />
        </button>
      </div>

      {/* Label edit popover */}
      {showLabelInput && (
        <div
          ref={labelPopoverRef}
          className="absolute top-full right-0 mt-1 w-44 bg-[#1a1b26] border border-[#2a2d3d] rounded shadow-xl p-2 z-50"
        >
          <input
            ref={labelInputRef}
            value={labelDraft}
            onChange={(e) => setLabelDraft(e.target.value)}
            onClick={(e) => e.stopPropagation()}
            onMouseDown={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") {
                if (labelZone !== null) labelsAndTags.setZoneLabel(labelZone, labelDraft.trim());
                setShowLabelInput(false);
              } else if (e.key === "Escape") {
                setShowLabelInput(false);
              }
            }}
            placeholder="zone label (comma-separated tags)"
            className="w-full bg-[#13141f] border border-[#2a2d3d] rounded px-1.5 py-1 text-[10px] text-[#c0caf5] placeholder-[#565f89]/60 outline-hidden focus:border-[#7aa2f7] transition-colors"
            maxLength={64}
          />
        </div>
      )}
    </div>
  );
}
