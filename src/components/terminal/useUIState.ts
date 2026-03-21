import { useReducer, useCallback } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import type { ViewMode } from "./ZoneGrid";

interface UIState {
  viewMode: ViewMode;
  showShortcutsOverlay: boolean;
  showCommandPalette: boolean;
  showTimeline: boolean;
  showControlPanel: boolean;
  controlPanelCollapsed: boolean;
  selectedZones: Set<number>;
  outputSearch: string;
  showOutputSearch: boolean;
  swapSource: number | null;
  resetRatiosKey: number;
  autoLayout: boolean;
  focusMode: boolean;
}

type UIAction =
  | { type: "SET_VIEW_MODE"; payload: ViewMode }
  | { type: "CYCLE_VIEW_MODE" }
  | { type: "SET_SHOW_SHORTCUTS"; payload: boolean }
  | { type: "TOGGLE_SHORTCUTS" }
  | { type: "SET_SHOW_COMMAND_PALETTE"; payload: boolean }
  | { type: "TOGGLE_COMMAND_PALETTE" }
  | { type: "SET_SHOW_TIMELINE"; payload: boolean }
  | { type: "TOGGLE_TIMELINE" }
  | { type: "SET_SHOW_CONTROL_PANEL"; payload: boolean }
  | { type: "TOGGLE_CONTROL_PANEL" }
  | { type: "SET_CONTROL_PANEL_COLLAPSED"; payload: boolean }
  | { type: "TOGGLE_CONTROL_PANEL_COLLAPSED" }
  | { type: "SET_SELECTED_ZONES"; payload: Set<number> }
  | { type: "TOGGLE_ZONE_SELECTION"; payload: number }
  | { type: "CLEAR_SELECTION" }
  | { type: "SET_OUTPUT_SEARCH"; payload: string }
  | { type: "SET_SHOW_OUTPUT_SEARCH"; payload: boolean }
  | { type: "TOGGLE_OUTPUT_SEARCH" }
  | { type: "SET_SWAP_SOURCE"; payload: number | null }
  | { type: "RESET_RATIOS" }
  | { type: "SET_AUTO_LAYOUT"; payload: boolean }
  | { type: "TOGGLE_AUTO_LAYOUT" }
  | { type: "SET_FOCUS_MODE"; payload: boolean }
  | { type: "TOGGLE_FOCUS_MODE" };

function uiReducer(state: UIState, action: UIAction): UIState {
  switch (action.type) {
    case "SET_VIEW_MODE":
      return { ...state, viewMode: action.payload };
    case "CYCLE_VIEW_MODE": {
      const next =
        state.viewMode === "auto" ? "full" : state.viewMode === "full" ? "compact" : "auto";
      return { ...state, viewMode: next };
    }
    case "SET_SHOW_SHORTCUTS":
      return { ...state, showShortcutsOverlay: action.payload };
    case "TOGGLE_SHORTCUTS":
      return { ...state, showShortcutsOverlay: !state.showShortcutsOverlay };
    case "SET_SHOW_COMMAND_PALETTE":
      return { ...state, showCommandPalette: action.payload };
    case "TOGGLE_COMMAND_PALETTE":
      return { ...state, showCommandPalette: !state.showCommandPalette };
    case "SET_SHOW_TIMELINE":
      return { ...state, showTimeline: action.payload };
    case "TOGGLE_TIMELINE":
      return { ...state, showTimeline: !state.showTimeline };
    case "SET_SHOW_CONTROL_PANEL": {
      instanceStorage.setItem("zone-control-panel", String(action.payload));
      return { ...state, showControlPanel: action.payload };
    }
    case "TOGGLE_CONTROL_PANEL": {
      const next = !state.showControlPanel;
      instanceStorage.setItem("zone-control-panel", String(next));
      return { ...state, showControlPanel: next };
    }
    case "SET_CONTROL_PANEL_COLLAPSED":
      return { ...state, controlPanelCollapsed: action.payload };
    case "TOGGLE_CONTROL_PANEL_COLLAPSED":
      return { ...state, controlPanelCollapsed: !state.controlPanelCollapsed };
    case "SET_SELECTED_ZONES":
      return { ...state, selectedZones: action.payload };
    case "TOGGLE_ZONE_SELECTION": {
      const next = new Set(state.selectedZones);
      if (next.has(action.payload)) next.delete(action.payload);
      else next.add(action.payload);
      return { ...state, selectedZones: next };
    }
    case "CLEAR_SELECTION":
      return { ...state, selectedZones: new Set() };
    case "SET_OUTPUT_SEARCH":
      return { ...state, outputSearch: action.payload };
    case "SET_SHOW_OUTPUT_SEARCH":
      if (!action.payload) {
        return { ...state, showOutputSearch: false, outputSearch: "" };
      }
      return { ...state, showOutputSearch: true };
    case "TOGGLE_OUTPUT_SEARCH":
      if (state.showOutputSearch) {
        return { ...state, showOutputSearch: false, outputSearch: "" };
      }
      return { ...state, showOutputSearch: true };
    case "SET_SWAP_SOURCE":
      return { ...state, swapSource: action.payload };
    case "RESET_RATIOS":
      return { ...state, resetRatiosKey: state.resetRatiosKey + 1 };
    case "SET_AUTO_LAYOUT": {
      instanceStorage.setItem("zone-auto-layout", String(action.payload));
      return { ...state, autoLayout: action.payload };
    }
    case "TOGGLE_AUTO_LAYOUT": {
      const next = !state.autoLayout;
      instanceStorage.setItem("zone-auto-layout", String(next));
      return { ...state, autoLayout: next };
    }
    case "SET_FOCUS_MODE": {
      instanceStorage.setItem("zone-focus-mode", String(action.payload));
      return { ...state, focusMode: action.payload };
    }
    case "TOGGLE_FOCUS_MODE": {
      const next = !state.focusMode;
      instanceStorage.setItem("zone-focus-mode", String(next));
      return { ...state, focusMode: next };
    }
    default:
      return state;
  }
}

function createInitialState(): UIState {
  return {
    viewMode: "auto",
    showShortcutsOverlay: false,
    showCommandPalette: false,
    showTimeline: false,
    showControlPanel: instanceStorage.getItem("zone-control-panel") === "true",
    controlPanelCollapsed: false,
    selectedZones: new Set(),
    outputSearch: "",
    showOutputSearch: false,
    swapSource: null,
    resetRatiosKey: 0,
    autoLayout: instanceStorage.getItem("zone-auto-layout") !== "false",
    focusMode: instanceStorage.getItem("zone-focus-mode") === "true",
  };
}

export function useUIState() {
  const [state, dispatch] = useReducer(uiReducer, undefined, createInitialState);

  const toggleFocusMode = useCallback(() => dispatch({ type: "TOGGLE_FOCUS_MODE" }), []);
  const toggleAutoLayout = useCallback(() => dispatch({ type: "TOGGLE_AUTO_LAYOUT" }), []);
  const cycleViewMode = useCallback(() => dispatch({ type: "CYCLE_VIEW_MODE" }), []);

  return { state, dispatch, toggleFocusMode, toggleAutoLayout, cycleViewMode } as const;
}

export type { UIState, UIAction };
