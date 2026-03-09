import { useEffect, useCallback, useRef, useState, useMemo, createRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { type TerminalInstanceHandle } from "./TerminalInstance";
import { TerminalTabBar } from "./TerminalTabBar";

import { TerminalNotification } from "./TerminalNotification";
import { TranscriptSessionSidebar } from "./TranscriptSessionSidebar";
import { TranscriptContentPanel } from "./TranscriptContentPanel";
import { TerminalAnalysisPanel } from "./TerminalAnalysisPanel";
import { TerminalFindingsPanel } from "./TerminalFindingsPanel";
import { useTerminalManager } from "./useTerminalManager";
import { useZoneLayout, LAYOUT_PRESETS, type SessionState } from "./useZoneLayout";
import { ZoneGrid, type ViewMode } from "./ZoneGrid";
import { ZoneLayoutPicker } from "./ZoneLayoutPicker";
import { ZoneStatusBar } from "./ZoneStatusBar";
import { BatchOperationsBar } from "./BatchOperationsBar";
import { KeyboardShortcutsOverlay } from "./KeyboardShortcutsOverlay";
import { ZoneMinimap } from "./ZoneMinimap";
import { ZoneProfilePicker } from "./ZoneProfilePicker";
import { CommandPalette } from "./CommandPalette";
import { ZoneDiffOverlay } from "./ZoneDiffOverlay";
import { ZoneTimeline } from "./ZoneTimeline";
import { ZoneControlPanel } from "./ZoneControlPanel";
import { useSessionPersistence } from "./useSessionPersistence";
import { WorkflowPreviewPanel } from "@qontinui/workflow-ui";
import { save, open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";

// ── Extracted hooks ────────────────────────────────────────────────────────────
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
import type { CommandResponse } from "./types";

interface TerminalPageProps {
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  onSessionCountChange?: (count: number) => void;
}

export function TerminalPage({
  onNavigateToBuilder,
  onNavigateToActive,
  onSessionCountChange,
}: TerminalPageProps) {
  // ══════════════════════════════════════════════════════════════════════════════
  // 1. Core terminal management (already extracted)
  // ══════════════════════════════════════════════════════════════════════════════

  const {
    tabs,
    activeId,
    setActiveId,
    initialized: _initialized,
    setInitialized,
    createTerminal,
    closeTerminal,
    renameTab,
    updateTab,
    reconnectToExistingSessions,
    markReconnected,
    createPlanTab,
  } = useTerminalManager();

  // ── Zone layout ─────────────────────────────────────────────────────────────
  // Memoize tabIds to prevent useZoneLayout's auto-assign effect from running
  // every render (tabs.map creates a new array reference each time).
  const tabIds = useMemo(() => tabs.map((t) => t.id), [tabs]);
  const zoneLayout = useZoneLayout(tabIds);

  // Sync zone focus → activeId so existing handlers (analysis, findings) work.
  // Guard: only set activeId to a tab that actually exists in tabs (prevents
  // setting activeId to a dead tab during the close cascade).
  useEffect(() => {
    if (
      zoneLayout.focusedTabId &&
      zoneLayout.focusedTabId !== activeId &&
      tabs.some((t) => t.id === zoneLayout.focusedTabId)
    ) {
      setActiveId(zoneLayout.focusedTabId);
    }
  }, [zoneLayout.focusedTabId, activeId, setActiveId, tabs]);

  // Report session count changes to parent for sidebar auto-collapse
  useEffect(() => {
    onSessionCountChange?.(tabs.length);
  }, [tabs.length, onSessionCountChange]);

  // ══════════════════════════════════════════════════════════════════════════════
  // 2. Extracted hooks
  // ══════════════════════════════════════════════════════════════════════════════

  // ── Event history ─────────────────────────────────────────────────────────
  const { eventHistory, addHistoryEvent, metrics, incrementMetric } = useEventHistory();

  // ── Focus history ─────────────────────────────────────────────────────────
  const focusHistory = useFocusHistory(zoneLayout.focusedZone, zoneLayout.setFocusedZone);

  // ── Zone labels and tags ──────────────────────────────────────────────────
  const labelsAndTags = useZoneLabelsAndTags(zoneLayout.layoutId, zoneLayout.assignments);

  // ── Ref to break circular dependency between hooks ────────────────────────
  // useWorkflowGeneration provides processOutput, but useSessionStateTracking
  // also needs to call it. We wire it up after both hooks are called.
  const processOutputRef = useRef<((tabId: string, text: string) => void) | undefined>(undefined);

  // ── Session state tracking ────────────────────────────────────────────────
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

  // ── Unread tracking ───────────────────────────────────────────────────────
  const { unreadZones: _unreadZones } = useUnreadTracking(
    zoneLayout.focusedZone,
    zoneLayout.assignments,
    stateTracking.lastOutputLines,
  );

  // ── Output snapshots ──────────────────────────────────────────────────────
  const snapshots = useOutputSnapshots(stateTracking.lastOutputLines);

  // ── Terminal refs ─────────────────────────────────────────────────────────
  const terminalRefs = useRef<Map<string, React.RefObject<TerminalInstanceHandle | null>>>(
    new Map(),
  );

  // Ensure refs exist for all tabs
  for (const tab of tabs) {
    if (!terminalRefs.current.has(tab.id)) {
      terminalRefs.current.set(tab.id, createRef<TerminalInstanceHandle>());
    }
  }
  // Clean up refs for removed tabs
  for (const key of terminalRefs.current.keys()) {
    if (!tabs.some((t) => t.id === key)) {
      terminalRefs.current.delete(key);
    }
  }

  // ── Restart handler (must be defined before hooks that depend on it) ───────
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
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular deps avoid re-creation on unrelated property changes
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

  // ── State transition effects ──────────────────────────────────────────────
  const transitionEffects = useStateTransitionEffects({
    sessionStates: stateTracking.sessionStates,
    prevSessionStatesRef: stateTracking.prevSessionStatesRef,
    tabs,
    assignments: zoneLayout.assignments,
    lastOutputLines: stateTracking.lastOutputLines,
    terminalRefs: terminalRefs.current,
    stateEntryTimeRef: stateTracking.stateEntryTimeRef,
    stateTimeAccum: stateTracking.stateTimeAccum,
    setFocusedZone: zoneLayout.setFocusedZone,
    handleRestartInZone,
    addHistoryEvent,
  });

  // ── Shell integration ─────────────────────────────────────────────────────
  // Shell integration needs setRightPanelMode/setSelectedTranscriptSessionId
  // from workflowGen (for handleResumeSession), but workflowGen needs
  // commandHistories from shell integration. Break the cycle with ref-based
  // indirection: the panel setters are only used inside callbacks (not effects),
  // so they're safe to wire up after both hooks are called.
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

  // ── Scrollback helpers (needed by workflow generation) ────────────────────
  const getScrollback = useCallback((tabId: string, maxLines = 500): string => {
    const ref = terminalRefs.current.get(tabId);
    return ref?.current?.getScrollback?.(maxLines) ?? "";
  }, []);

  const getActiveSelection = useCallback((): string => {
    if (!activeId) return "";
    const ref = terminalRefs.current.get(activeId);
    return ref?.current?.getSelection?.() ?? "";
  }, [activeId]);

  // ── Transcript sessions (called directly, shared across hooks) ──────────
  const {
    sessions: transcriptSessions,
    loading: sessionsLoading,
    refresh: refreshSessions,
    loadMessages,
  } = useTranscriptSessions();

  // ── Terminal findings (called directly) ─────────────────────────────────
  const { processOutput, activeFindings, allFindings } = useTerminalFindings(activeId ?? null);

  // ── Workflow generation ───────────────────────────────────────────────────
  const workflowGen = useWorkflowGeneration({
    activeId,
    tabs,
    loadMessages,
    onNavigateToBuilder,
    onNavigateToActive,
  });

  // ── Analysis ──────────────────────────────────────────────────────────────
  const analysis = useAnalysis({
    activeId,
    tabs,
    commandHistories: shellIntegration.commandHistories,
    getScrollback,
    getActiveSelection,
    latestPlanContent: workflowGen.latestPlanContent,
    setRightPanelMode: workflowGen.setRightPanelMode,
  });

  // ── Findings actions ──────────────────────────────────────────────────────
  const findingsActions = useFindingsActions({
    activeId,
    tabs,
    terminalRefs,
    createTerminal,
    pendingResumeRef: shellIntegration.pendingResumeRef,
    runGeneration: workflowGen.runGeneration,
    setRightPanelMode: workflowGen.setRightPanelMode,
  });

  // Wire up circular dependencies now that both hooks are called
  processOutputRef.current = processOutput;
  rightPanelModeSetterRef.current = workflowGen.setRightPanelMode;
  selectedSessionSetterRef.current = workflowGen.setSelectedTranscriptSessionId;

  // ── Window title ──────────────────────────────────────────────────────────
  const needsInputCount = Object.values(stateTracking.sessionStates).filter(
    (s) => s === "needs-input",
  ).length;
  const _workingCount = Object.values(stateTracking.sessionStates).filter(
    (s) => s === "working",
  ).length;
  const errorCount = Object.values(stateTracking.sessionStates).filter((s) => s === "error").length;

  useWindowTitle(needsInputCount, errorCount, zoneLayout.isMultiZone);

  // ══════════════════════════════════════════════════════════════════════════════
  // 3. Local UI state (kept inline — not extracted)
  // ══════════════════════════════════════════════════════════════════════════════

  const [viewMode, setViewMode] = useState<ViewMode>("auto");

  const [showShortcutsOverlay, setShowShortcutsOverlay] = useState(false);
  const [showCommandPalette, setShowCommandPalette] = useState(false);
  const [showTimeline, setShowTimeline] = useState(false);
  const [showControlPanel, setShowControlPanel] = useState(
    () => localStorage.getItem("zone-control-panel") === "true",
  );
  const [controlPanelCollapsed, setControlPanelCollapsed] = useState(false);
  const [selectedZones, setSelectedZones] = useState<Set<number>>(new Set());
  const [outputSearch, setOutputSearch] = useState("");
  const [showOutputSearch, setShowOutputSearch] = useState(false);
  const [swapSource, setSwapSource] = useState<number | null>(null);
  const [resetRatiosKey, setResetRatiosKey] = useState(0);
  const [autoLayout, setAutoLayout] = useState(
    () => localStorage.getItem("zone-auto-layout") !== "false",
  );
  const [focusMode, setFocusMode] = useState(
    () => localStorage.getItem("zone-focus-mode") === "true",
  );

  // ══════════════════════════════════════════════════════════════════════════════
  // 4. Session persistence
  // ══════════════════════════════════════════════════════════════════════════════

  const {
    saveSessionLayout,
    saveScrollbackBuffers,
    updateScrollbackPaths,
    getSavedLayout,
    clearSavedLayout,
    hasSavedLayout,
  } = useSessionPersistence();

  // Auto-save session layout for persistence across app restarts
  useEffect(() => {
    if (tabs.length === 0) return;
    saveSessionLayout({
      layoutId: zoneLayout.layoutId,
      tabs,
      assignments: zoneLayout.assignments,
      zoneLabels: labelsAndTags.zoneLabels,
      zoneNotes: labelsAndTags.zoneNotes,
      pinnedZones: labelsAndTags.pinnedZones,
      focusedZone: zoneLayout.focusedZone,
    });
  }, [
    tabs,
    zoneLayout.assignments,
    zoneLayout.layoutId,
    zoneLayout.focusedZone,
    labelsAndTags.zoneLabels,
    labelsAndTags.zoneNotes,
    labelsAndTags.pinnedZones,
    saveSessionLayout,
  ]);

  // Save scrollback buffers to disk when the window is about to close
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;
  const zoneLayoutRef = useRef(zoneLayout);
  zoneLayoutRef.current = zoneLayout;
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    getCurrentWindow()
      .onCloseRequested(async () => {
        const currentTabs = tabsRef.current;
        if (currentTabs.length === 0) return;

        try {
          const terminalTabs = currentTabs.filter((t) => t.type !== "plan");
          const pathMap = await saveScrollbackBuffers(terminalTabs);
          const tabIdToSessionIndex: Record<string, number> = {};
          const currentAssignments = zoneLayoutRef.current.assignments;
          const assignedTabIds = new Set(Object.values(currentAssignments));
          let idx = 0;
          for (const [, tabId] of Object.entries(currentAssignments)) {
            if (currentTabs.some((t) => t.id === tabId)) {
              tabIdToSessionIndex[tabId] = idx++;
            }
          }
          for (const tab of currentTabs) {
            if (!assignedTabIds.has(tab.id)) {
              tabIdToSessionIndex[tab.id] = idx++;
            }
          }
          updateScrollbackPaths(pathMap, tabIdToSessionIndex);
        } catch (err) {
          console.warn("[TerminalPage] Failed to save scrollback on close:", err);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      unlisten?.();
    };
  }, [saveScrollbackBuffers, updateScrollbackPaths]);

  // ══════════════════════════════════════════════════════════════════════════════
  // 5. Initialization
  // ══════════════════════════════════════════════════════════════════════════════

  // Diagnostic: detect unexpected unmount/remount cycles
  const mountCountRef = useRef(0);
  useEffect(() => {
    mountCountRef.current += 1;
    const mountNum = mountCountRef.current;
    if (mountNum > 1) {
      console.warn(
        `[TerminalPage] REMOUNTED (mount #${mountNum}) — all terminal tabs were lost. ` +
          `This usually means the parent component tree unmounted (e.g., auth state change).`,
      );
    }
    return () => {
      console.warn(
        `[TerminalPage] UNMOUNTED (was mount #${mountNum}), ${tabs.length} tab(s) will be destroyed`,
      );
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const didInit = useRef(false);
  const pendingRestoresRef = useRef<
    Array<{
      tabId: string;
      scrollbackPath?: string;
      isClaudeSession?: boolean;
      claudeSessionId?: string;
      claudeConfigDir?: string;
    }>
  >([]);

  useEffect(() => {
    if (didInit.current) return;
    didInit.current = true;

    (async () => {
      const reconnected = await reconnectToExistingSessions();
      if (!reconnected) {
        const saved = hasSavedLayout() ? getSavedLayout() : null;
        if (saved && saved.sessions.length > 0) {
          // Restore layout preset
          if (saved.layoutId !== zoneLayout.layoutId) {
            const preset = LAYOUT_PRESETS.find((p) => p.id === saved.layoutId);
            if (preset) zoneLayout.setLayoutId(preset.id);
          }

          const assignedSessions = saved.sessions.filter((s) => s.zoneIndex >= 0);
          const unassignedSessions = saved.sessions.filter((s) => s.zoneIndex < 0);

          for (const session of assignedSessions) {
            // Plan tabs: restore without creating a PTY
            if (session.type === "plan" && session.planFilePath) {
              const tabId = createPlanTab(session.planFilePath);
              if (tabId && session.zoneIndex >= 0) {
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
              continue;
            }

            const tabId = await createTerminal(session.title, session.workingDir);
            if (tabId && session.zoneIndex >= 0) {
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
            if (tabId && (session.scrollbackPath || session.isClaudeSession)) {
              pendingRestoresRef.current.push({
                tabId,
                scrollbackPath: session.scrollbackPath,
                isClaudeSession: session.isClaudeSession,
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: session.claudeConfigDir,
              });
            }
            if (tabId && session.claudeSessionId) {
              updateTab(tabId, {
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: session.claudeConfigDir,
              });
            }
          }

          for (const session of unassignedSessions) {
            if (session.type === "plan" && session.planFilePath) {
              createPlanTab(session.planFilePath);
              continue;
            }
            const tabId = await createTerminal(session.title, session.workingDir);
            if (tabId && (session.scrollbackPath || session.isClaudeSession)) {
              pendingRestoresRef.current.push({
                tabId,
                scrollbackPath: session.scrollbackPath,
                isClaudeSession: session.isClaudeSession,
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: session.claudeConfigDir,
              });
            }
            if (tabId && session.claudeSessionId) {
              updateTab(tabId, {
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: session.claudeConfigDir,
              });
            }
          }

          // Restore focused zone
          if (saved.focusedZone >= 0) {
            zoneLayout.setFocusedZone(saved.focusedZone);
          }

          // Replay scrollback buffers and resume Claude sessions after delay
          if (pendingRestoresRef.current.length > 0) {
            setTimeout(async () => {
              for (const restore of pendingRestoresRef.current) {
                const ref = terminalRefs.current.get(restore.tabId);
                const handle = ref?.current;

                if (restore.scrollbackPath && handle) {
                  try {
                    const result = await invoke<CommandResponse>("terminal_get_saved_scrollback", {
                      filePath: restore.scrollbackPath,
                    });
                    if (result.success && result.data) {
                      const encoded = (result.data as { data: string }).data;
                      if (encoded) {
                        const raw = atob(encoded);
                        const decoded = new TextDecoder().decode(
                          Uint8Array.from(raw, (c) => c.charCodeAt(0)),
                        );
                        handle.writeToDisplay(decoded);
                      }
                    }
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to restore scrollback for ${restore.tabId}:`,
                      err,
                    );
                  }
                }

                if (restore.isClaudeSession && restore.claudeSessionId && handle) {
                  try {
                    const resumeCmd = `claude --resume ${restore.claudeSessionId}\r`;
                    await new Promise((r) => setTimeout(r, 500));
                    handle.writeToTerminal(resumeCmd);
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to resume Claude session for ${restore.tabId}:`,
                      err,
                    );
                  }
                }
              }

              try {
                await invoke("terminal_cleanup_scrollback");
              } catch (err) {
                console.warn("[TerminalPage] Failed to cleanup scrollback files:", err);
              }

              pendingRestoresRef.current = [];
            }, 1500);
          }

          clearSavedLayout();
        } else {
          await createTerminal();
        }
      }
      setInitialized(true);
    })();
  }, [
    reconnectToExistingSessions,
    createTerminal,
    createPlanTab,
    setInitialized,
    getSavedLayout,
    hasSavedLayout,
    clearSavedLayout,
    zoneLayout,
    labelsAndTags,
    updateTab,
  ]);

  // ══════════════════════════════════════════════════════════════════════════════
  // 6. Exit handler
  // ══════════════════════════════════════════════════════════════════════════════

  const handleExit = useCallback(
    (terminalId: string, exitCode: number | null) => {
      updateTab(terminalId, { isAlive: false, exitCode });
      stateTracking.handleExit(terminalId, exitCode);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular dep on .handleExit only
    [updateTab, stateTracking.handleExit],
  );

  // ══════════════════════════════════════════════════════════════════════════════
  // 7. Zone interaction handlers
  // ══════════════════════════════════════════════════════════════════════════════

  const handleZoneClick = useCallback(
    (zoneIndex: number, ctrlKey?: boolean) => {
      if (ctrlKey) {
        setSelectedZones((prev) => {
          const next = new Set(prev);
          if (next.has(zoneIndex)) next.delete(zoneIndex);
          else next.add(zoneIndex);
          return next;
        });
      } else {
        zoneLayout.setFocusedZone(zoneIndex);
        setSelectedZones(new Set());
        const focusedTabId = zoneLayout.assignments[zoneIndex];
        if (focusedTabId) {
          transitionEffects.setUnseenNeedsInput((prev) => {
            if (!prev.has(focusedTabId)) return prev;
            const next = new Set(prev);
            next.delete(focusedTabId);
            return next;
          });
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular deps on specific properties
    [zoneLayout.setFocusedZone, zoneLayout.assignments, transitionEffects.setUnseenNeedsInput],
  );

  const handleZoneDoubleClick = useCallback(
    (zoneIndex: number) => {
      if (zoneLayout.isMultiZone) {
        zoneLayout.toggleMaximize(zoneIndex);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular deps on specific properties
    [zoneLayout.isMultiZone, zoneLayout.toggleMaximize],
  );

  // Open a markdown plan file in a new zone
  const handleOpenPlanFile = useCallback(async () => {
    const selected = (await openFileDialog({
      multiple: false,
      filters: [{ name: "Markdown", extensions: ["md", "txt", "markdown"] }],
    })) as string | null;
    if (!selected) return;
    const filePath = selected;
    const tabId = createPlanTab(filePath);
    if (tabId) {
      // Auto-assign to first empty zone
      const emptyZone = zoneLayout.layout.zones.findIndex((_, idx) => !zoneLayout.assignments[idx]);
      if (emptyZone >= 0) {
        zoneLayout.assignTabToZone(emptyZone, tabId);
        zoneLayout.setFocusedZone(emptyZone);
      }
    }
  }, [createPlanTab, zoneLayout]);

  // Create terminal and auto-assign to first empty zone.
  // When all zones are full, upgrade the layout to one with more zones
  // (regardless of the autoLayout toggle — otherwise the tab is invisible).
  // The auto-assign effect in useZoneLayout handles placing unassigned tabs
  // into empty zones after the layout change, so we don't need
  // requestAnimationFrame with stale closure values.
  const createAndAssignTerminal = useCallback(
    async (title?: string, workingDir?: string) => {
      incrementMetric("sessionsCreated");
      const tabId = await createTerminal(title, workingDir);
      if (!tabId) return tabId;

      const totalTabs = tabs.length + 1;
      const currentZoneCount = zoneLayout.layout.zones.length;

      // Check if there's a free zone in the current layout
      const hasEmptyZone = zoneLayout.layout.zones.some((_, idx) => !zoneLayout.assignments[idx]);

      // Upgrade layout if no room for the new tab
      if (totalTabs > currentZoneCount || (!hasEmptyZone && totalTabs > 1)) {
        let targetLayout: string;
        if (totalTabs >= 7) targetLayout = "full-grid";
        else if (totalTabs >= 5) targetLayout = "six-pack";
        else if (totalTabs >= 3) targetLayout = "quad";
        else targetLayout = "split";
        // Only upgrade, never downgrade
        if (targetLayout !== zoneLayout.layoutId) {
          zoneLayout.setLayoutId(targetLayout);
        }
      }

      // The auto-assign effect in useZoneLayout will place the new tab
      // into an empty zone when it detects an unassigned tab.
      return tabId;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular deps on specific properties
    [
      createTerminal,
      zoneLayout.layoutId,
      zoneLayout.setLayoutId,
      zoneLayout.layout,
      zoneLayout.assignments,
      tabs.length,
      incrementMetric,
    ],
  );

  // Sort zones by session state priority
  const handleSortZones = useCallback(() => {
    const STATE_PRIORITY: Record<SessionState, number> = {
      "needs-input": 0,
      error: 1,
      working: 2,
      idle: 3,
      completed: 4,
    };
    const entries = Object.entries(zoneLayout.assignments)
      .map(([z, tabId]) => ({
        zoneIndex: Number(z),
        tabId,
        priority: STATE_PRIORITY[stateTracking.sessionStates[tabId] ?? "idle"],
      }))
      .sort((a, b) => a.priority - b.priority);

    const sortedTabIds = entries.map((e) => e.tabId);
    for (let i = 0; i < sortedTabIds.length; i++) {
      zoneLayout.assignTabToZone(i, sortedTabIds[i]);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular deps on specific properties
  }, [zoneLayout.assignments, zoneLayout.assignTabToZone, stateTracking.sessionStates]);

  // Export all session output to a text file
  const handleExportOutput = useCallback(async () => {
    const timestamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const filePath = await save({
      defaultPath: `session-output-${timestamp}.txt`,
      filters: [{ name: "Text Files", extensions: ["txt"] }],
    });
    if (!filePath) return;

    const lines: string[] = [];
    lines.push(`Session Output Export — ${new Date().toLocaleString()}`);
    lines.push(`Layout: ${zoneLayout.layoutId}, Tabs: ${tabs.length}`);
    lines.push("=".repeat(60));

    for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) continue;
      const state = stateTracking.sessionStates[tabId] ?? "idle";
      const output = stateTracking.lastOutputLines[tabId] ?? [];
      lines.push("");
      lines.push(`--- Zone ${Number(zoneStr) + 1}: ${tab.title} [${state}] ---`);
      if (tab.workingDir) lines.push(`    Dir: ${tab.workingDir}`);
      if (output.length > 0) {
        lines.push(...output);
      } else {
        lines.push("    (no output)");
      }
    }

    const assignedTabIds = new Set(Object.values(zoneLayout.assignments));
    const unassigned = tabs.filter((t) => !assignedTabIds.has(t.id));
    if (unassigned.length > 0) {
      lines.push("");
      lines.push("--- Unassigned Sessions ---");
      for (const tab of unassigned) {
        const state = stateTracking.sessionStates[tab.id] ?? "idle";
        const output = stateTracking.lastOutputLines[tab.id] ?? [];
        lines.push(`  ${tab.title} [${state}]`);
        if (output.length > 0) lines.push(...output.map((l) => `    ${l}`));
      }
    }

    try {
      await writeTextFile(filePath, lines.join("\n"));
      workflowGen.setNotification({ message: `Exported to ${filePath}`, type: "success" });
    } catch (err) {
      workflowGen.setNotification({
        message: `Export failed: ${err instanceof Error ? err.message : String(err)}`,
        type: "error",
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- granular deps on specific properties
  }, [
    tabs,
    zoneLayout.layoutId,
    zoneLayout.assignments,
    stateTracking.sessionStates,
    stateTracking.lastOutputLines,
    workflowGen.setNotification,
  ]);

  // Export a single zone's output in the chosen format
  const handleExportZone = useCallback(
    async (zoneIndex: number, format: "text" | "markdown" | "json") => {
      const tabId = zoneLayout.assignments[zoneIndex];
      if (!tabId) return;
      const tab = tabs.find((t) => t.id === tabId);
      const lines = stateTracking.lastOutputLines[tabId] ?? [];
      const title = tab?.title ?? `Zone ${zoneIndex + 1}`;
      const state = stateTracking.sessionStates[tabId] ?? "idle";
      const label = labelsAndTags.zoneLabels[zoneIndex] ?? "";

      let content: string;
      const ext = format === "json" ? "json" : format === "markdown" ? "md" : "txt";

      if (format === "markdown") {
        content = [
          `# ${title}`,
          `- **Zone:** ${zoneIndex + 1}`,
          `- **State:** ${state}`,
          label ? `- **Tags:** ${label}` : "",
          `- **Lines:** ${lines.length}`,
          `- **Exported:** ${new Date().toISOString()}`,
          "",
          "```",
          ...lines,
          "```",
        ]
          .filter(Boolean)
          .join("\n");
      } else if (format === "json") {
        content = JSON.stringify(
          {
            zone: zoneIndex + 1,
            title,
            state,
            tags: label ? label.split(",").map((t) => t.trim()) : [],
            exportedAt: new Date().toISOString(),
            lineCount: lines.length,
            output: lines,
          },
          null,
          2,
        );
      } else {
        content = lines.join("\n");
      }

      try {
        const filePath = await save({
          defaultPath: `zone-${zoneIndex + 1}-output.${ext}`,
          filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
        });
        if (filePath) {
          await writeTextFile(filePath, content);
        }
      } catch (err) {
        console.error("Export failed:", err);
      }
    },
    [
      tabs,
      stateTracking.lastOutputLines,
      stateTracking.sessionStates,
      labelsAndTags.zoneLabels,
      zoneLayout.assignments,
    ],
  );

  // Helper: approve a single tab
  const approveTab = useCallback(
    (tabId: string) => {
      terminalRefs.current.get(tabId)?.current?.writeToTerminal("y\r");
      incrementMetric("totalApprovals");
    },
    [incrementMetric],
  );

  // Helper: reject a single tab
  const rejectTab = useCallback(
    (tabId: string) => {
      terminalRefs.current.get(tabId)?.current?.writeToTerminal("n\r");
      incrementMetric("totalRejections");
    },
    [incrementMetric],
  );

  // ══════════════════════════════════════════════════════════════════════════════
  // 8. Keyboard shortcuts
  // ══════════════════════════════════════════════════════════════════════════════

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Ctrl+Shift+T — new terminal
      if (e.ctrlKey && e.shiftKey && e.key === "T") {
        e.preventDefault();
        createAndAssignTerminal();
        return;
      }
      // Ctrl+Shift+W — close focused terminal
      if (e.ctrlKey && e.shiftKey && e.key === "W") {
        e.preventDefault();
        if (activeId) closeTerminal(activeId);
        return;
      }
      // Ctrl+Tab / Ctrl+Shift+Tab — cycle zones or tabs
      if (e.ctrlKey && e.key === "Tab") {
        e.preventDefault();
        if (zoneLayout.isMultiZone) {
          if (e.shiftKey) {
            zoneLayout.focusPrevZone();
          } else {
            zoneLayout.focusNextZone();
          }
        } else if (tabs.length > 1 && activeId) {
          const idx = tabs.findIndex((t) => t.id === activeId);
          const next = e.shiftKey ? (idx - 1 + tabs.length) % tabs.length : (idx + 1) % tabs.length;
          setActiveId(tabs[next].id);
        }
        return;
      }
      // Ctrl+Shift+N — jump to next session needing input
      if (e.ctrlKey && e.shiftKey && e.key === "N") {
        e.preventDefault();
        zoneLayout.focusNextNeedsInput(stateTracking.sessionStates);
        return;
      }
      // Ctrl+Shift+F — maximize/restore focused zone
      if (e.ctrlKey && e.shiftKey && e.key === "F") {
        e.preventDefault();
        zoneLayout.toggleMaximize(zoneLayout.focusedZone);
        return;
      }
      // Ctrl+Shift+M — cycle view mode
      if (e.ctrlKey && e.shiftKey && e.key === "M") {
        e.preventDefault();
        setViewMode((prev) => {
          if (prev === "auto") return "full";
          if (prev === "full") return "compact";
          return "auto";
        });
        return;
      }
      // Ctrl+Shift+A — toggle auto-focus on needs-input
      if (e.ctrlKey && e.shiftKey && e.key === "A") {
        e.preventDefault();
        transitionEffects.toggleAutoFocus();
        return;
      }
      // Ctrl+Shift+S — toggle sound notification
      if (e.ctrlKey && e.shiftKey && e.key === "S") {
        e.preventDefault();
        transitionEffects.toggleSound();
        return;
      }
      // Ctrl+Shift+Enter — approve all waiting sessions
      if (e.ctrlKey && e.shiftKey && e.key === "Enter") {
        e.preventDefault();
        const waiting = tabs.filter((t) => stateTracking.sessionStates[t.id] === "needs-input");
        incrementMetric("totalApprovals", waiting.length);
        addHistoryEvent("Approve all", `${waiting.length} sessions`, undefined, "#9ece6a");
        for (const tab of waiting) {
          terminalRefs.current.get(tab.id)?.current?.writeToTerminal("y\r");
        }
        return;
      }
      // Ctrl+Shift+[1-8] — quick layout switch
      if (e.ctrlKey && e.shiftKey && e.key >= "1" && e.key <= "8") {
        e.preventDefault();
        const num = parseInt(e.key, 10);
        const preset = LAYOUT_PRESETS.find((l) => l.shortcutKey === num);
        if (preset) {
          zoneLayout.setLayoutId(preset.id);
        }
        return;
      }
      // Ctrl+[1-9] — focus zone by number
      if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key >= "1" && e.key <= "9") {
        if (zoneLayout.isMultiZone) {
          const zoneIdx = parseInt(e.key, 10) - 1;
          if (zoneIdx < zoneLayout.layout.zones.length) {
            e.preventDefault();
            zoneLayout.setFocusedZone(zoneIdx);
          }
        }
        return;
      }
      // Ctrl+Shift+X — zone swap
      if (e.ctrlKey && e.shiftKey && e.key === "X") {
        e.preventDefault();
        if (swapSource === null) {
          setSwapSource(zoneLayout.focusedZone);
        } else if (swapSource !== zoneLayout.focusedZone) {
          const srcTabId = zoneLayout.assignments[swapSource];
          const dstTabId = zoneLayout.assignments[zoneLayout.focusedZone];
          if (srcTabId) zoneLayout.assignTabToZone(zoneLayout.focusedZone, srcTabId);
          if (dstTabId) zoneLayout.assignTabToZone(swapSource, dstTabId);
          setSwapSource(null);
        } else {
          setSwapSource(null);
        }
        return;
      }
      // Ctrl+Shift+/ — toggle output search
      if (e.ctrlKey && e.shiftKey && e.key === "/") {
        e.preventDefault();
        setShowOutputSearch((prev) => {
          if (prev) setOutputSearch("");
          return !prev;
        });
        return;
      }
      // Ctrl+Shift+O — toggle pin on focused zone
      if (e.ctrlKey && e.shiftKey && e.key === "O") {
        e.preventDefault();
        labelsAndTags.togglePin(zoneLayout.focusedZone);
        return;
      }
      // Ctrl+Shift+D — toggle focus mode
      if (e.ctrlKey && e.shiftKey && e.key === "D") {
        e.preventDefault();
        setFocusMode((prev) => {
          const next = !prev;
          localStorage.setItem("zone-focus-mode", String(next));
          return next;
        });
        return;
      }
      // Ctrl+Shift+R — restart focused zone
      if (e.ctrlKey && e.shiftKey && e.key === "R") {
        e.preventDefault();
        handleRestartInZone(zoneLayout.focusedZone);
        return;
      }
      // Ctrl+Shift+L — cycle through layout presets
      if (e.ctrlKey && e.shiftKey && e.key === "L") {
        e.preventDefault();
        const currentIdx = LAYOUT_PRESETS.findIndex((l) => l.id === zoneLayout.layoutId);
        const nextIdx = (currentIdx + 1) % LAYOUT_PRESETS.length;
        zoneLayout.setLayoutId(LAYOUT_PRESETS[nextIdx].id);
        return;
      }
      // Ctrl+Shift+K — toggle command palette
      if (e.ctrlKey && e.shiftKey && e.key === "K") {
        e.preventDefault();
        setShowCommandPalette((prev) => !prev);
        return;
      }
      // Ctrl+Shift+I — toggle zone timeline
      if (e.ctrlKey && e.shiftKey && e.key === "I") {
        e.preventDefault();
        setShowTimeline((prev) => !prev);
        return;
      }
      // Ctrl+Shift+P — toggle control panel
      if (e.ctrlKey && e.shiftKey && e.key === "P") {
        e.preventDefault();
        setShowControlPanel((prev) => {
          const next = !prev;
          localStorage.setItem("zone-control-panel", String(next));
          return next;
        });
        return;
      }
      // Ctrl+Shift+G — cycle through tag filters
      if (e.ctrlKey && e.shiftKey && e.key === "G") {
        e.preventDefault();
        if (labelsAndTags.allTags.length === 0) return;
        labelsAndTags.setActiveTagFilters((prev) => {
          const currentTag = prev.size === 1 ? [...prev][0] : null;
          const currentIdx = currentTag ? labelsAndTags.allTags.indexOf(currentTag) : -1;
          const nextIdx = currentIdx + 1;
          if (nextIdx >= labelsAndTags.allTags.length) {
            return new Set();
          }
          return new Set([labelsAndTags.allTags[nextIdx]]);
        });
        return;
      }
      // Ctrl+Shift+? — toggle keyboard shortcuts overlay
      if (e.ctrlKey && e.shiftKey && e.key === "?") {
        e.preventDefault();
        setShowShortcutsOverlay((prev) => !prev);
        return;
      }
      // Ctrl+Shift+Left — go back in focus history
      if (e.ctrlKey && e.shiftKey && e.key === "ArrowLeft") {
        e.preventDefault();
        focusHistory.goBack();
        return;
      }
      // Ctrl+Shift+Right — go forward in focus history
      if (e.ctrlKey && e.shiftKey && e.key === "ArrowRight") {
        e.preventDefault();
        focusHistory.goForward();
        return;
      }
      // Escape — cancel swap, clear selection, restore maximized zone, or close right panel
      if (e.key === "Escape") {
        if (swapSource !== null) {
          setSwapSource(null);
        } else if (selectedZones.size > 0) {
          setSelectedZones(new Set());
        } else if (zoneLayout.maximizedZone !== null) {
          zoneLayout.setMaximizedZone(null);
        } else if (workflowGen.rightPanelMode) {
          workflowGen.setRightPanelMode(null);
          workflowGen.setSelectedTranscriptSessionId(null);
        }
        return;
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeId,
    tabs,
    createAndAssignTerminal,
    closeTerminal,
    setActiveId,
    workflowGen.rightPanelMode,
    zoneLayout,
    stateTracking.sessionStates,
    swapSource,
    selectedZones,
    handleRestartInZone,
    labelsAndTags,
    focusHistory,
    transitionEffects,
    incrementMetric,
    addHistoryEvent,
  ]);

  // ══════════════════════════════════════════════════════════════════════════════
  // 9. JSX Rendering
  // ══════════════════════════════════════════════════════════════════════════════

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
                  onClick={() =>
                    setViewMode((prev) =>
                      prev === "auto" ? "full" : prev === "full" ? "compact" : "auto",
                    )
                  }
                  className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors"
                  title={`View mode: ${viewMode} (Ctrl+Shift+M to cycle)`}
                >
                  <span className="font-mono uppercase tracking-wider">{viewMode}</span>
                  <span className="text-[#565f89]/50">{zoneLayout.layout.zones.length}z</span>
                </button>
                <button
                  onClick={() => setResetRatiosKey((k) => k + 1)}
                  className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors"
                  title="Reset zone sizes to equal"
                >
                  Reset
                </button>
              </>
            )}
            <button
              onClick={() => {
                setAutoLayout((prev) => {
                  const next = !prev;
                  localStorage.setItem("zone-auto-layout", String(next));
                  return next;
                });
              }}
              className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
                autoLayout
                  ? "text-[#9ece6a] bg-[#9ece6a]/10"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
              }`}
              title={`Auto-layout: ${autoLayout ? "ON" : "OFF"} — automatically switch layout based on terminal count`}
            >
              Auto
            </button>
            <ZoneProfilePicker
              currentLayoutId={zoneLayout.layoutId}
              zoneLabels={labelsAndTags.zoneLabels}
              zoneNotes={labelsAndTags.zoneNotes}
              pinnedZones={labelsAndTags.pinnedZones}
              autoApprovePatterns={transitionEffects.autoApprovePatterns}
              onLoadProfile={(profile) => {
                zoneLayout.setLayoutId(profile.layoutId);
                labelsAndTags.setZoneLabels(profile.labels);
                labelsAndTags.setZoneNotes(profile.notes);
                labelsAndTags.setPinnedZones(new Set(profile.pins));
                transitionEffects.setAutoApprovePatterns(profile.autoApprovePatterns);
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
          const createdTabIds: string[] = [];
          for (let i = 0; i < count; i++) {
            const tabId = await createAndAssignTerminal();
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
      />
      <ZoneStatusBar
        tabs={tabs}
        assignments={zoneLayout.assignments}
        sessionStates={stateTracking.sessionStates}
        onJumpToNeedsInput={() => zoneLayout.focusNextNeedsInput(stateTracking.sessionStates)}
        onShowShortcuts={() => setShowShortcutsOverlay(true)}
        autoFocus={transitionEffects.autoFocusNeedsInput}
        onToggleAutoFocus={transitionEffects.toggleAutoFocus}
        soundEnabled={transitionEffects.soundEnabled}
        onToggleSound={transitionEffects.toggleSound}
        desktopNotify={transitionEffects.desktopNotify}
        onToggleDesktopNotify={() => {
          transitionEffects.setDesktopNotify((prev) => {
            const next = !prev;
            localStorage.setItem("zone-desktop-notify", String(next));
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
          setSelectedZones(zones);
        }}
        metrics={metrics.current}
        zoneLabels={labelsAndTags.zoneLabels}
        onExport={handleExportOutput}
        onSortZones={handleSortZones}
        eventHistory={eventHistory}
        labelColorMap={labelsAndTags.labelColorMap}
        focusMode={focusMode}
        autoApprovePatterns={transitionEffects.autoApprovePatterns}
        onSetAutoApprovePatterns={transitionEffects.setAutoApprovePatterns}
        autoApproveCount={transitionEffects.autoApproveCount}
        stateTimeAccum={stateTracking.stateTimeAccum.current}
        autoRestart={transitionEffects.autoRestart}
        onToggleAutoRestart={() => {
          transitionEffects.setAutoRestart((prev) => {
            const next = !prev;
            localStorage.setItem("zone-auto-restart", String(next));
            return next;
          });
        }}
        autoRestartCount={transitionEffects.autoRestartCount}
        onToggleFocusMode={() => {
          setFocusMode((prev) => {
            const next = !prev;
            localStorage.setItem("zone-focus-mode", String(next));
            return next;
          });
        }}
        activeTagFilters={labelsAndTags.activeTagFilters}
        onSetActiveTagFilters={labelsAndTags.setActiveTagFilters}
        allTags={labelsAndTags.allTags}
        lastOutputLines={stateTracking.lastOutputLines}
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
        onToggleFindings={findingsActions.handleToggleFindings}
        findingsActive={workflowGen.rightPanelMode === "findings"}
        findingsCount={activeFindings.length}
        onOpenPlanFile={handleOpenPlanFile}
      />
      <TerminalNotification
        message={workflowGen.notification?.message ?? null}
        type={workflowGen.notification?.type ?? "success"}
        onDismiss={() => workflowGen.setNotification(null)}
      />

      {/* Zone timeline (multi-zone only) */}
      {showTimeline && zoneLayout.isMultiZone && (
        <ZoneTimeline
          tabs={tabs}
          assignments={zoneLayout.assignments}
          sessionStates={stateTracking.sessionStates}
          eventHistory={eventHistory}
          onClose={() => setShowTimeline(false)}
        />
      )}

      {/* Output search bar */}
      {showOutputSearch && (
        <div className="flex items-center gap-2 px-3 h-8 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
          <span className="text-[10px] text-[#565f89] shrink-0">Search:</span>
          <input
            autoFocus
            value={outputSearch}
            onChange={(e) => setOutputSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setShowOutputSearch(false);
                setOutputSearch("");
              }
              e.stopPropagation();
            }}
            placeholder="Search across all session output..."
            className="flex-1 bg-[#1a1b26] border border-[#2a2d3d] rounded px-2 py-0.5 text-xs text-[#c0caf5] placeholder-[#565f89] outline-none focus:border-[#7aa2f7] transition-colors"
          />
          {outputSearch &&
            (() => {
              const query = outputSearch.toLowerCase();
              const matchCount = Object.entries(stateTracking.lastOutputLines).filter(([, lines]) =>
                lines.some((l) => l.toLowerCase().includes(query)),
              ).length;
              return (
                <span
                  className={`text-[10px] shrink-0 ${matchCount > 0 ? "text-[#9ece6a]" : "text-[#565f89]"}`}
                >
                  {matchCount} match{matchCount !== 1 ? "es" : ""}
                </span>
              );
            })()}
          <button
            onClick={() => {
              setShowOutputSearch(false);
              setOutputSearch("");
            }}
            className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors shrink-0"
          >
            <span className="text-xs">✕</span>
          </button>
        </div>
      )}

      {/* Main content: optional sidebar + zone grid + optional right panel */}
      <div className="flex-1 flex flex-row overflow-hidden">
        {/* Left sidebar */}
        {workflowGen.showSidebar && (
          <TranscriptSessionSidebar
            sessions={transcriptSessions}
            loading={sessionsLoading}
            selectedSessionId={workflowGen.selectedTranscriptSessionId}
            onSelectSession={workflowGen.handleSelectTranscriptSession}
            onRefresh={refreshSessions}
            onResume={shellIntegration.handleResumeSession}
          />
        )}

        {/* Terminal zone grid */}
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
              viewMode={viewMode}
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
              selectedZones={selectedZones}
              staleTabs={stateTracking.staleTabs}
              pinnedZones={labelsAndTags.pinnedZones}
              onTogglePin={labelsAndTags.togglePin}
              outputSearchQuery={outputSearch || undefined}
              swapSource={swapSource}
              activityData={stateTracking.activityData}
              zoneLabels={labelsAndTags.zoneLabels}
              onSetZoneLabel={labelsAndTags.setZoneLabel}
              onRestartInZone={handleRestartInZone}
              resetRatiosKey={resetRatiosKey}
              labelColorMap={labelsAndTags.labelColorMap}
              zoneTags={labelsAndTags.zoneTags}
              commandHistories={shellIntegration.commandHistories}
              focusMode={focusMode}
              zoneNotes={labelsAndTags.zoneNotes}
              onSetZoneNote={labelsAndTags.setZoneNote}
              onExportZone={handleExportZone}
              pendingRestarts={transitionEffects.pendingRestarts}
              onCancelRestart={transitionEffects.cancelPendingRestart}
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

          {/* Zone minimap for large grids */}
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

          {/* Batch operations floating bar */}
          {zoneLayout.isMultiZone && !transitionEffects.batchBarDismissed && (
            <BatchOperationsBar
              tabs={tabs}
              sessionStates={stateTracking.sessionStates}
              terminalRefs={terminalRefs.current}
              onDismiss={() => transitionEffects.setBatchBarDismissed(true)}
              selectedZones={selectedZones}
              assignments={zoneLayout.assignments}
              zoneLabels={labelsAndTags.zoneLabels}
              onSelectAllWaiting={() => {
                const waiting = new Set<number>();
                for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
                  if (stateTracking.sessionStates[tabId] === "needs-input") {
                    waiting.add(Number(zoneStr));
                  }
                }
                setSelectedZones(waiting);
              }}
              onClearSelection={() => setSelectedZones(new Set())}
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

        {/* Zone control panel */}
        {showControlPanel && zoneLayout.isMultiZone && (
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
            onClose={() => {
              setShowControlPanel(false);
              localStorage.setItem("zone-control-panel", "false");
            }}
            collapsed={controlPanelCollapsed}
            onToggleCollapsed={() => setControlPanelCollapsed((v) => !v)}
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
          />
        )}

        {/* Right panel — transcript content OR workflow preview */}
        {workflowGen.rightPanelMode === "transcript" && workflowGen.selectedTranscriptSessionId && (
          <TranscriptContentPanel
            sessionId={workflowGen.selectedTranscriptSessionId}
            session={
              transcriptSessions.find(
                (s) => s.session_id === workflowGen.selectedTranscriptSessionId,
              ) ?? null
            }
            messages={workflowGen.transcriptMessages}
            loading={workflowGen.loadingMessages}
            onGenerate={workflowGen.handleGenerateFromTranscript}
            onGenerateAndRun={workflowGen.handleGenerateAndRunFromTranscript}
            onBuildPlanWorkflow={workflowGen.handleBuildPlanWorkflow}
            onResume={shellIntegration.handleResumeSession}
            onClose={() => {
              workflowGen.setRightPanelMode(null);
              workflowGen.setSelectedTranscriptSessionId(null);
            }}
          />
        )}
        {workflowGen.rightPanelMode === "workflow" && (
          <div className="w-[420px] h-full shrink-0">
            <WorkflowPreviewPanel
              workflow={workflowGen.generatedWorkflow}
              isLoading={workflowGen.isGenerating}
              error={workflowGen.workflowError}
              onExecute={workflowGen.handleExecute}
              onEditInBuilder={workflowGen.handleEditInBuilder}
              onRegenerate={workflowGen.handleRegenerate}
              onSave={workflowGen.handleSaveWorkflow}
              onClose={() => workflowGen.setRightPanelMode(null)}
            />
          </div>
        )}
        {workflowGen.rightPanelMode === "analysis" && (
          <TerminalAnalysisPanel
            analysisType={analysis.analysisType}
            panels={analysis.analysisPanels}
            isAnalyzing={analysis.isAnalyzing}
            error={analysis.analysisError}
            onClose={() => workflowGen.setRightPanelMode(null)}
          />
        )}
        {workflowGen.rightPanelMode === "findings" && (
          <TerminalFindingsPanel
            findings={activeFindings}
            allFindings={allFindings}
            onClose={() => workflowGen.setRightPanelMode(null)}
            onRespond={findingsActions.handleFindingRespond}
            onFix={findingsActions.handleFixFinding}
            onGenerateWorkflow={findingsActions.handleGenerateFromFindings}
          />
        )}
      </div>

      {showShortcutsOverlay && (
        <KeyboardShortcutsOverlay onClose={() => setShowShortcutsOverlay(false)} />
      )}

      {snapshots.diffZones &&
        (() => {
          const [z1, z2] = snapshots.diffZones;
          const tab1 = tabs.find((t) => t.id === zoneLayout.assignments[z1]);
          const tab2 = tabs.find((t) => t.id === zoneLayout.assignments[z2]);
          return (
            <ZoneDiffOverlay
              leftLabel={`Zone ${z1 + 1}: ${tab1?.title ?? "empty"}`}
              rightLabel={`Zone ${z2 + 1}: ${tab2?.title ?? "empty"}`}
              leftLines={tab1 ? (stateTracking.lastOutputLines[tab1.id] ?? []) : []}
              rightLines={tab2 ? (stateTracking.lastOutputLines[tab2.id] ?? []) : []}
              onClose={() => snapshots.setDiffZones(null)}
            />
          );
        })()}

      {snapshots.snapshotDiff && (
        <ZoneDiffOverlay
          leftLabel="Snapshot"
          rightLabel="Current"
          leftLines={snapshots.snapshotDiff.snapshot}
          rightLines={snapshots.snapshotDiff.current}
          onClose={snapshots.clearSnapshotDiff}
        />
      )}

      {showCommandPalette && (
        <CommandPalette
          onClose={() => setShowCommandPalette(false)}
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
          onToggleFocusMode={() => {
            setFocusMode((prev) => {
              const next = !prev;
              localStorage.setItem("zone-focus-mode", String(next));
              return next;
            });
          }}
          focusMode={focusMode}
          onToggleAutoFocus={transitionEffects.toggleAutoFocus}
          autoFocus={transitionEffects.autoFocusNeedsInput}
          onToggleSound={transitionEffects.toggleSound}
          soundEnabled={transitionEffects.soundEnabled}
          zoneLabels={labelsAndTags.zoneLabels}
          onSetZoneLabel={labelsAndTags.setZoneLabel}
          zoneCount={zoneLayout.layout.zones.length}
          onCompareZones={(z1, z2) => {
            setShowCommandPalette(false);
            snapshots.setDiffZones([z1, z2]);
          }}
          onSnapshotZone={snapshots.snapshotZone}
          onCompareSnapshot={(tabId) => {
            snapshots.compareSnapshot(tabId);
            setShowCommandPalette(false);
          }}
          snapshotZones={snapshots.snapshotZones}
        />
      )}
    </div>
  );
}
