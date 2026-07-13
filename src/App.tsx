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
import { TenantProvider } from "./contexts/TenantContext";
import { SessionProvider } from "./contexts/SessionContext";
import { ContextualTutorial } from "./components/tutorial";
import { DemoVisualOverlay } from "./components/demo-video/DemoVisualOverlay";
import { getGraphQLClient } from "./lib/graphql-client";

import {
  UIBridgeProvider,
  AutoRegisterProvider,
  UIBridgeWindowProvider,
} from "@qontinui/ui-bridge";
import { getCurrentWindow } from "@tauri-apps/api/window";

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
import { useAccountMigrationNotifications } from "./hooks/useAccountMigrationNotifications";
import { useStateMachineRegistration } from "./hooks/useStateMachineRegistration";

import { ToastContainer } from "./components/ToastContainer";
import { BuildRefreshBanner } from "./components/BuildRefreshBanner";
import { AutoUpdateChecker } from "./components/AutoUpdateChecker";
import { ConflictModal } from "./components/ConflictModal";
import { StolenBanner } from "./components/StolenBanner";
import { MemoryFederationBanner } from "./components/MemoryFederationBanner";
import { IncomingHandoffToastBridge } from "./components/session/IncomingHandoffToastBridge";
import { WebIntegrationAuthBanner } from "./components/WebIntegrationAuthBanner";
import { ApprovalDialog } from "./components/dag-workflow-editor";
import StatusIndicator from "./components/StatusIndicator";
import ActionDetailModal from "./components/ActionDetailModal";
import ImageDetailModal from "./components/ImageDetailModal";
import { LoginScreen } from "./components/LoginScreen";
import { SetupWizard } from "./components/setup-wizard";
import { registerOpenableView } from "./lib/openable-views";
import { LogSourcePicker } from "./components/LogSourcePicker";
import { Sidebar } from "./components/navigation";
import { TerminalPage } from "./components/terminal";
import { TerminalPageTabBar } from "./components/terminal/TerminalPageTabBar";
import { SessionRecoveryBanner } from "./components/terminal/SessionRecoveryBanner";
import { useTerminalPages } from "./components/terminal/useTerminalPages";
import { useTerminalWindowActions } from "./components/terminal/useTerminalWindowActions";
import { TerminalPageProvider } from "./components/terminal/TerminalPageContext";
import { TerminalSessionProvider } from "./components/terminal/contexts";
import { WindowAssignmentsProvider } from "./components/terminal/contexts/WindowAssignmentsContext";
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
import { acquireSingletonListener } from "@/hooks/ui-bridge-events/singleton-listener";

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

  // Openable-view registry (plan 2026-07-08-ui-bridge-reach-and-verify-gated-flows,
  // P1). The wizard mounts only when `setupCompleted === false`, and the runner has
  // no router, so the UI Bridge cannot otherwise reach it.
  //
  // NON-DESTRUCTIVE: this flips the in-memory flag only. It deliberately does NOT
  // clear the persisted `setup_completed` setting — opening the wizard to look at
  // it must never reset the operator's install.
  useEffect(
    () =>
      registerOpenableView({
        name: "setup-wizard",
        description:
          "First-launch setup wizard (mounts only while setup is incomplete). Contains the GitHub clone picker.",
        preconditions: "none — opening flips in-memory view state, persistence is untouched",
        open: () => setSetupCompleted(false),
      }),
    [],
  );

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
  const { popOutPage } = useTerminalWindowActions();
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
  useAccountMigrationNotifications(showToast);

  // Legacy dev email/password auto-login (and its `test-auto-login-failed`
  // toast) was removed in the Cognito-legacy-auth teardown — sign-in is now
  // Cognito-only (`LoginScreen` → `cognito_sign_in`), so there is no
  // credential-bearing auto-login that can fail this way.

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
    if (auth.authStatus?.authenticated && !auth.loading && !auth.devAutoLoginPending) {
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

  // Auto-surface gate-continuation terminal sessions. The Rust side emits
  // `terminal-focus-request { terminal_id }` (scoped to the MAIN window via
  // `emit_to`) right after creating a docked continuation (or when a duplicate-
  // anchor dispatch is folded onto a live tab). This App-level listener owns
  // the MAIN-VIEW switch (`setActiveTab("terminal")`) — the only setter that
  // brings the Terminal panel on-screen, and reachable only here. The
  // complementary TAB SELECTION (`setActiveId(terminal_id)`) is driven inside
  // the per-page `useTerminalManager` (it owns `activeId`) on the
  // `terminal-created` event it already receives — so the two setters each stay
  // at the level that owns them, no cross-level prop threading.
  //
  // Wired via the #482 `acquireSingletonListener` primitive (ref-counted,
  // StrictMode-safe, race-safe teardown) — NOT a hand-rolled `useEffect` +
  // `listen()`, which leaks listeners on StrictMode double-mount.
  useEffect(() => {
    const release = acquireSingletonListener<{ terminal_id: string }>(
      "terminal-focus-request",
      () => {
        setActiveTab("terminal");
      },
    );
    return release;
  }, [setActiveTab]);

  // Stable nav handlers — passed into TerminalSessionProvider/PageSessionScope,
  // which is React.memo'd. Inline arrows here would change identity every
  // AppContent render, defeating the memo and (via the scope's register →
  // setValues → provider re-render feedback) driving an infinite render loop.
  const navigateToBuilder = useCallback(
    () => setActiveTab("unified-workflow-builder"),
    [setActiveTab],
  );
  const navigateToActive = useCallback(() => setActiveTab("active"), [setActiveTab]);

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
    return <LoginScreen />;
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
                  isPinned={terminalPages.isPinned}
                  onPopOut={() => {
                    // Open a new pop-out OS window (same process) that hosts its
                    // own terminal tabs — the visible counterpart to the
                    // `open-terminal-window` UI Bridge action. New terminals
                    // created in that window belong to it (window_assignments).
                    void invoke("open_terminal_window", { placement: null }).catch((err) =>
                      console.error("Failed to open pop-out window:", err),
                    );
                  }}
                  onPopOutPage={(pageId) => {
                    // Detach the whole page (all its terminals + zone layout)
                    // into its own bound pop-out window.
                    void popOutPage(pageId).catch((err) =>
                      console.error("Failed to pop out page:", err),
                    );
                  }}
                />
                {/*
                  Phase 4 — startup session-recovery banner. Subscribes to the
                  one-shot `session-recovery-summary` event emitted after
                  auto-reattach; renders a prominent (crash) or quiet (planned)
                  dismissible advisory in the top-right column, stacking with
                  CoordWarningBanner. Renders nothing when there's nothing to
                  report. Composes with the session-visibility surfaces rather
                  than owning any session state.
                */}
                <SessionRecoveryBanner />
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
                  {/*
                    Phase 3 (mount-hydration lift): the terminal session state
                    provider is lifted ABOVE TerminalPage and made page-scoped
                    INSIDE the provider (one always-mounted PageSessionScope per
                    page) rather than remounted per page via `key`. Switching
                    terminal pages no longer destroys any page's tab state, and
                    every page's `terminal-created` listener stays live so an
                    externally-created terminal (e.g. a docked gate
                    continuation) is never dropped while the operator is on
                    another page. WindowAssignmentsProvider sits above so
                    tab-ownership filtering can read "which window owns which
                    session" (a no-op in the single-window case).

                    `TerminalPageProvider` still carries the ACTIVE page id to
                    TerminalPage (== session.pageId) so the page's render logic
                    is untouched; the `key={activePageId}` remount is gone.
                  */}
                  <WindowAssignmentsProvider>
                    <TerminalSessionProvider
                      pages={terminalPages.pages}
                      activePageId={terminalPages.activePageId}
                      onNavigateToBuilder={navigateToBuilder}
                      onNavigateToActive={navigateToActive}
                    >
                      <TerminalPageProvider value={terminalPages.activePageId}>
                        <TerminalPage
                          onNavigateToBuilder={navigateToBuilder}
                          onNavigateToActive={navigateToActive}
                          onSessionCountChange={setTerminalSessionCount}
                        />
                      </TerminalPageProvider>
                    </TerminalSessionProvider>
                  </WindowAssignmentsProvider>
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
          {/* Surfaces incoming cross-machine session handoffs as toasts.
              Listens for `session-event` with kind=handoff_request — PR #258. */}
          <IncomingHandoffToastBridge />
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
      {/* Launch-time auto-update check: prompts + installs a newer signed
          release on startup. Renders nothing; fail-open. */}
      <AutoUpdateChecker />
      <UIBridgeProvider
        features={{ renderLog: true, control: true, debug: true }}
        browserCaptureConfig={{
          // Capture errors/warnings only. The runner's error-monitor consumes
          // console errors, resource errors, WS disconnects, and React error
          // overlays — all event-driven and rare.
          //
          // CRITICAL: the continuous performance/telemetry capture modules
          // (`network`, `navigation`, `longTasks`, `longAnimationFrames`,
          // `webVitals`, `memory`, `domMetrics`) default to ON in the SDK's
          // DEFAULT_CAPTURE_CONFIG, so this object — which previously only set
          // `console` — silently enabled all of them. `network` monkey-patches
          // `fetch`/XHR and tracks every request; on the runner that includes
          // the long-lived SSE stream and the 3s/10s background polls, whose
          // response data accumulates unboundedly → the WebView2 renderer grew
          // ~84MB/min while idle and crashed with "Out of memory" in under an
          // hour (verified via live A/B: capture ON → +84MB/min, OFF → flat).
          // Disable every continuous module explicitly; keep only the rare,
          // event-driven error captures the error-monitor actually needs.
          console: true,
          consoleLevels: ["error", "warn"],
          network: false,
          navigation: false,
          longTasks: false,
          longAnimationFrames: false,
          webVitals: false,
          memory: false,
          domMetrics: false,
          freezeDetector: false,
          hmr: false,
          resourceErrors: true,
          wsDisconnections: true,
          frameworkOverlays: true,
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
        {/*
          Phase 0 of plans/2026-06-03-runner-popout-terminal-windows.md.
          Declare this window's label once at the bridge root so every
          `useUIElement` below registers under it. For the main window this
          is "main" — the registry's default — so registration is byte-
          identical to before; pop-out windows (Phase 1) supply "term-N".
        */}
        <UIBridgeWindowProvider windowLabel={getCurrentWindow().label}>
          <AutoRegisterProvider
            enabled
            idStrategy="prefer-existing"
            debounceMs={100}
            excludeSelectors={["[data-no-register]"]}
            contentDiscovery={{ enabled: true, maxContentElements: 200 }}
          >
            <AuthProvider>
              {/*
              Plan 2026-05-22-coord-native-session-coordination §D12 + §Phase 4.
              TenantProvider wraps SessionProvider per plan: the active tenant
              is the default stamp for new sessions started via SessionContext.
              Sits inside AuthProvider so the tenant resolver can react to
              future auth state (paired_user.json reads happen at the Rust
              layer; placement here is for symmetry with NavigationProvider).
            */}
              <TenantProvider>
                <SessionProvider>
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
                </SessionProvider>
              </TenantProvider>
            </AuthProvider>
          </AutoRegisterProvider>
        </UIBridgeWindowProvider>
      </UIBridgeProvider>
    </ApolloProvider>
  );
}
