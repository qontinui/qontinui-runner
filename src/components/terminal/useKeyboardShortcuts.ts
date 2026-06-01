import { useEffect } from "react";
import { LAYOUT_PRESETS, type SessionState } from "./useZoneLayout";
import type { UIAction } from "./useUIState";
import type { Metrics } from "./useEventHistory";
import { getById } from "./commands";

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
      React.SetStateAction<"transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null>
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
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key === "T") {
        e.preventDefault();
        createAndAssignTerminal();
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "W") {
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
      if (e.ctrlKey && e.shiftKey && e.key === "N") {
        e.preventDefault();
        zoneLayout.focusNextNeedsInput(sessionStates);
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "F") {
        e.preventDefault();
        zoneLayout.toggleMaximize(zoneLayout.focusedZone);
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "M") {
        e.preventDefault();
        dispatch({ type: "CYCLE_VIEW_MODE" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "A") {
        e.preventDefault();
        transitionEffects.toggleAutoFocus();
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "S") {
        e.preventDefault();
        transitionEffects.toggleSound();
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "Enter") {
        e.preventDefault();
        const waiting = tabs.filter((t) => sessionStates[t.id] === "needs-input");
        incrementMetric("totalApprovals", waiting.length);
        addHistoryEvent("Approve all", `${waiting.length} sessions`, undefined, "#9ece6a");
        for (const tab of waiting) {
          terminalRefs.get(tab.id)?.current?.writeToTerminal("y\r");
        }
        return;
      }
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
      if (e.ctrlKey && e.shiftKey && e.key === "X") {
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
      if (e.ctrlKey && e.shiftKey && e.key === "/") {
        e.preventDefault();
        dispatch({ type: "TOGGLE_OUTPUT_SEARCH" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "O") {
        e.preventDefault();
        labelsAndTags.togglePin(zoneLayout.focusedZone);
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "D") {
        e.preventDefault();
        dispatch({ type: "TOGGLE_FOCUS_MODE" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "R") {
        e.preventDefault();
        handleRestartInZone(zoneLayout.focusedZone);
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "L") {
        e.preventDefault();
        const currentIdx = LAYOUT_PRESETS.findIndex((l) => l.id === zoneLayout.layoutId);
        const nextIdx = (currentIdx + 1) % LAYOUT_PRESETS.length;
        zoneLayout.setLayoutId(LAYOUT_PRESETS[nextIdx].id);
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "K") {
        e.preventDefault();
        dispatch({ type: "TOGGLE_COMMAND_PALETTE" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "I") {
        e.preventDefault();
        dispatch({ type: "TOGGLE_TIMELINE" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "P") {
        e.preventDefault();
        dispatch({ type: "TOGGLE_CONTROL_PANEL" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "G") {
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
      if (e.ctrlKey && e.shiftKey && e.key === "?") {
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
      if (e.ctrlKey && e.shiftKey && e.key === "H") {
        e.preventDefault();
        void getById("terminal.history")?.handler({}, { source: "hotkey" });
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "ArrowLeft") {
        e.preventDefault();
        focusHistory.goBack();
        return;
      }
      if (e.ctrlKey && e.shiftKey && e.key === "ArrowRight") {
        e.preventDefault();
        focusHistory.goForward();
        return;
      }
      // Ctrl+Shift+B: Toggle session manager sidebar
      if (e.ctrlKey && e.shiftKey && e.key === "B") {
        e.preventDefault();
        workflowGen.setShowSidebar((v) => !v);
        return;
      }
      // Ctrl+Shift+J: Jump to next frozen session (resume first frozen)
      if (e.ctrlKey && e.shiftKey && e.key === "J") {
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
