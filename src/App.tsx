import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ApolloProvider } from "@apollo/client/react";

import {
  ExecutionProvider,
  useExecution,
  EventManagerProvider,
  AutoContinueProvider,
} from "./contexts";
import { RenderLogWrapper, UIBridgeHooks } from "./lib/ui-bridge";
import { AuthProvider, useAuth } from "./components/AuthProvider";
import { TutorialProvider } from "./contexts/TutorialContext";
import { ContextualTutorial } from "./components/tutorial";
import { DemoVisualOverlay } from "./components/demo-video/DemoVisualOverlay";
import { getGraphQLClient } from "./lib/graphql-client";

import { UIBridgeProvider, AutoRegisterProvider } from "ui-bridge";

import { setupEventHandlers, eventRouter } from "./managers";
import {
  startBackgroundObserver,
  stopBackgroundObserver,
} from "./services/background-observer-service";

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
  useCloudRelayAutoConnect,
  UIBridgeEventHandler,
  SpecExecutionHandler,
} from "./hooks";
import { useGlobalLogSources } from "./hooks/useGlobalLogSources";
import { useCanaryAlerts } from "./hooks/useCanaryAlerts";
import { useGraphDataRefresh } from "./hooks/useGraphDataRefresh";
import { useObservationServices } from "./hooks/useObservationServices";
import { useToast } from "./hooks/useToast";
import { useErrorNotifications } from "./hooks/useErrorNotifications";
import { useStateMachineRegistration } from "./hooks/useStateMachineRegistration";

import { ToastContainer } from "./components/ToastContainer";
import StatusIndicator from "./components/StatusIndicator";
import ActionDetailModal from "./components/ActionDetailModal";
import ImageDetailModal from "./components/ImageDetailModal";
import { LoginScreen } from "./components/LoginScreen";
import { SetupWizard } from "./components/setup-wizard";
import { LogSourcePicker } from "./components/LogSourcePicker";
import { Sidebar } from "./components/navigation";
import { TerminalPage } from "./components/terminal";
import { TerminalPageTabBar } from "./components/terminal/TerminalPageTabBar";
import { useTerminalPages } from "./components/terminal/useTerminalPages";
import { TerminalPageProvider } from "./components/terminal/TerminalPageContext";
import { ReorganizeDialog, type ReorganizePlan } from "./components/terminal/ReorganizeDialog";
import { PerformanceOverlay } from "./components/dev";
import { CommandPalette } from "./components/unified-search/CommandPalette";
import { useTaskRuns } from "./hooks/useAiData";
import { getAllSpecs } from "./lib/spec-registry";
import { getGlobalSpecStore } from "@qontinui/ui-bridge/specs";
import { autoPopulateCtr, getGlobalCtr } from "@qontinui/ui-bridge/ctr";
import { getGlobalRegistry } from "@qontinui/ui-bridge/core";

import { instanceStorage } from "@/lib/instance-storage";

import {
  NavigationProvider,
  useNavigation,
  RunnerPageContext,
  TabContent,
  AppToasts,
  useAppNavigation,
  useRunLastWorkflow,
} from "./components/app";
import type { MainTabId, LogSubTab } from "./components/app";

import "./index.css";

declare global {
  interface WindowEventMap {
    "ui-bridge-navigate": CustomEvent<{ page: string; url?: string }>;
    "navigate-to-error-monitor": CustomEvent<{ taskRunId?: string; taskRunName?: string }>;
    "navigate-to-active": Event;
    "sm-show-exploration": Event;
    "sm-config-changed": CustomEvent<{ configId: string | null }>;
    "runner-name-changed": CustomEvent<string>;
  }
}

function AppContent() {
  const auth = useAuth();
  const isApiReady = useApiReady();
  const execution = useExecution();
  useStateMachineRegistration();

  const [setupCompleted, setSetupCompleted] = useState<boolean | null>(null);
  useEffect(() => {
    invoke<boolean>("check_setup_completed")
      .then(setSetupCompleted)
      .catch(() => setSetupCompleted(true));
  }, []);

  const { data: recentTaskRuns = [] } = useTaskRuns(1);
  const lastRun = recentTaskRuns.length > 0 ? recentTaskRuns[0] : null;
  const lastRunWorkflowName = lastRun?.workflow_name ?? lastRun?.task_name ?? null;
  const lastRunWorkflowId = lastRun?.workflow_name ?? null;

  const {
    activeTab,
    setActiveTab,
    sidebarCollapsed,
    handleSidebarCollapsedChange,
    setTerminalSessionCount,
    staleTaskMessage,
    setStaleTaskMessage,
    errorMonitorScope,
    clearErrorMonitorScope,
    ProfilerWrapper,
  } = useAppNavigation();

  const terminalPages = useTerminalPages();
  const [showReorganize, setShowReorganize] = useState(false);

  const handleReorganize = useCallback(
    async (plan: ReorganizePlan) => {
      // Create new pages for each group
      for (const group of plan.pages) {
        // Check if a page with this name already exists
        const existing = terminalPages.pages.find((p) => p.name === group.name);
        if (!existing) {
          terminalPages.addPage(group.name);
        }
      }

      // For now, just rename existing pages to match the AI proposal.
      // Full session moving (close/recreate terminals) requires terminal manager
      // access which is page-scoped. Rename is the safe first step.
      for (let i = 0; i < plan.pages.length && i < terminalPages.pages.length; i++) {
        terminalPages.renamePage(terminalPages.pages[i].id, plan.pages[i].name);
      }
    },
    [terminalPages],
  );

  const {
    isRunningLastWorkflow,
    runLastWorkflowError,
    setRunLastWorkflowError,
    handleRunLastWorkflow,
  } = useRunLastWorkflow(lastRun, setActiveTab);

  const [editWorkflowId, setEditWorkflowId] = useState<string | null>(null);
  const [activeLogSubTab, setActiveLogSubTab] = useState<LogSubTab>("general");
  const [showLogSourcePicker, setShowLogSourcePicker] = useState(false);

  useEffect(() => {
    if (activeTab !== "unified-workflow-builder") {
      setEditWorkflowId(null);
    }
  }, [activeTab]);

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
  } = useLogManager();

  const {
    viewData: actionLogViewData,
    loading: actionLogLoading,
    error: actionLogError,
    refresh: refreshActionLog,
  } = useActionLogView({
    autoRefreshInterval: execution.executionActive ? 1000 : 0,
  });

  const uiState = useUIState();
  const modalState = useModalState();
  const { logLevel, setLogLevel, filteredLogs } = useLogFilter(logs);
  const projectSelection = useProjectSelection();
  const projectLogs = useProjectLogs();
  const globalLogSources = useGlobalLogSources();
  const { alerts: canaryAlerts, dismissAlert: dismissCanaryAlert } = useCanaryAlerts();
  const { toasts, showToast, dismissToast } = useToast();
  useErrorNotifications(showToast);

  const { activities: backgroundActivities } = useBackgroundActivities({
    isExtracting: false,
    extractionUrl: undefined,
    extractionProgress: undefined,
  });

  const webSocket = useWebSocketAutoConnect({
    isAuthenticated: auth.authStatus?.authenticated ?? false,
    selectedProjectId: projectSelection.selectedProjectId,
    onLog: addLog,
  });

  useCloudRelayAutoConnect();

  // Subscribe to runner events and auto-invalidate graph analytics queries
  // when tasks complete, findings change, or workflows are generated.
  useGraphDataRefresh();

  // Initialize observation persistence services (session summaries + learning bridge)
  useObservationServices(projectSelection.selectedProjectId);

  // Start screenpipe-inspired background observer for activity timeline
  useEffect(() => {
    // Delay start to ensure UI Bridge registry is initialized
    const timer = setTimeout(() => {
      try {
        startBackgroundObserver();
      } catch (e) {
        console.debug("[App] BackgroundObserver start deferred:", e);
      }
    }, 5000);
    return () => {
      clearTimeout(timer);
      stopBackgroundObserver();
    };
  }, []);

  useEffect(() => {
    if (auth.authStatus?.authenticated && !auth.loading) {
      projectSelection.loadProjects();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- loadProjects changes with selectedProjectId; including it would reload projects on every selection change
  }, [auth.authStatus?.authenticated, auth.loading]);

  useEffect(() => {
    if (projectSelection.selectedProjectId && projectSelection.selectedProjectName) {
      projectLogs.loadConfig(
        projectSelection.selectedProjectId,
        projectSelection.selectedProjectName,
      );
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only reload config when project selection changes, not when projectLogs object reference changes
  }, [
    projectSelection.selectedProjectId,
    projectSelection.selectedProjectName,
    projectLogs.loadConfig,
  ]);

  useEffect(() => {
    const cleanup = setupEventHandlers(eventRouter, {
      setPythonStatus: execution.setPythonStatus,
      setConfigLoaded: execution.setConfigLoaded,
      setExecutionActive: execution.setExecutionActive,
    });

    return cleanup;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-time setup; setters are stable useState dispatchers accessed via context object
  }, []);

  useEffect(() => {
    if (activeTab === "logs" && activeLogSubTab === "actions") {
      refreshActionLog();
    }
  }, [activeTab, activeLogSubTab, refreshActionLog]);

  const handleCopyLogs = useCallback(async () => {
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
  }, [activeLogSubTab, copyLogs, actionLogViewData, uiState]);

  const clearActionLogs = useCallback(async () => {
    try {
      await invoke("clear_action_log");
      refreshActionLog();
    } catch (error) {
      console.error("Failed to clear action logs:", error);
    }
  }, [refreshActionLog]);

  const clearAllLogs = useCallback(async () => {
    clearGeneralLogs();
    clearImageLogs();
    clearAiOutputLogs();
    await clearActionLogs();
  }, [clearGeneralLogs, clearImageLogs, clearAiOutputLogs, clearActionLogs]);

  const handleGoToRecap = useCallback(() => {
    if (lastRun?.id) {
      instanceStorage.setJSON("qontinui-selected-task-run-id", lastRun.id);
    }
    if (lastRun?.workflow_name && execution.workflows.length > 0) {
      const matchingWorkflow = execution.workflows.find(
        (w) => w.name === lastRun.workflow_name || w.id === lastRun.workflow_name,
      );
      if (matchingWorkflow) {
        execution.selectWorkflowWithPersistence(matchingWorkflow.id);
      }
    }
    setActiveTab("run-recap");
  }, [lastRun, execution, setActiveTab]);

  const isAuthLoading = auth.loading || auth.devAutoLoginPending;
  const isInitializing = isAuthLoading || !isApiReady;
  if (isInitializing) {
    const loadingMessage = auth.devAutoLoginPending
      ? "Signing in..."
      : isAuthLoading
        ? "Checking authentication..."
        : "Starting API server...";
    return (
      <div className="min-h-screen bg-background grid-dots flex items-center justify-center">
        <div className="card p-8 text-center space-y-4">
          <div className="inline-block w-12 h-12 border-4 border-primary border-t-transparent rounded-full animate-spin" />
          <p className="text-muted-foreground">{loadingMessage}</p>
        </div>
      </div>
    );
  }

  if (!auth.authStatus?.authenticated) {
    return <LoginScreen onLogin={auth.login} />;
  }

  if (setupCompleted === false) {
    return <SetupWizard onComplete={() => setSetupCompleted(true)} />;
  }

  const lastRunId = lastRun?.id;

  return (
    <ProfilerWrapper>
      <RenderLogWrapper
        activeTab={activeTab}
        taskRunId={lastRunId}
        enableOnMount={true}
        enableMutationObserver={true}
        mutationDebounceMs={500}
      >
        <RunnerPageContext activeTab={activeTab} />
        <UIBridgeHooks
          activeTab={activeTab}
          sidebarCollapsed={sidebarCollapsed}
          isActionModalOpen={modalState.isActionModalOpen}
          isImageModalOpen={modalState.isImageModalOpen}
          showLogSourcePicker={showLogSourcePicker}
          executionActive={execution.executionActive}
        />
        <div className="h-screen w-screen bg-background grid-dots flex flex-col overflow-hidden min-w-[1200px] min-h-[700px]">
          <StatusIndicator
            pythonStatus={execution.pythonStatus}
            executionActive={execution.executionActive}
            backgroundActivities={backgroundActivities}
          />

          <div className="flex flex-1 overflow-hidden">
            <Sidebar
              activeTab={activeTab}
              onTabChange={(tab) => setActiveTab(tab as MainTabId)}
              collapsed={sidebarCollapsed}
              onCollapsedChange={handleSidebarCollapsedChange}
            />

            <main className="flex-1 overflow-hidden relative">
              <TabContent
                activeTab={activeTab}
                setActiveTab={setActiveTab}
                addLog={addLog}
                addAiOutputLog={addAiOutputLog}
                logs={logs}
                imageLogs={imageLogs}
                aiOutputLogs={aiOutputLogs}
                clearGeneralLogs={clearGeneralLogs}
                clearImageLogs={clearImageLogs}
                clearAiOutputLogs={clearAiOutputLogs}
                logCount={logCount}
                imageLogCount={imageLogCount}
                filteredLogs={filteredLogs}
                logLevel={logLevel}
                setLogLevel={setLogLevel}
                uiState={uiState}
                modalState={modalState}
                actionLogViewData={actionLogViewData}
                actionLogLoading={actionLogLoading}
                actionLogError={actionLogError}
                refreshActionLog={refreshActionLog}
                activeLogSubTab={activeLogSubTab}
                setActiveLogSubTab={setActiveLogSubTab}
                editWorkflowId={editWorkflowId}
                setEditWorkflowId={setEditWorkflowId}
                globalLogSourceSettings={globalLogSources.settings}
                projectSelection={projectSelection}
                projectLogs={projectLogs}
                webSocket={webSocket}
                lastRun={lastRun}
                lastRunWorkflowId={lastRunWorkflowId}
                lastRunWorkflowName={lastRunWorkflowName}
                isRunningLastWorkflow={isRunningLastWorkflow}
                handleRunLastWorkflow={handleRunLastWorkflow}
                handleGoToRecap={handleGoToRecap}
                handleCopyLogs={handleCopyLogs}
                clearActionLogs={clearActionLogs}
                clearAllLogs={clearAllLogs}
                errorMonitorScope={errorMonitorScope}
                clearErrorMonitorScope={clearErrorMonitorScope}
              />
              <div
                className={`absolute inset-0 flex flex-col ${activeTab === "terminal" ? "" : "hidden"}`}
              >
                <TerminalPageTabBar
                  pages={terminalPages.pages}
                  activePageId={terminalPages.activePageId}
                  onSelectPage={terminalPages.setActivePageId}
                  onAddPage={terminalPages.addPage}
                  onRemovePage={terminalPages.removePage}
                  onRenamePage={terminalPages.renamePage}
                  onReorganize={() => setShowReorganize(true)}
                />
                {showReorganize && (
                  <ReorganizeDialog
                    pages={terminalPages.pages}
                    onClose={() => setShowReorganize(false)}
                    onApply={async (plan) => {
                      await handleReorganize(plan);
                      setShowReorganize(false);
                    }}
                  />
                )}
                <div className="flex-1 min-h-0">
                  <TerminalPageProvider value={terminalPages.activePageId}>
                    <TerminalPage
                      key={terminalPages.activePageId}
                      onNavigateToBuilder={() => setActiveTab("unified-workflow-builder")}
                      onNavigateToActive={() => setActiveTab("active")}
                      onSessionCountChange={setTerminalSessionCount}
                    />
                  </TerminalPageProvider>
                </div>
              </div>
            </main>
          </div>

          <ActionDetailModal
            action={modalState.selectedAction}
            isOpen={modalState.isActionModalOpen}
            onClose={modalState.closeActionModal}
          />

          <ImageDetailModal
            entry={modalState.selectedImageEntry}
            isOpen={modalState.isImageModalOpen}
            onClose={modalState.closeImageModal}
          />

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

          <AppToasts
            runLastWorkflowError={runLastWorkflowError}
            onDismissRunError={() => setRunLastWorkflowError(null)}
            staleTaskMessage={staleTaskMessage}
            onDismissStaleTask={() => setStaleTaskMessage(null)}
          />

          {/* Canary rollback alerts */}
          {canaryAlerts.map((alert, idx) => (
            <div
              key={alert.id}
              className="fixed p-4 rounded-lg shadow-lg border max-w-md z-toast bg-card border-destructive/50"
              style={{ bottom: `${1 + (idx + 1) * 5}rem`, right: "1rem" }}
            >
              <div className="flex items-start gap-3">
                <div className="flex-1 min-w-0">
                  <h4 className="font-medium text-sm text-destructive">Canary Rollback</h4>
                  <p className="text-sm text-muted-foreground mt-1">{alert.message}</p>
                  <p className="text-xs text-muted-foreground mt-0.5">
                    Canary {alert.canary_id.slice(0, 12)}...
                    {alert.p_value != null && ` | p={${alert.p_value.toFixed(4)}}`}
                  </p>
                </div>
                <button
                  onClick={() => dismissCanaryAlert(alert.id)}
                  className="text-muted-foreground hover:text-foreground shrink-0"
                >
                  &times;
                </button>
              </div>
            </div>
          ))}

          <ToastContainer toasts={toasts} onDismiss={dismissToast} />
          <PerformanceOverlay position="bottom-right" />
          <CommandPalette />
        </div>
      </RenderLogWrapper>
    </ProfilerWrapper>
  );
}

function AppWithTutorials() {
  const { navigate } = useNavigation();

  return (
    <TutorialProvider onNavigate={navigate}>
      <AppContent />
      <ContextualTutorial />
      <DemoVisualOverlay />
    </TutorialProvider>
  );
}

function BundledSpecsLoader() {
  useEffect(() => {
    const store = getGlobalSpecStore();
    const specs = getAllSpecs();
    for (const spec of specs) {
      store.load(spec.specId, spec.config as Parameters<typeof store.load>[1]);
    }
    return () => {
      for (const spec of specs) {
        store.unload(spec.specId);
      }
    };
  }, []);
  return null;
}

function CtrAutoPopulator() {
  useEffect(() => {
    const registry = getGlobalRegistry();
    const ctr = getGlobalCtr();
    const unsubscribe = autoPopulateCtr(registry, ctr);
    return unsubscribe;
  }, []);
  return null;
}

export default function App() {
  return (
    <ApolloProvider client={getGraphQLClient()}>
      <UIBridgeProvider
        features={{ renderLog: true, control: true, debug: true }}
        browserCaptureConfig={{ console: true }}
      >
        <UIBridgeEventHandler />
        <SpecExecutionHandler />
        <BundledSpecsLoader />
        <CtrAutoPopulator />
        <AutoRegisterProvider
          enabled={import.meta.env.DEV}
          idStrategy="prefer-existing"
          debounceMs={100}
          excludeSelectors={["[data-no-register]"]}
          contentDiscovery={{ enabled: true, maxContentElements: 200 }}
        >
          <AuthProvider>
            <NavigationProvider>
              <EventManagerProvider>
                <ExecutionProvider
                  onLog={(_level, _message) => {
                    // Logs are handled by LogManager through event handlers
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
    </ApolloProvider>
  );
}
