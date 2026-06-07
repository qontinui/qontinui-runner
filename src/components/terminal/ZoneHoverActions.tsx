/**
 * Phase 5 — Per-zone hover controls.
 *
 * Five-button cluster pinned to a zone cell's top-right corner that
 * fades in on cell hover (with a 150ms enter delay to avoid flicker on
 * mouse-pass-through). Buttons: maximize/restore, restart, label,
 * export, close.
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

import { useCallback, useEffect, useRef, useState } from "react";
import {
  AppWindow,
  Download,
  Maximize2,
  Minimize2,
  RefreshCw,
  Tag,
  X,
} from "lucide-react";

import { useTerminalSession, useTransitionEffects, useZoneMetadata } from "./contexts";
import { useWindowAssignments } from "./contexts/WindowAssignmentsContext";
import { useTerminalWindowActions, type RunnerWindowRecord } from "./useTerminalWindowActions";

interface ZoneHoverActionsProps {
  zoneIdx: number;
  /** Threaded from `ZoneGrid`'s prop (sourced from `useZoneActions`).
   *  Optional so the export button can degrade to disabled rather than
   *  throwing when the prop isn't wired (unusual; covers test mounts). */
  onExportZone?: (zoneIdx: number, format: "text" | "markdown" | "json") => void;
}

const EXPORT_FORMATS = ["text", "markdown", "json"] as const;
type ExportFormat = (typeof EXPORT_FORMATS)[number];

export function ZoneHoverActions({ zoneIdx, onExportZone }: ZoneHoverActionsProps) {
  const session = useTerminalSession();
  const { zoneLayout, closeTerminal, sessionStates } = session;
  const transitionEffects = useTransitionEffects();
  const { labelsAndTags } = useZoneMetadata();
  const { ownerOf } = useWindowAssignments();
  const { popOutTab, moveTabToWindow, listWindows } = useTerminalWindowActions();

  const tabId = zoneLayout.assignments[zoneIdx];
  const isMaximized = zoneLayout.maximizedZone === zoneIdx;
  const state = tabId ? sessionStates[tabId] ?? "idle" : "idle";
  const canRestart = !!tabId && (state === "completed" || state === "error");
  const hasTab = !!tabId;

  const [showExportMenu, setShowExportMenu] = useState(false);
  const [showLabelInput, setShowLabelInput] = useState(false);
  const [labelDraft, setLabelDraft] = useState("");
  const [showWindowMenu, setShowWindowMenu] = useState(false);
  const [windowList, setWindowList] = useState<RunnerWindowRecord[]>([]);

  const exportRef = useRef<HTMLDivElement>(null);
  const labelPopoverRef = useRef<HTMLDivElement>(null);
  const labelInputRef = useRef<HTMLInputElement>(null);
  const windowMenuRef = useRef<HTMLDivElement>(null);

  // Outside-click close for both popovers.
  useEffect(() => {
    if (!showExportMenu && !showLabelInput && !showWindowMenu) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as Node;
      if (showExportMenu && exportRef.current && !exportRef.current.contains(target)) {
        setShowExportMenu(false);
      }
      if (showLabelInput && labelPopoverRef.current && !labelPopoverRef.current.contains(target)) {
        // Commit on outside-click — same convention as the launch-menu
        // close handler so a click-away doesn't silently discard typing.
        labelsAndTags.setZoneLabel(zoneIdx, labelDraft.trim());
        setShowLabelInput(false);
      }
      if (showWindowMenu && windowMenuRef.current && !windowMenuRef.current.contains(target)) {
        setShowWindowMenu(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showExportMenu, showLabelInput, showWindowMenu, labelDraft, labelsAndTags, zoneIdx]);

  useEffect(() => {
    if (showLabelInput) {
      // Microtask defer so the focus happens after the popover mounts
      // and selection ranges resolve cleanly.
      queueMicrotask(() => labelInputRef.current?.focus());
    }
  }, [showLabelInput]);

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
      setLabelDraft(labelsAndTags.zoneLabels[zoneIdx] ?? "");
      setShowLabelInput((prev) => !prev);
      setShowExportMenu(false);
    },
    [labelsAndTags.zoneLabels, zoneIdx],
  );

  const openExportMenu = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!onExportZone) return;
      setShowExportMenu((prev) => !prev);
      setShowLabelInput(false);
    },
    [onExportZone],
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
      setShowWindowMenu((prev) => {
        const next = !prev;
        if (next) {
          // Refresh the target list each open — pop-outs may have come/gone.
          listWindows()
            .then(setWindowList)
            .catch((err) => console.error("list_runner_windows failed:", err));
        }
        return next;
      });
    },
    [tabId, listWindows],
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

  return (
    <div
      // group-hover from the cell parent (`ZoneGrid`'s outer cell div
      // has the `group` class) controls reveal. Base state is
      // pointer-events-none so the invisible chrome doesn't block
      // clicks on the terminal underneath; hover flips it back so
      // buttons are clickable once visible. Enter delay lives on the
      // group-hover variant only — exit transition fires immediately
      // for a snappy "mouse left" feel.
      className="absolute top-1 right-1 z-30 opacity-0 pointer-events-none transition-opacity duration-200 group-hover:opacity-100 group-hover:pointer-events-auto group-hover:delay-150"
      data-page-element="zone-hover-actions"
      data-zone-index={zoneIdx}
    >
      <div className="flex items-center gap-0.5 px-1 py-0.5 bg-[#13141f]/90 border border-[#2a2d3d] rounded backdrop-blur-sm shadow-md">
        {/* Maximize / restore */}
        <button
          type="button"
          onClick={handleMaximize}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]/60 transition-colors"
          title={isMaximized ? "Restore zone (Ctrl+Shift+F)" : "Maximize zone (Ctrl+Shift+F)"}
          aria-label={isMaximized ? "Restore zone" : "Maximize zone"}
        >
          {isMaximized ? <Minimize2 className="w-3 h-3" /> : <Maximize2 className="w-3 h-3" />}
        </button>

        {/* Restart — visually gated */}
        <button
          type="button"
          onClick={handleRestart}
          disabled={!canRestart}
          className={`p-1 rounded transition-colors ${
            canRestart
              ? "text-[#565f89] hover:text-[#7dcfff] hover:bg-[#7dcfff]/10"
              : "text-[#414868] cursor-not-allowed"
          }`}
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
        <button
          type="button"
          onClick={openLabel}
          className={`p-1 rounded transition-colors ${
            showLabelInput
              ? "text-[#bb9af7] bg-[#bb9af7]/10"
              : "text-[#565f89] hover:text-[#bb9af7] hover:bg-[#bb9af7]/10"
          }`}
          title="Edit zone label"
          aria-label="Edit zone label"
          aria-expanded={showLabelInput}
        >
          <Tag className="w-3 h-3" />
        </button>

        {/* Export */}
        <div className="relative" ref={exportRef}>
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
        <div className="relative" ref={windowMenuRef}>
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

        {/* Close */}
        <button
          type="button"
          onClick={handleClose}
          disabled={!hasTab}
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
                labelsAndTags.setZoneLabel(zoneIdx, labelDraft.trim());
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
