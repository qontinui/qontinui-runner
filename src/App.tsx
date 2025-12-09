/**
 * App.tsx (Refactored - New Tab Structure)
 *
 * Main application component with reorganized tab structure:
 * - Run: Execute workflows (configuration + execution controls)
 * - Logs: View logs (General, Image Recognition, Actions)
 * - Capture: Screenshot capture operations
 * - Settings: Passive configuration only
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import * as Tabs from "@radix-ui/react-tabs";
import { Play, Camera, Settings as SettingsIcon, ScrollText, Globe, Package } from "lucide-react";

// Contexts
import { ExecutionProvider, useExecution, EventManagerProvider } from "./contexts";
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
} from "./hooks";

// Components
import StatusIndicator from "./components/StatusIndicator";
import { ConfigurationPanel } from "./components/ConfigurationPanel";
import { ExecutionControlPanel } from "./components/ExecutionControlPanel";
import { LogsTab } from "./components/LogsTab";
import { CaptureTab } from "./components/CaptureTab";
import { ExtractionTab } from "./components/ExtractionTab";
import { DatasetPackager } from "./components/DatasetPackager";
import ActionDetailModal from "./components/ActionDetailModal";
import ImageDetailModal from "./components/ImageDetailModal";
import { Settings } from "./components/Settings";
import { LoginScreen } from "./components/LoginScreen";

// Styles
import "./index.css";

type MainTab = "run" | "logs" | "capture" | "extract" | "dataset" | "settings";
type LogSubTab = "general" | "image" | "actions" | "ai";

const MAIN_TAB_STORAGE_KEY = "qontinui-main-active-tab";

/**
 * Main app content (inside providers)
 */
function AppContent() {
  // Auth state from context
  const auth = useAuth();

  // Execution state from context
  const execution = useExecution();

  // Main tab state
  const [activeMainTab, setActiveMainTab] = useState<MainTab>(() => {
    const stored = localStorage.getItem(MAIN_TAB_STORAGE_KEY);
    if (stored && ["run", "logs", "capture", "extract", "dataset", "settings"].includes(stored)) {
      return stored as MainTab;
    }
    return "run";
  });

  // Log sub-tab state
  const [activeLogSubTab, setActiveLogSubTab] = useState<LogSubTab>("general");

  // Persist main tab
  useEffect(() => {
    localStorage.setItem(MAIN_TAB_STORAGE_KEY, activeMainTab);
  }, [activeMainTab]);

  // Logs from LogManager
  const {
    logs,
    imageLogs,
    aiOutputLogs,
    addLog,
    clearGeneralLogs,
    clearImageLogs,
    clearAiOutputLogs,
    copyLogs,
    logCount,
    imageLogCount,
    aiOutputLogCount,
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

  // Auto-load projects when authenticated
  useEffect(() => {
    if (auth.authStatus?.authenticated && !auth.loading) {
      console.log("[APP] User authenticated, loading projects");
      projectSelection.loadProjects();
    }
  }, [auth.authStatus?.authenticated, auth.loading]);

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
    if (activeMainTab === "logs" && activeLogSubTab === "actions") {
      console.log("[TAB_SWITCH] Switched to Actions tab, refreshing action log");
      refreshActionLog();
    }
  }, [activeMainTab, activeLogSubTab, refreshActionLog]);

  // Event handlers
  const handleWorkflowSelect = (workflowId: string) => {
    execution.selectWorkflowWithPersistence(workflowId);
    uiState.setShowWorkflowDropdown(false);
  };

  const handleMonitorSelect = (index: number) => {
    execution.selectMonitorWithPersistence(index);
    uiState.setShowMonitorDropdown(false);
  };

  const handleCopyLogs = async () => {
    const logType =
      activeLogSubTab === "general" ? "general" : activeLogSubTab === "image" ? "image" : "actions";
    const success = await copyLogs(
      logType,
      logType === "actions" ? actionLogViewData?.actions : undefined,
    );
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

  // Show loading state while checking auth
  if (auth.loading) {
    return (
      <div className="min-h-screen bg-background grid-dots flex items-center justify-center">
        <div className="card p-8 text-center space-y-4">
          <div className="inline-block w-12 h-12 border-4 border-primary border-t-transparent rounded-full animate-spin" />
          <p className="text-muted-foreground">Checking authentication...</p>
        </div>
      </div>
    );
  }

  // Show login screen if not authenticated
  if (!auth.authStatus?.authenticated) {
    return <LoginScreen onLoginSuccess={auth.refreshAuth} />;
  }

  const mainTabs = [
    { id: "run" as const, label: "Run", icon: Play },
    { id: "logs" as const, label: "Logs", icon: ScrollText },
    { id: "capture" as const, label: "Capture", icon: Camera },
    { id: "extract" as const, label: "Extract", icon: Globe },
    { id: "dataset" as const, label: "Dataset", icon: Package },
    { id: "settings" as const, label: "Settings", icon: SettingsIcon },
  ];

  return (
    <div className="min-h-screen bg-background grid-dots flex flex-col">
      {/* Status Bar */}
      <StatusIndicator
        pythonStatus={execution.pythonStatus}
        configLoaded={execution.configLoaded}
        executionActive={execution.executionActive}
      />

      {/* Main Content with Tabs */}
      <Tabs.Root
        value={activeMainTab}
        onValueChange={(value) => setActiveMainTab(value as MainTab)}
        className="flex-1 flex flex-col container mx-auto"
      >
        {/* Main Tab Navigation */}
        <Tabs.List className="flex border-b border-border bg-card/30 px-4">
          {mainTabs.map((tab) => {
            const Icon = tab.icon;
            return (
              <Tabs.Trigger
                key={tab.id}
                value={tab.id}
                className={`
                  flex items-center gap-2 px-6 py-4 text-sm font-medium
                  border-b-2 transition-colors
                  data-[state=active]:border-primary data-[state=active]:text-primary
                  data-[state=inactive]:border-transparent data-[state=inactive]:text-muted-foreground
                  data-[state=inactive]:hover:text-foreground data-[state=inactive]:hover:bg-muted/30
                `}
              >
                <Icon className="w-4 h-4" />
                {tab.label}
              </Tabs.Trigger>
            );
          })}
        </Tabs.List>

        {/* Tab Content */}
        <div className="flex-1 overflow-hidden">
          {/* Run Tab */}
          <Tabs.Content value="run" className="h-full outline-none p-6 overflow-y-auto">
            <div className="space-y-6">
              {/* Control Panels Grid */}
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
                {/* Configuration Panel */}
                <ConfigurationPanel
                  config={execution.config}
                  collapsed={uiState.configPanelCollapsed}
                  onToggle={uiState.setConfigPanelCollapsed}
                  onLoadConfiguration={execution.loadConfiguration}
                  onLoadLastConfiguration={execution.loadLastConfiguration}
                />

                {/* Execution Control Panel */}
                <ExecutionControlPanel
                  collapsed={uiState.executionPanelCollapsed}
                  onToggle={uiState.setExecutionPanelCollapsed}
                  workflows={execution.workflows}
                  selectedWorkflow={execution.selectedWorkflow}
                  configLoaded={execution.configLoaded}
                  showWorkflowDropdown={uiState.showWorkflowDropdown}
                  onWorkflowDropdownToggle={uiState.setShowWorkflowDropdown}
                  onWorkflowSelect={handleWorkflowSelect}
                  selectedMonitor={execution.selectedMonitor}
                  availableMonitors={execution.availableMonitors}
                  showMonitorDropdown={uiState.showMonitorDropdown}
                  onMonitorDropdownToggle={uiState.setShowMonitorDropdown}
                  onMonitorSelect={handleMonitorSelect}
                  autoMinimize={execution.autoMinimize}
                  onAutoMinimizeChange={execution.setAutoMinimize}
                  executionActive={execution.executionActive}
                  onStartExecution={execution.startExecution}
                  onStopExecution={execution.stopExecution}
                />
              </div>
            </div>
          </Tabs.Content>

          {/* Logs Tab */}
          <Tabs.Content value="logs" className="h-full outline-none">
            <div className="h-full card m-4">
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
                aiOutputLines={aiOutputLogs}
                onClearAiOutput={clearAiOutputLogs}
                logCount={logCount}
                imageLogCount={imageLogCount}
                actionCount={actionLogViewData?.visible_count || 0}
                aiOutputCount={aiOutputLogCount}
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
          </Tabs.Content>

          {/* Capture Tab */}
          <Tabs.Content value="capture" className="h-full outline-none overflow-y-auto">
            <CaptureTab
              onLog={addLog}
              projects={projectSelection.projects}
              selectedProjectId={projectSelection.selectedProjectId}
            />
          </Tabs.Content>

          {/* Extract Tab */}
          <Tabs.Content value="extract" className="h-full outline-none overflow-y-auto">
            <ExtractionTab
              onLog={addLog}
              projects={projectSelection.projects}
              selectedProjectId={projectSelection.selectedProjectId}
            />
          </Tabs.Content>

          {/* Dataset Tab */}
          <Tabs.Content value="dataset" className="h-full outline-none overflow-y-auto">
            <DatasetPackager />
          </Tabs.Content>

          {/* Settings Tab */}
          <Tabs.Content value="settings" className="h-full outline-none">
            <div className="h-full">
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
              />
            </div>
          </Tabs.Content>
        </div>
      </Tabs.Root>

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
          onConfigurationPanelCollapse={() => {
            // Could be handled here if needed for other side effects
          }}
          onExecutionPanelCollapse={() => {
            // Could be handled here if needed for other side effects
          }}
        >
          <AppContent />
        </ExecutionProvider>
      </EventManagerProvider>
    </AuthProvider>
  );
}
