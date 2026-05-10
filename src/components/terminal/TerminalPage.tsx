import { useEffect, useCallback } from "react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { TerminalTabBar } from "./TerminalTabBar";
import { TerminalNotification } from "./TerminalNotification";
import { FileConflictBanner } from "./FileConflictBanner";
import { SessionManagerPanel } from "./SessionManagerPanel";
import { ZoneGrid } from "./ZoneGrid";
import { ZoneLayoutPicker } from "./ZoneLayoutPicker";
import { ZoneStatusBar } from "./ZoneStatusBar";
import { BatchOperationsBar } from "./BatchOperationsBar";
import { ZoneMinimap } from "./ZoneMinimap";
import { ZoneProfilePicker } from "./ZoneProfilePicker";
import { useTerminalPageId } from "./TerminalPageContext";
import {
  TerminalCoreProvider,
  useTerminalCore,
  SessionStateProvider,
  useSessionState,
  ZoneMetadataProvider,
  useZoneMetadata,
  TransitionEffectsProvider,
  useTransitionEffects,
  AiFeaturesProvider,
  useAiFeatures,
  ShellInfraProvider,
  useShellInfra,
  UIStateProvider,
  useUIStateCx,
} from "./contexts";
import { ZoneTimeline } from "./ZoneTimeline";
import { ZoneControlPanel } from "./ZoneControlPanel";
import { OutputSearchBar } from "./OutputSearchBar";
import { TerminalRightPanel } from "./TerminalRightPanel";
import { TerminalOverlays } from "./TerminalOverlays";

import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useTerminalInitialization } from "./useTerminalInitialization";
import { useZoneActions } from "./useZoneActions";
import { writeWhenReady as writeWhenReadyHelper } from "./writeWhenReady";
import { UIBridgeComponentScope } from "@qontinui/ui-bridge";
import { useCommitState } from "./useCommitState";
import { useTabSessionIdCapture } from "./useTabSessionIdCapture";

interface TerminalPageProps {
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  onSessionCountChange?: (count: number) => void;
}

export function TerminalPage(props: TerminalPageProps) {
  const pageId = useTerminalPageId();
  return (
    <TerminalCoreProvider pageId={pageId}>
      <SessionStateProvider>
        <ZoneMetadataProvider>
          <TransitionEffectsProvider>
            <UIStateProvider>
              <ShellInfraProvider>
                <AiFeaturesProvider
                  onNavigateToBuilder={props.onNavigateToBuilder}
                  onNavigateToActive={props.onNavigateToActive}
                >
                  <TerminalPageInner {...props} />
                </AiFeaturesProvider>
              </ShellInfraProvider>
            </UIStateProvider>
          </TransitionEffectsProvider>
        </ZoneMetadataProvider>
      </SessionStateProvider>
    </TerminalCoreProvider>
  );
}

function TerminalPageInner({
  onNavigateToBuilder: _onNavigateToBuilder,
  onNavigateToActive: _onNavigateToActive,
  onSessionCountChange,
}: TerminalPageProps) {
  const {
    tabs,
    activeId,
    setActiveId,
    initialized,
    setInitialized,
    createTerminal,
    closeTerminal,
    renameTab,
    updateTab,
    reconnectToExistingSessions,
    createPlanTab,
    pageId,
    zoneLayout,
    terminalRefs,
    pendingProfileSessionsRef,
  } = useTerminalCore();

  // Register page-level UI Bridge actions so AI agents can discover
  // and invoke terminal operations without knowing element IDs.
  useUIComponent({
    id: "terminal-page",
    name: "Terminal Page",
    description:
      "Multi-terminal workspace with zone-based layout. Agents can create terminals via HTTP POST /terminals with initialCommand.",
    actions: [
      {
        id: "create-terminal",
        label: "Create Terminal",
        description: "Spawn a new PTY-backed terminal tab and assign it to the next free zone.",
        handler: async () => {
          await createTerminal();
        },
      },
      {
        id: "list-terminals",
        label: "List Terminals",
        description: "Return [{id, title, isAlive}] for every currently mounted terminal tab.",
        // Coerce isAlive to a strict boolean so the field is always present in
        // the response payload — `undefined` values would otherwise be dropped
        // by JSON.stringify on the IPC boundary, leaving callers without the
        // PTY-liveness signal the cheatsheet promises.
        handler: () =>
          tabs.map((t) => ({
            id: t.id,
            title: t.title,
            isAlive: Boolean(t.isAlive),
          })),
      },
    ],
  });

  useEffect(() => {
    onSessionCountChange?.(tabs.length);
  }, [tabs.length, onSessionCountChange]);

  const { fileConflicts, fileLockStates, sessionPersistence } = useShellInfra();

  // Per-tab commit-readiness state (Plan §3). Sibling to fileLockStates;
  // both are passed through to TerminalTabBar for rendering.
  const commitStates = useCommitState(tabs);
  // Post-spawn polling hook that captures Claude CLI's session id from
  // the on-disk transcript and updates `tab.claudeSessionId` so the
  // commit traffic light has something to key on. Plan §1.5
  // "Frontend session-id capture".
  const { startCapture: startSessionIdCapture } = useTabSessionIdCapture({ updateTab, tabs });
  const {
    labelsAndTags,
    eventHistory,
    addHistoryEvent,
    metrics: _metrics,
    incrementMetric,
    focusHistory,
  } = useZoneMetadata();

  const stateTracking = useSessionState();

  const transitionEffects = useTransitionEffects();
  const { handleRestartInZone } = transitionEffects;

  const { workflowGen, sessionManager } = useAiFeatures();

  const {
    state: uiState,
    dispatch,
    toggleFocusMode: _toggleFocusMode,
    toggleAutoLayout,
    cycleViewMode,
  } = useUIStateCx();

  useTerminalInitialization({
    tabs,
    terminalRefs,
    reconnectToExistingSessions,
    createTerminal,
    createPlanTab,
    setInitialized,
    updateTab,
    zoneLayout,
    labelsAndTags,
    sessionPersistence,
    layoutState: {
      layoutId: zoneLayout.layoutId,
      zoneLabels: labelsAndTags.zoneLabels,
      zoneNotes: labelsAndTags.zoneNotes,
      pinnedZones: labelsAndTags.pinnedZones,
      focusedZone: zoneLayout.focusedZone,
    },
  });

  const handleExit = useCallback(
    (terminalId: string, exitCode: number | null) => {
      updateTab(terminalId, { isAlive: false, exitCode });
      stateTracking.handleExit(terminalId, exitCode);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [updateTab, stateTracking.handleExit],
  );

  const {
    handleZoneClick,
    handleZoneDoubleClick,
    handleOpenDocFile,
    createAndAssignTerminal,
    handleSortZones,
    handleExportOutput,
    handleExportZone,
  } = useZoneActions({
    tabs,
    dispatch,
    zoneLayout,
    stateTracking,
    labelsAndTags,
    transitionEffects,
    createTerminal,
    createPlanTab,
    incrementMetric,
    setNotification: workflowGen.setNotification,
  });

  useKeyboardShortcuts({
    activeId,
    tabs,
    dispatch,
    swapSource: uiState.swapSource,
    selectedZones: uiState.selectedZones,
    createAndAssignTerminal,
    closeTerminal,
    setActiveId,
    zoneLayout,
    sessionStates: stateTracking.sessionStates,
    handleRestartInZone,
    labelsAndTags,
    focusHistory,
    transitionEffects,
    incrementMetric,
    addHistoryEvent,
    terminalRefs: terminalRefs.current,
    workflowGen,
    sessionManager: {
      frozenCount: sessionManager.frozenCount,
      needsInputCount: sessionManager.needsInputCount,
      sessions: sessionManager.sessions,
      resumeSession: sessionManager.resumeSession,
    },
  });

  /** Pick the smallest layout preset that fits `totalTabs` tabs. */
  const pickLayout = (totalTabs: number): string => {
    if (totalTabs >= 7) return "full-grid";
    if (totalTabs >= 5) return "six-pack";
    if (totalTabs >= 3) return "quad";
    if (totalTabs >= 2) return "split";
    return "single";
  };

  const writeWhenReady = (tabId: string, text: string, maxWaitMs = 5000) =>
    writeWhenReadyHelper(terminalRefs.current, tabId, text, {
      maxWaitMs,
      onTimeout: (id) => console.warn(`[LaunchAI] terminal ref for ${id} never became ready`),
    });

  if (!initialized) {
    return (
      <div className="h-full flex items-center justify-center bg-[#1a1b26]">
        <div className="flex flex-col items-center gap-3">
          <div className="w-8 h-8 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
          <span className="text-[12px] text-[#565f89]">Loading terminals...</span>
        </div>
      </div>
    );
  }

  return (
    <UIBridgeComponentScope componentId="terminal-page">
      <div className="h-full flex flex-col bg-[#1a1b26]">
        <TerminalTabBar
          tabs={tabs}
          activeId={activeId}
          onSelect={(id) => {
            setActiveId(id);
            const zoneIdx = Object.entries(zoneLayout.assignments).find(
              ([, tabId]) => tabId === id,
            );
            if (zoneIdx) {
              zoneLayout.setFocusedZone(Number(zoneIdx[0]));
            }
          }}
          onClose={closeTerminal}
          onCreate={() => createAndAssignTerminal()}
          onRename={renameTab}
          sessionStates={stateTracking.sessionStates}
          layoutPicker={
            <div className="flex items-center gap-1">
              <ZoneLayoutPicker
                currentLayoutId={zoneLayout.layoutId}
                onSelectLayout={zoneLayout.setLayoutId}
                tabCount={tabs.length}
              />
              {zoneLayout.isMultiZone && (
                <>
                  <button
                    onClick={cycleViewMode}
                    className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors"
                    title={`View mode: ${uiState.viewMode} (Ctrl+Shift+M to cycle)`}
                  >
                    <span className="font-mono uppercase tracking-wider">{uiState.viewMode}</span>
                    <span className="text-[#565f89]/50">{zoneLayout.layout.zones.length}z</span>
                  </button>
                  <button
                    onClick={() => dispatch({ type: "RESET_RATIOS" })}
                    className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors"
                    title="Reset zone sizes to equal"
                  >
                    Reset
                  </button>
                </>
              )}
              <button
                onClick={toggleAutoLayout}
                className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
                  uiState.autoLayout
                    ? "text-[#9ece6a] bg-[#9ece6a]/10"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                }`}
                title={`Auto-layout: ${uiState.autoLayout ? "ON" : "OFF"} — automatically switch layout based on terminal count`}
              >
                Auto
              </button>
              <ZoneProfilePicker
                currentLayoutId={zoneLayout.layoutId}
                zoneLabels={labelsAndTags.zoneLabels}
                zoneNotes={labelsAndTags.zoneNotes}
                pinnedZones={labelsAndTags.pinnedZones}
                autoApprovePatterns={transitionEffects.autoApprovePatterns}
                pageId={pageId}
                zoneAssignments={zoneLayout.assignments}
                tabs={tabs}
                initialized={initialized}
                onLoadProfile={async (profile) => {
                  zoneLayout.setLayoutId(profile.layoutId);
                  labelsAndTags.setZoneLabels(profile.labels);
                  labelsAndTags.setZoneNotes(profile.notes);
                  labelsAndTags.setPinnedZones(new Set(profile.pins));
                  transitionEffects.setAutoApprovePatterns(profile.autoApprovePatterns);
                  // Create terminals for profile sessions that need them.
                  // Stash sessions BEFORE creating terminals so the assignments-watching
                  // effect sees them as soon as the first assignment lands.
                  if (profile.sessions && profile.sessions.length > 0) {
                    pendingProfileSessionsRef.current = profile.sessions;
                    const existingTabCount = tabs.length;
                    const neededCount = profile.sessions.length;
                    const toCreate = Math.max(0, neededCount - existingTabCount);
                    for (let i = 0; i < toCreate; i++) {
                      await createAndAssignTerminal();
                    }
                    // Edge case: if existing tabs already satisfy the count, no new
                    // terminals (and no zone-assignment changes) will be triggered, so
                    // the effect watching zoneLayout.assignments in TerminalCoreContext
                    // would never re-fire to consume pendingProfileSessionsRef. Bind
                    // each profile session's zoneIndex to one of the existing tabs;
                    // assignTabToZone always returns a new assignments object, so the
                    // effect re-runs and processes the pending sessions.
                    if (toCreate === 0) {
                      const assignedTabs = new Set<string>();
                      for (const s of profile.sessions) {
                        const candidate = tabs.find((t) => !assignedTabs.has(t.id));
                        if (!candidate) break;
                        assignedTabs.add(candidate.id);
                        zoneLayout.assignTabToZone(s.zoneIndex, candidate.id);
                      }
                    }
                  }
                }}
              />
            </div>
          }
          assignments={zoneLayout.isMultiZone ? zoneLayout.assignments : undefined}
          activityData={stateTracking.activityData}
          stateDurations={stateTracking.stateDurations}
          lastOutputLines={stateTracking.lastOutputLines}
          unreadTabs={transitionEffects.unseenNeedsInput}
          staleTabs={stateTracking.staleTabs}
          zoneLabels={labelsAndTags.zoneLabels}
          labelColorMap={labelsAndTags.labelColorMap}
          onQuickLaunch={async (count, autoCommand) => {
            const totalTabs = tabs.length + count;
            const layoutId = pickLayout(totalTabs);
            zoneLayout.setLayoutId(layoutId);
            const title = autoCommand ? autoCommand.slice(0, 20) : undefined;
            const createdTabIds: string[] = [];
            for (let i = 0; i < count; i++) {
              const tabId = await createAndAssignTerminal(title);
              if (tabId) createdTabIds.push(tabId);
            }
            if (autoCommand && createdTabIds.length > 0) {
              for (const tabId of createdTabIds) {
                writeWhenReady(tabId, `${autoCommand}\r`);
              }
            }
          }}
          onLaunchAiSession={async (count, configDir, context) => {
            const totalTabs = tabs.length + count;
            const layoutId = pickLayout(totalTabs);
            zoneLayout.setLayoutId(layoutId);

            const isWindows = navigator.platform.startsWith("Win");
            const customCmd = sessionManager.launchCommands?.[configDir];
            const cmd = customCmd
              ? customCmd
              : isWindows
                ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; claude`
                : `CLAUDE_CONFIG_DIR="${configDir}" claude`;
            // Smart tab naming: use custom command or account label
            const dirName = configDir.replace(/\\/g, "/").replace(/\/$/, "").split("/").pop() ?? "";
            const label = customCmd ?? dirName.match(/^\.claude-(.+)$/)?.[1] ?? "claude";
            const createdTabIds: string[] = [];
            for (let i = 0; i < count; i++) {
              const tabId = await createAndAssignTerminal(label);
              if (tabId) createdTabIds.push(tabId);
            }
            if (createdTabIds.length > 0) {
              const spawnAt = Date.now();
              // Type the launch command once the terminal ref is mounted
              for (const tabId of createdTabIds) {
                writeWhenReady(tabId, `${cmd}\r`);
                // Plan §1.5 — start polling transcript_get_latest so
                // tab.claudeSessionId fills in once Claude CLI writes
                // its first JSONL record. Pass `configDir` so the probe
                // scopes to the account this tab launched into; without
                // it, a concurrent session in another account writing
                // to the same project_path can win the freshest-mtime
                // race and the wrong session_id gets bound (P0 silent
                // fail for multi-account users).
                const tab = tabs.find((t) => t.id === tabId);
                startSessionIdCapture(tabId, tab?.workingDir ?? "", spawnAt, configDir);
              }
              // Type the initial instructions after Claude starts
              if (context) {
                const safeContext = context.replace(/\n/g, " ");
                setTimeout(() => {
                  for (const tabId of createdTabIds) {
                    writeWhenReady(tabId, `${safeContext}\r`);
                  }
                }, 8000);
              }
            }
          }}
          onLaunchMultiAiSessions={async (configDirs, context) => {
            const count = configDirs.length;
            const totalTabs = tabs.length + count;
            const layoutId = pickLayout(totalTabs);
            zoneLayout.setLayoutId(layoutId);

            const isWindows = navigator.platform.startsWith("Win");
            const cmds = sessionManager.launchCommands ?? {};
            const createdTabIds: string[] = [];
            const spawnAt = Date.now();
            for (let i = 0; i < count; i++) {
              const customCmd = cmds[configDirs[i]];
              const dirName =
                configDirs[i].replace(/\\/g, "/").replace(/\/$/, "").split("/").pop() ?? "";
              const label = customCmd ?? dirName.match(/^\.claude-(.+)$/)?.[1] ?? "claude";
              const tabId = await createAndAssignTerminal(label);
              if (tabId) {
                createdTabIds.push(tabId);
                const cmd = customCmd
                  ? customCmd
                  : isWindows
                    ? `$env:CLAUDE_CONFIG_DIR="${configDirs[i]}"; claude`
                    : `CLAUDE_CONFIG_DIR="${configDirs[i]}" claude`;
                // Stagger launch commands across accounts
                setTimeout(() => writeWhenReady(tabId, `${cmd}\r`), i * 300);
                // Plan §1.5 — kick off transcript-id polling per tab.
                // Pass each tab's own `configDirs[i]` so the probe scopes
                // to the account this tab is in; otherwise a faster-
                // writing concurrent account would shadow the right
                // session_id and silently break the commit traffic light.
                const tab = tabs.find((t) => t.id === tabId);
                startSessionIdCapture(tabId, tab?.workingDir ?? "", spawnAt, configDirs[i]);
              }
            }
            // Type initial instructions after Claude starts (stagger per session)
            if (context && createdTabIds.length > 0) {
              const safeContext = context.replace(/\n/g, " ");
              for (let j = 0; j < createdTabIds.length; j++) {
                const tabId = createdTabIds[j];
                setTimeout(() => writeWhenReady(tabId, `${safeContext}\r`), j * 300 + 8000);
              }
            }
          }}
          accountUsage={sessionManager.accountUsage}
          launchCommands={sessionManager.launchCommands}
          fileLocks={sessionManager.fileLocks}
          fileLockStates={fileLockStates}
          commitStates={commitStates}
          terminalRefs={terminalRefs.current}
        />
        <ZoneStatusBar
          onExport={handleExportOutput}
          onSortZones={handleSortZones}
          onOpenDocFile={handleOpenDocFile}
        />
        <TerminalNotification
          message={workflowGen.notification?.message ?? null}
          type={workflowGen.notification?.type ?? "success"}
          onDismiss={() => workflowGen.setNotification(null)}
        />
        <FileConflictBanner
          conflicts={fileConflicts.conflicts}
          recentAlert={fileConflicts.recentAlert}
          onDismissAlert={fileConflicts.dismissAlert}
        />

        {uiState.showTimeline && zoneLayout.isMultiZone && (
          <ZoneTimeline
            tabs={tabs}
            assignments={zoneLayout.assignments}
            sessionStates={stateTracking.sessionStates}
            eventHistory={eventHistory}
            onClose={() => dispatch({ type: "SET_SHOW_TIMELINE", payload: false })}
          />
        )}

        {uiState.showOutputSearch && (
          <OutputSearchBar
            outputSearch={uiState.outputSearch}
            onSearchChange={(v) => dispatch({ type: "SET_OUTPUT_SEARCH", payload: v })}
            onClose={() => dispatch({ type: "SET_SHOW_OUTPUT_SEARCH", payload: false })}
            lastOutputLines={stateTracking.lastOutputLines}
          />
        )}

        <div className="flex-1 flex flex-row overflow-hidden">
          {workflowGen.showSidebar && (
            <SessionManagerPanel
              manager={sessionManager}
              selectedSessionId={workflowGen.selectedTranscriptSessionId}
              sessionConflictCounts={fileConflicts.sessionConflictCounts}
            />
          )}

          <div className="flex-1 relative overflow-hidden">
            {tabs.length > 0 ? (
              <ZoneGrid
                onZoneClick={handleZoneClick}
                onZoneDoubleClick={handleZoneDoubleClick}
                onExit={handleExit}
                onExportZone={handleExportZone}
              />
            ) : (
              <div className="h-full flex flex-col items-center justify-center text-[#565f89] gap-2">
                <span className="text-sm">
                  No terminals open. Press{" "}
                  <kbd className="px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#a9b1d6] text-xs font-mono">
                    Ctrl+Shift+T
                  </kbd>{" "}
                  or click + to create one.
                </span>
              </div>
            )}

            {zoneLayout.isMultiZone && <ZoneMinimap />}

            {zoneLayout.isMultiZone && !transitionEffects.batchBarDismissed && (
              <BatchOperationsBar />
            )}
          </div>

          {uiState.showControlPanel && zoneLayout.isMultiZone && (
            <ZoneControlPanel onCreateTerminal={() => createAndAssignTerminal()} />
          )}

          <TerminalRightPanel />
        </div>

        <TerminalOverlays onSortZones={handleSortZones} onExport={handleExportOutput} />
      </div>
    </UIBridgeComponentScope>
  );
}
