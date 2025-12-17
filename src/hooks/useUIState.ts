/**
 * useUIState Hook
 *
 * Manages all UI state for dropdowns, modals, panels, and tabs.
 * Centralizes UI state management to avoid cluttering the main component.
 */

import { useState } from "react";

export type LogTab = "general" | "image" | "actions" | "ai" | "rag" | "external" | "settings";

export interface UseUIStateResult {
  // Tab state
  activeLogTab: LogTab;
  setActiveLogTab: (tab: LogTab) => void;

  // Dropdown states
  showWorkflowDropdown: boolean;
  setShowWorkflowDropdown: (show: boolean) => void;
  showLogFilter: boolean;
  setShowLogFilter: (show: boolean) => void;
  showMonitorDropdown: boolean;
  setShowMonitorDropdown: (show: boolean) => void;

  // Panel collapse states
  configPanelCollapsed: boolean;
  setConfigPanelCollapsed: (collapsed: boolean) => void;
  executionPanelCollapsed: boolean;
  setExecutionPanelCollapsed: (collapsed: boolean) => void;

  // Copy success state
  copySuccess: boolean;
  setCopySuccess: (success: boolean) => void;
  showCopySuccessFeedback: () => void;
}

/**
 * Hook for managing UI state (dropdowns, modals, panels, tabs)
 */
export function useUIState(): UseUIStateResult {
  const [activeLogTab, setActiveLogTab] = useState<LogTab>("general");
  const [showWorkflowDropdown, setShowWorkflowDropdown] = useState(false);
  const [showLogFilter, setShowLogFilter] = useState(false);
  const [showMonitorDropdown, setShowMonitorDropdown] = useState(false);
  const [copySuccess, setCopySuccess] = useState(false);
  const [configPanelCollapsed, setConfigPanelCollapsed] = useState(false);
  const [executionPanelCollapsed, setExecutionPanelCollapsed] = useState(false);

  const showCopySuccessFeedback = () => {
    setCopySuccess(true);
    setTimeout(() => setCopySuccess(false), 2000);
  };

  return {
    activeLogTab,
    setActiveLogTab,
    showWorkflowDropdown,
    setShowWorkflowDropdown,
    showLogFilter,
    setShowLogFilter,
    showMonitorDropdown,
    setShowMonitorDropdown,
    configPanelCollapsed,
    setConfigPanelCollapsed,
    executionPanelCollapsed,
    setExecutionPanelCollapsed,
    copySuccess,
    setCopySuccess,
    showCopySuccessFeedback,
  };
}
