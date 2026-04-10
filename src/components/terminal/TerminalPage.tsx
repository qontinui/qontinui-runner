import { useEffect, useCallback, useRef, useMemo } from "react";
import { useUIComponent } from "ui-bridge";
import { instanceStorage } from "@/lib/instance-storage";
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
  TerminalCoreProvider, useTerminalCore,
  SessionStateProvider, useSessionState,
  ZoneMetadataProvider, useZoneMetadata,
  TransitionEffectsProvider, useTransitionEffects,
  AiFeaturesProvider, useAiFeatures,
  ShellInfraProvider, useShellInfra,
  UIStateProvider, useUIStateCx,
} from "./contexts";
import { ZoneTimeline } from "./ZoneTimeline";
import { ZoneControlPanel } from "./ZoneControlPanel";
import { OutputSearchBar } from "./OutputSearchBar";
import { TerminalRightPanel } from "./TerminalRightPanel";
import { TerminalOverlays } from "./TerminalOverlays";

import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useTerminalInitialization } from "./useTerminalInitialization";
import { useZoneActions } from "./useZoneActions";

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
  onNavigateToBuilder,
  onNavigateToActive,
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
        handler: async () => {
          await createTerminal();
        },
      },
      {
        id: "list-terminals",
        label: "List Terminals",
        handler: () => tabs.map((t) => ({ id: t.id, title: t.title, isAlive: t.isAlive })),
      },
    ],
  });

  useEffect(() => {
    onSessionCountChange?.(tabs.length);
  }, [tabs.length, onSessionCountChange]);

  const { fileConflicts, fileLockStates, sessionPersistence } = useShellInfra();
  const {
    labelsAndTags,
    eventHistory,
    addHistoryEvent,
    metrics,
    incrementMetric,
    focusHistory,
  } = useZoneMetadata();

  const stateTracking = useSessionState();

  const transitionEffects = useTransitionEffects();
  const { handleRestartInZone } = transitionEffects;

  const {
    workflowGen,
    analysis,
    findingsActions,
    sessionManager,
  } = useAiFeatures();

  const {
    state: uiState,
    dispatch,
    toggleFocusMode,
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
    <div className="h-full flex flex-col bg-[#1a1b26]">
      <TerminalTabBar
        tabs={tabs}
        activeId={activeId}
        onSelect={(id) => {
          setActiveId(id);
          const zoneIdx = Object.entries(zoneLayout.assignments).find(([, tabId]) => tabId === id);
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
              onLoadProfile={async (profile) => {
                zoneLayout.setLayoutId(profile.layoutId);
                labelsAndTags.setZoneLabels(profile.labels);
                labelsAndTags.setZoneNotes(profile.notes);
                labelsAndTags.setPinnedZones(new Set(profile.pins));
                transitionEffects.setAutoApprovePatterns(profile.autoApprovePatterns);
                // Create terminals for profile sessions that need them
                if (profile.sessions && profile.sessions.length > 0) {
                  // Count how many new terminals we need (sessions without existing tabs)
                  const existingTabCount = tabs.length;
                  const neededCount = profile.sessions.length;
                  const toCreate = Math.max(0, neededCount - existingTabCount);
                  for (let i = 0; i < toCreate; i++) {
                    await createAndAssignTerminal();
                  }
                  // Stash sessions for resume after assignments settle (useEffect above)
                  pendingProfileSessionsRef.current = profile.sessions;
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
          const layoutMap: Record<number, string> = {
            2: "split",
            4: "quad",
            6: "six-pack",
            9: "full-grid",
          };
          const layoutId = layoutMap[count];
          if (layoutId) zoneLayout.setLayoutId(layoutId);
          const title = autoCommand ? autoCommand.slice(0, 20) : undefined;
          const createdTabIds: string[] = [];
          for (let i = 0; i < count; i++) {
            const tabId = await createAndAssignTerminal(title);
            if (tabId) createdTabIds.push(tabId);
          }
          if (autoCommand && createdTabIds.length > 0) {
            setTimeout(() => {
              for (const tabId of createdTabIds) {
                terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${autoCommand}\r`);
              }
            }, 1500);
          }
        }}
        onLaunchAiSession={async (count, configDir, context) => {
          const layoutMap: Record<number, string> = {
            2: "split",
            4: "quad",
            6: "six-pack",
            9: "full-grid",
          };
          if (count > 1) {
            const layoutId = layoutMap[count] ?? "full-grid";
            zoneLayout.setLayoutId(layoutId);
          }
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
            // Type the launch command
            setTimeout(() => {
              for (const tabId of createdTabIds) {
                terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${cmd}\r`);
              }
            }, 1500);
            // Type the initial instructions after Claude starts
            if (context) {
              const safeContext = context.replace(/\n/g, " ");
              setTimeout(() => {
                for (const tabId of createdTabIds) {
                  terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${safeContext}\r`);
                }
              }, 8000);
            }
          }
        }}
        onLaunchMultiAiSessions={async (configDirs, context) => {
          const layoutMap: Record<number, string> = {
            2: "split",
            4: "quad",
            6: "six-pack",
            9: "full-grid",
          };
          const count = configDirs.length;
          if (count > 1) {
            const layoutId = layoutMap[count] ?? "full-grid";
            zoneLayout.setLayoutId(layoutId);
          }
          const isWindows = navigator.platform.startsWith("Win");
          const cmds = sessionManager.launchCommands ?? {};
          const createdTabIds: string[] = [];
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
              setTimeout(
                () => {
                  terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${cmd}\r`);
                },
                1500 + i * 300,
              );
            }
          }
          // Type initial instructions after Claude starts (stagger per session)
          if (context && createdTabIds.length > 0) {
            const safeContext = context.replace(/\n/g, " ");
            for (let j = 0; j < createdTabIds.length; j++) {
              const tabId = createdTabIds[j];
              setTimeout(
                () => {
                  terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${safeContext}\r`);
                },
                1500 + j * 300 + 8000,
              );
            }
          }
        }}
        accountUsage={sessionManager.accountUsage}
        launchCommands={sessionManager.launchCommands}
        fileLocks={sessionManager.fileLocks}
        fileLockStates={fileLockStates}
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

          {zoneLayout.isMultiZone && (
            <ZoneMinimap
              layout={zoneLayout.layout}
              assignments={zoneLayout.assignments}
              sessionStates={stateTracking.sessionStates}
              focusedZone={zoneLayout.focusedZone}
              onFocusZone={zoneLayout.setFocusedZone}
              zoneTags={labelsAndTags.zoneTags}
              labelColorMap={labelsAndTags.labelColorMap}
            />
          )}

          {zoneLayout.isMultiZone && !transitionEffects.batchBarDismissed && (
            <BatchOperationsBar
              tabs={tabs}
              sessionStates={stateTracking.sessionStates}
              terminalRefs={terminalRefs.current}
              onDismiss={() => transitionEffects.setBatchBarDismissed(true)}
              selectedZones={uiState.selectedZones}
              assignments={zoneLayout.assignments}
              zoneLabels={labelsAndTags.zoneLabels}
              onSelectAllWaiting={() => {
                const waiting = new Set<number>();
                for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
                  if (stateTracking.sessionStates[tabId] === "needs-input") {
                    waiting.add(Number(zoneStr));
                  }
                }
                dispatch({ type: "SET_SELECTED_ZONES", payload: waiting });
              }}
              onClearSelection={() => dispatch({ type: "CLEAR_SELECTION" })}
              onMetrics={(type, count) => {
                if (type === "approve") {
                  incrementMetric("totalApprovals", count);
                  addHistoryEvent("Batch approve", `${count} sessions`, undefined, "#9ece6a");
                } else if (type === "reject") {
                  incrementMetric("totalRejections", count);
                  addHistoryEvent("Batch reject", `${count} sessions`, undefined, "#f7768e");
                } else if (type === "broadcast") {
                  incrementMetric("totalBroadcasts", count);
                  addHistoryEvent("Broadcast", `${count} sessions`, undefined, "#7aa2f7");
                }
              }}
            />
          )}
        </div>

        {uiState.showControlPanel && zoneLayout.isMultiZone && (
          <ZoneControlPanel
            onCreateTerminal={() => createAndAssignTerminal()}
          />
        )}

        <TerminalRightPanel />
      </div>

      <TerminalOverlays
        onSortZones={handleSortZones}
        onExport={handleExportOutput}
      />
    </div>
  );
}
