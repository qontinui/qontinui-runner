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
  ClipboardCheck,
  FileText,
  FileSearch,
  Bot,
  BarChart3,
  Database,
  TestTube,
  Activity,
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
  useApiReady,
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
  SpecExecutionHandler,
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
import { SetupWizard } from "./components/setup-wizard";
import { LogSourcePicker } from "./components/LogSourcePicker";
import { AiTab } from "./components/AiTab";
import { LibraryDashboard } from "./components/LibraryDashboard";
import {
  ChecksPage,
  CheckGroupsPage,
  ShellCommandsPage,
  TasksPage,
  ContextsPage,
  PlaywrightTestsPage,
} from "./components/library";
import { StepBuildersPage } from "./components/StepBuildersPage";
import { HelpTab } from "./components/HelpTab";
import { SchedulerTab } from "./components/scheduler";
import { TriggersTab } from "./components/triggers";
import { Sidebar } from "./components/navigation";
import { WorkflowBuilderTab } from "./components/workflow-builder";
import { ActiveDashboardPage } from "./components/active-dashboard";
import { HistoryTab } from "./components/HistoryTab";
import { ExecuteTab } from "./components/ExecuteTab";
import { WorkflowQueueTab } from "./components/workflow-queue";
// Monitor/Observe components
import { ExecutionReport } from "./components/findings";
import { StateExplorerTab } from "./components/state-explorer";
import { TestResultsTab } from "./components/test-results";
import { StatisticsTab } from "./components/statistics";
// Run-specific components
import { RunSelectionProvider } from "./contexts/RunSelectionContext";
import { RunPageLayout } from "./components/run-dashboard/RunPageLayout";
import { TraceViewerPage } from "./components/run-dashboard/TraceViewerPage";
import { RunActionsTab } from "./components/run-logs/RunActionsTab";
import { RunImageRecognitionTab } from "./components/run-logs/RunImageRecognitionTab";
import { AiDataViewerTab } from "./components/run-logs/AiDataViewerTab";
import { RunRecapTab } from "./components/run-recap";
import { useTaskRuns } from "./hooks/useAiData";
// Page Sweep builder
// Configure components
import { ExternalLogsTab as _ExternalLogsTab } from "./components/ExternalLogsTab";
import { CategoryManager } from "./components/findings/CategoryManager";
import { HooksManagerPanel } from "./components/hooks";
import { ErrorMonitorTab } from "./components/error-monitor";
import { ProcessManagerTab } from "./components/process-manager";
import { ReflectionDashboard } from "./components/reflection-dashboard/ReflectionDashboard";
import { GeneratorEvalPage } from "./pages/GeneratorEvalPage";
import { SpecsPage } from "./pages/specs/SpecsPage";
import { UIBridgeIntegrationPage } from "./pages/ui-bridge-integration/UIBridgeIntegrationPage";
import { TerminalPage } from "./components/terminal";
import { StateMachineBuilderPage } from "./pages/state-machine";

// Development tools
import { PerformanceOverlay } from "./components/dev";

import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Styles
import "./index.css";

type LogSubTab = "general" | "image" | "actions";

// Valid main tab IDs for the sidebar navigation
type MainTabId =
  | "gui-automation"
  | "workflow-queue"
  | "active"
  | "runs"
  | "history"
  | "error-monitor"
  | "processes"
  | "reflection"
  | "generator-eval"
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
  | "run-traces"
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
  | "step-builders"
  | "check-builder"
  | "check-group-builder"
  | "shell-command-builder"
  | "task-builder"
  | "context-builder"
  | "playwright-test-builder"
  | "unified-workflow-builder"
  | "state-machine"
  | "specs"
  | "capture"
  | "config-log-sources"
  | "config-findings"
  | "config-hooks"
  | "config-ui-bridge"
  | "triggers"
  | "tasks"
  | "settings"
  | "settings-account"
  | "settings-ai"
  | "settings-agentic"
  | "settings-self-healing"
  | "settings-playwright"
  | "settings-mobile"
  | "settings-cloud-relay"
  | "settings-mcp"
  | "settings-log-sources"
  | "settings-execution-variables"
  | "settings-general"
  | "settings-storage"
  | "settings-backup"
  | "settings-instances"
  | "settings-debug"
  | "settings-updates"
  | "terminal"
  | "help";

const VALID_TAB_IDS: MainTabId[] = [
  "gui-automation",
  "workflow-queue",
  "active",
  "runs",
  "history",
  "error-monitor",
  "processes",
  "reflection",
  "generator-eval",
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
  "run-traces",
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
  "step-builders",
  "check-builder",
  "check-group-builder",
  "shell-command-builder",
  "task-builder",
  "context-builder",
  "playwright-test-builder",
  "unified-workflow-builder",
  "state-machine",
  "specs",
  "capture",
  "config-log-sources",
  "config-findings",
  "config-hooks",
  "config-ui-bridge",
  "triggers",
  "tasks",
  "settings",
  "settings-account",
  "settings-ai",
  "settings-agentic",
  "settings-self-healing",
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
  "terminal",
  "help",
];

const MAIN_TAB_STORAGE_KEY = "qontinui-main-active-tab";
const SIDEBAR_COLLAPSED_KEY = "qontinui-sidebar-collapsed";

/**
 * Maps old tab IDs to new tab IDs for localStorage migration
 */
function migrateTabId(stored: string | null): MainTabId {
  if (!stored) return "gui-automation";

  // Map old tab names to new ones
  const migrations: Record<string, MainTabId> = {
    run: "gui-automation", // Execute tab renamed to GUI Automation
    history: "runs", // History tab renamed to Runs
    "ai-workflows": "run-ai-output",
    "ai-builder": "unified-workflow-builder",
    builder: "unified-workflow-builder",
    prompts: "library",
    scripts: "library", // Old "scripts" tab maps to library
    "script-builder": "library", // Old "script-builder" tab maps to library
    contexts: "library",
    scheduler: "tasks",
    dataset: "capture", // Dataset is now part of capture
    extract: "capture",
    "live-page-generator": "unified-workflow-builder", // Spec discovery now in AI Generate panel
    "spec-discovery": "unified-workflow-builder", // Spec discovery now in AI Generate panel
    "page-sweep": "unified-workflow-builder", // Page Sweep removed, multi-page now in SpecSourceSection
    "run-plan": "terminal", // Chat page removed, workflow generation now in Terminal
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
    "monitor-discoveries": "run-recap", // Discoveries removed, map to recap
    // Legacy monitor tab migrations
    monitor: "run-recap", // Map to recap
    issues: "run-findings", // Issues merged into Findings
    "run-issues": "run-findings", // Issues merged into Findings
    learnings: "run-recap", // Map to recap
    verification: "run-state-explorer",
    "run-verification": "run-state-explorer",
    "run-exploration": "run-state-explorer",
    "verification-builder": "library", // Builder removed, map to library
    statistics: "run-statistics",
    // Builder tab migrations (stored last-active-tab → step-builders landing)
    "check-builder": "step-builders",
    "check-group-builder": "step-builders",
    "shell-command-builder": "step-builders",
    "task-builder": "step-builders",
    "context-builder": "step-builders",
    "playwright-test-builder": "step-builders",
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

  return "gui-automation";
}

/**
 * Main app content (inside providers)
 */
function AppContent() {
  // Auth state from context
  const auth = useAuth();

  // Setup wizard state
  const [setupCompleted, setSetupCompleted] = useState<boolean | null>(null);
  useEffect(() => {
    invoke<boolean>("check_setup_completed")
      .then(setSetupCompleted)
      .catch(() => setSetupCompleted(true));
  }, []);

  // HTTP API readiness
  const isApiReady = useApiReady();

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
        run: "gui-automation",
        "gui-automation": "gui-automation",
        active: "active",
        "unified-workflow-builder": "unified-workflow-builder",
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
          run: "gui-automation",
          "gui-automation": "gui-automation",
          "run-recap": "run-recap",
          "run-dashboard": "run-recap", // Dashboard merged into Summary
          active: "active",
          history: "history",
          library: "library",
          logs: "logs",
          ai: "ai",
          settings: "settings",
          "unified-workflow-builder": "unified-workflow-builder",
          "error-monitor": "error-monitor",
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

  // Listen for error badge click to navigate to error monitor tab
  useEffect(() => {
    const handleNavigateToErrorMonitor = () => {
      console.log("[APP] Navigating to error-monitor tab from ErrorBadge click");
      setActiveTab("error-monitor");
    };

    window.addEventListener("navigate-to-error-monitor", handleNavigateToErrorMonitor);

    return () => {
      window.removeEventListener("navigate-to-error-monitor", handleNavigateToErrorMonitor);
    };
  }, []);

  // Listen for Quick Fix to navigate to active dashboard
  useEffect(() => {
    const handleNavigateToActive = () => {
      console.log("[APP] Navigating to active tab from Quick Fix");
      setActiveTab("active");
    };

    window.addEventListener("navigate-to-active", handleNavigateToActive);

    return () => {
      window.removeEventListener("navigate-to-active", handleNavigateToActive);
    };
  }, []);

  // Listen for stale task detection events from the backend
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      unlisten = await listen<{
        task_run_id: string;
        task_name: string;
        message: string;
      }>("stale-task-detected", (event) => {
        const { task_name, message } = event.payload;
        console.log("[APP] Stale task detected:", event.payload);
        setStaleTaskMessage(`${task_name}: ${message}`);
        // Auto-dismiss after 10 seconds
        setTimeout(() => setStaleTaskMessage(null), 10000);
      });
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Workflow ID to edit (when navigating from Library to Workflow Builder)
  const [editWorkflowId, setEditWorkflowId] = useState<string | null>(null);

  // Handle editing a workflow from Library
  const _handleEditWorkflow = useCallback((workflowId: string) => {
    setEditWorkflowId(workflowId);
    setActiveTab("unified-workflow-builder");
  }, []);

  // Store the last inline workflow definition for re-execution via "Run Last Workflow" button.
  // Inline workflows aren't saved to the DB, so we keep the definition here.
  const lastInlineWorkflowRef = useRef<{
    name: string;
    description?: string;
    setup_steps: unknown[];
    verification_steps: unknown[];
    agentic_steps: unknown[];
    completion_steps: unknown[];
    max_iterations?: number;
  } | null>(null);

  // State for "Run Last Workflow" button
  const [isRunningLastWorkflow, setIsRunningLastWorkflow] = useState(false);
  const [runLastWorkflowError, setRunLastWorkflowError] = useState<string | null>(null);

  // State for stale task detection toast
  const [staleTaskMessage, setStaleTaskMessage] = useState<string | null>(null);

  // The last workflow can be re-run if we have a workflow_name
  const lastRunWorkflowId = lastRun?.workflow_name ?? null;

  // Handle running the last workflow again
  const handleRunLastWorkflow = useCallback(async () => {
    if (!lastRun?.workflow_name) return;

    setIsRunningLastWorkflow(true);

    try {
      // Search for the unified workflow by name in the database
      const searchResponse = await tracedFetch(
        `${getApiBase()}/unified-workflows/search?q=${encodeURIComponent(lastRun.workflow_name)}`,
      );

      if (!searchResponse.ok) {
        throw new Error(`Failed to search workflows: ${searchResponse.statusText}`);
      }

      const searchResult = await searchResponse.json();
      // API returns data as an array directly, not wrapped in .workflows
      const workflows = searchResult.data ?? [];

      // Find exact match by name
      const workflow = workflows.find((w: { name: string }) => w.name === lastRun.workflow_name);

      if (workflow?.id) {
        // Found in DB — run via the /run endpoint
        tracedFetch(`${getApiBase()}/unified-workflows/${workflow.id}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({}),
        }).catch((error) => {
          console.error("[APP] Failed to run workflow:", error);
        });

        console.log("[APP] Started saved workflow:", workflow.name);
        setActiveTab("active");
      } else {
        // Not found in DB — try re-executing as an inline workflow.
        // The task_run stores workflow_name with "[Inline] " prefix, so strip it for matching.
        const rawName = lastRun.workflow_name.replace(/^\[Inline\]\s*/, "");
        let inlinePayload = lastInlineWorkflowRef.current;

        // If not in the React ref, try fetching from the backend's last-inline store
        if (!inlinePayload || inlinePayload.name !== rawName) {
          try {
            const inlineResponse = await tracedFetch(
              `${getApiBase()}/unified-workflows/last-inline`,
            );
            if (inlineResponse.ok) {
              const inlineResult = await inlineResponse.json();
              if (inlineResult.data?.name === rawName) {
                inlinePayload = inlineResult.data;
              }
            }
          } catch {
            // Ignore fetch errors for fallback
          }
        }

        if (inlinePayload && inlinePayload.name === rawName) {
          // Re-execute the inline workflow
          tracedFetch(`${getApiBase()}/unified-workflows/execute-inline`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(inlinePayload),
          }).catch((error) => {
            console.error("[APP] Failed to re-execute inline workflow:", error);
          });

          console.log("[APP] Re-executing inline workflow:", lastRun.workflow_name);
          setActiveTab("active");
        } else {
          console.warn("[APP] Workflow not found in DB or inline cache:", lastRun.workflow_name);
          setRunLastWorkflowError(
            `Workflow "${lastRun.workflow_name}" not found. It may have been lost after a runner restart.`,
          );
          setTimeout(() => setRunLastWorkflowError(null), 6000);
        }
      }
    } catch (error) {
      console.error("[APP] Failed to run last workflow:", error);
      setRunLastWorkflowError(
        `Failed to run workflow: ${error instanceof Error ? error.message : String(error)}`,
      );
      setTimeout(() => setRunLastWorkflowError(null), 6000);
    } finally {
      setIsRunningLastWorkflow(false);
    }
  }, [lastRun]);

  // Clear workflow ID when navigating away from workflow builder
  useEffect(() => {
    if (activeTab !== "unified-workflow-builder") {
      setEditWorkflowId(null);
    }
  }, [activeTab]);

  // Sidebar collapsed state
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "true";
  });

  // Auto-collapse sidebar when multiple terminal sessions are open
  const [terminalSessionCount, setTerminalSessionCount] = useState(0);
  const autoCollapsedRef = useRef(false);

  const handleSidebarCollapsedChange = useCallback((value: boolean) => {
    // Manual toggle clears auto-collapse tracking
    autoCollapsedRef.current = false;
    setSidebarCollapsed(value);
    localStorage.setItem(SIDEBAR_COLLAPSED_KEY, JSON.stringify(value));
  }, []);

  useEffect(() => {
    const shouldAutoCollapse = activeTab === "terminal" && terminalSessionCount > 1;

    if (shouldAutoCollapse && !sidebarCollapsed) {
      autoCollapsedRef.current = true;
      setSidebarCollapsed(true);
    } else if (!shouldAutoCollapse && autoCollapsedRef.current) {
      // Restore expanded state when auto-collapse conditions end
      autoCollapsedRef.current = false;
      setSidebarCollapsed(false);
    }
  }, [activeTab, terminalSessionCount, sidebarCollapsed]);

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

  // Log source picker modal state
  const [showLogSourcePicker, setShowLogSourcePicker] = useState(false);

  // Auto-load projects when authenticated
  useEffect(() => {
    if (auth.authStatus?.authenticated && !auth.loading) {
      console.log("[APP] User authenticated, loading projects");
      projectSelection.loadProjects();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
    // Set the last run ID in localStorage so RunSelectionContext picks it up
    if (lastRun?.id) {
      try {
        localStorage.setItem("qontinui-selected-task-run-id", JSON.stringify(lastRun.id));
      } catch {
        // Ignore storage errors
      }
    }
    // If we have a last run with a workflow name, try to select it
    if (lastRun?.workflow_name && execution.workflows.length > 0) {
      // Find matching workflow by name
      const matchingWorkflow = execution.workflows.find(
        (w) => w.name === lastRun.workflow_name || w.id === lastRun.workflow_name,
      );
      if (matchingWorkflow) {
        execution.selectWorkflowWithPersistence(matchingWorkflow.id);
      }
    }
    // Navigate to recap
    setActiveTab("run-recap");
  }, [lastRun, execution]);

  // Show loading state while checking auth, during dev auto-login, or while API server starts
  const isAuthLoading = auth.loading || auth.devAutoLoginPending;
  const isInitializing = isAuthLoading || !isApiReady;
  console.log(
    "[APP] Render - auth.loading:",
    auth.loading,
    "auth.devAutoLoginPending:",
    auth.devAutoLoginPending,
    "auth.authStatus:",
    auth.authStatus,
    "isApiReady:",
    isApiReady,
  );
  if (isInitializing) {
    const loadingMessage = auth.devAutoLoginPending
      ? "Signing in..."
      : isAuthLoading
        ? "Checking authentication..."
        : "Starting API server...";
    console.log("[APP] Rendering loading state:", loadingMessage);
    return (
      <div className="min-h-screen bg-background grid-dots flex items-center justify-center">
        <div className="card p-8 text-center space-y-4">
          <div className="inline-block w-12 h-12 border-4 border-primary border-t-transparent rounded-full animate-spin" />
          <p className="text-muted-foreground">{loadingMessage}</p>
        </div>
      </div>
    );
  }

  // Show login screen if not authenticated
  if (!auth.authStatus?.authenticated) {
    console.log("[APP] Rendering LoginScreen (not authenticated)");
    return <LoginScreen onLogin={auth.login} />;
  }

  // Show setup wizard on first launch
  if (setupCompleted === false) {
    return <SetupWizard onComplete={() => setSetupCompleted(true)} />;
  }

  console.log("[APP] Rendering main app (authenticated)");

  // Get last run task_run_id for render logging context
  const lastRunId = lastRun?.id;

  /**
   * Renders the content for the currently active tab
   */
  const renderTabContent = () => {
    switch (activeTab) {
      case "gui-automation":
        return <ExecuteTab onLog={addLog} onNavigateToActive={() => setActiveTab("active")} />;

      case "workflow-queue":
        return (
          <WorkflowQueueTab onNavigateToActive={() => setActiveTab("active")} onLog={addLog} />
        );

      case "active":
        return (
          <ActiveDashboardPage
            onGoToExecute={() => setActiveTab("gui-automation")}
            onGoToRecap={lastRun ? handleGoToRecap : undefined}
            onRunLastWorkflow={lastRunWorkflowId ? handleRunLastWorkflow : undefined}
            isRunningLastWorkflow={isRunningLastWorkflow}
            lastRunWorkflowName={lastRunWorkflowName}
            lastRunWorkflowId={lastRunWorkflowId}
          />
        );

      case "runs":
      case "history":
        return (
          <HistoryTab
            onNavigateToRun={() => setActiveTab("gui-automation")}
            onNavigateToAi={(runId) => {
              try {
                localStorage.setItem("qontinui-selected-task-run-id", JSON.stringify(runId));
              } catch {
                // Ignore storage errors
              }
              setActiveTab("run-recap");
            }}
          />
        );

      case "error-monitor":
        return (
          <div className="h-full overflow-hidden">
            <ErrorMonitorTab />
          </div>
        );

      case "processes":
        return (
          <div className="h-full overflow-hidden">
            <ProcessManagerTab />
          </div>
        );

      case "reflection":
        return (
          <div className="h-full overflow-hidden">
            <ReflectionDashboard />
          </div>
        );

      case "generator-eval":
        return (
          <div className="h-full overflow-hidden">
            <GeneratorEvalPage />
          </div>
        );

      // ========== NEW OBSERVE TABS ==========
      case "run-recap":
        return (
          <RunSelectionProvider>
            <RunPageLayout
              title="Recap"
              icon={ClipboardCheck}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <RunRecapTab
                  onNavigateToAiOutput={(phase, iteration) => {
                    try {
                      localStorage.setItem(
                        "qontinui-ai-output-navigate",
                        JSON.stringify({ phase, phaseIteration: iteration }),
                      );
                    } catch {
                      // Ignore storage errors
                    }
                    setActiveTab("run-ai-output");
                  }}
                />
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
              onNavigateToActive={() => setActiveTab("active")}
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
            <RunPageLayout
              title="Image Recognition"
              icon={Image}
              badgeCount={imageLogCount}
              onNavigateToActive={() => setActiveTab("active")}
            >
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
            <RunPageLayout
              title="Findings"
              icon={FileText}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <ExecutionReport />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-state-explorer":
        return (
          <RunSelectionProvider>
            <RunPageLayout
              title="State Explorer"
              icon={FileSearch}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <StateExplorerTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-tests":
        return (
          <RunSelectionProvider>
            <RunPageLayout
              title="Test Results"
              icon={TestTube}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <TestResultsTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-ai-output":
        return (
          <RunSelectionProvider>
            <RunPageLayout
              title="AI Output"
              icon={Bot}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <AiTab
                aiOutputLines={aiOutputLogs}
                onClearAiOutput={clearAiOutputLogs}
                onAddAiOutputLine={(line) =>
                  addAiOutputLog(
                    line.line,
                    line.source,
                    line.actionId,
                    line.taskRunId,
                    line.sessionId,
                    line.sessionName,
                    line.phase,
                    line.phaseIteration,
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
            <RunPageLayout
              title="Statistics"
              icon={BarChart3}
              onNavigateToActive={() => setActiveTab("active")}
            >
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
            <RunPageLayout
              title="AI Data Viewer"
              icon={Database}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <AiDataViewerTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        );

      case "run-traces":
        return (
          <RunSelectionProvider>
            <RunPageLayout
              title="Traces"
              icon={Activity}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <TraceViewerPage />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
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
                line.taskRunId,
                line.sessionId,
                line.sessionName,
                line.phase,
                line.phaseIteration,
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
        return <LibraryDashboard onLog={addLog} />;

      case "specs":
        return (
          <div className="h-full overflow-hidden">
            <SpecsPage
              onNavigateToWorkflowBuilder={(id) => {
                setEditWorkflowId(id);
                setActiveTab("unified-workflow-builder");
              }}
            />
          </div>
        );

      case "state-machine":
        return (
          <div className="h-full overflow-hidden">
            <StateMachineBuilderPage />
          </div>
        );

      case "step-builders":
        return <StepBuildersPage onNavigate={(id) => setActiveTab(id as MainTabId)} />;

      case "check-builder":
        return <ChecksPage />;

      case "check-group-builder":
        return <CheckGroupsPage />;

      case "shell-command-builder":
        return <ShellCommandsPage />;

      case "task-builder":
        return <TasksPage />;

      case "context-builder":
        return <ContextsPage />;

      case "playwright-test-builder":
        return <PlaywrightTestsPage />;

      case "unified-workflow-builder":
        return (
          <div className="h-full overflow-hidden">
            <WorkflowBuilderTab
              editWorkflowId={editWorkflowId}
              onNavigateToActive={() => setActiveTab("active")}
            />
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

      // ========== CONFIGURE TABS ==========
      case "config-log-sources": {
        // Show list of configured global log sources with their paths
        const sources = globalLogSources.settings?.sources || [];

        return (
          <div className="h-full flex flex-col overflow-hidden">
            {/* Compact header */}
            <div className="flex items-center justify-between px-6 py-3 border-b border-border shrink-0">
              <div className="flex items-center gap-2">
                <FileText className="w-4 h-4 text-muted-foreground" />
                <h1 className="text-lg font-semibold">Log Sources</h1>
                <span className="text-sm text-muted-foreground">
                  External log files configured for monitoring
                </span>
              </div>
              <button
                onClick={() => setActiveTab("settings-log-sources")}
                className="flex items-center gap-2 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
              >
                Configure Sources
              </button>
            </div>
            <div className="flex-1 overflow-y-auto p-6">
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

      case "config-ui-bridge":
        return (
          <div className="h-full overflow-hidden">
            <UIBridgeIntegrationPage />
          </div>
        );

      case "capture":
        return (
          <div className="h-full overflow-y-auto">
            <CaptureTab onLog={addLog} />
          </div>
        );

      case "triggers":
        return <TriggersTab />;

      case "tasks":
        return <SchedulerTab />;

      case "settings":
      case "settings-account":
      case "settings-ai":
      case "settings-agentic":
      case "settings-self-healing":
      case "settings-playwright":
      case "settings-mobile":
      case "settings-cloud-relay":
      case "settings-mcp":
      case "settings-log-sources":
      case "settings-execution-variables":
      case "settings-general":
      case "settings-storage":
      case "settings-backup":
      case "settings-instances":
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
          "settings-cloud-relay": "cloud-relay",
          "settings-mcp": "mcp",
          "settings-log-sources": "log-sources",
          "settings-execution-variables": "execution-variables",
          "settings-general": "general",
          "settings-storage": "storage",
          "settings-backup": "backup",
          "settings-instances": "instances",
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

      case "terminal":
        // Rendered always-mounted outside the switch (see below)
        return null;

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
            onCollapsedChange={handleSidebarCollapsedChange}
          />

          {/* Content Area */}
          <main className="flex-1 overflow-hidden relative">
            {renderTabContent()}
            {/* Terminal is always-mounted to preserve PTY sessions and scrollback across tab switches */}
            <div className={`absolute inset-0 ${activeTab === "terminal" ? "" : "hidden"}`}>
              <TerminalPage
                onNavigateToBuilder={() => setActiveTab("unified-workflow-builder")}
                onNavigateToActive={() => setActiveTab("active")}
                onSessionCountChange={setTerminalSessionCount}
              />
            </div>
          </main>
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

        {/* Log Source Picker Modal */}
        {projectLogs.config && (
          <LogSourcePicker
            isOpen={showLogSourcePicker}
            onClose={() => setShowLogSourcePicker(false)}
            selectedSourceIds={projectLogs.config.selectedSourceIds}
            globalProfileId={projectLogs.config.globalProfileId}
            onSave={(sourceIds, profileId) => {
              if (profileId) {
                projectLogs.setGlobalProfile(profileId);
              } else {
                projectLogs.setSelectedSources(sourceIds);
              }
              setShowLogSourcePicker(false);
            }}
          />
        )}

        {/* Run Last Workflow error toast */}
        {runLastWorkflowError && (
          <div className="fixed bottom-4 right-4 p-4 rounded-lg shadow-lg border max-w-md z-toast bg-card border-destructive/50">
            <div className="flex items-start gap-3">
              <div className="flex-1 min-w-0">
                <h4 className="font-medium text-sm text-destructive">Workflow Not Found</h4>
                <p className="text-sm text-muted-foreground mt-1">{runLastWorkflowError}</p>
              </div>
              <button
                onClick={() => setRunLastWorkflowError(null)}
                className="text-muted-foreground hover:text-foreground flex-shrink-0"
              >
                &times;
              </button>
            </div>
          </div>
        )}

        {/* Stale task detection toast */}
        {staleTaskMessage && (
          <div className="fixed bottom-4 right-4 p-4 rounded-lg shadow-lg border max-w-md z-toast bg-card border-yellow-500/50">
            <div className="flex items-start gap-3">
              <div className="flex-1 min-w-0">
                <h4 className="font-medium text-sm text-yellow-600 dark:text-yellow-400">
                  Possibly Stale Task
                </h4>
                <p className="text-sm text-muted-foreground mt-1">{staleTaskMessage}</p>
              </div>
              <button
                onClick={() => setStaleTaskMessage(null)}
                className="text-muted-foreground hover:text-foreground flex-shrink-0"
              >
                &times;
              </button>
            </div>
          </div>
        )}

        {/* Performance Overlay (dev mode only, toggle with Ctrl+Shift+P) */}
        <PerformanceOverlay position="bottom-right" />
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
      <SpecExecutionHandler />
      {/* AutoRegisterProvider enables automatic UI Bridge element registration */}
      {/* All interactive elements (buttons, inputs, links, etc.) are auto-registered */}
      <AutoRegisterProvider
        enabled={import.meta.env.DEV}
        idStrategy="prefer-existing"
        debounceMs={100}
        excludeSelectors={["[data-no-register]"]}
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
