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

import { UIBridgeProvider, AutoRegisterProvider } from "@qontinui/ui-bridge";

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
  useBackgroundActivities,
  UIBridgeEventHandler,
  UIBridgeInvokeHandler,
  UIBridgeEvaluateHandler,
  ScenarioProjectionHandler,
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
import { BuildRefreshBanner } from "./components/BuildRefreshBanner";
import { ConflictModal } from "./components/ConflictModal";
import { StolenBanner } from "./components/StolenBanner";
import { MemoryFederationBanner } from "./components/MemoryFederationBanner";
import { WebIntegrationAuthBanner } from "./components/WebIntegrationAuthBanner";
import { ApprovalDialog } from "./components/dag-workflow-editor";
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
import { Now1HzProvider } from "./components/terminal/useNow1Hz";
import { ReorganizeDialog, type ReorganizePlan } from "./components/terminal/ReorganizeDialog";
import { PerformanceOverlay, GiantSCCFixture } from "./components/dev";
import { CommandPalette } from "./components/unified-search/CommandPalette";
import {
  KnowledgeBrowser,
  useKnowledgeBrowserHotkey,
} from "./components/productivity/KnowledgeBrowser";
import { PromptExecutionProvider } from "./components/prompt-home/PromptExecutionContext";
import { PromptAutomationOverlay } from "./components/prompt-home/PromptAutomationOverlay";
import { BackgroundTaskPill } from "./components/prompt-home/BackgroundTaskPill";
import { useTaskRuns } from "./hooks/useAiData";
import { loadDiscoveredSpecs } from "./lib/ui-bridge/use-discovered-specs";
import { getGlobalSpecStore } from "@qontinui/ui-bridge/specs";
import { autoPopulateCtr, getGlobalCtr } from "@qontinui/ui-bridge/ctr";
import { getGlobalRegistry } from "@qontinui/ui-bridge";

import { instanceStorage } from "@/lib/instance-storage";
import { ACTIVE_TAB_STORAGE_KEY, TAB_LIST } from "@/components/app/tab-types";
import { toTabCanonical } from "@/hooks/ui-bridge-events/utils";

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
    // Phase 8 (manual-test remediation) — AuthProvider dispatches this when
    // a credential-bearing auto-login attempt fails non-recoverably so an
    // App-level listener can toast the operator. `cause` is one of
    // "network" (transient retries exhausted) or "credentials" (auth
    // error — bad password, account locked, etc.); `error` is the raw
    // error string for log correlation.
    "test-auto-login-failed": CustomEvent<{ cause: "network" | "credentials"; error: string }>;
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
      // Build a map of session ID → current page_id from terminal_list
      const listResult = await invoke<{
        success: boolean;
        data?: { terminals: Array<{ id: string; page_id: string; title: string }> };
      }>("terminal_list");
      const terminals = listResult?.data?.terminals ?? [];
      const terminalPageMap = new Map(terminals.map((t) => [t.id, t.page_id]));

      // Ensure all target pages exist
      const pageNameToId = new Map(terminalPages.pages.map((p) => [p.name, p.id]));
      for (const group of plan.pages) {
        if (!pageNameToId.has(group.name)) {
          const newId = terminalPages.addPage(group.name);
          pageNameToId.set(group.name, newId);
        }
      }

      // For each group, move terminals that aren't already on the target page.
      // Moving a terminal: save scrollback → close → create on target page → restore scrollback
      for (const group of plan.pages) {
        const targetPageId = pageNameToId.get(group.name);
        if (!targetPageId) continue;

        for (const sessionId of group.sessionIds) {
          const currentPage = terminalPageMap.get(sessionId);
          if (currentPage === targetPageId) continue; // Already on the right page

          // Save scrollback before closing
          try {
            await invoke("terminal_save_scrollback", { terminalId: sessionId });
          } catch {
            // Non-fatal — terminal may not have scrollback
          }

          // Close on old page
          try {
            await invoke("terminal_close", { terminalId: sessionId });
          } catch {
            console.warn(`Failed to close terminal ${sessionId} during reorganization`);
            continue;
          }

          // Create on target page (new terminal — scrollback not restored for simplicity)
          try {
            const terminal = terminals.find((t) => t.id === sessionId);
            await invoke("terminal_create", {
              title: terminal?.title ?? "Terminal",
              workingDir: null,
              pageId: targetPageId,
            });
          } catch {
            console.warn(`Failed to create terminal on page ${group.name}`);
          }
        }
      }

      // Rename existing pages to match the proposal
      for (const group of plan.pages) {
        const pageId = pageNameToId.get(group.name);
        if (pageId) {
          terminalPages.renamePage(pageId, group.name);
        }
      }

      // Remove pages that are now empty (no sessions assigned to them)
      const assignedPageIds = new Set(
        plan.pages.map((g) => pageNameToId.get(g.name)).filter(Boolean),
      );
      for (const page of terminalPages.pages) {
        if (!assignedPageIds.has(page.id) && page.id !== "default") {
          try {
            await terminalPages.removePage(page.id);
          } catch {
            // Non-fatal
          }
        }
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

  const [rawEditWorkflowId, setEditWorkflowId] = useState<string | null>(null);
  const [activeLogSubTab, setActiveLogSubTab] = useState<LogSubTab>("general");
  const [showLogSourcePicker, setShowLogSourcePicker] = useState(false);

  // Derive: only surface editWorkflowId when on the builder tab. Avoids a
  // sync setState in useEffect to clear on tab-switch (set-state-in-effect).
  const editWorkflowId = activeTab === "unified-workflow-builder" ? rawEditWorkflowId : null;

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

  // Phase 8 (manual-test remediation) — surface failed auto-login attempts.
  // AuthProvider dispatches `test-auto-login-failed` only when a credential-
  // bearing auto-login attempt produced a non-retryable error (or exhausted
  // retries). The "no credentials configured" case is silent — that's the
  // standard primary-runner path and would be noisy for normal users. The
  // toast is intentionally thin; the rich diagnostic ("reason=<...>") is in
  // runner-tauri.log via `tracing::info!(target: "test_auto_login_skipped")`.
  useEffect(() => {
    const onAutoLoginFailed = (e: Event) => {
      const detail = (e as CustomEvent<{ cause?: string }>).detail;
      const cause = detail?.cause === "network" ? "network" : "credentials";
      showToast(
        `Auto-login failed (${cause}) — see runner-tauri.log`,
        "error",
      );
    };
    window.addEventListener("test-auto-login-failed", onAutoLoginFailed);
    return () => window.removeEventListener("test-auto-login-failed", onAutoLoginFailed);
  }, [showToast]);

  const { activities: backgroundActivities } = useBackgroundActivities({
    isExtracting: false,
    extractionUrl: undefined,
    extractionProgress: undefined,
  });

  // Phase 3 — the runner ↔ qontinui-web channel is now a single outbound
  // WebSocket driven from the Rust side (`mcp::backend_relay`). The legacy
  // frontend-driven `useWebSocketAutoConnect` (user-JWT WebSocket via
  // Python bridge) and `useCloudRelayAutoConnect` (separate cloud-relay
  // toggle) are gone. The relay starts automatically at runner boot
  // whenever `WebIntegrationSettings.enabled && runner_token` are set.

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
        // eslint-disable-next-line no-console
        console.debug("[App] BackgroundObserver start deferred:", e);
      }
    }, 5000);
    return () => {
      clearTimeout(timer);
      stopBackgroundObserver();
    };
  }, []);

  useEffect(() => {
    // Gate the project fetch on the *settled* auth state. Without the
    // `devAutoLoginPending` check, a temp test runner that boots with
    // `QONTINUI_TEST_AUTO_LOGIN_*` env vars races: the initial
    // `check_auth_status` resolves first with `authenticated: false`,
    // `loading` flips false, and although this effect won't fire its
    // happy-path branch, a stale-keychain read from a sibling reload
    // path can still produce a "Not authenticated" surface before the
    // auto-login flow swaps the token. Waiting for the auto-login retry
    // chain to settle (devAutoLoginPending → false) ensures the
    // `get_user_projects` invoke runs only after the access token is in
    // place. The retry-on-Not-authenticated inside `loadProjects` still
    // covers the rare keychain-write/read ordering blip.
    if (
      auth.authStatus?.authenticated &&
      !auth.loading &&
      !auth.devAutoLoginPending
    ) {
      projectSelection.loadProjects();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- loadProjects changes with selectedProjectId; including it would reload projects on every selection change
  }, [auth.authStatus?.authenticated, auth.loading, auth.devAutoLoginPending]);

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

  // Runner-tier-decoupling Phase 1 — wizard runs FIRST so Tier 0/1 setup
  // can happen without ever hitting the login screen. The SetupWizard
  // dispatches `runner-tier-changed` after writing the tier; the
  // useRunnerTier hook in AuthProvider re-reads, and the appropriate
  // gate fires below.
  if (setupCompleted === false) {
    return <SetupWizard onComplete={() => setSetupCompleted(true)} />;
  }

  // LoginScreen is Tier 2 only. Tier 0/1 get a synthesized local-guest
  // auth from AuthProvider, so `auth.authStatus?.authenticated` is true
  // and we fall through to the main app.
  const isTier2 = auth.tier === "qontinui_account";
  if (isTier2 && !auth.authStatus?.authenticated) {
    return <LoginScreen onLogin={auth.login} />;
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
        {/*
          Top-of-app banner that surfaces "this runner needs to be authorized
          with qontinui-web" for fresh installs. Mounted inside UIBridgeProvider
          (via App > UIBridgeProvider > AppContent) so its useUIElement
          registrations land on the live registry. Mounted after the
          auth/setup gates above so we don't show it on the login or
          first-run wizard screens.
        */}
        <WebIntegrationAuthBanner />
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

          <ApprovalDialog />
          <ToastContainer toasts={toasts} onDismiss={dismissToast} />
          <PerformanceOverlay position="bottom-right" />
          <GiantSCCFixture />
          <CommandPalette />
          <GlobalKnowledgeBrowser />
          {/*
            Plan 2026-05-18-agent-spawn-coordination Phase 3 — spawn-time
            claim-conflict modal + post-acquire stolen-claim banner.
            Both listen for runner-side Tauri events; they render nothing
            until those events fire.
          */}
          <ConflictModal />
          <StolenBanner />
          <MemoryFederationBanner />
        </div>
      </RenderLogWrapper>
    </ProfilerWrapper>
  );
}

/**
 * Global Ctrl+Shift+K trigger for the knowledge browser modal. Mounted
 * at the AppContent root so the hotkey works from any tab.
 */
function GlobalKnowledgeBrowser() {
  const [open, setOpen] = useKnowledgeBrowserHotkey();
  return <KnowledgeBrowser mode="modal" open={open} onClose={() => setOpen(false)} />;
}

function AppWithTutorials() {
  const { navigate } = useNavigation();

  return (
    <TutorialProvider onNavigate={navigate}>
      <PromptExecutionProvider>
        {/*
          Now1HzProvider — Phase 2 of the stuck-session heartbeat plan.
          Runs ONE 1000ms interval and broadcasts Date.now() to every
          consumer (HoldingLockBanner, WaitingLockBanner,
          FileActivityPanel, TerminalTabBar tooltip ticks) so we don't
          burn three+ private intervals on the same cadence. Mounted
          here because every consumer of useNow1Hz lives under
          AppContent — TerminalPage, CoordinatorDashboard, etc.
        */}
        <Now1HzProvider>
          <AppContent />
          <ContextualTutorial />
          <DemoVisualOverlay />
          <PromptAutomationOverlay />
          {/*
            Persistent global pill that surfaces in-progress background tasks
            (currently the long-running UI Bridge integration generation
            triggered from the home prompt). Mounted inside
            PromptExecutionProvider but outside <AppContent>'s tab content so
            it stays visible across tab switches. See BackgroundTaskPill.tsx
            for the detection rule and dismiss flow.
          */}
          <BackgroundTaskPill />
        </Now1HzProvider>
      </PromptExecutionProvider>
    </TutorialProvider>
  );
}

function BundledSpecsLoader() {
  useEffect(() => {
    const store = getGlobalSpecStore();
    let cancelled = false;
    const loadedIds: string[] = [];
    void loadDiscoveredSpecs()
      .then((specs) => {
        if (cancelled) return;
        for (const spec of specs) {
          store.load(spec.specId, spec.config as Parameters<typeof store.load>[1]);
          loadedIds.push(spec.specId);
        }
      })
      .catch(() => {
        // Errors are surfaced by the loader; nothing to do here.
      });
    return () => {
      cancelled = true;
      for (const id of loadedIds) {
        store.unload(id);
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

/**
 * Registers a pluggable snapshot enricher that contributes the runner's
 * tab metadata (`availableTabs`, `tabActivation`) to every UI Bridge
 * snapshot. These fields describe a runner-shell feature and are not
 * part of the SDK's canonical snapshot schema, so they live here rather
 * than in the registry's built-in tracker enrichers (which Phase 1 of
 * the SDK Tracker Reshape moved into the SDK).
 *
 * Reads the active tab from `instanceStorage` fresh on each snapshot —
 * do NOT capture `activeTabId` outside the closure. Fallback matches
 * the `tabs_list` IPC handler ("prompt-home" — the first tab the runner
 * shell selects on launch).
 */
function RunnerTabsEnricher() {
  useEffect(() => {
    const registry = getGlobalRegistry();
    if (!registry) return;
    const dispose = registry.registerSnapshotEnricher("runner-tabs", () => {
      const activeTabId = instanceStorage.getItem(ACTIVE_TAB_STORAGE_KEY) ?? "prompt-home";
      const availableTabs = TAB_LIST.map((entry) => ({
        id: entry.id,
        label: entry.label,
        canonical: toTabCanonical(entry.id),
        active: entry.id === activeTabId,
      }));
      const tabActivation = {
        description: "Switch tabs without a UI click",
        method: "POST",
        path: "/ui-bridge/control/tab/activate",
        bodyExample: { tabId: "<one of availableTabs[].id>" },
      };
      return { availableTabs, tabActivation };
    });
    return dispose;
  }, []);
  return null;
}

/**
 * Fetches the list of Tauri event channel names emitted by the runner
 * backend (from GET /ui-bridge/sdk/tauri-event-names) and registers them
 * with the change-tracking subsystem so the change buffer subscribes to
 * backend events when enabled. Safe: logs and no-ops on fetch failure.
 */
function TauriEventNamesLoader() {
  useEffect(() => {
    let cancelled = false;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    // Bounded retry: cap attempts so we never spin forever when the backend
    // is permanently unreachable (e.g., a non-runner host consuming this
    // build). With initial 500ms and clamped exponential backoff to 4000ms,
    // 8 attempts span roughly 500 + 1000 + 2000 + 4000*5 = 23.5s — plenty
    // to cover a temp-runner warm-up without leaking a perpetual timer.
    const MAX_ATTEMPTS = 8;

    const attempt = async (delayMs: number, attemptNum: number): Promise<void> => {
      if (cancelled) return;
      try {
        const [{ getApiBase }, { setPendingTauriEventNames }] = await Promise.all([
          import("@/lib/runner-api"),
          import("./hooks/ui-bridge-events/useChangeTrackingEvents"),
        ]);
        // The API base URL is set asynchronously after the Tauri backend
        // publishes its bound port (see useApiReady). Until that resolves,
        // getApiBase() still returns the default primary-runner port
        // (http://localhost:9876), which doesn't serve this route for
        // a temp runner and returns 404. Retry until we hit an actual
        // success or give up after MAX_ATTEMPTS.
        const resp = await fetch(`${getApiBase()}/ui-bridge/sdk/tauri-event-names`);
        if (!resp.ok) {
          if (!cancelled) {
            if (attemptNum + 1 < MAX_ATTEMPTS) {
              const nextDelay = Math.min(delayMs * 2, 4000);
              retryTimer = setTimeout(() => void attempt(nextDelay, attemptNum + 1), nextDelay);
            } else {
              console.warn(
                `[TauriEventNamesLoader] fetch failed after ${MAX_ATTEMPTS} attempts: ${resp.status}`,
              );
            }
          }
          return;
        }
        const body = (await resp.json()) as { event_names?: unknown };
        const names = Array.isArray(body.event_names)
          ? body.event_names.filter((n): n is string => typeof n === "string")
          : [];
        if (!cancelled && names.length > 0) {
          setPendingTauriEventNames(names);
        }
      } catch (err) {
        if (!cancelled) {
          if (attemptNum + 1 < MAX_ATTEMPTS) {
            const nextDelay = Math.min(delayMs * 2, 4000);
            retryTimer = setTimeout(() => void attempt(nextDelay, attemptNum + 1), nextDelay);
          } else {
            console.warn(
              `[TauriEventNamesLoader] failed to load tauri event names after ${MAX_ATTEMPTS} attempts:`,
              err,
            );
          }
        }
      }
    };

    void attempt(500, 0);
    return () => {
      cancelled = true;
      if (retryTimer) clearTimeout(retryTimer);
    };
  }, []);
  return null;
}

export default function App() {
  return (
    <ApolloProvider client={getGraphQLClient()}>
      {/*
        BuildRefreshBanner sits outside UIBridgeProvider so it keeps watching
        even if the bridge tears down (e.g. during navigation/error states).
        Banner is hidden by default and only renders the toast when
        `invoke('get_build_id')` reports a value different from the
        `<meta name="build-id">` baked into the embedded index.html — i.e.
        the runner exe was swapped while this webview stayed open.
      */}
      <BuildRefreshBanner />
      <UIBridgeProvider
        features={{ renderLog: true, control: true, debug: true }}
        browserCaptureConfig={{
          console: true,
          consoleLevels: ['error', 'warn', 'debug', 'info', 'log'],
        }}
      >
        <UIBridgeEventHandler />
        <UIBridgeInvokeHandler />
        <UIBridgeEvaluateHandler />
        <ScenarioProjectionHandler />
        <SpecExecutionHandler />
        <BundledSpecsLoader />
        <CtrAutoPopulator />
        <RunnerTabsEnricher />
        <TauriEventNamesLoader />
        {/*
          Auto-register every interactive element in the runner app so that
          GET /ui-bridge/control/elements and snapshot reflect the live DOM,
          not just elements wired up through explicit useUIElement hooks.
          This was previously gated on `import.meta.env.DEV`, which silently
          disabled the registry in production builds (the temp-runner build
          and the embedded primary-runner build) — making the entire
          UI Bridge element-discovery API return near-empty results.

          Cost: a debounced MutationObserver scan (100ms debounce). The
          runner is a power-user tool with a tight UI Bridge automation
          loop, so the registry has to work in every shipped build, not
          just dev. Use [data-no-register] on any element you want to opt
          out individually.
        */}
        <AutoRegisterProvider
          enabled
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
