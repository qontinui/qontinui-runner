/**
 * App.tsx (Refactored - Sidebar Navigation Layout)
 *
 * Main application component with sidebar navigation:
 *
 * RUN group:
 * - Execute: Configure and start workflows
 * - Active: Real-time monitoring dashboard (GUI + AI)
 * - History: View past runs
 *
 * OBSERVE group:
 * - Logs: View logs (General, Image Recognition, Actions)
 * - AI Output: Full AI session output view
 * - Monitor tabs: Summary, Findings, Issues, etc.
 *
 * BUILD group:
 * - Library: Unified asset library
 * - Workflow/Script builders
 * - Capture: Screenshot capture
 *
 * Other: Configure, Schedule, System
 */

import { useEffect, useState, useCallback, createContext, useContext, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Zap,
  Image,
  ClipboardList,
  ClipboardCheck,
  FileText,
  FileSearch,
  Bot,
  BarChart3,
  Database,
  TestTube,
  Accessibility,
  Cloud,
  Puzzle,
} from "lucide-react";

// Contexts
import {
  ExecutionProvider,
  useExecution,
  EventManagerProvider,
  AutoContinueProvider,
} from "./contexts";
import { RenderLogWrapper } from "./lib/ui-bridge";
import { AuthProvider, useAuth } from "./components/AuthProvider";
import { TutorialProvider } from "./contexts/TutorialContext";
import { ContextualTutorial } from "./components/tutorial";

// UI Bridge for AI-driven UI automation
import { UIBridgeProvider, AutoRegisterProvider } from "ui-bridge";

// Navigation context for tutorials to navigate to pages
interface NavigationContextValue {
  navigate: (page: string) => void;
  registerNavigate: (fn: (page: string) => void) => void;
}

const NavigationContext = createContext<NavigationContextValue | null>(null);

function NavigationProvider({ children }: { children: React.ReactNode }) {
  const navigateFnRef = useRef<((page: string) => void) | null>(null);

  const navigate = useCallback((page: string) => {
    if (navigateFnRef.current) {
      navigateFnRef.current(page);
    }
  }, []);

  const registerNavigate = useCallback((fn: (page: string) => void) => {
    navigateFnRef.current = fn;
  }, []);

  return (
    <NavigationContext.Provider value={{ navigate, registerNavigate }}>
      {children}
    </NavigationContext.Provider>
  );
}

function useNavigation() {
  const context = useContext(NavigationContext);
  if (!context) {
    throw new Error("useNavigation must be used within NavigationProvider");
  }
  return context;
}

// Managers
import { setupEventHandlers, eventRouter } from "./managers";

// Hooks
import {
  useActionLogView,
  useLogManager,
  useUIState,
  useModalState,
  useLogFilter,
  useProjectSelection,
  useProjectLogs,
  useWebSocketAutoConnect,
  useBackgroundActivities,
  UIBridgeEventHandler,
} from "./hooks";
import { useGlobalLogSources } from "./hooks/useGlobalLogSources";

// Components
import StatusIndicator from "./components/StatusIndicator";
import { ConfigurationPanel as _ConfigurationPanel } from "./components/ConfigurationPanel";
import { ExecutionControlPanel as _ExecutionControlPanel } from "./components/ExecutionControlPanel";
import { LogsTab } from "./components/LogsTab";
import { CaptureTab } from "./components/CaptureTab";
import ActionDetailModal from "./components/ActionDetailModal";
import ImageDetailModal from "./components/ImageDetailModal";
import { Settings } from "./components/Settings";
import { LoginScreen } from "./components/LoginScreen";
import { LogSourceManager } from "./components/LogSourceManager";
import { AiTab } from "./components/AiTab";
import { LibraryDashboard } from "./components/LibraryDashboard";
import { HelpTab } from "./components/HelpTab";
import { SchedulerTab } from "./components/scheduler";
import { Sidebar } from "./components/navigation";
import { WorkflowBuilderTab } from "./components/workflow-builder";
import { ScriptsPage } from "./components/ScriptsPage";
import { MacroBuilderTab } from "./components/macro-builder";
import { TestBuilderTab } from "./components/test-builder";
import { CheckBuilderTab } from "./components/check-builder";
import { ShellCommandBuilderTab } from "./components/shell-command-builder";
import { TaskBuilderTab } from "./components/TaskBuilderTab";
import { ContextBuilderTab } from "./components/ContextBuilderTab";
import { StateExplorerBuilderTab } from "./components/StateExplorerBuilderTab";
import { ApiRequestBuilderTab } from "./components/ApiRequestBuilderTab";
import { AwasBuilderTab } from "./components/AwasBuilderTab";
import { ActiveDashboardPage } from "./components/active-dashboard";
import { HistoryTab } from "./components/HistoryTab";
import { ExecuteTab } from "./components/ExecuteTab";
// Monitor/Observe components
import { ExecutionReport } from "./components/findings";
import { StateExplorerTab } from "./components/state-explorer";
import { TestResultsTab } from "./components/test-results";
import { StatisticsTab } from "./components/statistics";
import { DiscoverySyncPanel } from "./components/discoveries";
// Run-specific components
import { RunSelectionProvider } from "./contexts/RunSelectionContext";
import { RunPageLayout } from "./components/run-dashboard/RunPageLayout";
import { RunActionsTab } from "./components/run-logs/RunActionsTab";
import { RunImageRecognitionTab } from "./components/run-logs/RunImageRecognitionTab";
import { AiDataViewerTab } from "./components/run-logs/AiDataViewerTab";
import { RunRecapTab } from "./components/run-recap";
import { useTaskRuns } from "./hooks/useAiData";
// Accessibility components
import { AccessibilityExplorerPanel } from "./components/accessibility";
// UI Bridge Inspector
import { ConnectedUIBridgeInspector } from "./components/ui-bridge";
// New dashboard components
import { LearningDashboard } from "./components/learning-dashboard";
import { CheckpointBrowser } from "./components/checkpoint-browser";
import { FlowDesigner } from "./components/flow-designer";
// Configure components
import { ExternalLogsTab } from "./components/ExternalLogsTab";
import { CategoryManager } from "./components/findings/CategoryManager";
import { HooksManagerPanel } from "./components/hooks";

// Styles
import "./index.css";

type LogSubTab = "general" | "image" | "actions";

// Valid main tab IDs for the sidebar navigation
type MainTabId =
  | "run"
  | "active"
  | "history"
  // Observe group - new structure
  | "run-recap"
  | "run-actions"
  | "run-image"
  | "run-findings"
  | "run-state-explorer"
  | "run-tests"
  | "run-ai-output"
  | "run-statistics"
  | "run-ai-data"
  | "run-accessibility"
  | "run-ui-bridge"
  | "learning"
  | "checkpoints"
  | "discoveries"
  | "flow-designer"
  // Legacy tab IDs for migration
  | "ai"
  | "logs"
  | "run-summary" // Legacy: migrates to run-recap
  | "monitor-summary" // Legacy: migrates to run-recap
  | "monitor-findings"
  | "monitor-issues"
  | "monitor-learnings"
  | "monitor-state-explorer"
  | "monitor-statistics"
  | "monitor-discoveries"
  | "library"
  | "task-builder"
  | "unified-workflow-builder"
  | "macro-builder"
  | "script-builder"
  | "context-builder"
  | "state-explorer-builder"
  | "api-request-builder"
  | "awas-builder"
  | "test-builder"
  | "check-builder"
  | "shell-command-builder"
  | "capture"
  | "config-log-sources"
  | "config-findings"
  | "config-hooks"
  | "tasks"
  | "settings"
  | "settings-account"
  | "settings-ai"
  | "settings-agentic"
  | "settings-self-healing"
  | "settings-playwright"
  | "settings-mobile"
  | "settings-mcp"
  | "settings-log-sources"
  | "settings-general"
  | "settings-storage"
  | "settings-backup"
  | "settings-debug"
  | "settings-updates"
  | "help";

const VALID_TAB_IDS: MainTabId[] = [
  "run",
  "active",
  "history",
  // New observe tabs
  "run-recap",
  "run-actions",
  "run-image",
  "run-findings",
  "run-state-explorer",
  "run-tests",
  "run-ai-output",
  "run-statistics",
  "run-ai-data",
  "run-accessibility",
  "run-ui-bridge",
  "learning",
  "checkpoints",
  "discoveries",
  "flow-designer",
  // Legacy (for migration)
  "ai",
  "logs",
  "run-summary", // Legacy: migrates to run-recap
  "monitor-summary", // Legacy: migrates to run-recap
  "monitor-findings",
  "monitor-issues",
  "monitor-learnings",
  "monitor-state-explorer",
  "monitor-statistics",
  "monitor-discoveries",
  "library",
  "task-builder",
  "unified-workflow-builder",
  "macro-builder",
  "script-builder",
  "context-builder",
  "state-explorer-builder",
  "api-request-builder",
  "awas-builder",
  "test-builder",
  "check-builder",
  "shell-command-builder",
  "capture",
  "config-log-sources",
  "config-findings",
  "config-hooks",
  "tasks",
  "settings",
  "settings-account",
  "settings-ai",
  "settings-agentic",
  "settings-self-healing",
  "settings-playwright",
  "settings-mobile",
  "settings-mcp",
  "settings-log-sources",
  "settings-general",
  "settings-storage",
  "settings-backup",
  "settings-debug",
  "settings-updates",
  "help",
];

const MAIN_TAB_STORAGE_KEY = "qontinui-main-active-tab";
const SIDEBAR_COLLAPSED_KEY = "qontinui-sidebar-collapsed";

/**
 * Maps old tab IDs to new tab IDs for localStorage migration
 */
function migrateTabId(stored: string | null): MainTabId {
  if (!stored) return "run";

  // Map old tab names to new ones
  const migrations: Record<string, MainTabId> = {
    "ai-workflows": "run-ai-output",
    "ai-builder": "unified-workflow-builder",
    builder: "unified-workflow-builder",
    prompts: "library",
    scripts: "script-builder",
    contexts: "library",
    scheduler: "tasks",
    dataset: "capture", // Dataset is now part of capture
    extract: "capture",
    // Old observe tab migrations to new structure
    logs: "run-recap",
    "run-dashboard": "run-recap", // Dashboard merged into Summary (formerly Recap)
    ai: "run-ai-output",
    "run-summary": "run-recap", // Summary merged into Recap
    "monitor-summary": "run-recap", // Summary merged into Recap
    "monitor-findings": "run-findings",
    "monitor-issues": "run-findings", // Issues merged into Findings
    "monitor-learnings": "run-recap", // Learnings removed, map to recap
    "monitor-verification": "run-state-explorer",
    "monitor-state-explorer": "run-state-explorer",
    "monitor-statistics": "run-statistics",
    "monitor-discoveries": "discoveries",
    // Legacy monitor tab migrations
    monitor: "run-recap", // Map to recap
    issues: "run-findings", // Issues merged into Findings
    "run-issues": "run-findings", // Issues merged into Findings
    learnings: "run-recap", // Map to recap
    verification: "run-state-explorer",
    "run-verification": "run-state-explorer",
    "run-exploration": "run-state-explorer",
    "verification-builder": "state-explorer-builder",
    statistics: "run-statistics",
    // Configure tab migrations
    "log-sources": "config-log-sources",
    "log-locations": "config-log-sources",
  };

  if (stored in migrations) {
    return migrations[stored];
  }

  // Check if it's already a valid new tab ID
  if (VALID_TAB_IDS.includes(stored as MainTabId)) {
    return stored as MainTabId;
  }

  return "run";
}

/**
 * Main app content (inside providers)
 */
function AppContent() {
  // Auth state from context
  const auth = useAuth();

  // Execution state from context
  const execution = useExecution();

  // Get recent task runs for "View Last Run" feature
  const { data: recentTaskRuns = [] } = useTaskRuns(1);
  const lastRun = recentTaskRuns.length > 0 ? recentTaskRuns[0] : null;
  const lastRunWorkflowName = lastRun?.workflow_name ?? lastRun?.task_name ?? null;

  // Navigation context for tutorials
  const { registerNavigate } = useNavigation();

  // Main tab state
  const [activeTab, setActiveTab] = useState<MainTabId>(() => {
    const stored = localStorage.getItem(MAIN_TAB_STORAGE_KEY);
    return migrateTabId(stored);
  });

  // Register navigation function for tutorials
  useEffect(() => {
    registerNavigate((page: string) => {
      // Map tutorial focus pages to actual tab IDs
      const pageToTab: Record<string, MainTabId> = {
        run: "run",
        active: "active",
        "unified-workflow-builder": "unified-workflow-builder",
        "macro-builder": "macro-builder",
        "script-builder": "script-builder",
        "check-builder": "check-builder",
        library: "library",
        ai: "ai",
        settings: "settings",
        help: "help",
      };
      const tabId = pageToTab[page];
      if (tabId) {
        setActiveTab(tabId);
      }
    });
  }, [registerNavigate]);

  // Listen for test-navigation events (for UI testing)
  // Allows Python tests to trigger navigation via HTTP API
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setupNavigationListener = async () => {
      unlisten = await listen<{
        type: string;
        page: string;
        task_run_id?: number;
        select_run?: number;
      }>("test-navigation", (event) => {
        console.log("[APP] Received test-navigation event:", event.payload);
        const { page, task_run_id, select_run } = event.payload;

        // Map page names to tab IDs (same mapping as tutorials)
        const pageToTab: Record<string, MainTabId> = {
          "run": "run",
          "run-recap": "run-recap",
          "run-dashboard": "run-recap", // Dashboard merged into Summary
          "active": "active",
          "history": "history",
          "library": "library",
          "logs": "logs",
          "ai": "ai",
          "settings": "settings",
          "test-builder": "test-builder",
          "unified-workflow-builder": "unified-workflow-builder",
        };

        const tabId = pageToTab[page];
        if (tabId) {
          console.log(`[APP] Navigating to tab: ${tabId}`);
          setActiveTab(tabId);

          // If select_run is provided and we're going to a run-related page,
          // we might need to select the run (this would require RunSelectionContext)
          // For now, just log it - the run selection will need to be done via
          // the RunSelectionContext or a separate mechanism
          if (select_run) {
            console.log(`[APP] Run selection requested: ${select_run}`);
          }
          if (task_run_id) {
            console.log(`[APP] Task run ID provided: ${task_run_id}`);
          }
        } else {
          console.warn(`[APP] Unknown page for navigation: ${page}`);
        }
      });
    };

    setupNavigationListener();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  // Script ID to edit (when navigating from Library to Script Builder)
  const [editScriptId, setEditScriptId] = useState<string | null>(null);

  // Workflow ID to edit (when navigating from Library to Workflow Builder)
  const [editWorkflowId, setEditWorkflowId] = useState<string | null>(null);

  // Macro ID to edit (when navigating from Library to Macro Builder)
  const [editMacroId, setEditMacroId] = useState<string | null>(null);

  // Builder edit IDs (when navigating from Library Dashboard to builders)
  const [editTaskId, setEditTaskId] = useState<string | null>(null);
  const [editScriptletId, setEditScriptletId] = useState<string | null>(null);
  const [editContextId, setEditContextId] = useState<string | null>(null);
  const [editExplorationId, setEditExplorationId] = useState<string | null>(null);
  const [editApiRequestId, setEditApiRequestId] = useState<string | null>(null);
  const [editAwasConfigId, setEditAwasConfigId] = useState<string | null>(null);

  // Handle editing a script from Library
  const handleEditScript = useCallback((scriptId: string) => {
    setEditScriptId(scriptId);
    setActiveTab("script-builder");
  }, []);

  // Handle editing a workflow from Library
  const handleEditWorkflow = useCallback((workflowId: string) => {
    setEditWorkflowId(workflowId);
    setActiveTab("unified-workflow-builder");
  }, []);

  // Handle editing a macro from Library
  const handleEditMacro = useCallback((macroId: string) => {
    setEditMacroId(macroId);
    setActiveTab("macro-builder");
  }, []);

  // Handle navigation from Library Dashboard to any builder
  const handleNavigateToBuilder = useCallback(
    (builderTab: string, itemId: string, itemType: string) => {
      // Set the appropriate edit ID based on item type
      switch (itemType) {
        case "task":
          setEditTaskId(itemId);
          break;
        case "scriptlet":
          setEditScriptletId(itemId);
          break;
        case "context":
          setEditContextId(itemId);
          break;
        case "exploration":
        case "state-explorer":
          setEditExplorationId(itemId);
          break;
        case "api-request":
          setEditApiRequestId(itemId);
          break;
        case "awas":
        case "awas-config":
          setEditAwasConfigId(itemId);
          break;
        case "script":
          setEditScriptId(itemId);
          break;
        case "workflow":
        case "unified-workflow":
          setEditWorkflowId(itemId);
          break;
        case "macro":
          setEditMacroId(itemId);
          break;
      }
      // Navigate to the builder tab
      setActiveTab(builderTab as MainTabId);
    },
    [],
  );

  // Clear script ID when navigating away from script builder
  useEffect(() => {
    if (activeTab !== "script-builder") {
      setEditScriptId(null);
    }
  }, [activeTab]);

  // Clear workflow ID when navigating away from workflow builder
  useEffect(() => {
    if (activeTab !== "unified-workflow-builder") {
      setEditWorkflowId(null);
    }
  }, [activeTab]);

  // Clear macro ID when navigating away from macro builder
  useEffect(() => {
    if (activeTab !== "macro-builder") {
      setEditMacroId(null);
    }
  }, [activeTab]);

  // Clear builder edit IDs when navigating away
  useEffect(() => {
    if (activeTab !== "task-builder") setEditTaskId(null);
    if (activeTab !== "script-builder") setEditScriptletId(null);
    if (activeTab !== "context-builder") setEditContextId(null);
    if (activeTab !== "state-explorer-builder") setEditExplorationId(null);
    if (activeTab !== "api-request-builder") setEditApiRequestId(null);
    if (activeTab !== "awas-builder") setEditAwasConfigId(null);
  }, [activeTab]);

  // Sidebar collapsed state
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "true";
  });

  // Log sub-tab state
  const [activeLogSubTab, setActiveLogSubTab] = useState<LogSubTab>("general");

  // Persist main tab
  useEffect(() => {
    localStorage.setItem(MAIN_TAB_STORAGE_KEY, activeTab);
  }, [activeTab]);

  // Logs from LogManager
  const {
    logs,
    imageLogs,
    aiOutputLogs,
    addLog,
    addAiOutputLog,
    clearGeneralLogs,
    clearImageLogs,
    clearAiOutputLogs,
    copyLogs,
    logCount,
    imageLogCount,
    aiOutputLogCount: _aiOutputLogCount,
  } = useLogManager();

  // Action log view
  const {
    viewData: actionLogViewData,
    loading: actionLogLoading,
    error: actionLogError,
    refresh: refreshActionLog,
  } = useActionLogView({
    autoRefreshInterval: execution.executionActive ? 1000 : 0,
  });

  // UI state management
  const uiState = useUIState();

  // Modal state management
  const modalState = useModalState();

  // Log filtering
  const { logLevel, setLogLevel, filteredLogs } = useLogFilter(logs);

  // Project selection (shared between Capture and Settings)
  const projectSelection = useProjectSelection();

  // Project logs (external logs from target application)
  const projectLogs = useProjectLogs();

  // Global log sources (shared across all projects)
  const globalLogSources = useGlobalLogSources();

  // Background activities aggregation
  // Note: Extraction tracking handled internally via executor events
  const { activities: backgroundActivities } = useBackgroundActivities({
    isExtracting: false,
    extractionUrl: undefined,
    extractionProgress: undefined,
  });

  // WebSocket auto-connect (runs at App level to ensure it's always active)
  const webSocket = useWebSocketAutoConnect({
    isAuthenticated: auth.authStatus?.authenticated ?? false,
    selectedProjectId: projectSelection.selectedProjectId,
    onLog: addLog,
  });

  // Log source manager modal state
  const [showLogSourceManager, setShowLogSourceManager] = useState(false);

  // Auto-load projects when authenticated
  useEffect(() => {
    if (auth.authStatus?.authenticated && !auth.loading) {
      console.log("[APP] User authenticated, loading projects");
      projectSelection.loadProjects();
    }
  }, [auth.authStatus?.authenticated, auth.loading]);

  // Load project logs config when a project is selected or on mount if already selected
  useEffect(() => {
    if (projectSelection.selectedProjectId && projectSelection.selectedProjectName) {
      console.log("[APP] Loading project logs for:", projectSelection.selectedProjectName);
      projectLogs.loadConfig(
        projectSelection.selectedProjectId,
        projectSelection.selectedProjectName,
      );
    }
  }, [projectSelection.selectedProjectId, projectSelection.selectedProjectName]);

  // Setup event handlers on mount (ONCE only)
  useEffect(() => {
    console.log("[APP] Setting up event handlers");
    const cleanup = setupEventHandlers(eventRouter, {
      setPythonStatus: execution.setPythonStatus,
      setConfigLoaded: execution.setConfigLoaded,
      setExecutionActive: execution.setExecutionActive,
    });

    return cleanup;
  }, []); // Empty deps - run only on mount to prevent duplicate event handlers

  // Refresh action log when switching to Actions sub-tab
  useEffect(() => {
    if (activeTab === "logs" && activeLogSubTab === "actions") {
      console.log("[TAB_SWITCH] Switched to Actions tab, refreshing action log");
      refreshActionLog();
    }
  }, [activeTab, activeLogSubTab, refreshActionLog]);

  // Event handlers
  const _handleWorkflowSelect = (workflowId: string) => {
    execution.selectWorkflowWithPersistence(workflowId);
    uiState.setShowWorkflowDropdown(false);
  };

  const _handleMonitorSelectionChange = (indices: number[]) => {
    // Use multi-monitor selection with persistence
    if (indices.length > 0) {
      execution.selectMonitorsWithPersistence(indices);
    }
  };

  const handleCopyLogs = async () => {
    let success = false;

    switch (activeLogSubTab) {
      case "general":
        success = await copyLogs("general");
        break;
      case "image":
        success = await copyLogs("image");
        break;
      case "actions":
        success = await copyLogs("actions", { actionLogs: actionLogViewData?.actions });
        break;
    }

    if (success) {
      uiState.showCopySuccessFeedback();
    }
  };

  const clearActionLogs = async () => {
    try {
      await invoke("clear_action_log");
      refreshActionLog();
    } catch (error) {
      console.error("Failed to clear action logs:", error);
    }
  };

  const clearAllLogs = async () => {
    clearGeneralLogs();
    clearImageLogs();
    clearAiOutputLogs();
    await clearActionLogs();
  };

  /**
   * Navigate to Recap page and set session workflow to last run's workflow
   * NOTE: This hook must be defined before conditional returns to satisfy React's rules of hooks
   */
  const handleGoToRecap = useCallback(() => {
    // If we have a last run with a workflow name, try to select it
    if (lastRun?.workflow_name && execution.workflows.length > 0) {
      // Find matching workflow by name
      const matchingWorkflow = execution.workflows.find(
        (w) => w.name === lastRun.workflow_name || w.id === lastRun.workflow_name
      );
      if (matchingWorkflow) {
        execution.selectWorkflowWithPersistence(matchingWorkflow.id);
      }
    }
    // Navigate to recap
    setActiveTab("run-recap");
  }, [lastRun, execution]);

  // Show loading state while checking auth or during dev auto-login
  const isAuthLoading = auth.loading || auth.devAutoLoginPending;
  console.log(
    "[APP] Render - auth.loading:",
    auth.loading,
    "auth.devAutoLoginPending:",
    auth.devAutoLoginPending,
    "auth.authStatus:",
    auth.authStatus,
  );
  if (isAuthLoading) {
    console.log("[APP] Rendering loading state");
    return (
      <div className="min-h-screen bg-background grid-dots flex items-center justify-center">
        <div className="card p-8 text-center space-y-4">
          <div className="inline-block w-12 h-12 border-4 border-primary border-t-transparent rounded-full animate-spin" />
          <p className="text-muted-foreground">
            {auth.devAutoLoginPending ? "Signing in..." : "Checking authentication..."}
          </p>
        </div>
      </div>
    );
  }

  // Show login screen if not authenticated
  if (!auth.authStatus?.authenticated) {
    console.log("[APP] Rendering LoginScreen (not authenticated)");
    return <LoginScreen onLogin={auth.login} />;
  }

  console.log("[APP] Rendering main app (authenticated)");

  // Get last run task_run_id for render logging context
  const lastRunId = lastRun?.id;

  /**
   * Renders the content for the currently active tab
   */
  const renderTabContent = () => {
    switch (activeTab) {
      case "run":
        return <ExecuteTab onLog={addLog} onNavigateToActive={() => setActiveTab("active")} />;

      case "active":
        return (
          <ActiveDashboardPage
            onGoToExecute={() => setActiveTab("run")}
            onGoToRecap={lastRun ? handleGoToRecap : undefined}
            lastRunWorkflowName={lastRunWorkflowName}
          />
        );

      case "history":
        return (
          <HistoryTab
            onNavigateToRun={() => setActiveTab("run")}
            onNavigateToAi={() => setActiveTab("ai")}
          />
        );

      // ========== NEW OBSERVE TABS ==========
      case "run-recap":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="Recap" icon={ClipboardCheck}>
              <div className="h-full overflow-hidden">
                <RunRecapTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-actions":
        return (
          <RunSelectionProvider>
            <RunPageLayout
              title="Actions"
              icon={Zap}
              badgeCount={actionLogViewData?.visible_count || 0}
            >
              <div className="h-full p-4 overflow-hidden">
                <div className="h-full card overflow-hidden">
                  <RunActionsTab
                    actionLogData={actionLogViewData}
                    actionLogLoading={actionLogLoading}
                    actionLogError={actionLogError}
                    onActionRowClick={modalState.openActionModal}
                    actionCount={actionLogViewData?.visible_count || 0}
                  />
                </div>
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-image":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="Image Recognition" icon={Image} badgeCount={imageLogCount}>
              <div className="h-full p-4 overflow-hidden">
                <div className="h-full card overflow-hidden">
                  <RunImageRecognitionTab
                    imageLogs={imageLogs}
                    onImageRowClick={modalState.openImageModal}
                    imageLogCount={imageLogCount}
                  />
                </div>
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      // run-summary is now migrated to run-recap (handled by migrateTabId)

      case "run-findings":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="Findings" icon={FileText}>
              <div className="h-full overflow-hidden">
                <ExecutionReport />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-state-explorer":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="State Explorer" icon={FileSearch}>
              <div className="h-full overflow-hidden">
                <StateExplorerTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-tests":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="Test Results" icon={TestTube}>
              <div className="h-full overflow-hidden">
                <TestResultsTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-ai-output":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="AI Output" icon={Bot}>
              <AiTab
                aiOutputLines={aiOutputLogs}
                onClearAiOutput={clearAiOutputLogs}
                onAddAiOutputLine={(line) =>
                  addAiOutputLog(
                    line.line,
                    line.source,
                    line.actionId,
                    line.sessionId,
                    line.sessionName,
                  )
                }
                onNavigateToLibrary={() => setActiveTab("library")}
              />
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-statistics":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="Statistics" icon={BarChart3}>
              <div className="h-full overflow-hidden">
                <StatisticsTab
                  configId={execution.config?.path ?? null}
                  configName={execution.config?.name}
                />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-ai-data":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="AI Data Viewer" icon={Database}>
              <div className="h-full overflow-hidden">
                <AiDataViewerTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-accessibility":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="Accessibility Explorer" icon={Accessibility}>
              <div className="h-full overflow-hidden">
                <AccessibilityExplorerPanel />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-ui-bridge":
        return (
          <RunSelectionProvider>
            <RunPageLayout title="UI Bridge Inspector" icon={Puzzle}>
              <div className="h-full overflow-hidden p-4">
                <ConnectedUIBridgeInspector />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "discoveries":
        return (
          <div className="h-full overflow-y-auto p-4 space-y-4">
            <div>
              <div className="flex items-center gap-2 mb-2">
                <Cloud className="w-6 h-6 text-blue-500" />
                <h1 className="text-xl font-bold text-white">GUI Discoveries</h1>
              </div>
              <p className="text-sm text-gray-400">
                Detects patterns from GUI automation runs including new UI elements, state transitions,
                timing updates, and flaky behaviors. Requires running GUI automation workflows (not AI-only tasks).
              </p>
            </div>
            <DiscoverySyncPanel />
          </div>
        );

      case "learning":
        return (
          <div className="h-full overflow-hidden">
            <LearningDashboard />
          </div>
        );

      case "checkpoints":
        return (
          <div className="h-full overflow-hidden">
            <CheckpointBrowser />
          </div>
        );

      case "flow-designer":
        return (
          <div className="h-full overflow-hidden">
            <FlowDesigner />
          </div>
        );

      // ========== LEGACY TABS (for backward compatibility) ==========
      case "ai":
        return (
          <AiTab
            aiOutputLines={aiOutputLogs}
            onClearAiOutput={clearAiOutputLogs}
            onAddAiOutputLine={(line) =>
              addAiOutputLog(
                line.line,
                line.source,
                line.actionId,
                line.sessionId,
                line.sessionName,
              )
            }
            onNavigateToLibrary={() => setActiveTab("library")}
          />
        );

      case "logs":
        return (
          <div className="flex-1 flex flex-col min-h-0 p-4 h-full overflow-hidden">
            <div className="flex-1 flex flex-col min-h-0 card overflow-hidden">
              <LogsTab
                logs={logs}
                filteredLogs={filteredLogs}
                logLevel={logLevel}
                onLogLevelChange={setLogLevel}
                showLogFilter={uiState.showLogFilter}
                onToggleLogFilter={uiState.setShowLogFilter}
                imageLogs={imageLogs}
                onImageRowClick={modalState.openImageModal}
                actionLogData={actionLogViewData}
                actionLogLoading={actionLogLoading}
                actionLogError={actionLogError}
                onActionRowClick={modalState.openActionModal}
                logCount={logCount}
                imageLogCount={imageLogCount}
                actionCount={actionLogViewData?.visible_count || 0}
                onClearGeneralLogs={clearGeneralLogs}
                onClearImageLogs={clearImageLogs}
                onClearActionLogs={clearActionLogs}
                onClearAllLogs={clearAllLogs}
                onCopyLogs={handleCopyLogs}
                copySuccess={uiState.copySuccess}
                activeSubTab={activeLogSubTab}
                onSubTabChange={setActiveLogSubTab}
              />
            </div>
          </div>
        );

      case "library":
        return <LibraryDashboard onNavigateToBuilder={handleNavigateToBuilder} onLog={addLog} />;

      case "task-builder":
        return (
          <div className="h-full overflow-hidden">
            <TaskBuilderTab
              onLog={addLog}
              editTaskId={editTaskId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      case "unified-workflow-builder":
        return (
          <div className="h-full overflow-hidden">
            <WorkflowBuilderTab
              editWorkflowId={editWorkflowId}
              onNavigateToActive={() => setActiveTab("active")}
            />
          </div>
        );

      case "macro-builder":
        return (
          <div className="h-full overflow-hidden">
            <MacroBuilderTab editWorkflowId={editMacroId} />
          </div>
        );

      case "script-builder":
        return (
          <div className="h-full overflow-hidden">
            <ScriptsPage
              onLog={addLog}
              editScriptId={editScriptId}
              editScriptletId={editScriptletId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      case "context-builder":
        return (
          <div className="h-full overflow-hidden">
            <ContextBuilderTab
              onLog={addLog}
              editContextId={editContextId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      case "state-explorer-builder":
        return (
          <div className="h-full overflow-hidden">
            <StateExplorerBuilderTab
              onLog={addLog}
              editExplorationId={editExplorationId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      case "api-request-builder":
        return (
          <div className="h-full overflow-hidden">
            <ApiRequestBuilderTab
              onLog={addLog}
              editRequestId={editApiRequestId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      case "awas-builder":
        return (
          <div className="h-full overflow-hidden">
            <AwasBuilderTab
              onLog={addLog}
              editConfigId={editAwasConfigId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      case "test-builder":
        return (
          <div className="h-full overflow-hidden">
            <TestBuilderTab onLog={addLog} />
          </div>
        );

      case "check-builder":
        return (
          <div className="h-full overflow-hidden">
            <CheckBuilderTab onLog={addLog} />
          </div>
        );

      case "shell-command-builder":
        return (
          <div className="h-full overflow-hidden">
            <ShellCommandBuilderTab onLog={addLog} />
          </div>
        );

      // ========== MONITOR TABS ==========
      // monitor-summary is now migrated to run-recap (handled by migrateTabId)

      case "monitor-findings":
        return (
          <div className="h-full overflow-hidden">
            <ExecutionReport />
          </div>
        );

      // monitor-issues removed - merged into Findings

      case "monitor-state-explorer":
        return (
          <div className="h-full overflow-hidden">
            <StateExplorerTab />
          </div>
        );

      case "monitor-statistics":
        return (
          <div className="h-full overflow-hidden">
            <StatisticsTab
              configId={execution.config?.path ?? null}
              configName={execution.config?.name}
            />
          </div>
        );

      case "monitor-discoveries":
        return (
          <div className="h-full overflow-y-auto p-4">
            <DiscoverySyncPanel />
          </div>
        );

      // ========== CONFIGURE TABS ==========
      case "config-log-sources": {
        // Show list of configured global log sources with their paths
        const sources = globalLogSources.settings?.sources || [];

        return (
          <div className="h-full overflow-y-auto p-4">
            <div className="max-w-4xl mx-auto">
              {/* Header */}
              <div className="flex items-center justify-between mb-6">
                <div>
                  <h1 className="text-2xl font-bold">Log Sources</h1>
                  <p className="text-muted-foreground text-sm mt-1">
                    External log files configured for monitoring
                  </p>
                </div>
                <button
                  onClick={() => setActiveTab("settings-log-sources")}
                  className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
                >
                  <FileText className="w-4 h-4" />
                  Configure Sources
                </button>
              </div>

              {/* Sources list */}
              {sources.length === 0 ? (
                <div className="text-center py-12 text-muted-foreground">
                  <FileText className="w-12 h-12 mx-auto mb-4 opacity-50" />
                  <p className="text-lg font-medium mb-2">No Log Sources Configured</p>
                  <p className="text-sm mb-4">
                    Add external log files to monitor your applications
                  </p>
                  <button
                    onClick={() => setActiveTab("settings-log-sources")}
                    className="px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90"
                  >
                    Configure Log Sources
                  </button>
                </div>
              ) : (
                <div className="space-y-3">
                  {sources.map((source) => (
                    <div
                      key={source.id}
                      className="flex items-center gap-4 p-4 bg-card border border-border rounded-lg"
                      style={{ borderLeftWidth: "4px", borderLeftColor: source.color || "#6b7280" }}
                    >
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2 mb-1">
                          <span className="font-medium">{source.name}</span>
                          <span
                            className={`text-xs px-2 py-0.5 rounded ${source.enabled ? "bg-green-500/20 text-green-500" : "bg-muted text-muted-foreground"}`}
                          >
                            {source.enabled ? "Enabled" : "Disabled"}
                          </span>
                          <span className="text-xs px-2 py-0.5 bg-muted text-muted-foreground rounded">
                            {source.category}
                          </span>
                        </div>
                        <p className="text-sm text-muted-foreground truncate" title={source.path}>
                          {source.path}
                        </p>
                        {source.description && (
                          <p className="text-xs text-muted-foreground mt-1">{source.description}</p>
                        )}
                      </div>
                      <div className="text-xs text-muted-foreground">{source.tail_lines} lines</div>
                    </div>
                  ))}
                </div>
              )}

              {/* View logs link */}
              {sources.length > 0 && (
                <div className="mt-6 pt-6 border-t border-border">
                  <p className="text-sm text-muted-foreground">
                    To view log content during workflow runs, use the{" "}
                    <button
                      onClick={() => setActiveTab("run-recap")}
                      className="text-primary hover:underline"
                    >
                      Session Summary
                    </button>
                    .
                  </p>
                </div>
              )}
            </div>
          </div>
        );
      }

      case "config-findings":
        return (
          <div className="h-full overflow-y-auto">
            <CategoryManager onLog={addLog} />
          </div>
        );

      case "config-hooks":
        return (
          <div className="h-full overflow-hidden">
            <HooksManagerPanel />
          </div>
        );

      case "capture":
        return (
          <div className="h-full overflow-y-auto">
            <CaptureTab onLog={addLog} />
          </div>
        );

      case "tasks":
        return <SchedulerTab />;

      case "settings":
      case "settings-account":
      case "settings-ai":
      case "settings-agentic":
      case "settings-self-healing":
      case "settings-playwright":
      case "settings-mobile":
      case "settings-mcp":
      case "settings-log-sources":
      case "settings-execution-variables":
      case "settings-general":
      case "settings-storage":
      case "settings-backup":
      case "settings-debug":
      case "settings-updates": {
        // Map main tab ID to settings sub-tab
        const settingsTabMap: Record<string, string> = {
          settings: "account",
          "settings-account": "account",
          "settings-ai": "ai",
          "settings-agentic": "agentic",
          "settings-self-healing": "self-healing",
          "settings-playwright": "playwright",
          "settings-mobile": "mobile",
          "settings-mcp": "mcp",
          "settings-log-sources": "log-sources",
          "settings-execution-variables": "execution-variables",
          "settings-general": "general",
          "settings-storage": "storage",
          "settings-backup": "backup",
          "settings-debug": "advanced",
          "settings-updates": "updates",
        };
        const defaultSettingsTab = settingsTabMap[activeTab] || "account";

        return (
          <div className="h-full overflow-hidden">
            <Settings
              defaultTab={defaultSettingsTab}
              onLog={addLog}
              onDebugModeChange={async (enabled) => {
                try {
                  await invoke("set_debug_settings", {
                    settings: {
                      enable_image_debug: enabled,
                      top_matches_count: 5,
                    },
                  });
                  addLog("info", `Image debug mode ${enabled ? "enabled" : "disabled"}`);
                } catch (error) {
                  addLog("error", `Failed to set debug mode: ${error}`);
                }
              }}
              projects={projectSelection.projects}
              selectedProjectId={projectSelection.selectedProjectId}
              onProjectSelect={projectSelection.setSelectedProject}
              onLoadProjects={projectSelection.loadProjects}
              webSocketState={webSocket}
            />
          </div>
        );
      }

      case "help":
        return <HelpTab />;

      default:
        return null;
    }
  };

  return (
    <RenderLogWrapper
      activeTab={activeTab}
      taskRunId={lastRunId}
      enableOnMount={true}
      enableMutationObserver={true}
      mutationDebounceMs={500}
    >
      <div className="h-screen w-screen bg-background grid-dots flex flex-col overflow-hidden min-w-[1200px] min-h-[700px]">
        {/* Status Bar - Sticky Top */}
        <StatusIndicator
          pythonStatus={execution.pythonStatus}
          configLoaded={execution.configLoaded}
          executionActive={execution.executionActive}
          backgroundActivities={backgroundActivities}
        />

        {/* Main Content: Sidebar + Content Area */}
        <div className="flex flex-1 overflow-hidden">
          {/* Sidebar Navigation */}
          <Sidebar
            activeTab={activeTab}
            onTabChange={(tab) => setActiveTab(tab as MainTabId)}
            collapsed={sidebarCollapsed}
            onCollapsedChange={setSidebarCollapsed}
          />

          {/* Content Area */}
          <main className="flex-1 overflow-hidden">{renderTabContent()}</main>
        </div>

        {/* Action Detail Modal */}
        <ActionDetailModal
          action={modalState.selectedAction}
          isOpen={modalState.isActionModalOpen}
          onClose={modalState.closeActionModal}
        />

        {/* Image Detail Modal */}
        <ImageDetailModal
          entry={modalState.selectedImageEntry}
          isOpen={modalState.isImageModalOpen}
          onClose={modalState.closeImageModal}
        />

        {/* Log Source Manager Modal */}
        {projectLogs.config && (
          <LogSourceManager
            config={projectLogs.config}
            isOpen={showLogSourceManager}
            onClose={() => setShowLogSourceManager(false)}
            onSave={(sources) => {
              projectLogs.setLogSources(sources);
              projectLogs.saveConfig(sources); // Pass sources directly to avoid React async state issue
              setShowLogSourceManager(false);
            }}
            projectDirectory={projectLogs.directories?.base}
          />
        )}
      </div>
    </RenderLogWrapper>
  );
}

/**
 * Inner app with navigation-aware tutorial provider
 */
function AppWithTutorials() {
  const { navigate } = useNavigation();

  return (
    <TutorialProvider onNavigate={navigate}>
      <AppContent />
      <ContextualTutorial />
    </TutorialProvider>
  );
}

/**
 * Main App component with providers
 */
export default function App() {
  return (
    <UIBridgeProvider features={{ renderLog: true, control: true, debug: true }}>
      <UIBridgeEventHandler />
      {/* AutoRegisterProvider enables automatic UI Bridge element registration */}
      {/* All interactive elements (buttons, inputs, links, etc.) are auto-registered */}
      <AutoRegisterProvider
        enabled={import.meta.env.DEV}
        idStrategy="prefer-existing"
        debounceMs={100}
        excludeSelectors={['[data-no-register]']}
      >
        <AuthProvider>
          <NavigationProvider>
            <EventManagerProvider>
              <ExecutionProvider
                onLog={(level, message) => {
                  // Logs are now handled by LogManager through event handlers
                  console.log(`[LOG] ${level}: ${message}`);
                }}
              >
                <AutoContinueProvider>
                  <AppWithTutorials />
                </AutoContinueProvider>
              </ExecutionProvider>
            </EventManagerProvider>
          </NavigationProvider>
        </AuthProvider>
      </AutoRegisterProvider>
    </UIBridgeProvider>
  );
}
