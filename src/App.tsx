/**
 * App.tsx (Refactored)
 *
 * Main application component - now focused on layout and composition only.
 * Business logic moved to contexts and managers.
 */

import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Play,
  Square,
  FileText,
  Cpu,
  Zap,
  Terminal,
  Trash2,
  Filter,
  ChevronDown,
  Copy,
  Check,
  Image,
  Settings as SettingsIcon,
} from "lucide-react";

// Contexts
import {
  ExecutionProvider,
  useExecution,
  EventManagerProvider,
} from "./contexts";

// Managers
import { setupEventHandlers } from "./managers";
import { eventRouter } from "./managers";

// Hooks
import { useActionLogView, useLogManager } from "./hooks";

// Components
import StatusIndicator from "./components/StatusIndicator";
import CollapsiblePanel from "./components/CollapsiblePanel";
import RecordingControl from "./components/RecordingControl";
import ActionLogTable from "./components/ActionLogTable";
import ActionDetailModal from "./components/ActionDetailModal";
import ImageLogTable from "./components/ImageLogTable";
import ImageDetailModal from "./components/ImageDetailModal";
import { Settings } from "./components/Settings";

// Types
import { ActionLogEntry } from "./types/displayProfile";
import { ImageRecognitionEntry } from "./managers/LogManager";

// Styles
import "./index.css";

/**
 * Main app content (inside providers)
 */
function AppContent() {
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

  // UI state (what remains in App.tsx)
  const [autoScroll] = useState(true);
  const [logLevel, setLogLevel] = useState("all");
  const [activeLogTab, setActiveLogTab] = useState<"general" | "image" | "actions" | "settings">(
    "general"
  );
  const [showWorkflowDropdown, setShowWorkflowDropdown] = useState(false);
  const [showLogFilter, setShowLogFilter] = useState(false);
  const [showMonitorDropdown, setShowMonitorDropdown] = useState(false);
  const [copySuccess, setCopySuccess] = useState(false);
  const [configPanelCollapsed, setConfigPanelCollapsed] = useState(false);
  const [executionPanelCollapsed, setExecutionPanelCollapsed] = useState(false);
  const [selectedAction, setSelectedAction] = useState<ActionLogEntry | null>(null);
  const [isActionModalOpen, setIsActionModalOpen] = useState(false);
  const [selectedImageEntry, setSelectedImageEntry] = useState<ImageRecognitionEntry | null>(null);
  const [isImageModalOpen, setIsImageModalOpen] = useState(false);
  const logViewerRef = useRef<HTMLDivElement>(null);

  // Setup event handlers on mount (ONCE only)
  useEffect(() => {
    console.log("[APP] Setting up event handlers");
    const cleanup = setupEventHandlers(eventRouter, {
      setPythonStatus: execution.setPythonStatus,
      setConfigLoaded: execution.setConfigLoaded,
      setExecutionActive: execution.setExecutionActive,
    });

    // Cleanup on unmount
    return cleanup;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // Empty deps - run only on mount to prevent duplicate event handlers

  // Refresh action log when switching to Actions tab
  useEffect(() => {
    console.log("[TAB_SWITCH] useEffect triggered, activeLogTab:", activeLogTab);
    if (activeLogTab === "actions") {
      console.log("[TAB_SWITCH] Switched to Actions tab, refreshing action log");
      refreshActionLog();
    }
  }, [activeLogTab, refreshActionLog]);

  // Auto-scroll logs
  useEffect(() => {
    if (autoScroll && logViewerRef.current && activeLogTab === "general") {
      const scrollContainer = logViewerRef.current;
      requestAnimationFrame(() => {
        scrollContainer.scrollTop = scrollContainer.scrollHeight;
      });
    }
  }, [logs, autoScroll, activeLogTab]);

  // Handle log filter
  const filteredLogs = logLevel === "all" ? logs : logs.filter((log) => log.level === logLevel);

  // Copy logs handler
  const handleCopyLogs = async () => {
    const logType = activeLogTab === "general" ? "general" : activeLogTab === "image" ? "image" : "actions";
    const success = await copyLogs(
      logType,
      logType === "actions" ? actionLogViewData?.actions : undefined
    );
    if (success) {
      setCopySuccess(true);
      setTimeout(() => setCopySuccess(false), 2000);
    }
  };

  // Clear action logs
  const clearActionLogs = async () => {
    try {
      await invoke("clear_action_log");
      refreshActionLog();
    } catch (error) {
      console.error("Failed to clear action logs:", error);
    }
  };

  // Clear all logs
  const clearAllLogs = async () => {
    clearGeneralLogs();
    clearImageLogs();
    await clearActionLogs();
  };

  // Handle workflow selection
  const handleWorkflowSelect = (workflowId: string) => {
    execution.setSelectedWorkflow(workflowId);
    setShowWorkflowDropdown(false);
  };

  // Handle monitor selection
  const handleMonitorSelect = (index: number) => {
    execution.setSelectedMonitor(index);
    setShowMonitorDropdown(false);
  };

  // Handle action row click
  const handleActionClick = (action: ActionLogEntry) => {
    setSelectedAction(action);
    setIsActionModalOpen(true);
  };

  // Handle image log row click
  const handleImageClick = (entry: ImageRecognitionEntry) => {
    setSelectedImageEntry(entry);
    setIsImageModalOpen(true);
  };

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
          <p className="text-muted-foreground">Workflow Automation & Recording</p>
        </div>

        {/* Control Panels Grid */}
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
          {/* Configuration Panel */}
          <CollapsiblePanel
            title="Configuration"
            icon={<FileText className="w-4 h-4" />}
            collapsed={configPanelCollapsed}
            onToggle={setConfigPanelCollapsed}
          >
            <div className="space-y-4">
              <button
                onClick={execution.loadConfiguration}
                className="w-full btn-primary flex items-center justify-center gap-2"
              >
                <FileText className="w-4 h-4" />
                Load Configuration
              </button>

              <button
                onClick={execution.loadLastConfiguration}
                className="w-full btn-secondary flex items-center justify-center gap-2"
              >
                <FileText className="w-4 h-4" />
                Load Last Config
              </button>

              {execution.config && (
                <div className="space-y-2">
                  <div className="p-3 bg-accent/50 rounded-lg border border-border/50">
                    <p className="font-medium text-sm mb-2">{execution.config.name}</p>
                    <div className="text-sm space-y-1">
                      <p className="text-muted-foreground">States: {execution.config.statesCount}</p>
                      <p className="text-muted-foreground">
                        Workflows: {execution.config.workflowsCount}
                      </p>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </CollapsiblePanel>

          {/* Execution Control Panel */}
          <CollapsiblePanel
            title="Execution Control"
            icon={<Cpu className="w-4 h-4" />}
            collapsed={executionPanelCollapsed}
            onToggle={setExecutionPanelCollapsed}
          >
            <div className="space-y-4">
              {/* Workflow Selector */}
              <div className="relative">
                <button
                  onClick={() => setShowWorkflowDropdown(!showWorkflowDropdown)}
                  disabled={!execution.configLoaded || execution.workflows.length === 0}
                  className="w-full btn-secondary flex items-center justify-between gap-2"
                >
                  <span className="truncate">
                    {execution.selectedWorkflow
                      ? execution.workflows.find((w) => w.id === execution.selectedWorkflow)
                          ?.name || "Select Workflow"
                      : "Select Workflow"}
                  </span>
                  <ChevronDown className="w-4 h-4 flex-shrink-0" />
                </button>

                {showWorkflowDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-lg shadow-lg max-h-60 overflow-y-auto">
                    {execution.workflows.map((workflow) => (
                      <button
                        key={workflow.id}
                        onClick={() => handleWorkflowSelect(workflow.id)}
                        className="w-full px-4 py-2 text-left hover:bg-accent hover:text-accent-foreground transition-colors"
                      >
                        {workflow.name}
                      </button>
                    ))}
                  </div>
                )}
              </div>

              {/* Monitor Selector */}
              <div className="relative">
                <button
                  onClick={() => setShowMonitorDropdown(!showMonitorDropdown)}
                  className="w-full btn-secondary flex items-center justify-between gap-2"
                >
                  <span>Monitor {execution.selectedMonitor}</span>
                  <ChevronDown className="w-4 h-4" />
                </button>

                {showMonitorDropdown && (
                  <div className="absolute z-10 w-full mt-1 bg-card border border-border rounded-lg shadow-lg">
                    {execution.availableMonitors.map((index) => (
                      <button
                        key={index}
                        onClick={() => handleMonitorSelect(index)}
                        className="w-full px-4 py-2 text-left hover:bg-accent hover:text-accent-foreground transition-colors"
                      >
                        Monitor {index}
                      </button>
                    ))}
                  </div>
                )}
              </div>

              {/* Auto-minimize Toggle */}
              {execution.availableMonitors.length === 1 && (
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={execution.autoMinimize}
                    onChange={(e) => execution.setAutoMinimize(e.target.checked)}
                    className="rounded"
                  />
                  <span>Auto-minimize window on start</span>
                </label>
              )}

              {/* Start/Stop Buttons */}
              <div className="flex gap-2">
                <button
                  onClick={execution.startExecution}
                  disabled={
                    execution.executionActive ||
                    !execution.configLoaded ||
                    !execution.selectedWorkflow
                  }
                  className="flex-1 btn-success flex items-center justify-center gap-2"
                >
                  <Play className="w-4 h-4" />
                  Start
                </button>
                <button
                  onClick={execution.stopExecution}
                  disabled={!execution.executionActive}
                  className="flex-1 btn-danger flex items-center justify-center gap-2"
                >
                  <Square className="w-4 h-4" />
                  Stop
                </button>
              </div>
            </div>
          </CollapsiblePanel>

          {/* Recording Control Panel */}
          <RecordingControl
            pythonStatus={execution.pythonStatus}
            configLoaded={execution.configLoaded}
            onLog={addLog}
          />
        </div>

        {/* Log Viewer */}
        <div className="card">
          {/* Tab Navigation */}
          <div className="flex items-center justify-between border-b border-border">
            <div className="flex gap-1 p-1">
              <button
                onClick={() => setActiveLogTab("general")}
                className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
                  activeLogTab === "general"
                    ? "bg-accent text-accent-foreground font-medium"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <Terminal className="w-4 h-4" />
                <span>General</span>
                {logCount > 0 && (
                  <span className="ml-1 px-2 py-0.5 text-xs bg-primary text-primary-foreground rounded-full">
                    {logCount}
                  </span>
                )}
              </button>

              <button
                onClick={() => setActiveLogTab("image")}
                className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
                  activeLogTab === "image"
                    ? "bg-accent text-accent-foreground font-medium"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <Image className="w-4 h-4" />
                <span>Image Recognition</span>
                {imageLogCount > 0 && (
                  <span className="ml-1 px-2 py-0.5 text-xs bg-purple-600 text-white rounded-full">
                    {imageLogCount}
                  </span>
                )}
              </button>

              <button
                onClick={() => setActiveLogTab("actions")}
                className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
                  activeLogTab === "actions"
                    ? "bg-accent text-accent-foreground font-medium"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <Zap className="w-4 h-4" />
                <span>Actions</span>
                {actionLogViewData && actionLogViewData.visible_count > 0 && (
                  <span className="ml-1 px-2 py-0.5 text-xs bg-green-600 text-white rounded-full">
                    {actionLogViewData.visible_count}
                  </span>
                )}
              </button>

              <button
                onClick={() => setActiveLogTab("settings")}
                className={`px-4 py-2 rounded-lg flex items-center gap-2 transition-colors ${
                  activeLogTab === "settings"
                    ? "bg-accent text-accent-foreground font-medium"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <SettingsIcon className="w-4 h-4" />
                <span>Settings</span>
              </button>
            </div>

            {/* Tab Actions */}
            <div className="flex items-center gap-2 px-4">
              {activeLogTab === "general" && (
                <>
                  <div className="relative">
                    <button
                      onClick={() => setShowLogFilter(!showLogFilter)}
                      className="p-2 hover:bg-accent rounded-lg transition-colors"
                    >
                      <Filter className="w-4 h-4" />
                    </button>
                    {showLogFilter && (
                      <div className="absolute right-0 mt-1 bg-card border border-border rounded-lg shadow-lg z-10">
                        {["all", "info", "warning", "error", "debug", "success"].map((level) => (
                          <button
                            key={level}
                            onClick={() => {
                              setLogLevel(level);
                              setShowLogFilter(false);
                            }}
                            className={`w-full px-4 py-2 text-left hover:bg-accent transition-colors ${
                              logLevel === level ? "bg-accent" : ""
                            }`}
                          >
                            {level.charAt(0).toUpperCase() + level.slice(1)}
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                  <button
                    onClick={clearGeneralLogs}
                    className="p-2 hover:bg-accent rounded-lg transition-colors"
                    title="Clear logs"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                </>
              )}

              {activeLogTab === "image" && (
                <button
                  onClick={clearImageLogs}
                  className="p-2 hover:bg-accent rounded-lg transition-colors"
                  title="Clear image logs"
                >
                  <Trash2 className="w-4 h-4" />
                </button>
              )}

              {activeLogTab === "actions" && (
                <button
                  onClick={clearActionLogs}
                  className="p-2 hover:bg-accent rounded-lg transition-colors"
                  title="Clear action logs"
                >
                  <Trash2 className="w-4 h-4" />
                </button>
              )}

              {/* Clear All button - visible on all tabs */}
              <button
                onClick={clearAllLogs}
                className="p-2 hover:bg-destructive hover:text-destructive-foreground rounded-lg transition-colors flex items-center"
                title="Clear all logs (General, Image, and Actions)"
              >
                <Trash2 className="w-4 h-4" />
                <Trash2 className="w-4 h-4 -ml-1" />
              </button>

              <button
                onClick={handleCopyLogs}
                className="p-2 hover:bg-accent rounded-lg transition-colors"
                title="Copy to clipboard"
              >
                {copySuccess ? (
                  <Check className="w-4 h-4 text-green-600" />
                ) : (
                  <Copy className="w-4 h-4" />
                )}
              </button>
            </div>
          </div>

          {/* Tab Content */}
          <div className="p-4">
            {/* General Tab */}
            {activeLogTab === "general" && (
              <div
                ref={logViewerRef}
                className="log-container font-mono text-sm space-y-1"
                style={{ maxHeight: "400px", overflowY: "auto" }}
              >
                {filteredLogs.length === 0 ? (
                  <div className="text-center text-muted-foreground py-8">
                    No logs yet. Start a workflow to see execution logs.
                  </div>
                ) : (
                  filteredLogs.map((log) => (
                    <div key={log.id} className="flex gap-2">
                      <span className="text-muted-foreground flex-shrink-0">[{log.timestamp}]</span>
                      <span
                        className={`flex-shrink-0 ${
                          log.level === "error"
                            ? "text-red-600"
                            : log.level === "warning"
                              ? "text-orange-600"
                              : log.level === "success"
                                ? "text-green-600"
                                : log.level === "debug"
                                  ? "text-blue-600"
                                  : "text-foreground"
                        }`}
                      >
                        [{log.level.toUpperCase()}]
                      </span>
                      <span className="break-all">{log.message}</span>
                    </div>
                  ))
                )}
              </div>
            )}

            {/* Image Recognition Tab */}
            {activeLogTab === "image" && (
              <div style={{ maxHeight: "400px", overflowY: "auto" }}>
                <ImageLogTable imageLogs={imageLogs} onRowClick={handleImageClick} />
              </div>
            )}

            {/* Actions Tab */}
            {activeLogTab === "actions" && (
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
                    onRowClick={handleActionClick}
                  />
                )}
              </>
            )}

            {/* Settings Tab */}
            {activeLogTab === "settings" && (
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
        action={selectedAction}
        isOpen={isActionModalOpen}
        onClose={() => setIsActionModalOpen(false)}
      />

      {/* Image Detail Modal */}
      <ImageDetailModal
        entry={selectedImageEntry}
        isOpen={isImageModalOpen}
        onClose={() => setIsImageModalOpen(false)}
      />
    </div>
  );
}

/**
 * Main App component with providers
 */
export default function App() {
  return (
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
  );
}
