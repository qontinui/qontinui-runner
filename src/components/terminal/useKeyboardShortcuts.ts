import { useEffect } from "react";
import { LAYOUT_PRESETS, FLOW_GRID_ID, type SessionState } from "./useZoneLayout";
import type { UIAction } from "./useUIState";
import type { Metrics } from "./useEventHistory";
import { getById } from "./commands";
import { GLOBAL_CHORDS, isCtrlShiftChord, matchesChord } from "@/lib/globalChords";

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
      if (e.ctrlKey && e.key === "Tab") {
        e.preventDefault();
        if (zoneLayout.isMultiZone) {
          if (e.shiftKey) {
            zoneLayout.focusPrevZone();
          } else {
            zoneLayout.focusNextZone();
          }
        } else if (tabs.length > 1 && activeId) {
          const idx = tabs.findIndex((t) => t.id === activeId);
          const next = e.shiftKey ? (idx - 1 + tabs.length) % tabs.length : (idx + 1) % tabs.length;
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
        const waiting = tabs.filter((t) => sessionStates[t.id] === "needs-input");
        incrementMetric("totalApprovals", waiting.length);
        addHistoryEvent("Approve all", `${waiting.length} sessions`, undefined, "#9ece6a");
        for (const tab of waiting) {
          terminalRefs.get(tab.id)?.current?.writeToTerminal("y\r");
        }
        return;
      }
      // Digits, not a single key — a range test, so it can't route through
      // `isCtrlShiftChord`. CapsLock does not affect digit keys, so the
      // trap the helper exists for doesn't reach this branch.
      if (e.ctrlKey && e.shiftKey && e.key >= "1" && e.key <= "8") {
        e.preventDefault();
        const num = parseInt(e.key, 10);
        const preset = LAYOUT_PRESETS.find((l) => l.shortcutKey === num);
        if (preset) {
          zoneLayout.setLayoutId(preset.id);
        }
        return;
      }
      if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key >= "1" && e.key <= "9") {
        if (zoneLayout.isMultiZone) {
          const zoneIdx = parseInt(e.key, 10) - 1;
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
        void getById("terminal.history")?.handler({}, { source: "hotkey" });
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
  ]);
}
