/**
 * App.tsx (Refactored - SRP Compliant)
 *
 * Main application component - thin orchestration layer.
 * Responsibilities:
 * - Provider composition
 * - High-level layout structure
 * - Component orchestration
 *
 * All business logic, state management, and UI concerns have been
 * extracted to contexts, hooks, and dedicated components.
 */

import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FileText } from "lucide-react";

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
  useAutoScroll,
} from "./hooks";

// Components
import StatusIndicator from "./components/StatusIndicator";
import { ConfigurationPanel } from "./components/ConfigurationPanel";
import { ExecutionControlPanel } from "./components/ExecutionControlPanel";
import { LogTabNavigation } from "./components/LogTabNavigation";
import { LogTabActions } from "./components/LogTabActions";
import { GeneralLogTab } from "./components/GeneralLogTab";
import ImageLogTable from "./components/ImageLogTable";
import ActionLogTable from "./components/ActionLogTable";
import ActionDetailModal from "./components/ActionDetailModal";
import ImageDetailModal from "./components/ImageDetailModal";
import { Settings } from "./components/Settings";
import { LoginScreen } from "./components/LoginScreen";

// Styles
import "./index.css";

/**
 * Main app content (inside providers)
 */
function AppContent() {
  // Auth state from context
  const auth = useAuth();

  // Execution state from context
  const execution = useExecution();

  // Logs from LogManager
  const {
    logs,
    imageLogs,
    addLog,
    clearGeneralLogs,
    clearImageLogs,
    copyLogs,
    logCount,
    imageLogCount,
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

  // Auto-scroll for general logs
  const logViewerRef = useRef<HTMLDivElement>(null);
  useAutoScroll({
    enabled: uiState.activeLogTab === "general",
    containerRef: logViewerRef,
    dependencies: [logs],
  });

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

  // Refresh action log when switching to Actions tab
  useEffect(() => {
    console.log("[TAB_SWITCH] useEffect triggered, activeLogTab:", uiState.activeLogTab);
    if (uiState.activeLogTab === "actions") {
      console.log("[TAB_SWITCH] Switched to Actions tab, refreshing action log");
      refreshActionLog();
    }
  }, [uiState.activeLogTab, refreshActionLog]);

  // Event handlers
  const handleWorkflowSelect = (workflowId: string) => {
    execution.setSelectedWorkflow(workflowId);
    uiState.setShowWorkflowDropdown(false);
  };

  const handleMonitorSelect = (index: number) => {
    execution.setSelectedMonitor(index);
    uiState.setShowMonitorDropdown(false);
  };

  const handleCopyLogs = async () => {
    const logType =
      uiState.activeLogTab === "general"
        ? "general"
        : uiState.activeLogTab === "image"
          ? "image"
          : "actions";
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

  return (
    <div className="min-h-screen bg-background grid-dots">
      {/* Status Bar */}
      <div className="border-b border-border bg-card/50 backdrop-blur-sm">
        <div className="container mx-auto px-6 py-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-4">
              <div className="flex items-center gap-2">
                <StatusIndicator
                  pythonStatus={execution.pythonStatus}
                  configLoaded={execution.configLoaded}
                  executionActive={execution.executionActive}
                />
                <span className="text-sm font-medium text-muted-foreground">
                  Python Status:{" "}
                  <span
                    className={
                      execution.pythonStatus === "running" ? "text-green-600" : "text-orange-600"
                    }
                  >
                    {execution.pythonStatus}
                  </span>
                </span>
              </div>
              {execution.config && (
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <FileText className="w-4 h-4" />
                  <span>{execution.config.name}</span>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Main Content */}
      <div className="container mx-auto p-6 space-y-6">
        {/* Header */}
        <div className="text-center space-y-2">
          <h1 className="text-4xl font-bold gradient-text">Qontinui Runner</h1>
          <p className="text-muted-foreground">Workflow Automation</p>
        </div>

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

        {/* Log Viewer */}
        <div className="card">
          {/* Tab Navigation */}
          <div className="flex items-center justify-between border-b border-border">
            <LogTabNavigation
              activeTab={uiState.activeLogTab}
              onTabChange={uiState.setActiveLogTab}
              logCount={logCount}
              imageLogCount={imageLogCount}
              actionCount={actionLogViewData?.visible_count || 0}
            />

            {/* Tab Actions */}
            <LogTabActions
              activeTab={uiState.activeLogTab}
              showLogFilter={uiState.showLogFilter}
              onToggleLogFilter={uiState.setShowLogFilter}
              logLevel={logLevel}
              onLogLevelChange={setLogLevel}
              onClearGeneralLogs={clearGeneralLogs}
              onClearImageLogs={clearImageLogs}
              onClearActionLogs={clearActionLogs}
              onClearAllLogs={clearAllLogs}
              onCopyLogs={handleCopyLogs}
              copySuccess={uiState.copySuccess}
            />
          </div>

          {/* Tab Content */}
          <div className="p-4">
            {/* General Tab */}
            {uiState.activeLogTab === "general" && (
              <GeneralLogTab logs={filteredLogs} containerRef={logViewerRef} />
            )}

            {/* Image Recognition Tab */}
            {uiState.activeLogTab === "image" && (
              <div style={{ maxHeight: "400px", overflowY: "auto" }}>
                <ImageLogTable imageLogs={imageLogs} onRowClick={modalState.openImageModal} />
              </div>
            )}

            {/* Actions Tab */}
            {uiState.activeLogTab === "actions" && (
              <>
                {actionLogLoading && (
                  <div className="text-center text-muted-foreground py-8">
                    Loading action log...
                  </div>
                )}
                {actionLogError && (
                  <div className="text-center text-red-600 py-8">Error: {actionLogError}</div>
                )}
                {!actionLogLoading && !actionLogError && actionLogViewData && (
                  <ActionLogTable
                    actions={actionLogViewData.actions}
                    onRowClick={modalState.openActionModal}
                  />
                )}
              </>
            )}

            {/* Settings Tab */}
            {uiState.activeLogTab === "settings" && (
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
              />
            )}
          </div>
        </div>
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
