import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { useUIComponent } from "ui-bridge";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import { TerminalTabBar } from "./TerminalTabBar";
import { TerminalNotification } from "./TerminalNotification";
import { FileConflictBanner } from "./FileConflictBanner";
import { useFileConflicts } from "./useFileConflicts";
import { SessionManagerPanel } from "./SessionManagerPanel";
import { useSessionManager } from "./useSessionManager";
import { ZoneGrid } from "./ZoneGrid";
import { ZoneLayoutPicker } from "./ZoneLayoutPicker";
import { ZoneStatusBar } from "./ZoneStatusBar";
import { BatchOperationsBar } from "./BatchOperationsBar";
import { ZoneMinimap } from "./ZoneMinimap";
import { ZoneProfilePicker } from "./ZoneProfilePicker";
import { useTerminalPageId } from "./TerminalPageContext";
import { TerminalCoreProvider, useTerminalCore } from "./contexts";
import { ZoneTimeline } from "./ZoneTimeline";
import { ZoneControlPanel } from "./ZoneControlPanel";
import { useSessionPersistence } from "./useSessionPersistence";
import { OutputSearchBar } from "./OutputSearchBar";
import { TerminalRightPanel } from "./TerminalRightPanel";
import { TerminalOverlays } from "./TerminalOverlays";

import { useEventHistory } from "./useEventHistory";
import { useFocusHistory } from "./useFocusHistory";
import { useUnreadTracking } from "./useUnreadTracking";
import { useOutputSnapshots } from "./useOutputSnapshots";
import { useWindowTitle } from "./useWindowTitle";
import { useZoneLabelsAndTags } from "./useZoneLabelsAndTags";
import { useSessionStateTracking } from "./useSessionStateTracking";
import { useStateTransitionEffects } from "./useStateTransitionEffects";
import { useShellIntegration } from "./useShellIntegration";
import { useWorkflowGeneration } from "./useWorkflowGeneration";
import { useAnalysis } from "./useAnalysis";
import { useFindingsActions } from "./useFindingsActions";
import { useTranscriptSessions } from "./useTranscriptSessions";
import { useTerminalFindings } from "./useTerminalFindings";
import { useUIState } from "./useUIState";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useTerminalInitialization } from "./useTerminalInitialization";
import { useZoneActions } from "./useZoneActions";
import { useFileLockTracking } from "./useFileLockTracking";

interface TerminalPageProps {
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  onSessionCountChange?: (count: number) => void;
}

export function TerminalPage(props: TerminalPageProps) {
  const pageId = useTerminalPageId();
  return (
    <TerminalCoreProvider pageId={pageId}>
      <TerminalPageInner {...props} />
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
    markReconnected,
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

  const fileConflicts = useFileConflicts();
  const fileLockStates = useFileLockTracking(tabs);
  const { eventHistory, addHistoryEvent, metrics, incrementMetric } = useEventHistory();
  const focusHistory = useFocusHistory(zoneLayout.focusedZone, zoneLayout.setFocusedZone);
  const labelsAndTags = useZoneLabelsAndTags(zoneLayout.layoutId, zoneLayout.assignments, pageId);

  const processOutputRef = useRef<((tabId: string, text: string) => void) | undefined>(undefined);

  const stateTracking = useSessionStateTracking({
    tabs,
    processOutput: (tabId, text) => processOutputRef.current?.(tabId, text),
    getBufferLines: (tabId, maxLines) => {
      const ref = terminalRefs.current.get(tabId);
      const scrollback = ref?.current?.getScrollback?.(maxLines) ?? "";
      if (!scrollback) return [];
      return scrollback
        .split("\n")
        .filter((l) => l.trim().length > 0)
        .slice(-maxLines);
    },
  });

  const { unreadZones: _unreadZones } = useUnreadTracking(
    zoneLayout.focusedZone,
    zoneLayout.assignments,
    stateTracking.lastOutputLines,
  );

  const snapshots = useOutputSnapshots(stateTracking.lastOutputLines);

  const handleRestartInZoneRef = useRef<(zoneIdx: number) => void>(() => {});

  const handleRestartInZone = useCallback(
    async (zoneIdx: number) => {
      const oldTabId = zoneLayout.assignments[zoneIdx];
      const oldTab = tabs.find((t) => t.id === oldTabId);
      const state = oldTabId ? (stateTracking.sessionStates[oldTabId] ?? "idle") : "idle";
      if (state !== "completed" && state !== "error") return;
      const label = labelsAndTags.zoneLabels[zoneIdx];
      const tabId = await createTerminal(
        oldTab?.title ? `${oldTab.title} (2)` : undefined,
        oldTab?.workingDir ?? undefined,
      );
      if (tabId) {
        zoneLayout.assignTabToZone(zoneIdx, tabId);
        zoneLayout.setFocusedZone(zoneIdx);
        if (label) {
          labelsAndTags.setZoneLabel(zoneIdx, label);
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      zoneLayout.assignments,
      zoneLayout.assignTabToZone,
      zoneLayout.setFocusedZone,
      tabs,
      stateTracking.sessionStates,
      labelsAndTags.zoneLabels,
      labelsAndTags.setZoneLabel,
      createTerminal,
    ],
  );
  handleRestartInZoneRef.current = handleRestartInZone;

  const transitionEffects = useStateTransitionEffects({
    sessionStates: stateTracking.sessionStates,
    prevSessionStatesRef: stateTracking.prevSessionStatesRef,
    tabs,
    assignments: zoneLayout.assignments,
    lastOutputLines: stateTracking.lastOutputLines,
    terminalRefs: terminalRefs.current,
    stateEntryTimeRef: stateTracking.stateEntryTimeRef,
    stateTimeAccumRef: stateTracking.stateTimeAccum,
    setFocusedZone: zoneLayout.setFocusedZone,
    handleRestartInZone,
    addHistoryEvent,
  });

  const rightPanelModeSetterRef = useRef<
    React.Dispatch<React.SetStateAction<"transcript" | "workflow" | "analysis" | "findings" | null>>
  >(() => {});
  const selectedSessionSetterRef = useRef<React.Dispatch<React.SetStateAction<string | null>>>(
    () => {},
  );

  const shellIntegration = useShellIntegration({
    tabs,
    updateTab,
    renameTab,
    createTerminal,
    setSessionStates: stateTracking.setSessionStates,
    terminalRefs,
    setRightPanelMode: (v) => rightPanelModeSetterRef.current(v as never),
    setSelectedTranscriptSessionId: (v) => selectedSessionSetterRef.current(v as never),
  });

  const getScrollback = useCallback((tabId: string, maxLines = 500): string => {
    const ref = terminalRefs.current.get(tabId);
    return ref?.current?.getScrollback?.(maxLines) ?? "";
  }, []);

  const getActiveSelection = useCallback((): string => {
    if (!activeId) return "";
    const ref = terminalRefs.current.get(activeId);
    return ref?.current?.getSelection?.() ?? "";
  }, [activeId]);

  const {
    sessions: transcriptSessions,
    loading: sessionsLoading,
    refresh: refreshSessions,
    loadMessages,
  } = useTranscriptSessions();

  const sessionManager = useSessionManager({
    tabs,
    sessionStates: stateTracking.sessionStates,
    staleTabs: stateTracking.staleTabs,
    transcriptSessions,
    sessionsLoading,
    desktopNotify: transitionEffects.desktopNotify,
    onRefreshSessions: refreshSessions,
    onResumeSession: shellIntegration.handleResumeSession,
    onSelectSession: (sessionId: string) => {
      selectedSessionSetterRef.current(sessionId);
      rightPanelModeSetterRef.current("transcript" as never);
    },
  });

  const { processOutput, activeFindings, allFindings } = useTerminalFindings(activeId ?? null);

  const workflowGen = useWorkflowGeneration({
    activeId,
    tabs,
    loadMessages,
    onNavigateToBuilder,
    onNavigateToActive,
  });

  const analysis = useAnalysis({
    activeId,
    tabs,
    commandHistories: shellIntegration.commandHistories,
    getScrollback,
    getActiveSelection,
    latestPlanContent: workflowGen.latestPlanContent,
    setRightPanelMode: workflowGen.setRightPanelMode,
  });

  const findingsActions = useFindingsActions({
    activeId,
    tabs,
    terminalRefs,
    createTerminal,
    pendingResumeRef: shellIntegration.pendingResumeRef,
    runGeneration: workflowGen.runGeneration,
    setRightPanelMode: workflowGen.setRightPanelMode,
  });

  // Session summary (AI Tier 2)
  const [sessionSummary, setSessionSummary] = useState<string | null>(null);
  const [sessionSummaryLoading, setSessionSummaryLoading] = useState(false);

  const handleSummarizeSession = useCallback(
    async (messages: Array<{ msg_type: string; text: string }>) => {
      setSessionSummaryLoading(true);
      setSessionSummary(null);
      try {
        // Build text from transcript messages
        const text = messages
          .map((m) => `${m.msg_type === "user" ? "User" : "Assistant"}: ${m.text}`)
          .join("\n\n");
        const result = await invoke<{ success: boolean; data?: unknown; message?: string }>(
          "analyze_session_summary",
          { input: text },
        );
        if (result.success && result.data) {
          // Extract markdown content from first panel in the response
          const data = result.data as
            | Array<{ type?: string; content?: string }>
            | { panels?: Array<{ content?: string }> };
          let summaryContent: string | null = null;

          if (Array.isArray(data)) {
            // Direct array of panels
            const markdownPanel = data.find((p) => p.type === "markdown" || p.content);
            summaryContent = markdownPanel?.content ?? null;
          } else if (data && "panels" in data && Array.isArray(data.panels)) {
            summaryContent = data.panels[0]?.content ?? null;
          }

          setSessionSummary(summaryContent || "Summary generated but no content available.");
        } else {
          setSessionSummary(result.message || "Unable to generate summary.");
        }
      } catch {
        setSessionSummary("Failed to generate summary.");
      } finally {
        setSessionSummaryLoading(false);
      }
    },
    [],
  );

  processOutputRef.current = processOutput;
  rightPanelModeSetterRef.current = workflowGen.setRightPanelMode;
  selectedSessionSetterRef.current = workflowGen.setSelectedTranscriptSessionId;

  const needsInputCount = Object.values(stateTracking.sessionStates).filter(
    (s) => s === "needs-input",
  ).length;
  const _workingCount = Object.values(stateTracking.sessionStates).filter(
    (s) => s === "working",
  ).length;
  const errorCount = Object.values(stateTracking.sessionStates).filter((s) => s === "error").length;

  useWindowTitle(needsInputCount, errorCount, zoneLayout.isMultiZone);

  const {
    state: uiState,
    dispatch,
    toggleFocusMode,
    toggleAutoLayout,
    cycleViewMode,
  } = useUIState();

  const sessionPersistence = useSessionPersistence(pageId);

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

  const approveTab = useCallback(
    (tabId: string) => {
      terminalRefs.current.get(tabId)?.current?.writeToTerminal("y\r");
      incrementMetric("totalApprovals");
    },
    [incrementMetric],
  );

  const rejectTab = useCallback(
    (tabId: string) => {
      terminalRefs.current.get(tabId)?.current?.writeToTerminal("n\r");
      incrementMetric("totalRejections");
    },
    [incrementMetric],
  );

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
        tabs={tabs}
        assignments={zoneLayout.assignments}
        sessionStates={stateTracking.sessionStates}
        onJumpToNeedsInput={() => zoneLayout.focusNextNeedsInput(stateTracking.sessionStates)}
        onShowShortcuts={() => dispatch({ type: "SET_SHOW_SHORTCUTS", payload: true })}
        autoFocus={transitionEffects.autoFocusNeedsInput}
        onToggleAutoFocus={transitionEffects.toggleAutoFocus}
        soundEnabled={transitionEffects.soundEnabled}
        onToggleSound={transitionEffects.toggleSound}
        desktopNotify={transitionEffects.desktopNotify}
        onToggleDesktopNotify={() => {
          transitionEffects.setDesktopNotify((prev) => {
            const next = !prev;
            instanceStorage.setItem("zone-desktop-notify", String(next));
            return next;
          });
        }}
        stateDurations={stateTracking.stateDurations}
        onSelectByState={(state) => {
          const zones = new Set<number>();
          for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
            if ((stateTracking.sessionStates[tabId] ?? "idle") === state) {
              zones.add(Number(zoneStr));
            }
          }
          dispatch({ type: "SET_SELECTED_ZONES", payload: zones });
        }}
        metrics={metrics.current}
        zoneLabels={labelsAndTags.zoneLabels}
        onExport={handleExportOutput}
        onSortZones={handleSortZones}
        eventHistory={eventHistory}
        labelColorMap={labelsAndTags.labelColorMap}
        focusMode={uiState.focusMode}
        autoApprovePatterns={transitionEffects.autoApprovePatterns}
        onSetAutoApprovePatterns={transitionEffects.setAutoApprovePatterns}
        autoApproveCount={transitionEffects.autoApproveCount}
        stateTimeAccum={stateTracking.stateTimeAccum.current}
        autoRestart={transitionEffects.autoRestart}
        onToggleAutoRestart={() => {
          transitionEffects.setAutoRestart((prev) => {
            const next = !prev;
            instanceStorage.setItem("zone-auto-restart", String(next));
            return next;
          });
        }}
        autoRestartCount={transitionEffects.autoRestartCount}
        onToggleFocusMode={toggleFocusMode}
        activeTagFilters={labelsAndTags.activeTagFilters}
        onSetActiveTagFilters={labelsAndTags.setActiveTagFilters}
        allTags={labelsAndTags.allTags}
        lastOutputLines={stateTracking.lastOutputLines}
        frozenSessionCount={sessionManager.frozenCount}
        showSidebar={workflowGen.showSidebar}
        onToggleSidebar={() => workflowGen.setShowSidebar((v) => !v)}
        isGenerating={workflowGen.isGenerating}
        isAnalyzing={analysis.isAnalyzing}
        onAnalyze={analysis.handleAnalyze}
        onGenerateFromSession={workflowGen.handleGenerateFromLatestSession}
        planFileName={workflowGen.planFileName}
        isPlanLoading={workflowGen.isPlanLoading}
        onRefreshPlan={workflowGen.loadPlanContent}
        onBuildPlanFromFile={workflowGen.handleBuildPlanFromFile}
        onBuildPlanImplementationFromFile={workflowGen.handleBuildPlanImplementationFromFile}
        onToggleFindings={findingsActions.handleToggleFindings}
        findingsActive={workflowGen.rightPanelMode === "findings"}
        findingsCount={activeFindings.length}
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
              layout={zoneLayout.layout}
              assignments={zoneLayout.assignments}
              tabs={tabs}
              focusedZone={zoneLayout.focusedZone}
              maximizedZone={zoneLayout.maximizedZone}
              sessionStates={stateTracking.sessionStates}
              lastOutputLines={stateTracking.lastOutputLines}
              viewMode={uiState.viewMode}
              terminalRefs={terminalRefs.current}
              onZoneClick={handleZoneClick}
              onZoneDoubleClick={handleZoneDoubleClick}
              onExit={handleExit}
              onFirstInput={shellIntegration.handleFirstInput}
              onShellIntegration={shellIntegration.handleShellIntegration}
              onOutput={stateTracking.handleOutput}
              onReconnected={markReconnected}
              onAssignTab={zoneLayout.assignTabToZone}
              flashingTabs={transitionEffects.flashingTabs}
              stateDurations={stateTracking.stateDurations}
              selectedZones={uiState.selectedZones}
              staleTabs={stateTracking.staleTabs}
              pinnedZones={labelsAndTags.pinnedZones}
              onTogglePin={labelsAndTags.togglePin}
              outputSearchQuery={uiState.outputSearch || undefined}
              swapSource={uiState.swapSource}
              activityData={stateTracking.activityData}
              zoneLabels={labelsAndTags.zoneLabels}
              onSetZoneLabel={labelsAndTags.setZoneLabel}
              onRestartInZone={handleRestartInZone}
              resetRatiosKey={uiState.resetRatiosKey}
              labelColorMap={labelsAndTags.labelColorMap}
              zoneTags={labelsAndTags.zoneTags}
              commandHistories={shellIntegration.commandHistories}
              focusMode={uiState.focusMode}
              zoneNotes={labelsAndTags.zoneNotes}
              onSetZoneNote={labelsAndTags.setZoneNote}
              onExportZone={handleExportZone}
              pendingRestarts={transitionEffects.pendingRestarts}
              onCancelRestart={transitionEffects.cancelPendingRestart}
              pageId={pageId}
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
            tabs={tabs}
            assignments={zoneLayout.assignments}
            sessionStates={stateTracking.sessionStates}
            zoneLabels={labelsAndTags.zoneLabels}
            zoneNotes={labelsAndTags.zoneNotes}
            labelColorMap={labelsAndTags.labelColorMap}
            focusedZone={zoneLayout.focusedZone}
            zoneCount={zoneLayout.layout.zones.length}
            lastOutputLines={stateTracking.lastOutputLines}
            onFocusZone={zoneLayout.setFocusedZone}
            onSetZoneLabel={labelsAndTags.setZoneLabel}
            onSetZoneNotes={labelsAndTags.setZoneNote}
            onClose={() => dispatch({ type: "SET_SHOW_CONTROL_PANEL", payload: false })}
            collapsed={uiState.controlPanelCollapsed}
            onToggleCollapsed={() => dispatch({ type: "TOGGLE_CONTROL_PANEL_COLLAPSED" })}
            onCreateTerminal={() => createAndAssignTerminal()}
            pinnedZones={labelsAndTags.pinnedZones}
            onTogglePin={labelsAndTags.togglePin}
            onSwapZones={(src, dst) => {
              const srcTabId = zoneLayout.assignments[src];
              const dstTabId = zoneLayout.assignments[dst];
              if (srcTabId) zoneLayout.assignTabToZone(dst, srcTabId);
              if (dstTabId) zoneLayout.assignTabToZone(src, dstTabId);
            }}
            onLoadWorkspace={async (workspace) => {
              if (workspace.layoutId !== zoneLayout.layoutId) {
                zoneLayout.setLayoutId(workspace.layoutId);
              }
              for (const session of workspace.sessions) {
                if (session.zoneIndex < 0) {
                  await createTerminal(session.title, session.workingDir);
                  continue;
                }
                const tabId = await createTerminal(session.title, session.workingDir);
                if (tabId) {
                  zoneLayout.assignTabToZone(session.zoneIndex, tabId);
                }
                if (session.label) {
                  labelsAndTags.setZoneLabel(session.zoneIndex, session.label);
                }
                if (session.notes) {
                  labelsAndTags.setZoneNote(session.zoneIndex, session.notes);
                }
                if (session.pinned) {
                  labelsAndTags.setPinnedZones((prev) => new Set([...prev, session.zoneIndex]));
                }
              }
            }}
            layoutId={zoneLayout.layoutId}
            pageId={pageId}
          />
        )}

        <TerminalRightPanel
          rightPanelMode={workflowGen.rightPanelMode}
          selectedTranscriptSessionId={workflowGen.selectedTranscriptSessionId}
          transcriptSessions={transcriptSessions}
          transcriptMessages={workflowGen.transcriptMessages}
          loadingMessages={workflowGen.loadingMessages}
          onGenerateFromTranscript={workflowGen.handleGenerateFromTranscript}
          onGenerateAndRunFromTranscript={workflowGen.handleGenerateAndRunFromTranscript}
          onBuildPlanWorkflow={workflowGen.handleBuildPlanWorkflow}
          onBuildPlanImplementationWorkflow={workflowGen.handleBuildPlanImplementationWorkflow}
          onResumeSession={shellIntegration.handleResumeSession}
          onSummarizeSession={handleSummarizeSession}
          sessionSummary={sessionSummary}
          sessionSummaryLoading={sessionSummaryLoading}
          onClosePanel={() => {
            workflowGen.setRightPanelMode(null);
            workflowGen.setSelectedTranscriptSessionId(null);
          }}
          generatedWorkflow={workflowGen.generatedWorkflow}
          isGenerating={workflowGen.isGenerating}
          workflowError={workflowGen.workflowError}
          onExecute={workflowGen.handleExecute}
          onEditInBuilder={workflowGen.handleEditInBuilder}
          onRegenerate={workflowGen.handleRegenerate}
          onSaveWorkflow={workflowGen.handleSaveWorkflow}
          onCloseWorkflow={() => workflowGen.setRightPanelMode(null)}
          analysisType={analysis.analysisType}
          analysisPanels={analysis.analysisPanels}
          isAnalyzing={analysis.isAnalyzing}
          analysisError={analysis.analysisError}
          onCloseAnalysis={() => workflowGen.setRightPanelMode(null)}
          activeFindings={activeFindings}
          allFindings={allFindings}
          onFindingRespond={findingsActions.handleFindingRespond}
          onFixFinding={findingsActions.handleFixFinding}
          onGenerateFromFindings={findingsActions.handleGenerateFromFindings}
          onCloseFindings={() => workflowGen.setRightPanelMode(null)}
        />
      </div>

      <TerminalOverlays
        showShortcutsOverlay={uiState.showShortcutsOverlay}
        onCloseShortcuts={() => dispatch({ type: "SET_SHOW_SHORTCUTS", payload: false })}
        showCommandPalette={uiState.showCommandPalette}
        onCloseCommandPalette={() => dispatch({ type: "SET_SHOW_COMMAND_PALETTE", payload: false })}
        tabs={tabs}
        assignments={zoneLayout.assignments}
        sessionStates={stateTracking.sessionStates}
        focusedZone={zoneLayout.focusedZone}
        onFocusZone={zoneLayout.setFocusedZone}
        onApproveTab={approveTab}
        onRejectTab={rejectTab}
        onRestartZone={handleRestartInZone}
        onTogglePin={labelsAndTags.togglePin}
        pinnedZones={labelsAndTags.pinnedZones}
        onApproveAll={() => {
          const ni = tabs.filter((t) => stateTracking.sessionStates[t.id] === "needs-input");
          incrementMetric("totalApprovals", ni.length);
          addHistoryEvent("Approve all", `${ni.length} sessions`, undefined, "#9ece6a");
          for (const tab of ni) {
            terminalRefs.current.get(tab.id)?.current?.writeToTerminal("y\r");
          }
        }}
        onSortZones={handleSortZones}
        onExport={handleExportOutput}
        onToggleFocusMode={toggleFocusMode}
        focusMode={uiState.focusMode}
        onToggleAutoFocus={transitionEffects.toggleAutoFocus}
        autoFocus={transitionEffects.autoFocusNeedsInput}
        onToggleSound={transitionEffects.toggleSound}
        soundEnabled={transitionEffects.soundEnabled}
        zoneLabels={labelsAndTags.zoneLabels}
        onSetZoneLabel={labelsAndTags.setZoneLabel}
        zoneCount={zoneLayout.layout.zones.length}
        onCompareZones={(z1, z2) => {
          dispatch({ type: "SET_SHOW_COMMAND_PALETTE", payload: false });
          snapshots.setDiffZones([z1, z2]);
        }}
        onSnapshotZone={snapshots.snapshotZone}
        onCompareSnapshot={(tabId) => {
          snapshots.compareSnapshot(tabId);
          dispatch({ type: "SET_SHOW_COMMAND_PALETTE", payload: false });
        }}
        snapshotZones={snapshots.snapshotZones}
        diffZones={snapshots.diffZones}
        lastOutputLines={stateTracking.lastOutputLines}
        onCloseDiff={() => snapshots.setDiffZones(null)}
        snapshotDiff={snapshots.snapshotDiff}
        onCloseSnapshotDiff={snapshots.clearSnapshotDiff}
      />
    </div>
  );
}
