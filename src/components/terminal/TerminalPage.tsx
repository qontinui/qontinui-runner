import { useEffect, useCallback, useRef, useState, createRef, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { type TerminalInstanceHandle, type ShellIntegrationEvent } from "./TerminalInstance";
import { TerminalTabBar } from "./TerminalTabBar";
import { TerminalActionBar } from "./TerminalActionBar";
import { TerminalNotification } from "./TerminalNotification";
import { TranscriptSessionSidebar } from "./TranscriptSessionSidebar";
import { TranscriptContentPanel } from "./TranscriptContentPanel";
import {
  useTranscriptSessions,
  type TranscriptMessage,
  type TranscriptSession,
} from "./useTranscriptSessions";
import { TerminalAnalysisPanel, type AnalysisType } from "./TerminalAnalysisPanel";
import { TerminalFindingsPanel } from "./TerminalFindingsPanel";
import { useTerminalManager } from "./useTerminalManager";
import { useTerminalFindings } from "./useTerminalFindings";
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
import { findingsTracker } from "@/services/FindingsTracker";
import type { Finding } from "@/types/findings";
import { WorkflowPreviewPanel } from "@qontinui/workflow-ui";
import type { UnifiedWorkflow, CanvasPanel } from "@qontinui/shared-types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { parsePlanMarkdown, summarizeParsedPlan } from "@/lib/workflow-builder/parsePlanMarkdown";
import { buildPlanWorkflow } from "@/lib/workflow-builder/buildPlanWorkflow";
import { detectSessionState } from "./sessionStateDetector";
import { playNeedsInputChime, playCompletionChime, playErrorAlert } from "./notificationSound";
import { save } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";

interface CommandResponse {
  success: boolean;
  message: string | null;
  data: unknown;
}

interface GenerateWorkflowResponse {
  success: boolean;
  error?: string;
  workflow?: UnifiedWorkflow;
}

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
  } = useTerminalManager();

  // ── Zone layout ─────────────────────────────────────────────────────────────
  const tabIds = tabs.map((t) => t.id);
  const zoneLayout = useZoneLayout(tabIds);

  // Sync zone focus → activeId so existing handlers (analysis, findings) work
  useEffect(() => {
    if (zoneLayout.focusedTabId && zoneLayout.focusedTabId !== activeId) {
      setActiveId(zoneLayout.focusedTabId);
    }
  }, [zoneLayout.focusedTabId, activeId, setActiveId]);

  // Report session count changes to parent for sidebar auto-collapse
  useEffect(() => {
    onSessionCountChange?.(tabs.length);
  }, [tabs.length, onSessionCountChange]);

  // ── Zone focus history (back/forward navigation) ───────────────────────────
  const focusHistoryRef = useRef<number[]>([]);
  const focusHistoryIndexRef = useRef(-1);
  const isNavigatingHistoryRef = useRef(false);

  useEffect(() => {
    if (isNavigatingHistoryRef.current) {
      isNavigatingHistoryRef.current = false;
      return;
    }
    const history = focusHistoryRef.current;
    // Don't push if same as current position in history
    if (history[focusHistoryIndexRef.current] === zoneLayout.focusedZone) return;

    // Truncate forward history
    focusHistoryRef.current = history.slice(0, focusHistoryIndexRef.current + 1);
    focusHistoryRef.current.push(zoneLayout.focusedZone);

    // Cap at 20 entries
    if (focusHistoryRef.current.length > 20) {
      focusHistoryRef.current = focusHistoryRef.current.slice(-20);
    }
    focusHistoryIndexRef.current = focusHistoryRef.current.length - 1;
  }, [zoneLayout.focusedZone]);

  // Session state tracking (for status borders)
  const [sessionStates, setSessionStates] = useState<Record<string, SessionState>>({});
  const lastOutputTimeRef = useRef<Record<string, number>>({});

  // Last output lines per tab (for compact view)
  const [lastOutputLines, setLastOutputLines] = useState<Record<string, string[]>>({});

  // Track last-seen line count per tab for unread indicators
  const lastSeenLineCountRef = useRef<Record<string, number>>({});

  // Update last-seen line count when zone gains focus (clears unread indicator)
  useEffect(() => {
    const tabId = zoneLayout.assignments[zoneLayout.focusedZone];
    if (tabId) {
      lastSeenLineCountRef.current[tabId] = (lastOutputLines[tabId] ?? []).length;
    }
  }, [zoneLayout.focusedZone, zoneLayout.assignments, lastOutputLines]);

  // Compute which zones have unread output
  const unreadZones = useMemo(() => {
    const unread = new Set<string>();
    for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
      const currentCount = (lastOutputLines[tabId] ?? []).length;
      const lastSeen = lastSeenLineCountRef.current[tabId] ?? 0;
      if (currentCount > lastSeen && Number(zoneStr) !== zoneLayout.focusedZone) {
        unread.add(tabId);
      }
    }
    return unread;
  }, [lastOutputLines, zoneLayout.assignments, zoneLayout.focusedZone]);

  // Zone view mode: "auto" (focused=full, others=compact when 4+ zones), "full", "compact"
  const [viewMode, setViewMode] = useState<ViewMode>("auto");

  // Status bar collapsed state
  const [statusBarCollapsed, setStatusBarCollapsed] = useState(false);

  // Batch operations bar dismissed state (resets when needs-input count changes)
  const [batchBarDismissed, setBatchBarDismissed] = useState(false);
  const [showShortcutsOverlay, setShowShortcutsOverlay] = useState(false);
  const [showCommandPalette, setShowCommandPalette] = useState(false);
  const [showTimeline, setShowTimeline] = useState(false);
  const [showControlPanel, setShowControlPanel] = useState(
    () => localStorage.getItem("zone-control-panel") === "true",
  );
  const [controlPanelCollapsed, setControlPanelCollapsed] = useState(false);
  const [diffZones, setDiffZones] = useState<[number, number] | null>(null);
  const outputSnapshotsRef = useRef<Record<string, string[]>>({});
  const [snapshotDiff, setSnapshotDiff] = useState<{
    tabId: string;
    snapshot: string[];
    current: string[];
  } | null>(null);
  const [snapshotCounter, setSnapshotCounter] = useState(0);
  const [flashingTabs, setFlashingTabs] = useState<Set<string>>(new Set());
  const stateEntryTimeRef = useRef<Record<string, number>>({});
  const [stateDurations, setStateDurations] = useState<Record<string, string>>({});
  const [selectedZones, setSelectedZones] = useState<Set<number>>(new Set());
  const [staleTabs, setStaleTabs] = useState<Set<string>>(new Set());
  const [pinnedZones, setPinnedZones] = useState<Set<number>>(() => {
    try {
      const stored = localStorage.getItem(`zone-pinned-${zoneLayout.layoutId}`);
      if (stored) return new Set(JSON.parse(stored) as number[]);
    } catch {
      // intentionally empty
    }
    return new Set();
  });
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
  const [autoRestart, setAutoRestart] = useState(
    () => localStorage.getItem("zone-auto-restart") === "true",
  );
  const autoRestartCountRef = useRef(0);
  const handleRestartInZoneRef = useRef<(zoneIdx: number) => void>(() => {});
  const [pendingRestarts, setPendingRestarts] = useState<Record<number, number>>({});
  const pendingRestartTimersRef = useRef<Record<number, ReturnType<typeof setTimeout>>>({});
  const metricsRef = useRef({
    totalApprovals: 0,
    totalRejections: 0,
    totalBroadcasts: 0,
    sessionsCreated: 0,
  });
  const [zoneLabels, setZoneLabels] = useState<Record<number, string>>(() => {
    try {
      const stored = localStorage.getItem(`zone-labels-${zoneLayout.layoutId}`);
      if (stored) return JSON.parse(stored) as Record<number, string>;
    } catch {
      // intentionally empty
    }
    return {};
  });
  const [activeTagFilters, setActiveTagFilters] = useState<Set<string>>(new Set());
  const [unseenNeedsInput, setUnseenNeedsInput] = useState<Set<string>>(new Set());
  const [zoneNotes, setZoneNotes] = useState<Record<number, string>>(() => {
    try {
      const stored = localStorage.getItem(`zone-notes-${zoneLayout.layoutId}`);
      if (stored) return JSON.parse(stored) as Record<number, string>;
    } catch {
      // intentionally empty
    }
    return {};
  });

  // Auto-approve rules: regex patterns that auto-send "y\r" when needs-input output matches
  const [autoApprovePatterns, setAutoApprovePatterns] = useState<string[]>(() => {
    try {
      const stored = localStorage.getItem("zone-auto-approve-patterns");
      if (stored) return JSON.parse(stored) as string[];
    } catch {
      // intentionally empty
    }
    return [];
  });
  const autoApproveCountRef = useRef(0);

  // Notification history log
  type HistoryEntry = { time: number; type: string; session: string; zone?: number; color: string };
  const [eventHistory, setEventHistory] = useState<HistoryEntry[]>([]);
  const addHistoryEvent = useCallback(
    (type: string, session: string, zone?: number, color = "#a9b1d6") => {
      setEventHistory((prev) => [
        ...prev.slice(-99),
        { time: Date.now(), type, session, zone, color },
      ]);
    },
    [],
  );
  const cancelPendingRestart = useCallback((zoneIndex: number) => {
    const timer = pendingRestartTimersRef.current[zoneIndex];
    if (timer) {
      clearTimeout(timer);
      delete pendingRestartTimersRef.current[zoneIndex];
    }
    setPendingRestarts((prev) => {
      const next = { ...prev };
      delete next[zoneIndex];
      return next;
    });
  }, []);

  // Group label → color mapping (auto-assigned from preset palette)
  const labelColorMap = useMemo(() => {
    const GROUP_COLORS = ["#bb9af7", "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#7dcfff"];
    const map: Record<string, string> = {};
    const tagSet = new Set<string>();
    for (const label of Object.values(zoneLabels)) {
      if (!label) continue;
      for (const tag of label
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean)) {
        tagSet.add(tag);
      }
    }
    const uniqueTags = [...tagSet].sort();
    for (let i = 0; i < uniqueTags.length; i++) {
      map[uniqueTags[i]] = GROUP_COLORS[i % GROUP_COLORS.length];
    }
    return map;
  }, [zoneLabels]);

  // All unique tags across zones (sorted) — used by keyboard shortcut to cycle
  const allTags = useMemo(() => {
    const tagSet = new Set<string>();
    for (const label of Object.values(zoneLabels)) {
      if (!label) continue;
      for (const tag of label
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean)) {
        tagSet.add(tag);
      }
    }
    return [...tagSet].sort();
  }, [zoneLabels]);

  // Set of tab IDs that have output snapshots stored
  const snapshotZones = useMemo(
    () => new Set(Object.keys(outputSnapshotsRef.current)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [snapshotCounter],
  );

  // Activity sparkline: ring buffer of output byte counts per 2s interval per tab
  const activityBuffersRef = useRef<Record<string, number[]>>({});
  const [autoFocusNeedsInput, setAutoFocusNeedsInput] = useState(
    () => localStorage.getItem("zone-auto-focus") === "true",
  );
  const [soundEnabled, setSoundEnabled] = useState(
    () => localStorage.getItem("zone-sound-notify") === "true",
  );
  const [desktopNotify, setDesktopNotify] = useState(
    () => localStorage.getItem("zone-desktop-notify") === "true",
  );

  // Request notification permission when desktop notifications are enabled
  useEffect(() => {
    if (desktopNotify && "Notification" in window && Notification.permission === "default") {
      Notification.requestPermission();
    }
  }, [desktopNotify]);

  const prevNeedsInputCountRef = useRef(0);
  const prevSessionStatesRef = useRef<Record<string, SessionState>>({});

  // Cumulative time tracking per state (in ms)
  const stateTimeAccum = useRef<Record<SessionState, number>>({
    idle: 0,
    working: 0,
    "needs-input": 0,
    completed: 0,
    error: 0,
  });

  // Session persistence — save/restore sessions across app restarts
  const {
    saveSessionLayout,
    saveScrollbackBuffers,
    updateScrollbackPaths,
    getSavedLayout,
    clearSavedLayout,
    hasSavedLayout,
  } = useSessionPersistence();

  // Persist auto-approve patterns
  useEffect(() => {
    localStorage.setItem("zone-auto-approve-patterns", JSON.stringify(autoApprovePatterns));
  }, [autoApprovePatterns]);

  // Persist zone labels, notes, and pinned zones to localStorage
  useEffect(() => {
    localStorage.setItem(`zone-labels-${zoneLayout.layoutId}`, JSON.stringify(zoneLabels));
  }, [zoneLabels, zoneLayout.layoutId]);

  useEffect(() => {
    localStorage.setItem(`zone-notes-${zoneLayout.layoutId}`, JSON.stringify(zoneNotes));
  }, [zoneNotes, zoneLayout.layoutId]);

  useEffect(() => {
    localStorage.setItem(`zone-pinned-${zoneLayout.layoutId}`, JSON.stringify([...pinnedZones]));
  }, [pinnedZones, zoneLayout.layoutId]);

  // Auto-save session layout for persistence across app restarts
  useEffect(() => {
    if (tabs.length === 0) return;
    saveSessionLayout({
      layoutId: zoneLayout.layoutId,
      tabs,
      assignments: zoneLayout.assignments,
      zoneLabels,
      zoneNotes,
      pinnedZones,
      focusedZone: zoneLayout.focusedZone,
    });
  }, [
    tabs,
    zoneLayout.assignments,
    zoneLayout.layoutId,
    zoneLayout.focusedZone,
    zoneLabels,
    zoneNotes,
    pinnedZones,
    saveSessionLayout,
  ]);

  // Save scrollback buffers to disk when the window is about to close
  // so terminal history can be restored on next launch
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
          // Save scrollback for all active tabs
          const pathMap = await saveScrollbackBuffers(currentTabs);

          // Build a mapping from tabId -> session index in the saved layout
          const tabIdToSessionIndex: Record<string, number> = {};
          const currentAssignments = zoneLayoutRef.current.assignments;
          const assignedTabIds = new Set(Object.values(currentAssignments));
          let idx = 0;
          // Assigned sessions come first
          for (const [, tabId] of Object.entries(currentAssignments)) {
            if (currentTabs.some((t) => t.id === tabId)) {
              tabIdToSessionIndex[tabId] = idx++;
            }
          }
          // Then unassigned tabs
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

  // Reload labels/pins when layout changes, preserving pinned tabs across layouts
  const prevLayoutIdRef2 = useRef(zoneLayout.layoutId);
  const prevAssignmentsRef = useRef<Record<number, string>>(zoneLayout.assignments);
  useEffect(() => {
    if (prevLayoutIdRef2.current !== zoneLayout.layoutId) {
      // Capture which TAB IDs were pinned using the previous layout's assignments
      const pinnedTabIds = new Set<string>();
      for (const zoneIdx of pinnedZones) {
        const tabId = prevAssignmentsRef.current[zoneIdx];
        if (tabId) pinnedTabIds.add(tabId);
      }

      prevLayoutIdRef2.current = zoneLayout.layoutId;
      prevAssignmentsRef.current = zoneLayout.assignments;

      try {
        const storedLabels = localStorage.getItem(`zone-labels-${zoneLayout.layoutId}`);
        setZoneLabels(storedLabels ? JSON.parse(storedLabels) : {});
      } catch {
        setZoneLabels({});
      }

      // Restore pins: merge stored pins with migrated tab-based pins
      let newPins = new Set<number>();
      try {
        const storedPins = localStorage.getItem(`zone-pinned-${zoneLayout.layoutId}`);
        if (storedPins) newPins = new Set(JSON.parse(storedPins) as number[]);
      } catch {
        // intentionally empty
      }
      // Add pins for tabs that were pinned in the previous layout
      for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
        if (pinnedTabIds.has(tabId)) {
          newPins.add(Number(zoneStr));
        }
      }
      setPinnedZones(newPins);

      try {
        const storedNotes = localStorage.getItem(`zone-notes-${zoneLayout.layoutId}`);
        setZoneNotes(storedNotes ? JSON.parse(storedNotes) : {});
      } catch {
        setZoneNotes({});
      }
    } else {
      // Keep assignments ref in sync for non-layout-change updates (e.g., tab swaps)
      prevAssignmentsRef.current = zoneLayout.assignments;
    }
  }, [zoneLayout.layoutId, zoneLayout.assignments]); // eslint-disable-line react-hooks/exhaustive-deps

  // Shell integration: structured command history per tab
  const [commandHistories, setCommandHistories] = useState<
    Record<string, { command: string; exitCode: number; timestamp: number }[]>
  >({});
  const pendingCommandRef = useRef<Record<string, string>>({});

  // Diagnostic: detect unexpected unmount/remount cycles that destroy terminal state
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

  // Refs to terminal instances
  const terminalRefs = useRef<Map<string, React.RefObject<TerminalInstanceHandle | null>>>(
    new Map(),
  );

  // Generation state
  const [isGenerating, setIsGenerating] = useState(false);

  // Workflow preview panel state
  const [generatedWorkflow, setGeneratedWorkflow] = useState<UnifiedWorkflow | null>(null);
  const [workflowError, setWorkflowError] = useState<string | undefined>();

  // Notification state
  const [notification, setNotification] = useState<{
    message: string;
    type: "success" | "error";
  } | null>(null);

  // Last generation params for regeneration
  const lastGenerationParamsRef = useRef<{
    description: string;
    inlineContext: string;
  } | null>(null);

  // ── Sidebar + content panel state ──────────────────────────────────────────
  const [showSidebar, setShowSidebar] = useState(false);
  const [selectedTranscriptSessionId, setSelectedTranscriptSessionId] = useState<string | null>(
    null,
  );
  const [transcriptMessages, setTranscriptMessages] = useState<TranscriptMessage[]>([]);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [rightPanelMode, setRightPanelMode] = useState<
    "transcript" | "workflow" | "analysis" | "findings" | null
  >(null);

  // Session-scoped findings from terminal output
  const { processOutput, activeFindings, allFindings } = useTerminalFindings(activeId ?? null);

  // Analysis state
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [analysisType, setAnalysisType] = useState<AnalysisType>("session-summary");
  const [analysisPanels, setAnalysisPanels] = useState<CanvasPanel[] | null>(null);
  const [analysisError, setAnalysisError] = useState<string | undefined>();

  // Plan content state
  const [latestPlanContent, setLatestPlanContent] = useState("");
  const [planFileName, setPlanFileName] = useState<string | null>(null);
  const [isPlanLoading, setIsPlanLoading] = useState(false);

  const {
    sessions,
    loading: sessionsLoading,
    refresh: refreshSessions,
    loadMessages,
  } = useTranscriptSessions();

  // ── Plan content ───────────────────────────────────────────────────────────

  const loadPlanContent = useCallback(async () => {
    setIsPlanLoading(true);
    try {
      const result = await invoke<CommandResponse>("get_latest_plan_content");
      if (result.success && result.data) {
        const d = result.data as { found: boolean; filename?: string; content?: string };
        if (d.found && d.content && d.filename) {
          setLatestPlanContent(d.content);
          setPlanFileName(d.filename);
        } else {
          setLatestPlanContent("");
          setPlanFileName(null);
        }
      }
    } catch {
      // Silently ignore — plan content is best-effort
    } finally {
      setIsPlanLoading(false);
    }
  }, []);

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

  // On mount: try to reconnect to existing Rust PTY sessions,
  // then try to restore saved session layout, else create a fresh terminal
  const didInit = useRef(false);
  // Track restored sessions that need scrollback replay or Claude resume
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
        // No live PTY sessions — check for saved session layout
        const saved = hasSavedLayout() ? getSavedLayout() : null;
        if (saved && saved.sessions.length > 0) {
          // Restore layout preset
          if (saved.layoutId !== zoneLayout.layoutId) {
            const preset = LAYOUT_PRESETS.find((p) => p.id === saved.layoutId);
            if (preset) zoneLayout.setLayoutId(preset.id);
          }

          // Recreate terminals with saved configs
          const assignedSessions = saved.sessions.filter((s) => s.zoneIndex >= 0);
          const unassignedSessions = saved.sessions.filter((s) => s.zoneIndex < 0);

          for (const session of assignedSessions) {
            const tabId = await createTerminal(session.title, session.workingDir);
            if (tabId && session.zoneIndex >= 0) {
              zoneLayout.assignTabToZone(session.zoneIndex, tabId);
            }
            // Restore labels and notes
            if (session.label) {
              setZoneLabels((prev) => ({ ...prev, [session.zoneIndex]: session.label! }));
            }
            if (session.notes) {
              setZoneNotes((prev) => ({ ...prev, [session.zoneIndex]: session.notes! }));
            }
            if (session.pinned) {
              setPinnedZones((prev) => new Set([...prev, session.zoneIndex]));
            }
            // Track sessions needing scrollback restore or Claude resume
            if (tabId && (session.scrollbackPath || session.isClaudeSession)) {
              pendingRestoresRef.current.push({
                tabId,
                scrollbackPath: session.scrollbackPath,
                isClaudeSession: session.isClaudeSession,
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: session.claudeConfigDir,
              });
            }
            // Propagate Claude session info to tab state
            if (tabId && session.claudeSessionId) {
              updateTab(tabId, {
                claudeSessionId: session.claudeSessionId,
                claudeConfigDir: session.claudeConfigDir,
              });
            }
          }

          // Recreate unassigned sessions
          for (const session of unassignedSessions) {
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

          // Replay scrollback buffers and resume Claude sessions after a short delay
          // to allow TerminalInstance components to mount and obtain refs
          if (pendingRestoresRef.current.length > 0) {
            setTimeout(async () => {
              for (const restore of pendingRestoresRef.current) {
                const ref = terminalRefs.current.get(restore.tabId);
                const handle = ref?.current;

                // Replay saved scrollback buffer directly to the display
                // (writeToDisplay writes to xterm without sending to PTY stdin)
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

                // Resume Claude Code session by writing the resume command to PTY stdin
                if (restore.isClaudeSession && restore.claudeSessionId && handle) {
                  try {
                    const resumeCmd = `claude --resume ${restore.claudeSessionId}\r`;
                    // Small delay to let the shell initialize before sending the command
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

              // Clean up scrollback files from disk
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
    setInitialized,
    getSavedLayout,
    hasSavedLayout,
    clearSavedLayout,
    zoneLayout,
    setZoneLabels,
    setZoneNotes,
    setPinnedZones,
    updateTab,
  ]);

  // Load plan content once on mount (best-effort)
  useEffect(() => {
    loadPlanContent();
  }, [loadPlanContent]);

  // ── Session state detection ──────────────────────────────────────────────────

  // Periodically check for idle sessions (no output for 10s while at shell prompt)
  // and stale sessions (working but no output for 60s — may be stuck)
  useEffect(() => {
    const interval = setInterval(() => {
      const now = Date.now();
      setSessionStates((prev) => {
        const next = { ...prev };
        let changed = false;
        for (const tab of tabs) {
          const lastOutput = lastOutputTimeRef.current[tab.id] ?? 0;
          const current = next[tab.id] ?? "idle";
          if (!tab.isAlive && current !== "completed" && current !== "error") {
            next[tab.id] = tab.exitCode === 0 || tab.exitCode === null ? "completed" : "error";
            changed = true;
          } else if (current === "working" && now - lastOutput > 10000) {
            next[tab.id] = "idle";
            changed = true;
          }
        }
        return changed ? next : prev;
      });

      // Detect stale "working" sessions (no output for 60s)
      const newStale = new Set<string>();
      for (const tab of tabs) {
        const lastOutput = lastOutputTimeRef.current[tab.id] ?? 0;
        const state = sessionStates[tab.id] ?? "idle";
        if (state === "working" && lastOutput > 0 && now - lastOutput > 60000) {
          newStale.add(tab.id);
        }
      }
      setStaleTabs((prev) => {
        if (prev.size !== newStale.size || [...newStale].some((id) => !prev.has(id))) {
          return newStale;
        }
        return prev;
      });
    }, 2000);
    return () => clearInterval(interval);
  }, [tabs, sessionStates]);

  // Detect transitions to needs-input and trigger flash animation + auto-focus
  useEffect(() => {
    const prev = prevSessionStatesRef.current;
    const now = Date.now();
    const newFlashing: string[] = [];
    const newErrors: string[] = [];
    const newCompleted: string[] = [];
    for (const [tabId, state] of Object.entries(sessionStates)) {
      // Track entry time for any state change
      if (prev[tabId] !== state) {
        // Accumulate time in previous state
        const prevState = prev[tabId];
        if (prevState && stateEntryTimeRef.current[tabId]) {
          const elapsed = now - stateEntryTimeRef.current[tabId];
          stateTimeAccum.current[prevState] += elapsed;
        }
        stateEntryTimeRef.current[tabId] = now;
        // Log state transitions to history
        const tab = tabs.find((t) => t.id === tabId);
        const zone = Object.entries(zoneLayout.assignments).find(([, id]) => id === tabId);
        const zoneNum = zone ? Number(zone[0]) : undefined;
        if (state === "needs-input") {
          addHistoryEvent("Needs input", tab?.title ?? tabId, zoneNum, "#e0af68");
        } else if (state === "error" && prev[tabId] !== "error") {
          addHistoryEvent("Error", tab?.title ?? tabId, zoneNum, "#f7768e");
        } else if (state === "completed" && prev[tabId] !== "completed") {
          addHistoryEvent("Completed", tab?.title ?? tabId, zoneNum, "#9ece6a");
          // Auto-restart: schedule restart for completed sessions (exit 0) after 2s
          if (autoRestart && zoneNum !== undefined) {
            const completedTab = tabs.find((t) => t.id === tabId);
            if (completedTab && (completedTab.exitCode === 0 || completedTab.exitCode === null)) {
              const capturedZoneNum = zoneNum;
              const capturedTitle = completedTab.title ?? tabId;
              const restartAt = Date.now() + 2000;
              setPendingRestarts((prev) => ({ ...prev, [capturedZoneNum]: restartAt }));

              const timer = setTimeout(() => {
                handleRestartInZoneRef.current(capturedZoneNum);
                autoRestartCountRef.current++;
                addHistoryEvent("Auto-restarted", capturedTitle, capturedZoneNum, "#7dcfff");
                setPendingRestarts((prev) => {
                  const next = { ...prev };
                  delete next[capturedZoneNum];
                  return next;
                });
                delete pendingRestartTimersRef.current[capturedZoneNum];
              }, 2000);

              pendingRestartTimersRef.current[capturedZoneNum] = timer;
            }
          }
        }
      }
      if (state === "needs-input" && prev[tabId] !== "needs-input") {
        newFlashing.push(tabId);
      }
      if (state === "error" && prev[tabId] !== "error") {
        newErrors.push(tabId);
      }
      if (state === "completed" && prev[tabId] !== "completed") {
        newCompleted.push(tabId);
      }
    }
    prevSessionStatesRef.current = sessionStates;
    // Track unseen needs-input
    if (newFlashing.length > 0) {
      setUnseenNeedsInput((old) => {
        const next = new Set(old);
        for (const id of newFlashing) next.add(id);
        return next;
      });
    }
    // Auto-approve: check last output lines against patterns
    if (newFlashing.length > 0 && autoApprovePatterns.length > 0) {
      for (const tabId of newFlashing) {
        const lines = lastOutputLines[tabId] ?? [];
        const lastFew = lines.slice(-5).join("\n");
        const matched = autoApprovePatterns.some((pattern) => {
          try {
            return new RegExp(pattern, "i").test(lastFew);
          } catch {
            return false;
          }
        });
        if (matched) {
          const ref = terminalRefs.current.get(tabId);
          ref?.current?.writeToTerminal("y\r");
          autoApproveCountRef.current++;
          const tab = tabs.find((t) => t.id === tabId);
          addHistoryEvent("Auto-approved", tab?.title ?? tabId, undefined, "#9ece6a");
        }
      }
    }
    if (newFlashing.length > 0) {
      setFlashingTabs((old) => {
        const next = new Set(old);
        for (const id of newFlashing) next.add(id);
        return next;
      });
      // Auto-focus: jump to the first newly-needs-input zone
      if (autoFocusNeedsInput) {
        const firstFlashing = newFlashing[0];
        const zoneIdx = Object.entries(zoneLayout.assignments).find(
          ([, tabId]) => tabId === firstFlashing,
        );
        if (zoneIdx) {
          zoneLayout.setFocusedZone(Number(zoneIdx[0]));
        }
      }
      // Play notification sound
      if (soundEnabled) {
        playNeedsInputChime();
      }
      // Desktop notifications for needs-input transitions
      if (
        desktopNotify &&
        document.hidden &&
        "Notification" in window &&
        Notification.permission === "granted"
      ) {
        for (const tabId of newFlashing) {
          const tab = tabs.find((t) => t.id === tabId);
          const zoneNum = Object.entries(zoneLayout.assignments).find(
            ([, tid]) => tid === tabId,
          )?.[0];
          new Notification("Session needs input", {
            body: `Zone ${zoneNum ? Number(zoneNum) + 1 : "?"}: ${tab?.title ?? tabId}`,
            tag: `zone-input-${tabId}`,
          });
        }
      }
      // Clear flash after animation duration (1s)
      const timer = setTimeout(() => {
        setFlashingTabs((old) => {
          const next = new Set(old);
          for (const id of newFlashing) next.delete(id);
          return next;
        });
      }, 1000);
      return () => clearTimeout(timer);
    }
    // Desktop notifications for error transitions
    if (
      newErrors.length > 0 &&
      desktopNotify &&
      document.hidden &&
      "Notification" in window &&
      Notification.permission === "granted"
    ) {
      for (const tabId of newErrors) {
        const tab = tabs.find((t) => t.id === tabId);
        const zoneNum = Object.entries(zoneLayout.assignments).find(
          ([, tid]) => tid === tabId,
        )?.[0];
        new Notification("Session error", {
          body: `Zone ${zoneNum ? Number(zoneNum) + 1 : "?"}: ${tab?.title ?? tabId}`,
          tag: `zone-error-${tabId}`,
        });
      }
    }
    // Play completion chime for completed transitions
    if (soundEnabled && newCompleted.length > 0) {
      playCompletionChime();
    }
    // Play error alert for error transitions
    if (soundEnabled && newErrors.length > 0) {
      playErrorAlert();
    }
  }, [
    sessionStates,
    autoFocusNeedsInput,
    soundEnabled,
    desktopNotify,
    zoneLayout,
    autoApprovePatterns,
    lastOutputLines,
    tabs,
    addHistoryEvent,
    autoRestart,
  ]);

  // Update formatted durations every 10 seconds
  useEffect(() => {
    const formatDuration = (ms: number): string => {
      const seconds = Math.floor(ms / 1000);
      if (seconds < 60) return `${seconds}s`;
      const minutes = Math.floor(seconds / 60);
      if (minutes < 60) return `${minutes}m`;
      const hours = Math.floor(minutes / 60);
      const remainMin = minutes % 60;
      return `${hours}h${remainMin > 0 ? `${remainMin}m` : ""}`;
    };

    const update = () => {
      const now = Date.now();
      const durations: Record<string, string> = {};
      for (const [tabId, entryTime] of Object.entries(stateEntryTimeRef.current)) {
        durations[tabId] = formatDuration(now - entryTime);
      }
      setStateDurations(durations);
    };

    update();
    const interval = setInterval(update, 10000);
    return () => clearInterval(interval);
  }, [sessionStates]); // Re-start interval when states change for immediate update

  const handleExit = useCallback(
    (terminalId: string, exitCode: number | null) => {
      updateTab(terminalId, { isAlive: false, exitCode });
      setSessionStates((prev) => ({
        ...prev,
        [terminalId]: exitCode === 0 || exitCode === null ? "completed" : "error",
      }));
    },
    [updateTab],
  );

  const handleShellIntegration = useCallback(
    (tabId: string, event: ShellIntegrationEvent) => {
      // If this tab has a pending resume command, fire it on the first prompt
      if (event.type === "prompt_start") {
        const pending = pendingResumeRef.current;
        if (pending && pending.tabId === tabId) {
          pendingResumeRef.current = null;
          // Small defer so the prompt finishes rendering before we write
          setTimeout(() => {
            const ref = terminalRefs.current.get(tabId);
            ref?.current?.writeToTerminal(`${pending.resumeCmd}\r`);
          }, 50);
        }
        // Shell prompt → could be idle or needs-input
        // If a Claude Code session is running, prompt_start typically means
        // it's waiting for user input. For a bare shell, it's idle.
        setSessionStates((prev) => {
          const tab = tabs.find((t) => t.id === tabId);
          if (tab?.claudeSessionId) {
            return { ...prev, [tabId]: "needs-input" };
          }
          return { ...prev, [tabId]: "idle" };
        });
      }
      if (event.type === "command_execute") {
        setSessionStates((prev) => ({ ...prev, [tabId]: "working" }));
      }
      if (event.type === "cwd") {
        updateTab(tabId, { workingDir: event.path });
        // Auto-name tab from project directory if still using default name
        const tab = tabs.find((t) => t.id === tabId);
        if (tab && /^Terminal \d+$/.test(tab.title)) {
          const dirName = event.path.split(/[/\\]/).pop();
          if (dirName) {
            renameTab(tabId, dirName);
          }
        }
      } else if (event.type === "command_line") {
        pendingCommandRef.current[tabId] = event.command;
      } else if (event.type === "command_done") {
        const cmd = pendingCommandRef.current[tabId];
        if (cmd) {
          delete pendingCommandRef.current[tabId];
          setCommandHistories((prev) => ({
            ...prev,
            [tabId]: [
              ...(prev[tabId] ?? []).slice(-99),
              { command: cmd, exitCode: event.exitCode, timestamp: Date.now() },
            ],
          }));
        }
      }
    },
    [updateTab, renameTab, tabs],
  );

  // ── Terminal output handler with session state tracking ────────────────────

  // Tick activity sparkline buffers every 2 seconds
  useEffect(() => {
    const interval = setInterval(() => {
      const buffers = activityBuffersRef.current;
      for (const tabId of Object.keys(buffers)) {
        // Push current accumulator and reset; keep last 30 points
        buffers[tabId] = [...(buffers[tabId] ?? []), 0].slice(-30);
      }
    }, 2000);
    return () => clearInterval(interval);
  }, []);

  const handleOutput = useCallback(
    (tabId: string, text: string) => {
      lastOutputTimeRef.current[tabId] = Date.now();

      // Accumulate bytes for sparkline
      if (!activityBuffersRef.current[tabId]) {
        activityBuffersRef.current[tabId] = [];
      }
      const buf = activityBuffersRef.current[tabId];
      if (buf.length === 0) buf.push(0);
      buf[buf.length - 1] += text.length;

      // Use the session state detector for pattern matching
      setSessionStates((prev) => {
        const current = prev[tabId] ?? "idle";
        const detected = detectSessionState(text, current);
        if (detected && detected !== current) {
          return { ...prev, [tabId]: detected };
        }
        return prev;
      });

      // Track last output lines for compact view (keep last 20 non-empty lines)
      // Normal display shows 6; hover preview shows up to 20
      // Strip ANSI escape sequences for cleaner display
      // eslint-disable-next-line no-control-regex
      const stripped = text.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "").replace(/\r/g, "");
      const newLines = stripped.split("\n").filter((l) => l.trim().length > 0);
      if (newLines.length > 0) {
        setLastOutputLines((prev) => {
          const existing = prev[tabId] ?? [];
          const combined = [...existing, ...newLines].slice(-20);
          return { ...prev, [tabId]: combined };
        });
      }

      processOutput(tabId, text);
    },
    [processOutput],
  );

  // ── Resume Claude Code session in terminal ─────────────────────────────────

  // Tracks the tab ID and session ID awaiting the first shell prompt to send the command.
  const pendingResumeRef = useRef<{ tabId: string; resumeCmd: string } | null>(null);

  const handleResumeSession = useCallback(
    async (session: TranscriptSession) => {
      // Derive a short label from the session ID for the tab title
      const tabTitle = `claude ${session.session_id.slice(0, 8)}`;
      const tabId = await createTerminal(tabTitle, session.project_path);
      if (!tabId) return;

      // Track which Claude session is running in this tab so "Generate Workflow"
      // can find the correct transcript instead of picking a random recent session.
      updateTab(tabId, {
        claudeSessionId: session.session_id,
        claudeConfigDir: session.config_dir,
      });

      // Close the transcript panel so the terminal is visible
      setRightPanelMode(null);
      setSelectedTranscriptSessionId(null);

      // Queue the resume command — it will be sent once the shell emits its first prompt.
      // Include the config_dir so Claude CLI searches the right directory.
      // Windows terminals use PowerShell ($env:VAR), others use bash (VAR=val cmd).
      const configDir = session.config_dir;
      const isWindows = navigator.platform.startsWith("Win");
      let resumeCmd: string;
      if (configDir) {
        resumeCmd = isWindows
          ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; claude --resume ${session.session_id}`
          : `CLAUDE_CONFIG_DIR="${configDir}" claude --resume ${session.session_id}`;
      } else {
        resumeCmd = `claude --resume ${session.session_id}`;
      }
      pendingResumeRef.current = { tabId, resumeCmd };

      // Fallback: send after 1.5 s regardless (in case shell integration isn't active)
      setTimeout(() => {
        const pending = pendingResumeRef.current;
        if (!pending || pending.tabId !== tabId) return;
        pendingResumeRef.current = null;
        const ref = terminalRefs.current.get(tabId);
        ref?.current?.writeToTerminal(`${pending.resumeCmd}\r`);
      }, 1500);
    },
    [createTerminal, updateTab],
  );

  // ── Session selection ──────────────────────────────────────────────────────

  const handleSelectTranscriptSession = useCallback(
    async (sessionId: string) => {
      setSelectedTranscriptSessionId(sessionId);
      setRightPanelMode("transcript");
      setLoadingMessages(true);
      try {
        const msgs = await loadMessages(sessionId);
        setTranscriptMessages(msgs);
      } finally {
        setLoadingMessages(false);
      }
    },
    [loadMessages],
  );

  // ── Core generation logic ──────────────────────────────────────────────────

  const runGeneration = useCallback(async (description: string, inlineContext: string) => {
    lastGenerationParamsRef.current = { description, inlineContext };
    setIsGenerating(true);
    setRightPanelMode("workflow");
    setGeneratedWorkflow(null);
    setWorkflowError(undefined);

    try {
      const result = await invoke<CommandResponse>("generate_workflow_standalone", {
        description,
        inlineContext,
      });

      if (result.success && result.data) {
        const data = result.data as GenerateWorkflowResponse;
        if (data.workflow) {
          setGeneratedWorkflow(data.workflow as UnifiedWorkflow);
          setNotification({
            message: `Workflow generated: "${data.workflow.name}"`,
            type: "success",
          });
        } else {
          const errMsg = data.error || "Generation returned no workflow";
          setWorkflowError(errMsg);
          setNotification({ message: errMsg, type: "error" });
        }
      } else {
        const errMsg = result.message || "Workflow generation failed";
        setWorkflowError(errMsg);
        setNotification({ message: errMsg, type: "error" });
      }
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : "Failed to generate workflow";
      setWorkflowError(errMsg);
      setNotification({ message: errMsg, type: "error" });
    } finally {
      setIsGenerating(false);
    }
  }, []);

  // ── Generate from active or latest session ──────────────────────────────────

  const handleGenerateFromLatestSession = useCallback(async () => {
    // Prefer the Claude session tracked on the active terminal tab (set on resume).
    // Fall back to the most recent session for the tab's project directory.
    const activeTab = tabs.find((t) => t.id === activeId);

    if (activeTab?.claudeSessionId) {
      setShowSidebar(true);
      await handleSelectTranscriptSession(activeTab.claudeSessionId);
      return;
    }

    try {
      // Pass the active tab's working directory so the backend scopes the
      // search to sessions for that project instead of picking globally.
      const result = await invoke<CommandResponse>("transcript_get_latest", {
        projectPath: activeTab?.workingDir ?? null,
      });
      if (result.success && result.data) {
        const session = result.data as { session_id: string };
        setShowSidebar(true);
        await handleSelectTranscriptSession(session.session_id);
      } else {
        setNotification({
          message: "No Claude Code sessions found for this project",
          type: "error",
        });
      }
    } catch (err) {
      setNotification({
        message: `Failed to detect session: ${err instanceof Error ? err.message : err}`,
        type: "error",
      });
    }
  }, [activeId, tabs, handleSelectTranscriptSession]);

  // ── Generation entry points ────────────────────────────────────────────────

  const handleGenerateFromTranscript = useCallback(
    async (description: string, inlineContext: string) => {
      await runGeneration(description, inlineContext);
    },
    [runGeneration],
  );

  const handleGenerateAndRunFromTranscript = useCallback(
    async (description: string, inlineContext: string) => {
      lastGenerationParamsRef.current = { description, inlineContext };
      setIsGenerating(true);
      setRightPanelMode("workflow");
      setGeneratedWorkflow(null);
      setWorkflowError(undefined);

      try {
        const result = await invoke<CommandResponse>("generate_workflow_standalone", {
          description,
          inlineContext,
        });

        if (result.success && result.data) {
          const data = result.data as GenerateWorkflowResponse;
          if (data.workflow) {
            // Auto-execute immediately — skip the preview panel
            await tracedFetch(`${getApiBase()}/unified-workflows/execute-inline`, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(data.workflow),
            });
            setRightPanelMode(null);
            setNotification({
              message: `Running: "${data.workflow.name}"`,
              type: "success",
            });
            onNavigateToActive?.();
          } else {
            const errMsg = data.error || "Generation returned no workflow";
            setGeneratedWorkflow(null);
            setWorkflowError(errMsg);
            setNotification({ message: errMsg, type: "error" });
          }
        } else {
          const errMsg = result.message || "Workflow generation failed";
          setWorkflowError(errMsg);
          setNotification({ message: errMsg, type: "error" });
        }
      } catch (err) {
        const errMsg = err instanceof Error ? err.message : "Failed to generate workflow";
        setWorkflowError(errMsg);
        setNotification({ message: errMsg, type: "error" });
      } finally {
        setIsGenerating(false);
      }
    },
    [onNavigateToActive],
  );

  // ── Workflow preview panel handlers ────────────────────────────────────────

  const handleExecute = useCallback(async () => {
    if (!generatedWorkflow) return;
    try {
      await tracedFetch(`${getApiBase()}/unified-workflows/execute-inline`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(generatedWorkflow),
      });
      setRightPanelMode(null);
      onNavigateToActive?.();
    } catch (e) {
      console.error("[TerminalPage] Failed to execute workflow:", e);
    }
  }, [generatedWorkflow, onNavigateToActive]);

  const handleSaveWorkflow = useCallback(async () => {
    if (!generatedWorkflow) return;
    try {
      await tracedFetch(`${getApiBase()}/unified-workflows`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(generatedWorkflow),
      });
      setNotification({ message: "Workflow saved to library", type: "success" });
    } catch (e) {
      console.error("[TerminalPage] Failed to save workflow:", e);
    }
  }, [generatedWorkflow]);

  const handleEditInBuilder = useCallback(() => {
    if (!generatedWorkflow) return;
    try {
      localStorage.setItem("qontinui-generated-workflow", JSON.stringify(generatedWorkflow));
    } catch {
      // ignore storage errors
    }
    onNavigateToBuilder?.();
  }, [generatedWorkflow, onNavigateToBuilder]);

  const handleRegenerate = useCallback(async () => {
    if (!lastGenerationParamsRef.current) return;
    const { description, inlineContext } = lastGenerationParamsRef.current;
    await runGeneration(description, inlineContext);
  }, [runGeneration]);

  // ── Build plan workflow from markdown text ─────────────────────────────────

  const handleBuildPlanWorkflow = useCallback((planContent: string) => {
    try {
      const phases = parsePlanMarkdown(planContent);
      if (phases.length === 0) {
        setNotification({ message: "No plan structure found in content", type: "error" });
        return;
      }

      const summary = summarizeParsedPlan(phases);
      const workflow = buildPlanWorkflow({ phases });

      setGeneratedWorkflow(workflow);
      setWorkflowError(undefined);
      setRightPanelMode("workflow");
      setNotification({
        message: `Plan workflow built: ${summary.phaseCount} phases, ${summary.verificationCount} checks (${summary.deterministicCount} deterministic, ${summary.aiCount} AI)`,
        type: "success",
      });
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : "Failed to parse plan";
      setWorkflowError(errMsg);
      setNotification({ message: errMsg, type: "error" });
    }
  }, []);

  const handleBuildPlanFromFile = useCallback(() => {
    if (!latestPlanContent.trim()) {
      setNotification({ message: "No plan file loaded", type: "error" });
      return;
    }
    handleBuildPlanWorkflow(latestPlanContent);
  }, [latestPlanContent, handleBuildPlanWorkflow]);

  // ── Analysis helper: read scrollback from a tab ───────────────────────────

  const getScrollback = useCallback((tabId: string, maxLines = 500): string => {
    const ref = terminalRefs.current.get(tabId);
    return ref?.current?.getScrollback?.(maxLines) ?? "";
  }, []);

  const getActiveSelection = useCallback((): string => {
    if (!activeId) return "";
    const ref = terminalRefs.current.get(activeId);
    return ref?.current?.getSelection?.() ?? "";
  }, [activeId]);

  // ── Analysis handler ────────────────────────────────────────────────────

  const handleAnalyze = useCallback(
    async (type: AnalysisType) => {
      setAnalysisType(type);
      setIsAnalyzing(true);
      setAnalysisPanels(null);
      setAnalysisError(undefined);
      setRightPanelMode("analysis");

      // Collect the right input per analysis type
      let input = "";
      if (type === "session-summary") {
        // Prefer structured command history over raw ANSI-polluted scrollback
        const history = commandHistories[activeId ?? ""] ?? [];
        input =
          history.length > 0
            ? history.map((e) => `$ ${e.command}  [exit ${e.exitCode}]`).join("\n")
            : activeId
              ? getScrollback(activeId, 500)
              : "";
      } else if (type === "architecture") {
        // Prefer: plan content → terminal selection → scrollback
        if (latestPlanContent.trim().length > 0) {
          const sel = getActiveSelection();
          input =
            sel.trim().length > 20
              ? `${latestPlanContent}\n\n---\nSelected terminal context:\n${sel}`
              : latestPlanContent;
        } else {
          const sel = getActiveSelection();
          input = sel.trim().length > 20 ? sel : activeId ? getScrollback(activeId, 300) : "";
        }
      } else if (type === "change-impact") {
        const sel = getActiveSelection();
        input = sel.trim().length > 0 ? sel : activeId ? getScrollback(activeId, 200) : "";
      } else if (type === "progress") {
        // Prefer plan content as the plan; always append scrollback for evidence
        const scrollback = activeId ? getScrollback(activeId, 300) : "";
        if (latestPlanContent.trim().length > 0) {
          input = `${latestPlanContent}\n\n---\nTerminal activity (for progress evidence):\n${scrollback}`;
        } else {
          const sel = getActiveSelection();
          input = sel.trim().length > 20 ? `${sel}\n\n---\n${scrollback}` : scrollback;
        }
      } else if (type === "cross-tab") {
        const parts: string[] = [];
        for (const tab of tabs) {
          const history = commandHistories[tab.id] ?? [];
          const content =
            history.length > 0
              ? history.map((e) => `$ ${e.command}  [exit ${e.exitCode}]`).join("\n")
              : getScrollback(tab.id, 200);
          if (content.trim().length > 0) {
            parts.push(`--- Tab: ${tab.title} ---\n${content}`);
          }
        }
        input = parts.join("\n\n");
      } else if (type === "page-architecture") {
        input = "";
      }

      const commandMap: Record<AnalysisType, string> = {
        "session-summary": "analyze_session_summary",
        architecture: "analyze_architecture",
        "change-impact": "analyze_change_impact",
        progress: "analyze_plan_progress",
        "cross-tab": "analyze_cross_tab",
        "page-architecture": "analyze_page_architecture",
      };

      try {
        const args = type === "page-architecture" ? {} : { input };
        const result = await invoke<CommandResponse>(commandMap[type], args);

        if (result.success && result.data) {
          const data = result.data as { panels?: CanvasPanel[] };
          setAnalysisPanels(data.panels ?? []);
        } else {
          setAnalysisError(result.message || "Analysis failed");
        }
      } catch (err) {
        setAnalysisError(err instanceof Error ? err.message : "Analysis failed");
      } finally {
        setIsAnalyzing(false);
      }
    },
    [activeId, tabs, getScrollback, getActiveSelection, latestPlanContent, commandHistories],
  );

  // ── Findings handlers ────────────────────────────────────────────────────

  const handleFindingRespond = useCallback(
    (findingId: string, text: string) => {
      findingsTracker.provideUserResponse(findingId, text);
      if (activeId) {
        terminalRefs.current.get(activeId)?.current?.writeToTerminal(text + "\r");
      }
    },
    [activeId],
  );

  const handleFixFinding = useCallback(
    async (finding: Finding) => {
      const activeTab = tabs.find((t) => t.id === activeId);
      const workingDir = activeTab?.workingDir;
      const tabTitle = `fix: ${finding.title.slice(0, 20)}`;
      const tabId = await createTerminal(tabTitle, workingDir);
      if (!tabId) return;

      setRightPanelMode(null); // close findings panel to show terminal

      const title = finding.title.replace(/"/g, '\\"');
      const desc = finding.description.replace(/"/g, '\\"').slice(0, 500);
      const filePart = finding.codeContext?.file
        ? ` File: ${finding.codeContext.file}${finding.codeContext.line ? ":" + finding.codeContext.line : ""}.`
        : "";
      const resumeCmd = `claude "Fix this issue: ${title}.${filePart} Details: ${desc}"`;

      pendingResumeRef.current = { tabId, resumeCmd };
      setTimeout(() => {
        const pending = pendingResumeRef.current;
        if (!pending || pending.tabId !== tabId) return;
        pendingResumeRef.current = null;
        terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${pending.resumeCmd}\r`);
      }, 1500);
    },
    [activeId, tabs, createTerminal],
  );

  const handleGenerateFromFindings = useCallback(
    async (findings: Finding[]) => {
      const description =
        "Fix the following unresolved findings from the current development session";
      const inlineContext = findings
        .map((f) => {
          let entry = `- [${f.categoryId}:${f.severity}] ${f.title}`;
          if (f.description) entry += `\n  ${f.description}`;
          if (f.codeContext?.file) {
            entry += `\n  File: ${f.codeContext.file}`;
            if (f.codeContext.line) entry += `:${f.codeContext.line}`;
          }
          if (f.pendingQuestion) entry += `\n  Question: ${f.pendingQuestion.question}`;
          return entry;
        })
        .join("\n\n");
      await runGeneration(description, inlineContext);
    },
    [runGeneration],
  );

  const handleToggleFindings = useCallback(() => {
    setRightPanelMode((prev) => (prev === "findings" ? null : "findings"));
  }, []);

  // ── Auto-naming from first input ──────────────────────────────────────────

  const handleFirstInput = useCallback(
    (terminalId: string, input: string) => {
      const tab = tabs.find((t) => t.id === terminalId);
      if (!tab) return;
      if (/^Terminal \d+$/.test(tab.title)) {
        renameTab(terminalId, input.slice(0, 30).trim());
      }
    },
    [tabs, renameTab],
  );

  // ── Zone interaction handlers ─────────────────────────────────────────────

  const toggleAutoFocus = useCallback(() => {
    setAutoFocusNeedsInput((prev) => {
      const next = !prev;
      localStorage.setItem("zone-auto-focus", String(next));
      return next;
    });
  }, []);

  const toggleSound = useCallback(() => {
    setSoundEnabled((prev) => {
      const next = !prev;
      localStorage.setItem("zone-sound-notify", String(next));
      // Play a test chime when enabling
      if (next) playNeedsInputChime();
      return next;
    });
  }, []);

  const handleZoneClick = useCallback(
    (zoneIndex: number, ctrlKey?: boolean) => {
      if (ctrlKey) {
        // Ctrl+click toggles zone selection
        setSelectedZones((prev) => {
          const next = new Set(prev);
          if (next.has(zoneIndex)) {
            next.delete(zoneIndex);
          } else {
            next.add(zoneIndex);
          }
          return next;
        });
      } else {
        // Regular click focuses and clears selection
        zoneLayout.setFocusedZone(zoneIndex);
        setSelectedZones(new Set());
        // Mark the focused tab as "seen" for unseen badge
        const focusedTabId = zoneLayout.assignments[zoneIndex];
        if (focusedTabId) {
          setUnseenNeedsInput((prev) => {
            if (!prev.has(focusedTabId)) return prev;
            const next = new Set(prev);
            next.delete(focusedTabId);
            return next;
          });
        }
      }
    },
    [zoneLayout],
  );

  const handleZoneDoubleClick = useCallback(
    (zoneIndex: number) => {
      if (zoneLayout.isMultiZone) {
        zoneLayout.toggleMaximize(zoneIndex);
      }
    },
    [zoneLayout],
  );

  // Create terminal and auto-assign to first empty zone
  const createAndAssignTerminal = useCallback(
    async (title?: string, workingDir?: string) => {
      metricsRef.current.sessionsCreated++;
      const tabId = await createTerminal(title, workingDir);
      if (!tabId) return tabId;

      // Auto-switch layout when in "single" and adding a second+ terminal
      const totalTabs = tabs.length + 1; // +1 for the newly created tab
      if (autoLayout && zoneLayout.layoutId === "single" && totalTabs >= 2) {
        let targetLayout = "split";
        if (totalTabs >= 7) targetLayout = "full-grid";
        else if (totalTabs >= 5) targetLayout = "six-pack";
        else if (totalTabs >= 3) targetLayout = "quad";
        zoneLayout.setLayoutId(targetLayout);
      }

      if (zoneLayout.isMultiZone || (autoLayout && totalTabs >= 2)) {
        // Find first empty zone (layout may have just changed)
        // Use a small defer to let the layout update propagate
        requestAnimationFrame(() => {
          const emptyZone = zoneLayout.layout.zones.findIndex(
            (_, idx) => !zoneLayout.assignments[idx],
          );
          if (emptyZone >= 0) {
            zoneLayout.assignTabToZone(emptyZone, tabId);
            zoneLayout.setFocusedZone(emptyZone);
          }
        });
      }
      return tabId;
    },
    [createTerminal, zoneLayout, tabs.length, autoLayout],
  );

  // Sort zones by session state priority (needs-input first, then error, working, idle, completed)
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
        priority: STATE_PRIORITY[sessionStates[tabId] ?? "idle"],
      }))
      .sort((a, b) => a.priority - b.priority);

    // Reassign: sorted tabs go into zone slots 0, 1, 2, ...
    const tabIds = entries.map((e) => e.tabId);
    for (let i = 0; i < tabIds.length; i++) {
      zoneLayout.assignTabToZone(i, tabIds[i]);
    }
  }, [zoneLayout, sessionStates]);

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
      const state = sessionStates[tabId] ?? "idle";
      const output = lastOutputLines[tabId] ?? [];
      lines.push("");
      lines.push(`--- Zone ${Number(zoneStr) + 1}: ${tab.title} [${state}] ---`);
      if (tab.workingDir) lines.push(`    Dir: ${tab.workingDir}`);
      if (output.length > 0) {
        lines.push(...output);
      } else {
        lines.push("    (no output)");
      }
    }

    // Include unassigned tabs
    const assignedTabIds = new Set(Object.values(zoneLayout.assignments));
    const unassigned = tabs.filter((t) => !assignedTabIds.has(t.id));
    if (unassigned.length > 0) {
      lines.push("");
      lines.push("--- Unassigned Sessions ---");
      for (const tab of unassigned) {
        const state = sessionStates[tab.id] ?? "idle";
        const output = lastOutputLines[tab.id] ?? [];
        lines.push(`  ${tab.title} [${state}]`);
        if (output.length > 0) lines.push(...output.map((l) => `    ${l}`));
      }
    }

    try {
      await writeTextFile(filePath, lines.join("\n"));
      setNotification({ message: `Exported to ${filePath}`, type: "success" });
    } catch (err) {
      setNotification({
        message: `Export failed: ${err instanceof Error ? err.message : String(err)}`,
        type: "error",
      });
    }
  }, [tabs, zoneLayout, sessionStates, lastOutputLines]);

  // Export a single zone's output in the chosen format
  const handleExportZone = useCallback(
    async (zoneIndex: number, format: "text" | "markdown" | "json") => {
      const tabId = zoneLayout.assignments[zoneIndex];
      if (!tabId) return;
      const tab = tabs.find((t) => t.id === tabId);
      const lines = lastOutputLines[tabId] ?? [];
      const title = tab?.title ?? `Zone ${zoneIndex + 1}`;
      const state = sessionStates[tabId] ?? "idle";
      const label = zoneLabels[zoneIndex] ?? "";

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
    [tabs, lastOutputLines, sessionStates, zoneLabels, zoneLayout.assignments],
  );

  // Restart a terminal in a specific zone (completed/errored)
  const handleRestartInZone = useCallback(
    async (zoneIdx: number) => {
      const oldTabId = zoneLayout.assignments[zoneIdx];
      const oldTab = tabs.find((t) => t.id === oldTabId);
      const state = oldTabId ? (sessionStates[oldTabId] ?? "idle") : "idle";
      if (state !== "completed" && state !== "error") return;
      const label = zoneLabels[zoneIdx];
      const tabId = await createTerminal(
        oldTab?.title ? `${oldTab.title} (2)` : undefined,
        oldTab?.workingDir ?? undefined,
      );
      if (tabId) {
        zoneLayout.assignTabToZone(zoneIdx, tabId);
        zoneLayout.setFocusedZone(zoneIdx);
        if (label) {
          setZoneLabels((prev) => ({ ...prev, [zoneIdx]: label }));
        }
      }
    },
    [zoneLayout, tabs, sessionStates, zoneLabels, createTerminal],
  );
  handleRestartInZoneRef.current = handleRestartInZone;

  // ── Keyboard shortcuts ────────────────────────────────────────────────────

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
      // Ctrl+Tab / Ctrl+Shift+Tab — cycle zones (in multi-zone) or tabs (in single)
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
      // Ctrl+Shift+L — open layout picker (handled by ZoneLayoutPicker focus)
      // Ctrl+Shift+N — jump to next session needing input
      if (e.ctrlKey && e.shiftKey && e.key === "N") {
        e.preventDefault();
        zoneLayout.focusNextNeedsInput(sessionStates);
        return;
      }
      // Ctrl+Shift+F — maximize/restore focused zone
      if (e.ctrlKey && e.shiftKey && e.key === "F") {
        e.preventDefault();
        zoneLayout.toggleMaximize(zoneLayout.focusedZone);
        return;
      }
      // Ctrl+Shift+M — cycle view mode (auto → full → compact)
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
        toggleAutoFocus();
        return;
      }
      // Ctrl+Shift+S — toggle sound notification
      if (e.ctrlKey && e.shiftKey && e.key === "S") {
        e.preventDefault();
        toggleSound();
        return;
      }
      // Ctrl+Shift+Enter — approve all waiting sessions
      if (e.ctrlKey && e.shiftKey && e.key === "Enter") {
        e.preventDefault();
        const needsInput = tabs.filter((t) => sessionStates[t.id] === "needs-input");
        metricsRef.current.totalApprovals += needsInput.length;
        addHistoryEvent("Approve all", `${needsInput.length} sessions`, undefined, "#9ece6a");
        for (const tab of needsInput) {
          const ref = terminalRefs.current.get(tab.id);
          ref?.current?.writeToTerminal("y\r");
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
      // Ctrl+[1-9] — focus zone by number (in multi-zone layouts)
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
      // Ctrl+Shift+X — zone swap (first press marks source, second press swaps)
      if (e.ctrlKey && e.shiftKey && e.key === "X") {
        e.preventDefault();
        if (swapSource === null) {
          setSwapSource(zoneLayout.focusedZone);
        } else if (swapSource !== zoneLayout.focusedZone) {
          // Perform the swap
          const srcTabId = zoneLayout.assignments[swapSource];
          const dstTabId = zoneLayout.assignments[zoneLayout.focusedZone];
          if (srcTabId) zoneLayout.assignTabToZone(zoneLayout.focusedZone, srcTabId);
          if (dstTabId) zoneLayout.assignTabToZone(swapSource, dstTabId);
          setSwapSource(null);
        } else {
          // Same zone — cancel
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
        setPinnedZones((prev) => {
          const next = new Set(prev);
          if (next.has(zoneLayout.focusedZone)) next.delete(zoneLayout.focusedZone);
          else next.add(zoneLayout.focusedZone);
          return next;
        });
        return;
      }
      // Ctrl+Shift+D — toggle focus mode (dim non-focused zones)
      if (e.ctrlKey && e.shiftKey && e.key === "D") {
        e.preventDefault();
        setFocusMode((prev) => {
          const next = !prev;
          localStorage.setItem("zone-focus-mode", String(next));
          return next;
        });
        return;
      }
      // Ctrl+Shift+R — restart focused zone (completed/error only)
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
        if (allTags.length === 0) return;
        setActiveTagFilters((prev) => {
          const currentTag = prev.size === 1 ? [...prev][0] : null;
          const currentIdx = currentTag ? allTags.indexOf(currentTag) : -1;
          const nextIdx = currentIdx + 1;
          if (nextIdx >= allTags.length) {
            return new Set();
          }
          return new Set([allTags[nextIdx]]);
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
        if (focusHistoryIndexRef.current > 0) {
          focusHistoryIndexRef.current--;
          isNavigatingHistoryRef.current = true;
          zoneLayout.setFocusedZone(focusHistoryRef.current[focusHistoryIndexRef.current]);
        }
        return;
      }
      // Ctrl+Shift+Right — go forward in focus history
      if (e.ctrlKey && e.shiftKey && e.key === "ArrowRight") {
        e.preventDefault();
        if (focusHistoryIndexRef.current < focusHistoryRef.current.length - 1) {
          focusHistoryIndexRef.current++;
          isNavigatingHistoryRef.current = true;
          zoneLayout.setFocusedZone(focusHistoryRef.current[focusHistoryIndexRef.current]);
        }
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
        } else if (rightPanelMode) {
          setRightPanelMode(null);
          setSelectedTranscriptSessionId(null);
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
    rightPanelMode,
    zoneLayout,
    sessionStates,
    swapSource,
    selectedZones,
    handleRestartInZone,
    allTags,
  ]);

  // ── Status bar summary for multi-zone ─────────────────────────────────────

  const needsInputCount = Object.values(sessionStates).filter((s) => s === "needs-input").length;
  const workingCount = Object.values(sessionStates).filter((s) => s === "working").length;
  const errorCount = Object.values(sessionStates).filter((s) => s === "error").length;

  // Un-dismiss batch bar when needs-input count increases
  useEffect(() => {
    if (needsInputCount > prevNeedsInputCountRef.current) {
      setBatchBarDismissed(false);
    }
    prevNeedsInputCountRef.current = needsInputCount;
  }, [needsInputCount]);

  // Update window title with waiting count
  useEffect(() => {
    const actionCount = needsInputCount + errorCount;
    if (zoneLayout.isMultiZone && actionCount > 0) {
      getCurrentWindow()
        .setTitle(`(${actionCount} waiting) Terminal - Qontinui Runner`)
        .catch(() => {});
    } else {
      getCurrentWindow()
        .setTitle("Qontinui Runner")
        .catch(() => {});
    }
    return () => {
      getCurrentWindow()
        .setTitle("Qontinui Runner")
        .catch(() => {});
    };
  }, [needsInputCount, errorCount, zoneLayout.isMultiZone]);

  return (
    <div className="h-full flex flex-col bg-[#1a1b26]">
      <TerminalTabBar
        tabs={tabs}
        activeId={activeId}
        onSelect={(id) => {
          setActiveId(id);
          // Find which zone this tab is in and focus it
          const zoneIdx = Object.entries(zoneLayout.assignments).find(([, tabId]) => tabId === id);
          if (zoneIdx) {
            zoneLayout.setFocusedZone(Number(zoneIdx[0]));
          }
        }}
        onClose={closeTerminal}
        onCreate={() => createAndAssignTerminal()}
        onRename={renameTab}
        sessionStates={sessionStates}
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
                  onClick={() => {
                    setResetRatiosKey((k) => k + 1);
                  }}
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
              zoneLabels={zoneLabels}
              zoneNotes={zoneNotes}
              pinnedZones={pinnedZones}
              autoApprovePatterns={autoApprovePatterns}
              onLoadProfile={(profile) => {
                zoneLayout.setLayoutId(profile.layoutId);
                setZoneLabels(profile.labels);
                setZoneNotes(profile.notes);
                setPinnedZones(new Set(profile.pins));
                setAutoApprovePatterns(profile.autoApprovePatterns);
              }}
            />
          </div>
        }
        statusSummary={
          zoneLayout.isMultiZone
            ? {
                needsInput: needsInputCount,
                working: workingCount,
                errors: errorCount,
                unseen: unseenNeedsInput.size,
              }
            : undefined
        }
        onQuickLaunch={async (count, autoCommand) => {
          // Pick matching layout
          const layoutMap: Record<number, string> = {
            2: "split",
            4: "quad",
            6: "six-pack",
            9: "full-grid",
          };
          const layoutId = layoutMap[count];
          if (layoutId) zoneLayout.setLayoutId(layoutId);
          // Create terminals sequentially so they get auto-assigned to zones
          const createdTabIds: string[] = [];
          for (let i = 0; i < count; i++) {
            const tabId = await createAndAssignTerminal();
            if (tabId) createdTabIds.push(tabId);
          }
          // Auto-send command to each terminal after a delay for shell to initialize
          if (autoCommand && createdTabIds.length > 0) {
            setTimeout(() => {
              for (const tabId of createdTabIds) {
                const ref = terminalRefs.current.get(tabId);
                ref?.current?.writeToTerminal(`${autoCommand}\r`);
              }
            }, 1500);
          }
        }}
      />
      <TerminalActionBar
        showSidebar={showSidebar}
        onToggleSidebar={() => setShowSidebar((v) => !v)}
        isGenerating={isGenerating}
        isAnalyzing={isAnalyzing}
        onAnalyze={handleAnalyze}
        onGenerateFromSession={handleGenerateFromLatestSession}
        planFileName={planFileName}
        isPlanLoading={isPlanLoading}
        onRefreshPlan={loadPlanContent}
        onBuildPlanFromFile={handleBuildPlanFromFile}
        onToggleFindings={handleToggleFindings}
        findingsActive={rightPanelMode === "findings"}
        findingsCount={activeFindings.length}
      />
      <TerminalNotification
        message={notification?.message ?? null}
        type={notification?.type ?? "success"}
        onDismiss={() => setNotification(null)}
      />

      {/* Status bar (multi-zone only) */}
      {zoneLayout.isMultiZone && (
        <ZoneStatusBar
          tabs={tabs}
          assignments={zoneLayout.assignments}
          sessionStates={sessionStates}
          focusedZone={zoneLayout.focusedZone}
          collapsed={statusBarCollapsed}
          onToggleCollapsed={() => setStatusBarCollapsed((v) => !v)}
          onJumpToNeedsInput={() => zoneLayout.focusNextNeedsInput(sessionStates)}
          onFocusZone={zoneLayout.setFocusedZone}
          onShowShortcuts={() => setShowShortcutsOverlay(true)}
          autoFocus={autoFocusNeedsInput}
          onToggleAutoFocus={toggleAutoFocus}
          soundEnabled={soundEnabled}
          onToggleSound={toggleSound}
          desktopNotify={desktopNotify}
          onToggleDesktopNotify={() => {
            setDesktopNotify((prev) => {
              const next = !prev;
              localStorage.setItem("zone-desktop-notify", String(next));
              return next;
            });
          }}
          stateDurations={stateDurations}
          onSelectByState={(state) => {
            const zones = new Set<number>();
            for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
              if ((sessionStates[tabId] ?? "idle") === state) {
                zones.add(Number(zoneStr));
              }
            }
            setSelectedZones(zones);
          }}
          pinnedZones={pinnedZones}
          staleTabs={staleTabs}
          metrics={metricsRef.current}
          zoneLabels={zoneLabels}
          onSetZoneLabel={(z, label) => setZoneLabels((prev) => ({ ...prev, [z]: label }))}
          flashingTabs={flashingTabs}
          onExport={handleExportOutput}
          onSortZones={handleSortZones}
          eventHistory={eventHistory}
          labelColorMap={labelColorMap}
          focusMode={focusMode}
          autoApprovePatterns={autoApprovePatterns}
          onSetAutoApprovePatterns={setAutoApprovePatterns}
          autoApproveCount={autoApproveCountRef.current}
          stateTimeAccum={stateTimeAccum.current}
          autoRestart={autoRestart}
          onToggleAutoRestart={() => {
            setAutoRestart((prev) => {
              const next = !prev;
              localStorage.setItem("zone-auto-restart", String(next));
              return next;
            });
          }}
          autoRestartCount={autoRestartCountRef.current}
          onToggleFocusMode={() => {
            setFocusMode((prev) => {
              const next = !prev;
              localStorage.setItem("zone-focus-mode", String(next));
              return next;
            });
          }}
          activeTagFilters={activeTagFilters}
          onSetActiveTagFilters={setActiveTagFilters}
          allTags={allTags}
          activityData={activityBuffersRef.current}
          sessionStartTimes={stateEntryTimeRef.current}
          lastOutputLines={lastOutputLines}
          unreadTabs={unreadZones}
          onApproveTab={(tabId) => {
            terminalRefs.current.get(tabId)?.current?.writeToTerminal("y\r");
            metricsRef.current.totalApprovals++;
          }}
          onRestartZone={handleRestartInZone}
          onTogglePin={(zoneIdx) => {
            setPinnedZones((prev) => {
              const next = new Set(prev);
              if (next.has(zoneIdx)) next.delete(zoneIdx);
              else next.add(zoneIdx);
              return next;
            });
          }}
        />
      )}

      {/* Zone timeline (multi-zone only) */}
      {showTimeline && zoneLayout.isMultiZone && (
        <ZoneTimeline
          tabs={tabs}
          assignments={zoneLayout.assignments}
          sessionStates={sessionStates}
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
              const matchCount = Object.entries(lastOutputLines).filter(([, lines]) =>
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
        {showSidebar && (
          <TranscriptSessionSidebar
            sessions={sessions}
            loading={sessionsLoading}
            selectedSessionId={selectedTranscriptSessionId}
            onSelectSession={handleSelectTranscriptSession}
            onRefresh={refreshSessions}
            onResume={handleResumeSession}
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
              sessionStates={sessionStates}
              lastOutputLines={lastOutputLines}
              viewMode={viewMode}
              terminalRefs={terminalRefs.current}
              onZoneClick={handleZoneClick}
              onZoneDoubleClick={handleZoneDoubleClick}
              onExit={handleExit}
              onFirstInput={handleFirstInput}
              onShellIntegration={handleShellIntegration}
              onOutput={handleOutput}
              onReconnected={markReconnected}
              onAssignTab={zoneLayout.assignTabToZone}
              flashingTabs={flashingTabs}
              stateDurations={stateDurations}
              selectedZones={selectedZones}
              staleTabs={staleTabs}
              pinnedZones={pinnedZones}
              onTogglePin={(zoneIdx) => {
                setPinnedZones((prev) => {
                  const next = new Set(prev);
                  if (next.has(zoneIdx)) next.delete(zoneIdx);
                  else next.add(zoneIdx);
                  return next;
                });
              }}
              outputSearchQuery={outputSearch || undefined}
              swapSource={swapSource}
              activityData={activityBuffersRef.current}
              zoneLabels={zoneLabels}
              onSetZoneLabel={(zoneIdx, label) => {
                setZoneLabels((prev) => {
                  if (!label) {
                    const next = { ...prev };
                    delete next[zoneIdx];
                    return next;
                  }
                  return { ...prev, [zoneIdx]: label };
                });
              }}
              onRestartInZone={handleRestartInZone}
              resetRatiosKey={resetRatiosKey}
              labelColorMap={labelColorMap}
              zoneTags={Object.fromEntries(
                Object.entries(zoneLabels).map(([z, label]) => [
                  Number(z),
                  label
                    ? label
                        .split(",")
                        .map((t) => t.trim())
                        .filter(Boolean)
                    : [],
                ]),
              )}
              commandHistories={commandHistories}
              focusMode={focusMode}
              zoneNotes={zoneNotes}
              onSetZoneNote={(zoneIdx, note) => {
                setZoneNotes((prev) => {
                  if (!note) {
                    const next = { ...prev };
                    delete next[zoneIdx];
                    return next;
                  }
                  return { ...prev, [zoneIdx]: note };
                });
              }}
              onExportZone={handleExportZone}
              pendingRestarts={pendingRestarts}
              onCancelRestart={cancelPendingRestart}
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
              sessionStates={sessionStates}
              focusedZone={zoneLayout.focusedZone}
              onFocusZone={zoneLayout.setFocusedZone}
              zoneTags={Object.fromEntries(
                Object.entries(zoneLabels).map(([z, label]) => [
                  Number(z),
                  label
                    ? label
                        .split(",")
                        .map((t) => t.trim())
                        .filter(Boolean)
                    : [],
                ]),
              )}
              labelColorMap={labelColorMap}
            />
          )}

          {/* Batch operations floating bar */}
          {zoneLayout.isMultiZone && !batchBarDismissed && (
            <BatchOperationsBar
              tabs={tabs}
              sessionStates={sessionStates}
              terminalRefs={terminalRefs.current}
              onDismiss={() => setBatchBarDismissed(true)}
              selectedZones={selectedZones}
              assignments={zoneLayout.assignments}
              zoneLabels={zoneLabels}
              onSelectAllWaiting={() => {
                const waiting = new Set<number>();
                for (const [zoneStr, tabId] of Object.entries(zoneLayout.assignments)) {
                  if (sessionStates[tabId] === "needs-input") {
                    waiting.add(Number(zoneStr));
                  }
                }
                setSelectedZones(waiting);
              }}
              onClearSelection={() => setSelectedZones(new Set())}
              onMetrics={(type, count) => {
                if (type === "approve") {
                  metricsRef.current.totalApprovals += count;
                  addHistoryEvent("Batch approve", `${count} sessions`, undefined, "#9ece6a");
                } else if (type === "reject") {
                  metricsRef.current.totalRejections += count;
                  addHistoryEvent("Batch reject", `${count} sessions`, undefined, "#f7768e");
                } else if (type === "broadcast") {
                  metricsRef.current.totalBroadcasts += count;
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
            sessionStates={sessionStates}
            zoneLabels={zoneLabels}
            zoneNotes={zoneNotes}
            labelColorMap={labelColorMap}
            focusedZone={zoneLayout.focusedZone}
            zoneCount={zoneLayout.layout.zones.length}
            lastOutputLines={lastOutputLines}
            onFocusZone={zoneLayout.setFocusedZone}
            onSetZoneLabel={(zoneIdx, label) => {
              setZoneLabels((prev) => {
                if (!label) {
                  const next = { ...prev };
                  delete next[zoneIdx];
                  return next;
                }
                return { ...prev, [zoneIdx]: label };
              });
            }}
            onSetZoneNotes={(zoneIdx, note) => {
              setZoneNotes((prev) => {
                if (!note) {
                  const next = { ...prev };
                  delete next[zoneIdx];
                  return next;
                }
                return { ...prev, [zoneIdx]: note };
              });
            }}
            onClose={() => {
              setShowControlPanel(false);
              localStorage.setItem("zone-control-panel", "false");
            }}
            collapsed={controlPanelCollapsed}
            onToggleCollapsed={() => setControlPanelCollapsed((v) => !v)}
            onCreateTerminal={() => createAndAssignTerminal()}
            pinnedZones={pinnedZones}
            onTogglePin={(zoneIdx) => {
              setPinnedZones((prev) => {
                const next = new Set(prev);
                if (next.has(zoneIdx)) next.delete(zoneIdx);
                else next.add(zoneIdx);
                return next;
              });
            }}
            onSwapZones={(src, dst) => {
              const srcTabId = zoneLayout.assignments[src];
              const dstTabId = zoneLayout.assignments[dst];
              if (srcTabId) zoneLayout.assignTabToZone(dst, srcTabId);
              if (dstTabId) zoneLayout.assignTabToZone(src, dstTabId);
            }}
            onLoadWorkspace={async (workspace) => {
              // Switch layout if needed
              if (workspace.layoutId !== zoneLayout.layoutId) {
                zoneLayout.setLayoutId(workspace.layoutId);
              }
              // Create terminals for each session in the workspace
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
                  setZoneLabels((prev) => ({ ...prev, [session.zoneIndex]: session.label! }));
                }
                if (session.notes) {
                  setZoneNotes((prev) => ({ ...prev, [session.zoneIndex]: session.notes! }));
                }
                if (session.pinned) {
                  setPinnedZones((prev) => new Set([...prev, session.zoneIndex]));
                }
              }
            }}
            layoutId={zoneLayout.layoutId}
          />
        )}

        {/* Right panel — transcript content OR workflow preview */}
        {rightPanelMode === "transcript" && selectedTranscriptSessionId && (
          <TranscriptContentPanel
            sessionId={selectedTranscriptSessionId}
            session={sessions.find((s) => s.session_id === selectedTranscriptSessionId) ?? null}
            messages={transcriptMessages}
            loading={loadingMessages}
            onGenerate={handleGenerateFromTranscript}
            onGenerateAndRun={handleGenerateAndRunFromTranscript}
            onBuildPlanWorkflow={handleBuildPlanWorkflow}
            onResume={handleResumeSession}
            onClose={() => {
              setRightPanelMode(null);
              setSelectedTranscriptSessionId(null);
            }}
          />
        )}
        {rightPanelMode === "workflow" && (
          <div className="w-[420px] h-full shrink-0">
            <WorkflowPreviewPanel
              workflow={generatedWorkflow}
              isLoading={isGenerating}
              error={workflowError}
              onExecute={handleExecute}
              onEditInBuilder={handleEditInBuilder}
              onRegenerate={handleRegenerate}
              onSave={handleSaveWorkflow}
              onClose={() => setRightPanelMode(null)}
            />
          </div>
        )}
        {rightPanelMode === "analysis" && (
          <TerminalAnalysisPanel
            analysisType={analysisType}
            panels={analysisPanels}
            isAnalyzing={isAnalyzing}
            error={analysisError}
            onClose={() => setRightPanelMode(null)}
          />
        )}
        {rightPanelMode === "findings" && (
          <TerminalFindingsPanel
            findings={activeFindings}
            allFindings={allFindings}
            onClose={() => setRightPanelMode(null)}
            onRespond={handleFindingRespond}
            onFix={handleFixFinding}
            onGenerateWorkflow={handleGenerateFromFindings}
          />
        )}
      </div>

      {showShortcutsOverlay && (
        <KeyboardShortcutsOverlay onClose={() => setShowShortcutsOverlay(false)} />
      )}

      {diffZones &&
        (() => {
          const [z1, z2] = diffZones;
          const tab1 = tabs.find((t) => t.id === zoneLayout.assignments[z1]);
          const tab2 = tabs.find((t) => t.id === zoneLayout.assignments[z2]);
          return (
            <ZoneDiffOverlay
              leftLabel={`Zone ${z1 + 1}: ${tab1?.title ?? "empty"}`}
              rightLabel={`Zone ${z2 + 1}: ${tab2?.title ?? "empty"}`}
              leftLines={tab1 ? (lastOutputLines[tab1.id] ?? []) : []}
              rightLines={tab2 ? (lastOutputLines[tab2.id] ?? []) : []}
              onClose={() => setDiffZones(null)}
            />
          );
        })()}

      {snapshotDiff && (
        <ZoneDiffOverlay
          leftLabel="Snapshot"
          rightLabel="Current"
          leftLines={snapshotDiff.snapshot}
          rightLines={snapshotDiff.current}
          onClose={() => setSnapshotDiff(null)}
        />
      )}

      {showCommandPalette && (
        <CommandPalette
          onClose={() => setShowCommandPalette(false)}
          tabs={tabs}
          assignments={zoneLayout.assignments}
          sessionStates={sessionStates}
          focusedZone={zoneLayout.focusedZone}
          onFocusZone={zoneLayout.setFocusedZone}
          onApproveTab={(tabId) => {
            terminalRefs.current.get(tabId)?.current?.writeToTerminal("y\r");
            metricsRef.current.totalApprovals++;
          }}
          onRejectTab={(tabId) => {
            terminalRefs.current.get(tabId)?.current?.writeToTerminal("n\r");
            metricsRef.current.totalRejections++;
          }}
          onRestartZone={handleRestartInZone}
          onTogglePin={(z) => {
            setPinnedZones((prev) => {
              const next = new Set(prev);
              if (next.has(z)) next.delete(z);
              else next.add(z);
              return next;
            });
          }}
          pinnedZones={pinnedZones}
          onApproveAll={() => {
            const ni = tabs.filter((t) => sessionStates[t.id] === "needs-input");
            metricsRef.current.totalApprovals += ni.length;
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
          onToggleAutoFocus={toggleAutoFocus}
          autoFocus={autoFocusNeedsInput}
          onToggleSound={toggleSound}
          soundEnabled={soundEnabled}
          zoneLabels={zoneLabels}
          onSetZoneLabel={(z, label) => {
            setZoneLabels((prev) => {
              if (!label) {
                const next = { ...prev };
                delete next[z];
                return next;
              }
              return { ...prev, [z]: label };
            });
          }}
          zoneCount={zoneLayout.layout.zones.length}
          onCompareZones={(z1, z2) => {
            setShowCommandPalette(false);
            setDiffZones([z1, z2]);
          }}
          onSnapshotZone={(tabId) => {
            outputSnapshotsRef.current[tabId] = [...(lastOutputLines[tabId] ?? [])];
            setSnapshotCounter((c) => c + 1);
          }}
          onCompareSnapshot={(tabId) => {
            const snapshot = outputSnapshotsRef.current[tabId];
            if (snapshot) {
              setSnapshotDiff({
                tabId,
                snapshot,
                current: lastOutputLines[tabId] ?? [],
              });
            }
            setShowCommandPalette(false);
          }}
          snapshotZones={snapshotZones}
        />
      )}
    </div>
  );
}
