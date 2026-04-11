// FILE: src/lib/ui-bridge/UIBridgeHooks.tsx
/**
 * UIBridgeHooks — Global UI Bridge hook registrations.
 *
 * Renders nothing. Place inside the UIBridgeProvider tree so that all
 * hooks can talk to the bridge context.
 *
 * NOTE: usePageContext is already called by <RunnerPageContext> in App.tsx,
 * so we do NOT duplicate it here. This file covers route awareness,
 * state-machine, keyboard shortcuts, and annotations only.
 */

import { useMemo } from "react";
import {
  useRouteAwareness,
  useUIState,
  useUITransition,
  useUIStateGroup,
  useKeyboardShortcuts,
  type ShortcutDef,
} from "ui-bridge";

// ---------------------------------------------------------------------------
// Props — the host component (AppContent) passes live state so hooks can
// reference real values without pulling in unrelated contexts.
// ---------------------------------------------------------------------------
export interface UIBridgeHooksProps {
  activeTab: string;
  sidebarCollapsed: boolean;
  isActionModalOpen: boolean;
  isImageModalOpen: boolean;
  showLogSourcePicker: boolean;
  executionActive: boolean;
  /** Pass true when the tutorial overlay is visible */
  tutorialOpen?: boolean;
  /** Pass true when the workflow builder save dialog is open */
  workflowSaveDialogOpen?: boolean;
}

// -------------------------------------------------------------------------
// Main navigation tab groups — aligned with RunnerPageContext sections.
// -------------------------------------------------------------------------
const TAB_GROUPS = {
  run: ["gui-automation", "workflow-queue", "active"],
  observe: [
    "runs",
    "history",
    "run-recap",
    "run-actions",
    "run-image",
    "run-findings",
    "run-state-explorer",
    "run-tests",
    "run-ai-output",
    "run-statistics",
    "run-ai-data",
    "run-traces",
  ],
  build: [
    "unified-workflow-builder",
    "step-builders",
    "library",
    "capture",
    "check-builder",
    "check-group-builder",
    "shell-command-builder",
    "task-builder",
    "context-builder",
    "playwright-test-builder",
  ],
  configure: ["config-findings", "config-hooks", "config-log-sources", "triggers", "tasks"],
  tools: ["terminal", "specs", "state-machine", "generator-eval", "config-ui-bridge"],
  system: [
    "settings",
    "settings-account",
    "settings-ai",
    "settings-agentic",
    "settings-self-healing",
    "settings-world-state-verifier",
    "settings-playwright",
    "settings-mobile",
    "settings-cloud-relay",
    "settings-mcp",
    "settings-log-sources",
    "settings-execution-variables",
    "settings-general",
    "settings-storage",
    "settings-backup",
    "settings-instances",
    "settings-debug",
    "settings-updates",
    "help",
    "error-monitor",
    "processes",
    "reflection",
    "architecture",
  ],
} satisfies Record<string, readonly string[]>;

/** All section state IDs — used as fromStates for cross-section transitions. */
const ANY_SECTION = [
  "section-run",
  "section-observe",
  "section-build",
  "section-configure",
  "section-tools",
  "section-system",
] as const;

export function UIBridgeHooks({
  activeTab,
  sidebarCollapsed,
  isActionModalOpen,
  isImageModalOpen,
  showLogSourcePicker,
  executionActive,
  tutorialOpen = false,
  workflowSaveDialogOpen = false,
}: UIBridgeHooksProps) {
  // -------------------------------------------------------------------------
  // 0. Route Awareness (tab-based navigation — no React Router)
  // -------------------------------------------------------------------------

  const routeInfo = useMemo(() => {
    // Derive section from TAB_GROUPS
    let section: string | undefined;
    for (const [group, tabs] of Object.entries(TAB_GROUPS)) {
      if (tabs.includes(activeTab) || (group === "system" && activeTab.startsWith("settings"))) {
        section = group;
        break;
      }
    }

    return {
      pattern: `/${activeTab}`,
      params: {} as Record<string, string>,
      queryParams: {} as Record<string, string>,
      routeStack: section ? [`/${section}`, `/${activeTab}`] : [`/${activeTab}`],
    };
  }, [activeTab]);

  useRouteAwareness(routeInfo);

  // -------------------------------------------------------------------------
  // 1. UI States
  // -------------------------------------------------------------------------

  // Sidebar collapsed / expanded
  useUIState({
    id: "sidebar-collapsed",
    name: "Sidebar Collapsed",
    activeWhen: () => sidebarCollapsed,
    group: "sidebar",
    metadata: { persistent: true },
  });

  useUIState({
    id: "sidebar-expanded",
    name: "Sidebar Expanded",
    activeWhen: () => !sidebarCollapsed,
    group: "sidebar",
  });

  useUIStateGroup({
    id: "sidebar",
    name: "Sidebar Visibility",
    states: ["sidebar-collapsed", "sidebar-expanded"],
  });

  // Action detail modal (blocking)
  useUIState({
    id: "action-detail-modal",
    name: "Action Detail Modal",
    activeWhen: () => isActionModalOpen,
    blocking: true,
  });

  // Image detail modal (blocking)
  useUIState({
    id: "image-detail-modal",
    name: "Image Detail Modal",
    activeWhen: () => isImageModalOpen,
    blocking: true,
  });

  // Log source picker modal (blocking)
  useUIState({
    id: "log-source-picker-modal",
    name: "Log Source Picker",
    activeWhen: () => showLogSourcePicker,
    blocking: true,
  });

  // Tutorial overlay (blocking — covers the entire screen)
  useUIState({
    id: "tutorial-overlay",
    name: "Tutorial Overlay",
    activeWhen: () => tutorialOpen,
    blocking: true,
    metadata: { category: "onboarding" },
  });

  // Workflow builder save dialog (blocking)
  useUIState({
    id: "workflow-save-dialog",
    name: "Workflow Save Dialog",
    activeWhen: () => workflowSaveDialogOpen,
    blocking: true,
    metadata: { category: "workflow-builder" },
  });

  // Execution active / idle states
  useUIState({
    id: "execution-active",
    name: "Workflow Execution Active",
    activeWhen: () => executionActive,
    metadata: { category: "execution" },
  });

  useUIState({
    id: "execution-idle",
    name: "Workflow Execution Idle",
    activeWhen: () => !executionActive,
    metadata: { category: "execution" },
  });

  useUIStateGroup({
    id: "execution-status",
    name: "Execution Status",
    states: ["execution-active", "execution-idle"],
  });

  // -------------------------------------------------------------------------
  // Section states — one for each navigation section
  // -------------------------------------------------------------------------

  useUIState({
    id: "section-run",
    name: "Run Section Active",
    activeWhen: () => TAB_GROUPS.run.includes(activeTab),
    group: "nav-section",
  });

  useUIState({
    id: "section-observe",
    name: "Observe Section Active",
    activeWhen: () => TAB_GROUPS.observe.includes(activeTab),
    group: "nav-section",
  });

  useUIState({
    id: "section-build",
    name: "Build Section Active",
    activeWhen: () => TAB_GROUPS.build.includes(activeTab),
    group: "nav-section",
  });

  useUIState({
    id: "section-configure",
    name: "Configure Section Active",
    activeWhen: () => TAB_GROUPS.configure.includes(activeTab),
    group: "nav-section",
  });

  useUIState({
    id: "section-tools",
    name: "Tools Section Active",
    activeWhen: () => TAB_GROUPS.tools.includes(activeTab),
    group: "nav-section",
  });

  useUIState({
    id: "section-system",
    name: "System Section Active",
    activeWhen: () => TAB_GROUPS.system.includes(activeTab) || activeTab.startsWith("settings"),
    group: "nav-section",
  });

  useUIStateGroup({
    id: "nav-section",
    name: "Navigation Sections",
    states: [
      "section-run",
      "section-observe",
      "section-build",
      "section-configure",
      "section-tools",
      "section-system",
    ],
  });

  // -------------------------------------------------------------------------
  // Per-tab states (key tabs that are useful for automation targeting)
  // -------------------------------------------------------------------------

  useUIState({
    id: "tab-terminal",
    name: "Terminal Tab Active",
    activeWhen: () => activeTab === "terminal",
    group: "active-tab",
  });

  useUIState({
    id: "tab-active-dashboard",
    name: "Active Dashboard Tab",
    activeWhen: () => activeTab === "active",
    group: "active-tab",
  });

  useUIState({
    id: "tab-workflow-builder",
    name: "Workflow Builder Tab",
    activeWhen: () => activeTab === "unified-workflow-builder",
    group: "active-tab",
  });

  useUIState({
    id: "tab-gui-automation",
    name: "Execute Tab",
    activeWhen: () => activeTab === "gui-automation",
    group: "active-tab",
  });

  useUIState({
    id: "tab-history",
    name: "History Tab",
    activeWhen: () => activeTab === "runs" || activeTab === "history",
    group: "active-tab",
  });

  useUIState({
    id: "tab-specs",
    name: "Specs Tab",
    activeWhen: () => activeTab === "specs",
    group: "active-tab",
  });

  useUIState({
    id: "tab-error-monitor",
    name: "Error Monitor Tab",
    activeWhen: () => activeTab === "error-monitor",
    group: "active-tab",
  });

  useUIState({
    id: "tab-settings",
    name: "Settings Tab",
    activeWhen: () => activeTab.startsWith("settings"),
    group: "active-tab",
  });

  useUIState({
    id: "tab-library",
    name: "Library Tab",
    activeWhen: () => activeTab === "library",
    group: "active-tab",
  });

  useUIState({
    id: "tab-workflow-queue",
    name: "Workflow Queue Tab",
    activeWhen: () => activeTab === "workflow-queue",
    group: "active-tab",
  });

  useUIState({
    id: "tab-capture",
    name: "Capture Tab",
    activeWhen: () => activeTab === "capture",
    group: "active-tab",
  });

  useUIState({
    id: "tab-processes",
    name: "Process Manager Tab",
    activeWhen: () => activeTab === "processes",
    group: "active-tab",
  });

  useUIState({
    id: "tab-reflection",
    name: "Reflection Dashboard Tab",
    activeWhen: () => activeTab === "reflection",
    group: "active-tab",
  });

  useUIState({
    id: "tab-run-recap",
    name: "Run Recap Tab",
    activeWhen: () => activeTab === "run-recap",
    group: "active-tab",
  });

  useUIState({
    id: "tab-run-ai-output",
    name: "AI Output Tab",
    activeWhen: () => activeTab === "run-ai-output",
    group: "active-tab",
  });

  useUIState({
    id: "tab-state-machine",
    name: "UI Bridge State Machine Tab",
    activeWhen: () => activeTab === "state-machine",
    group: "active-tab",
  });

  useUIState({
    id: "tab-architecture",
    name: "Architecture View Tab",
    activeWhen: () => activeTab === "architecture",
    group: "active-tab",
  });

  useUIState({
    id: "tab-config-ui-bridge",
    name: "UI Bridge Integration Tab",
    activeWhen: () => activeTab === "config-ui-bridge",
    group: "active-tab",
  });

  useUIState({
    id: "tab-generator-eval",
    name: "Generator Eval Tab",
    activeWhen: () => activeTab === "generator-eval",
    group: "active-tab",
  });

  // -------------------------------------------------------------------------
  // 2. Transitions
  // -------------------------------------------------------------------------

  // Modal open/close transitions
  useUITransition({
    id: "open-action-modal",
    name: "Open Action Detail Modal",
    fromStates: ["section-observe"],
    activateStates: ["action-detail-modal"],
    exitStates: [],
  });

  useUITransition({
    id: "close-action-modal",
    name: "Close Action Detail Modal",
    fromStates: ["action-detail-modal"],
    activateStates: [],
    exitStates: ["action-detail-modal"],
  });

  useUITransition({
    id: "open-image-modal",
    name: "Open Image Detail Modal",
    fromStates: ["section-observe"],
    activateStates: ["image-detail-modal"],
    exitStates: [],
  });

  useUITransition({
    id: "close-image-modal",
    name: "Close Image Detail Modal",
    fromStates: ["image-detail-modal"],
    activateStates: [],
    exitStates: ["image-detail-modal"],
  });

  useUITransition({
    id: "open-log-source-picker",
    name: "Open Log Source Picker",
    fromStates: [...ANY_SECTION],
    activateStates: ["log-source-picker-modal"],
    exitStates: [],
  });

  useUITransition({
    id: "close-log-source-picker",
    name: "Close Log Source Picker",
    fromStates: ["log-source-picker-modal"],
    activateStates: [],
    exitStates: ["log-source-picker-modal"],
  });

  useUITransition({
    id: "toggle-sidebar",
    name: "Toggle Sidebar",
    fromStates: ["sidebar-collapsed", "sidebar-expanded"],
    activateStates: [],
    exitStates: [],
  });

  useUITransition({
    id: "open-tutorial",
    name: "Open Tutorial Overlay",
    fromStates: [...ANY_SECTION],
    activateStates: ["tutorial-overlay"],
    exitStates: [],
  });

  useUITransition({
    id: "close-tutorial",
    name: "Close Tutorial Overlay",
    fromStates: ["tutorial-overlay"],
    activateStates: [],
    exitStates: ["tutorial-overlay"],
  });

  useUITransition({
    id: "start-execution",
    name: "Start Workflow Execution",
    fromStates: ["execution-idle"],
    activateStates: ["execution-active"],
    exitStates: ["execution-idle"],
  });

  useUITransition({
    id: "stop-execution",
    name: "Stop Workflow Execution",
    fromStates: ["execution-active"],
    activateStates: ["execution-idle"],
    exitStates: ["execution-active"],
  });

  useUITransition({
    id: "open-workflow-save-dialog",
    name: "Open Workflow Save Dialog",
    fromStates: ["tab-workflow-builder"],
    activateStates: ["workflow-save-dialog"],
    exitStates: [],
  });

  useUITransition({
    id: "close-workflow-save-dialog",
    name: "Close Workflow Save Dialog",
    fromStates: ["workflow-save-dialog"],
    activateStates: [],
    exitStates: ["workflow-save-dialog"],
  });

  // Navigation transitions
  useUITransition({
    id: "navigate-to-terminal",
    name: "Navigate to Terminal",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-terminal"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-active-dashboard",
    name: "Navigate to Active Dashboard",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-active-dashboard"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-gui-automation",
    name: "Navigate to Execute Tab",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-gui-automation"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-workflow-builder",
    name: "Navigate to Workflow Builder",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-workflow-builder"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-history",
    name: "Navigate to History",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-history"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-library",
    name: "Navigate to Library",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-library"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-run-recap",
    name: "Navigate to Run Recap",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-run-recap"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-specs",
    name: "Navigate to Specs",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-specs"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-error-monitor",
    name: "Navigate to Error Monitor",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-error-monitor"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-settings",
    name: "Navigate to Settings",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-settings"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-state-machine",
    name: "Navigate to UI Bridge State Machine",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-state-machine"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-config-ui-bridge",
    name: "Navigate to UI Bridge Integration",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-config-ui-bridge"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-architecture",
    name: "Navigate to Architecture View",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-architecture"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-processes",
    name: "Navigate to Process Manager",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-processes"],
    exitStates: [],
  });

  useUITransition({
    id: "navigate-to-workflow-queue",
    name: "Navigate to Workflow Queue",
    fromStates: [...ANY_SECTION],
    activateStates: ["tab-workflow-queue"],
    exitStates: [],
  });

  // -------------------------------------------------------------------------
  // 3. Keyboard Shortcuts
  // -------------------------------------------------------------------------

  const shortcuts = useMemo<ShortcutDef[]>(
    () => [
      // ---- Terminal Page shortcuts ----
      {
        combo: "Ctrl+Shift+T",
        description: "Create new terminal session",
        scope: "terminal",
        category: "Terminal Management",
      },
      {
        combo: "Ctrl+Shift+W",
        description: "Close focused terminal session",
        scope: "terminal",
        category: "Terminal Management",
      },
      {
        combo: "Ctrl+Tab",
        description: "Next zone or next tab",
        scope: "terminal",
        category: "Navigation",
      },
      {
        combo: "Ctrl+Shift+Tab",
        description: "Previous zone or previous tab",
        scope: "terminal",
        category: "Navigation",
      },
      {
        combo: "Ctrl+Shift+N",
        description: "Jump to next session needing input",
        scope: "terminal",
        category: "Navigation",
      },
      {
        combo: "Ctrl+Shift+F",
        description: "Maximize or restore focused zone",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+M",
        description: "Cycle view mode (auto/full/compact)",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+A",
        description: "Toggle auto-focus on needs-input",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+S",
        description: "Toggle sound notification",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+Enter",
        description: "Approve all waiting sessions",
        scope: "terminal",
        category: "Batch Actions",
      },
      {
        combo: "Ctrl+Shift+1",
        description: "Layout: Single zone",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+2",
        description: "Layout: 2 zones side-by-side",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+3",
        description: "Layout: 3 zones",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+4",
        description: "Layout: 4 zones grid",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+5",
        description: "Layout: 5 zones",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+6",
        description: "Layout: 6 zones",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+7",
        description: "Layout: 7 zones",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+8",
        description: "Layout: 8 zones",
        scope: "terminal",
        category: "Layouts",
      },
      { combo: "Ctrl+1", description: "Focus zone 1", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+2", description: "Focus zone 2", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+3", description: "Focus zone 3", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+4", description: "Focus zone 4", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+5", description: "Focus zone 5", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+6", description: "Focus zone 6", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+7", description: "Focus zone 7", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+8", description: "Focus zone 8", scope: "terminal", category: "Navigation" },
      { combo: "Ctrl+9", description: "Focus zone 9", scope: "terminal", category: "Navigation" },
      {
        combo: "Ctrl+Shift+X",
        description: "Zone swap (press twice: source then target)",
        scope: "terminal",
        category: "Actions",
      },
      {
        combo: "Ctrl+Shift+/",
        description: "Toggle output search",
        scope: "terminal",
        category: "Search",
      },
      {
        combo: "Ctrl+Shift+O",
        description: "Toggle pin on focused zone",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+D",
        description: "Toggle focus mode (dim non-focused zones)",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+R",
        description: "Restart focused zone (completed/error only)",
        scope: "terminal",
        category: "Actions",
      },
      {
        combo: "Ctrl+Shift+L",
        description: "Cycle through layout presets",
        scope: "terminal",
        category: "Layouts",
      },
      {
        combo: "Ctrl+Shift+K",
        description: "Toggle command palette",
        scope: "terminal",
        category: "Palettes",
      },
      {
        combo: "Ctrl+Shift+I",
        description: "Toggle zone timeline",
        scope: "terminal",
        category: "View",
      },
      {
        combo: "Ctrl+Shift+P",
        description: "Toggle control panel",
        scope: "terminal",
        category: "Panels",
      },
      {
        combo: "Ctrl+Shift+G",
        description: "Cycle through tag filters",
        scope: "terminal",
        category: "Filters",
      },
      {
        combo: "Ctrl+Shift+?",
        description: "Toggle keyboard shortcuts overlay",
        scope: "terminal",
        category: "Help",
      },
      {
        combo: "Ctrl+Shift+Left",
        description: "Go back in focus history",
        scope: "terminal",
        category: "Navigation",
      },
      {
        combo: "Ctrl+Shift+Right",
        description: "Go forward in focus history",
        scope: "terminal",
        category: "Navigation",
      },
      {
        combo: "Escape",
        description: "Cancel swap / clear selection / restore maximized zone / close right panel",
        scope: "terminal",
        category: "Actions",
      },

      // ---- Active Dashboard shortcuts ----
      {
        combo: "?",
        description: "Toggle shortcuts modal",
        scope: "active-dashboard",
        category: "Help",
      },
      {
        combo: "Ctrl+R",
        description: "Refresh dashboard data",
        scope: "active-dashboard",
        category: "Actions",
      },
      {
        combo: "Ctrl+1",
        description: "Switch to widget 1",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+2",
        description: "Switch to widget 2",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+3",
        description: "Switch to widget 3",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+4",
        description: "Switch to widget 4",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+5",
        description: "Switch to widget 5",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+6",
        description: "Switch to widget 6",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+7",
        description: "Switch to widget 7",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Ctrl+8",
        description: "Switch to widget 8",
        scope: "active-dashboard",
        category: "Widget Selection",
      },
      {
        combo: "Escape",
        description: "Dismiss completion summary or shortcuts modal",
        scope: "active-dashboard",
        category: "Actions",
      },
      {
        combo: "Enter",
        description: "Approve first waiting approval request",
        scope: "active-dashboard",
        category: "Approval",
      },

      // ---- Multi-run bar shortcuts ----
      {
        combo: "Ctrl+Tab",
        description: "Cycle to next active run",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "Ctrl+Shift+Tab",
        description: "Cycle to previous active run",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "Ctrl+`",
        description: "Cycle to next active run (alt)",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "ArrowLeft",
        description: "Select previous run",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "ArrowRight",
        description: "Select next run",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "Home",
        description: "Jump to first run",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "End",
        description: "Jump to last run",
        scope: "active-runs",
        category: "Navigation",
      },
      {
        combo: "1-9",
        description: "Quick select run by position",
        scope: "active-runs",
        category: "Navigation",
      },

      // ---- Sidebar navigation shortcuts ----
      {
        combo: "ArrowDown",
        description: "Navigate to next sidebar item",
        scope: "sidebar",
        category: "Navigation",
      },
      {
        combo: "ArrowUp",
        description: "Navigate to previous sidebar item",
        scope: "sidebar",
        category: "Navigation",
      },
      {
        combo: "ArrowRight",
        description: "Expand sidebar item or open flyout",
        scope: "sidebar",
        category: "Navigation",
      },
      {
        combo: "ArrowLeft",
        description: "Collapse sidebar item or close flyout",
        scope: "sidebar",
        category: "Navigation",
      },
      {
        combo: "Enter",
        description: "Activate selected sidebar item",
        scope: "sidebar",
        category: "Navigation",
      },

      // ---- Tutorial overlay shortcuts ----
      {
        combo: "ArrowRight",
        description: "Next tutorial step",
        scope: "tutorial",
        category: "Tutorial Navigation",
      },
      {
        combo: "ArrowLeft",
        description: "Previous tutorial step",
        scope: "tutorial",
        category: "Tutorial Navigation",
      },
      {
        combo: "Escape",
        description: "Close tutorial",
        scope: "tutorial",
        category: "Tutorial Navigation",
      },
      {
        combo: "Enter",
        description: "Complete tutorial (on last step)",
        scope: "tutorial",
        category: "Tutorial Navigation",
      },

      // ---- HTML Viewer modal shortcuts ----
      {
        combo: "Ctrl+F",
        description: "Focus HTML search input",
        scope: "html-viewer",
        category: "Search",
      },

      // ---- Command Palette shortcuts ----
      {
        combo: "ArrowDown",
        description: "Select next command",
        scope: "command-palette",
        category: "Navigation",
      },
      {
        combo: "ArrowUp",
        description: "Select previous command",
        scope: "command-palette",
        category: "Navigation",
      },
      {
        combo: "Enter",
        description: "Execute selected command",
        scope: "command-palette",
        category: "Actions",
      },
      {
        combo: "Escape",
        description: "Close command palette",
        scope: "command-palette",
        category: "Actions",
      },
    ],
    [],
  );

  useKeyboardShortcuts(shortcuts);

  return null;
}
