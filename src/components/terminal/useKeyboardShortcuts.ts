import { useEffect } from "react";
import { LAYOUT_PRESETS, FLOW_GRID_ID, type SessionState } from "./useZoneLayout";
import type { UIAction } from "./useUIState";
import type { Metrics } from "./useEventHistory";
import { deliverApprovals } from "./approveAll";
import { runRegistryAction } from "./commands";
import {
  GLOBAL_CHORDS,
  GLOBAL_DIGIT_CHORDS,
  isCtrlShiftChord,
  matchesChord,
  matchesDigitChord,
} from "@/lib/globalChords";
import { isSurfaceVisible } from "@/lib/surfaceVisible";

interface UseKeyboardShortcutsParams {
  activeId: string | null;
  tabs: Array<{ id: string }>;
  dispatch: React.Dispatch<UIAction>;
  swapSource: number | null;
  selectedZones: Set<number>;
  createAndAssignTerminal: () => Promise<string | null>;
  closeTerminal: (id: string) => void;
  setActiveId: (id: string) => void;
  zoneLayout: {
    isMultiZone: boolean;
    focusedZone: number;
    layoutId: string;
    maximizedZone: number | null;
    assignments: Record<number, string>;
    layout: { zones: unknown[] };
    setFocusedZone: (idx: number) => void;
    focusPrevZone: () => void;
    focusNextZone: () => void;
    focusNextNeedsInput: (states: Record<string, SessionState>) => void;
    toggleMaximize: (idx: number) => void;
    setMaximizedZone: (idx: number | null) => void;
    setLayoutId: (id: string) => void;
    assignTabToZone: (idx: number, tabId: string) => void;
  };
  sessionStates: Record<string, SessionState>;
  handleRestartInZone: (zoneIdx: number) => void;
  labelsAndTags: {
    allTags: string[];
    togglePin: (zoneIdx: number) => void;
    setActiveTagFilters: React.Dispatch<React.SetStateAction<Set<string>>>;
  };
  focusHistory: {
    goBack: () => void;
    goForward: () => void;
  };
  transitionEffects: {
    toggleAutoFocus: () => void;
    toggleSound: () => void;
    setUnseenNeedsInput: React.Dispatch<React.SetStateAction<Set<string>>>;
  };
  incrementMetric: (key: keyof Metrics, amount?: number) => void;
  addHistoryEvent: (action: string, detail: string, zoneIdx?: number, color?: string) => void;
  terminalRefs: Map<string, React.RefObject<{ writeToTerminal: (data: string) => void } | null>>;
  workflowGen: {
    rightPanelMode: string | null;
    showSidebar: boolean;
    setShowSidebar: React.Dispatch<React.SetStateAction<boolean>>;
    setRightPanelMode: React.Dispatch<
      React.SetStateAction<
        "transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null
      >
    >;
    setSelectedTranscriptSessionId: React.Dispatch<React.SetStateAction<string | null>>;
  };
  sessionManager?: {
    frozenCount: number;
    needsInputCount: number;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    sessions: Array<any>;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    resumeSession: (session: any) => void;
  };
  /**
   * The terminal page's root element. Every chord below is inert while
   * it is not visible.
   *
   * `App.tsx` keeps `TerminalPage` MOUNTED behind a `hidden` div on every
   * other tab so PTYs survive a tab switch — and this `window` listener
   * survived with it, leaving all ~23 chords live on the Builder, Logs
   * and the Active dashboard. That is not a theoretical leak: one
   * `Ctrl+3` pressed on the Active dashboard switched the dashboard's
   * widget AND moved this page's focused zone, because two `window`
   * listeners on the same target both run. Guarding the listener on the
   * surface it acts on is the class fix — a chord for an off-screen
   * surface is neither claimed nor swallowed.
   *
   * REQUIRED, not optional. An optional ref that a future call site
   * forgets fails CLOSED — every chord silently dead — which is the
   * hardest kind of regression to notice on a keyboard surface.
   */
  surfaceRef: React.RefObject<HTMLElement | null>;
}

export function useKeyboardShortcuts({
  activeId,
  tabs,
  dispatch,
  swapSource,
  selectedZones,
  createAndAssignTerminal,
  closeTerminal,
  setActiveId,
  zoneLayout,
  sessionStates,
  handleRestartInZone,
  labelsAndTags,
  focusHistory,
  transitionEffects,
  incrementMetric,
  addHistoryEvent,
  terminalRefs,
  workflowGen,
  sessionManager,
  surfaceRef,
}: UseKeyboardShortcutsParams) {
  useEffect(() => {
    // Every `Ctrl+Shift+<key>` test below goes through `isCtrlShiftChord`,
    // which lowercases both sides. A literal `e.key === "T"` is a CapsLock
    // trap: with CapsLock on, Shift INVERTS the case, so the browser
    // reports `"t"` and the chord silently does nothing. Only Ctrl+Shift+K
    // had been normalised (it collided with the KnowledgeBrowser, which is
    // how anyone noticed); the ~20 chords around it were all dead under
    // CapsLock — new terminal, close, maximize, swap, restart, the session
    // sidebar, all of it.
    const handler = (e: KeyboardEvent) => {
      // Off-screen surface → every chord below is inert. See `surfaceRef`.
      if (!isSurfaceVisible(surfaceRef.current)) return;
      if (isCtrlShiftChord(e, "t")) {
        e.preventDefault();
        createAndAssignTerminal();
        return;
      }
      if (isCtrlShiftChord(e, "w")) {
        e.preventDefault();
        if (activeId) closeTerminal(activeId);
        return;
      }
      // Ctrl+Tab / Ctrl+Shift+Tab. Routed through the shared table rather
      // than a hand-rolled `e.key === "Tab"`: `active-dashboard/ActiveRunsBar`
      // claims the same two chords on its own `window` listener, so both
      // handlers run on one press. That collision is documented in
      // `KNOWN_SHARED_CHORDS`; the hand-rolled spelling made it invisible
      // to the enforcement scanner, which is the defect this routing closes.
      const cyclePrev = matchesChord(e, GLOBAL_CHORDS.cyclePrev);
      if (matchesChord(e, GLOBAL_CHORDS.cycleNext) || cyclePrev) {
        e.preventDefault();
        if (zoneLayout.isMultiZone) {
          if (cyclePrev) {
            zoneLayout.focusPrevZone();
          } else {
            zoneLayout.focusNextZone();
          }
        } else if (tabs.length > 1 && activeId) {
          const idx = tabs.findIndex((t) => t.id === activeId);
          const next = cyclePrev ? (idx - 1 + tabs.length) % tabs.length : (idx + 1) % tabs.length;
          setActiveId(tabs[next].id);
        }
        return;
      }
      if (isCtrlShiftChord(e, "n")) {
        e.preventDefault();
        zoneLayout.focusNextNeedsInput(sessionStates);
        return;
      }
      if (isCtrlShiftChord(e, "f")) {
        e.preventDefault();
        zoneLayout.toggleMaximize(zoneLayout.focusedZone);
        return;
      }
      if (isCtrlShiftChord(e, "m")) {
        e.preventDefault();
        dispatch({ type: "CYCLE_VIEW_MODE" });
        return;
      }
      if (isCtrlShiftChord(e, "a")) {
        e.preventDefault();
        transitionEffects.toggleAutoFocus();
        return;
      }
      if (isCtrlShiftChord(e, "s")) {
        e.preventDefault();
        transitionEffects.toggleSound();
        return;
      }
      if (isCtrlShiftChord(e, "Enter")) {
        e.preventDefault();
        // Same delivery path as `/approve-all` and the overlay button.
        //
        // This chord is where the silent-skip cost the most: it incremented
        // `totalApprovals` and wrote an "N sessions" history event from
        // `waiting.length` BEFORE writing anything, through an optional chain
        // that is a no-op for any pane without a mounted `TerminalInstance`.
        // So the page's metrics card and event log recorded approvals that
        // reached no process — and those are exactly the numbers `/metrics`
        // and `/history` render. The counter now counts deliveries.
        const waiting = tabs.filter((t) => sessionStates[t.id] === "needs-input");
        void deliverApprovals(
          waiting.map((t) => t.id),
          terminalRefs,
          "y\r",
        ).then((report) => {
          if (report.delivered > 0) incrementMetric("totalApprovals", report.delivered);
          addHistoryEvent(
            "Approve all",
            report.delivered === report.targeted
              ? `${report.delivered} sessions`
              : `${report.delivered} of ${report.targeted} sessions`,
            undefined,
            report.delivered === report.targeted ? "#9ece6a" : "#e0af68",
          );
        });
        return;
      }
      // Digit RANGES. They used to be hand-rolled `e.key >= "1" && e.key
      // <= "8"` comparisons on the theory that a range "can't route
      // through `isCtrlShiftChord`" — true of that helper, and the reason
      // both ranges stayed outside every chord table. The scanner's claim
      // counters see only `matchesChord(...)` / `isCtrlShiftChord(...)`
      // text, so a range contributed NOTHING to count: the `Ctrl+1..8`
      // collision with `active-dashboard/DashboardPage` (one press moved
      // this page's focused zone while the operator was on the dashboard)
      // was invisible to a suite that was green. `matchesDigitChord`
      // gives the range a spelling the scanner can expand and count.
      const presetDigit = matchesDigitChord(e, GLOBAL_DIGIT_CHORDS.terminalLayoutPreset);
      if (presetDigit !== null) {
        e.preventDefault();
        const preset = LAYOUT_PRESETS.find((l) => l.shortcutKey === presetDigit);
        if (preset) {
          zoneLayout.setLayoutId(preset.id);
        }
        return;
      }
      const zoneDigit = matchesDigitChord(e, GLOBAL_DIGIT_CHORDS.terminalFocusZone);
      if (zoneDigit !== null) {
        if (zoneLayout.isMultiZone) {
          const zoneIdx = zoneDigit - 1;
          if (zoneIdx < zoneLayout.layout.zones.length) {
            e.preventDefault();
            zoneLayout.setFocusedZone(zoneIdx);
          }
        }
        return;
      }
      if (isCtrlShiftChord(e, "x")) {
        e.preventDefault();
        if (swapSource === null) {
          dispatch({ type: "SET_SWAP_SOURCE", payload: zoneLayout.focusedZone });
        } else if (swapSource !== zoneLayout.focusedZone) {
          const srcTabId = zoneLayout.assignments[swapSource];
          const dstTabId = zoneLayout.assignments[zoneLayout.focusedZone];
          if (srcTabId) zoneLayout.assignTabToZone(zoneLayout.focusedZone, srcTabId);
          if (dstTabId) zoneLayout.assignTabToZone(swapSource, dstTabId);
          dispatch({ type: "SET_SWAP_SOURCE", payload: null });
        } else {
          dispatch({ type: "SET_SWAP_SOURCE", payload: null });
        }
        return;
      }
      if (isCtrlShiftChord(e, "/")) {
        e.preventDefault();
        dispatch({ type: "TOGGLE_OUTPUT_SEARCH" });
        return;
      }
      if (isCtrlShiftChord(e, "o")) {
        e.preventDefault();
        labelsAndTags.togglePin(zoneLayout.focusedZone);
        return;
      }
      if (isCtrlShiftChord(e, "d")) {
        e.preventDefault();
        dispatch({ type: "TOGGLE_FOCUS_MODE" });
        return;
      }
      if (isCtrlShiftChord(e, "r")) {
        e.preventDefault();
        handleRestartInZone(zoneLayout.focusedZone);
        return;
      }
      if (isCtrlShiftChord(e, "l")) {
        e.preventDefault();
        // In flow-grid (synthetic id, not in LAYOUT_PRESETS) `findIndex` returns
        // -1 and the cycle would jump to preset[0]/`single`, collapsing 10+ live
        // sessions back into a 1-zone layout. v1 wart: no-op the cycle in flow
        // mode rather than cycling into a preset that re-hides sessions.
        if (zoneLayout.layoutId === FLOW_GRID_ID) return;
        const currentIdx = LAYOUT_PRESETS.findIndex((l) => l.id === zoneLayout.layoutId);
        const nextIdx = (currentIdx + 1) % LAYOUT_PRESETS.length;
        zoneLayout.setLayoutId(LAYOUT_PRESETS[nextIdx].id);
        return;
      }
      // Ctrl+Shift+K — command palette. The chord comes from the shared
      // TABLE (not an inline literal) because it is claimed from other
      // component trees too: the KnowledgeBrowser owns Ctrl+Shift+E and
      // unified search owns the SHIFTLESS Ctrl+K (see `lib/globalChords`),
      // so this no longer opens two overlays at once.
      if (matchesChord(e, GLOBAL_CHORDS.commandPalette)) {
        e.preventDefault();
        // Claims the chord for this surface. It does NOT suppress another
        // listener attached to `window` itself — only
        // `stopImmediatePropagation` would, and that would silently break
        // whichever handler happened to register second. Distinct letters
        // per surface is the fix; this just keeps the event from
        // travelling any further.
        e.stopPropagation();
        dispatch({ type: "TOGGLE_COMMAND_PALETTE" });
        return;
      }
      if (isCtrlShiftChord(e, "i")) {
        e.preventDefault();
        dispatch({ type: "TOGGLE_TIMELINE" });
        return;
      }
      if (isCtrlShiftChord(e, "p")) {
        e.preventDefault();
        dispatch({ type: "TOGGLE_CONTROL_PANEL" });
        return;
      }
      if (isCtrlShiftChord(e, "g")) {
        e.preventDefault();
        if (labelsAndTags.allTags.length === 0) return;
        labelsAndTags.setActiveTagFilters((prev) => {
          const currentTag = prev.size === 1 ? [...prev][0] : null;
          const currentIdx = currentTag ? labelsAndTags.allTags.indexOf(currentTag) : -1;
          const nextIdx = currentIdx + 1;
          if (nextIdx >= labelsAndTags.allTags.length) {
            return new Set<string>();
          }
          return new Set([labelsAndTags.allTags[nextIdx]]);
        });
        return;
      }
      if (isCtrlShiftChord(e, "?")) {
        e.preventDefault();
        dispatch({ type: "TOGGLE_SHORTCUTS" });
        return;
      }
      // Ctrl+Shift+H → pop the event-history result card. Fires the
      // `terminal.history` registry action (single source of truth) so
      // this binding can't drift from the /history slash command — that
      // handler closes over the page's `showCard`. The hook has no
      // `showCard` of its own, so reaching the registry by id is the
      // clean path (no prop-drilling a card callback through every page).
      if (isCtrlShiftChord(e, "h")) {
        e.preventDefault();
        // Through `runRegistryAction`, not `getById(...).handler(...)`: a bare
        // handler call skips argument binding and the arity gate, which is the
        // same hole `callRegistry` had. `{}` is trivially valid, so this costs
        // nothing today and cannot rot the day the binding gains a step.
        void runRegistryAction("terminal.history", {}, "hotkey");
        return;
      }
      if (isCtrlShiftChord(e, "ArrowLeft")) {
        e.preventDefault();
        focusHistory.goBack();
        return;
      }
      if (isCtrlShiftChord(e, "ArrowRight")) {
        e.preventDefault();
        focusHistory.goForward();
        return;
      }
      // Ctrl+Shift+B: Toggle session manager sidebar
      if (isCtrlShiftChord(e, "b")) {
        e.preventDefault();
        workflowGen.setShowSidebar((v) => !v);
        return;
      }
      // Ctrl+Shift+J: Jump to next frozen session (resume first frozen)
      if (isCtrlShiftChord(e, "j")) {
        e.preventDefault();
        if (sessionManager) {
          const frozen = sessionManager.sessions.find(
            (s: { liveStatus: string }) => s.liveStatus === "frozen",
          );
          if (frozen) {
            // Pass the full session object (includes _transcript) to resumeSession
            sessionManager.resumeSession(frozen);
            addHistoryEvent(
              "Resume frozen",
              `Session ${frozen.sessionId?.slice(0, 8) ?? ""}`,
              undefined,
              "#f7768e",
            );
          }
        }
        return;
      }
      if (e.key === "Escape") {
        if (swapSource !== null) {
          dispatch({ type: "SET_SWAP_SOURCE", payload: null });
        } else if (selectedZones.size > 0) {
          dispatch({ type: "CLEAR_SELECTION" });
        } else if (zoneLayout.maximizedZone !== null) {
          zoneLayout.setMaximizedZone(null);
        } else if (workflowGen.rightPanelMode) {
          workflowGen.setRightPanelMode(null);
          workflowGen.setSelectedTranscriptSessionId(null);
        }
        return;
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeId,
    tabs,
    dispatch,
    createAndAssignTerminal,
    closeTerminal,
    setActiveId,
    workflowGen.rightPanelMode,
    zoneLayout,
    sessionStates,
    swapSource,
    selectedZones,
    handleRestartInZone,
    labelsAndTags,
    focusHistory,
    transitionEffects,
    incrementMetric,
    addHistoryEvent,
    terminalRefs,
    sessionManager,
    surfaceRef,
  ]);
}
