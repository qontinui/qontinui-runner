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

import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

// Contexts
import {
  ExecutionProvider,
  useExecution,
  EventManagerProvider,
  AutoContinueProvider,
} from "./contexts";
import { AuthProvider, useAuth } from "./components/AuthProvider";

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
} from "./hooks";

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
import { LibraryTab } from "./components/LibraryTab";
import { HelpTab } from "./components/HelpTab";
import { SchedulerTab } from "./components/scheduler";
import { Sidebar } from "./components/navigation";
import { AiBuilderTab, AiBuilderProvider } from "./components/AiBuilderTab";
import { ScriptBuilderTab } from "./components/ScriptBuilderTab";
import { ActiveTab } from "./components/ActiveTab";
import { HistoryTab } from "./components/HistoryTab";
import { ExecuteTab } from "./components/ExecuteTab";
// Monitor/Observe components
import { ExecutionSummaryTab } from "./components/ai-workflows/ExecutionSummaryTab";
import { ExecutionReport } from "./components/findings";
import { IssuesPanel } from "./components/IssuesPanel";
import { VerificationTab } from "./components/verification";
import { StatisticsTab } from "./components/statistics";
import { DiscoverySyncPanel } from "./components/discoveries";
// Run-specific components
import { RunSelectionProvider } from "./contexts/RunSelectionContext";
import { RunDashboard } from "./components/run-dashboard/RunDashboard";
import { GeneralLogsTab } from "./components/GeneralLogsTab";
import { RunActionsTab } from "./components/run-logs/RunActionsTab";
import { RunImageRecognitionTab } from "./components/run-logs/RunImageRecognitionTab";
import { AiDataViewerTab } from "./components/run-logs/AiDataViewerTab";
// Configure components
import { ExternalLogsTab } from "./components/ExternalLogsTab";
import { CategoryManager } from "./components/findings/CategoryManager";

// Styles
import "./index.css";

type LogSubTab = "general" | "image" | "actions";

// Valid main tab IDs for the sidebar navigation
type MainTabId =
  | "run"
  | "active"
  | "history"
  // Observe group - new structure
  | "general-logs"
  | "run-dashboard"
  | "run-actions"
  | "run-image"
  | "run-summary"
  | "run-findings"
  | "run-issues"
  | "run-verification"
  | "run-ai-output"
  | "run-statistics"
  | "run-ai-data"
  | "discoveries"
  // Legacy tab IDs for migration
  | "ai"
  | "logs"
  | "monitor-summary"
  | "monitor-findings"
  | "monitor-issues"
  | "monitor-learnings"
  | "monitor-verification"
  | "monitor-statistics"
  | "monitor-discoveries"
  | "library"
  | "workflow-builder"
  | "script-builder"
  | "capture"
  | "config-log-sources"
  | "config-findings"
  | "tasks"
  | "settings"
  | "help";

const VALID_TAB_IDS: MainTabId[] = [
  "run",
  "active",
  "history",
  // New observe tabs
  "general-logs",
  "run-dashboard",
  "run-actions",
  "run-image",
  "run-summary",
  "run-findings",
  "run-issues",
  "run-verification",
  "run-ai-output",
  "run-statistics",
  "run-ai-data",
  "discoveries",
  // Legacy (for migration)
  "ai",
  "logs",
  "monitor-summary",
  "monitor-findings",
  "monitor-issues",
  "monitor-learnings",
  "monitor-verification",
  "monitor-statistics",
  "monitor-discoveries",
  "library",
  "workflow-builder",
  "script-builder",
  "capture",
  "config-log-sources",
  "config-findings",
  "tasks",
  "settings",
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
    "ai-builder": "workflow-builder",
    builder: "workflow-builder",
    prompts: "library",
    scripts: "script-builder",
    contexts: "library",
    scheduler: "tasks",
    dataset: "capture", // Dataset is now part of capture
    extract: "capture",
    // Old observe tab migrations to new structure
    logs: "general-logs",
    ai: "run-ai-output",
    "monitor-summary": "run-summary",
    "monitor-findings": "run-findings",
    "monitor-issues": "run-issues",
    "monitor-learnings": "run-summary", // Learnings removed, map to summary
    "monitor-verification": "run-verification",
    "monitor-statistics": "run-statistics",
    "monitor-discoveries": "discoveries",
    // Legacy monitor tab migrations
    monitor: "run-summary",
    issues: "run-issues",
    learnings: "run-summary",
    verification: "run-verification",
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

  // Main tab state
  const [activeTab, setActiveTab] = useState<MainTabId>(() => {
    const stored = localStorage.getItem(MAIN_TAB_STORAGE_KEY);
    return migrateTabId(stored);
  });

  // Script ID to edit (when navigating from Library to Script Builder)
  const [editScriptId, setEditScriptId] = useState<string | null>(null);

  // Workflow ID to edit (when navigating from Library to Workflow Builder)
  const [editWorkflowId, setEditWorkflowId] = useState<string | null>(null);

  // Handle editing a script from Library
  const handleEditScript = useCallback((scriptId: string) => {
    setEditScriptId(scriptId);
    setActiveTab("script-builder");
  }, []);

  // Handle editing a workflow from Library
  const handleEditWorkflow = useCallback((workflowId: string) => {
    setEditWorkflowId(workflowId);
    setActiveTab("workflow-builder");
  }, []);

  // Clear script ID when navigating away from script builder
  useEffect(() => {
    if (activeTab !== "script-builder") {
      setEditScriptId(null);
    }
  }, [activeTab]);

  // Clear workflow ID when navigating away from workflow builder
  useEffect(() => {
    if (activeTab !== "workflow-builder") {
      setEditWorkflowId(null);
    }
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

  /**
   * Renders the content for the currently active tab
   */
  const renderTabContent = () => {
    switch (activeTab) {
      case "run":
        return <ExecuteTab onLog={addLog} onNavigateToActive={() => setActiveTab("active")} />;

      case "active":
        return (
          <ActiveTab
            imageLogs={imageLogs}
            aiOutputLines={aiOutputLogs}
            actionLogData={actionLogViewData}
            onClearAiOutput={clearAiOutputLogs}
            onActionRowClick={modalState.openActionModal}
            onImageRowClick={modalState.openImageModal}
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
      case "general-logs":
        return (
          <div className="flex-1 flex flex-col min-h-0 p-4 h-full overflow-hidden">
            <div className="flex-1 flex flex-col min-h-0 card overflow-hidden">
              <GeneralLogsTab
                logs={logs}
                filteredLogs={filteredLogs}
                logLevel={logLevel}
                onLogLevelChange={setLogLevel}
                showLogFilter={uiState.showLogFilter}
                onToggleLogFilter={uiState.setShowLogFilter}
                logCount={logCount}
                onClearGeneralLogs={clearGeneralLogs}
                onCopyLogs={handleCopyLogs}
                copySuccess={uiState.copySuccess}
              />
            </div>
          </div>
        );

      case "run-dashboard":
        return (
          <RunSelectionProvider>
            <RunDashboard onNavigate={(tabId) => setActiveTab(tabId as MainTabId)} />
          </RunSelectionProvider>
        );

      case "run-actions":
        return (
          <RunSelectionProvider>
            <div className="flex-1 flex flex-col min-h-0 p-4 h-full overflow-hidden">
              <div className="flex-1 flex flex-col min-h-0 card overflow-hidden">
                <RunActionsTab
                  actionLogData={actionLogViewData}
                  actionLogLoading={actionLogLoading}
                  actionLogError={actionLogError}
                  onActionRowClick={modalState.openActionModal}
                  actionCount={actionLogViewData?.visible_count || 0}
                />
              </div>
            </div>
          </RunSelectionProvider>
        );

      case "run-image":
        return (
          <RunSelectionProvider>
            <div className="flex-1 flex flex-col min-h-0 p-4 h-full overflow-hidden">
              <div className="flex-1 flex flex-col min-h-0 card overflow-hidden">
                <RunImageRecognitionTab
                  imageLogs={imageLogs}
                  onImageRowClick={modalState.openImageModal}
                  imageLogCount={imageLogCount}
                />
              </div>
            </div>
          </RunSelectionProvider>
        );

      case "run-summary":
        return (
          <RunSelectionProvider>
            <div className="h-full overflow-hidden">
              <ExecutionSummaryTab />
            </div>
          </RunSelectionProvider>
        );

      case "run-findings":
        return (
          <RunSelectionProvider>
            <div className="h-full overflow-hidden">
              <ExecutionReport />
            </div>
          </RunSelectionProvider>
        );

      case "run-issues":
        return (
          <RunSelectionProvider>
            <div className="h-full overflow-y-auto p-4">
              <IssuesPanel />
            </div>
          </RunSelectionProvider>
        );

      case "run-verification":
        return (
          <RunSelectionProvider>
            <div className="h-full overflow-hidden">
              <VerificationTab />
            </div>
          </RunSelectionProvider>
        );

      case "run-ai-output":
        return (
          <RunSelectionProvider>
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
          </RunSelectionProvider>
        );

      case "run-statistics":
        return (
          <RunSelectionProvider>
            <div className="h-full overflow-hidden">
              <StatisticsTab
                configId={execution.config?.path ?? null}
                configName={execution.config?.name}
              />
            </div>
          </RunSelectionProvider>
        );

      case "run-ai-data":
        return (
          <RunSelectionProvider>
            <div className="h-full overflow-hidden">
              <AiDataViewerTab />
            </div>
          </RunSelectionProvider>
        );

      case "discoveries":
        return (
          <div className="h-full overflow-y-auto p-4">
            <DiscoverySyncPanel />
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
        return (
          <LibraryTab
            onLog={addLog}
            onNavigateToActive={() => setActiveTab("active")}
            onNavigateToBuilder={() => setActiveTab("workflow-builder")}
            onNavigateToScriptBuilder={() => setActiveTab("script-builder")}
            onEditScript={handleEditScript}
            onEditWorkflow={handleEditWorkflow}
            aiOutputLines={aiOutputLogs}
          />
        );

      case "workflow-builder":
        return (
          <div className="h-full overflow-y-auto">
            <AiBuilderProvider
              projectLogs={projectLogs}
              onNavigateToLogLocations={() => setActiveTab("logs")}
              onNavigateToAiOutput={() => setActiveTab("ai")}
              editWorkflowId={editWorkflowId}
            >
              <AiBuilderTab
                projectLogs={projectLogs}
                onNavigateToLogLocations={() => setActiveTab("logs")}
                onNavigateToAiOutput={() => setActiveTab("ai")}
                editWorkflowId={editWorkflowId}
              />
            </AiBuilderProvider>
          </div>
        );

      case "script-builder":
        return (
          <div className="h-full overflow-y-auto">
            <ScriptBuilderTab
              onLog={addLog}
              editScriptId={editScriptId}
              onNavigateToLibrary={() => setActiveTab("library")}
            />
          </div>
        );

      // ========== MONITOR TABS ==========
      case "monitor-summary":
        return (
          <div className="h-full overflow-hidden">
            <ExecutionSummaryTab />
          </div>
        );

      case "monitor-findings":
        return (
          <div className="h-full overflow-hidden">
            <ExecutionReport />
          </div>
        );

      case "monitor-issues":
        return (
          <div className="h-full overflow-y-auto p-4">
            <IssuesPanel />
          </div>
        );

      // monitor-learnings removed - functionality moved to Summary/Findings/Issues

      case "monitor-verification":
        return (
          <div className="h-full overflow-hidden">
            <VerificationTab />
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
      case "config-log-sources":
        return (
          <div className="h-full overflow-y-auto p-4">
            <ExternalLogsTab
              config={projectLogs.config}
              sources={projectLogs.logsState.sources}
              loading={projectLogs.logsState.loading}
              error={projectLogs.logsState.error}
              lastRefresh={projectLogs.logsState.lastRefresh}
              onRefresh={projectLogs.refreshLogs}
              onConfigureSources={() => setShowLogSourceManager(true)}
            />
          </div>
        );

      case "config-findings":
        return (
          <div className="h-full overflow-y-auto">
            <CategoryManager onLog={addLog} />
          </div>
        );

      case "capture":
        return (
          <div className="h-full overflow-y-auto">
            <CaptureTab
              onLog={addLog}
              projects={projectSelection.projects}
              selectedProjectId={projectSelection.selectedProjectId}
              selectedProjectName={projectSelection.selectedProjectName}
            />
          </div>
        );

      case "tasks":
        return <SchedulerTab />;

      case "settings":
        return (
          <div className="h-full overflow-hidden">
            <Settings
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

      case "help":
        return <HelpTab />;

      default:
        return null;
    }
  };

  return (
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
        />
      )}
    </div>
  );
}

/**
 * Main App component with providers
 */
export default function App() {
  return (
    <AuthProvider>
      <EventManagerProvider>
        <ExecutionProvider
          onLog={(level, message) => {
            // Logs are now handled by LogManager through event handlers
            console.log(`[LOG] ${level}: ${message}`);
          }}
        >
          <AutoContinueProvider>
            <AppContent />
          </AutoContinueProvider>
        </ExecutionProvider>
      </EventManagerProvider>
    </AuthProvider>
  );
}
