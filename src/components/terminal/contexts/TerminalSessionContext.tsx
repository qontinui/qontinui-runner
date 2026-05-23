/**
 * TerminalSessionContext — plan 2026-05-22-coord-native-session-coordination
 * §Phase 4.
 *
 * Collapses the previous 4-way split into one provider with one
 * `useTerminalSession()` hook. The 4 collapsed contexts:
 *
 *   - TerminalCoreContext   (PTY tabs, zone layout, terminal refs)
 *   - SessionStateContext   (per-tab session-state machine, snapshots,
 *                            findings, unread tracking)
 *   - ShellInfraContext     (file conflicts, file lock tracking, session
 *                            persistence)
 *   - AiFeaturesContext     (workflow gen, analysis, transcripts,
 *                            session manager, shell integration, session
 *                            summary)
 *
 * Kept SEPARATE per the plan (orthogonal UI concerns, not session
 * state):
 *
 *   - TransitionEffectsContext
 *   - UIStateContext
 *   - ZoneMetadataContext
 *
 * The new src/contexts/SessionContext.tsx is a different concept — it
 * wraps the COORD-native session_* Tauri surface. This file is the
 * runner's LOCAL terminal-page state collapse. The two are intentional
 * separate scopes per plan §Phase 4 / §D6 (typed intent → coord) and
 * the runner's existing terminal-page wiring (which is heavy with
 * coord-unrelated UI state like zone layout, AI workflow generation,
 * findings, transcripts).
 *
 * Provider order in TerminalPage.tsx:
 *
 *   <TerminalSessionProvider pageId={pageId}>
 *     <ZoneMetadataProvider>
 *       <TransitionEffectsProvider>
 *         <UIStateProvider>
 *           {children}
 *         </UIStateProvider>
 *       </TransitionEffectsProvider>
 *     </ZoneMetadataProvider>
 *   </TerminalSessionProvider>
 *
 * The previously-circular dependencies (TransitionEffects + ZoneMetadata
 * read from TerminalCore + SessionState) collapse to a single
 * upward read from `useTerminalSession()` — no cycle.
 */

import {
  createContext,
  useState,
  useCallback,
  useEffect,
  useRef,
  useMemo,
  createRef,
  useContext,
  type ReactNode,
  type RefObject,
} from "react";
import { invoke } from "@tauri-apps/api/core";

import { useTerminalManager } from "../useTerminalManager";
import { useZoneLayout } from "../useZoneLayout";
import { type TerminalInstanceHandle } from "../TerminalInstance";
import { type ZoneSessionInfo } from "../ZoneProfilePicker";
import { writeWhenReady } from "../writeWhenReady";

import { useSessionStateTracking } from "../useSessionStateTracking";
import { useUnreadTracking } from "../useUnreadTracking";
import { useOutputSnapshots } from "../useOutputSnapshots";
import { useTerminalFindings } from "../useTerminalFindings";

import { useFileConflicts } from "../useFileConflicts";
import {
  useFileLockTracking,
  type LockState,
  type IncomingYieldRequest,
  type IncomingLongWaitSignal,
} from "../useFileLockTracking";
import { useSessionPersistence } from "../useSessionPersistence";

import { useShellIntegration } from "../useShellIntegration";
import { useWorkflowGeneration } from "../useWorkflowGeneration";
import { useAnalysis } from "../useAnalysis";
import { useFindingsActions } from "../useFindingsActions";
import { useTranscriptSessions } from "../useTranscriptSessions";
import { useSessionManager } from "../useSessionManager";

import { instanceStorage } from "@/lib/instance-storage";

// ---------------------------------------------------------------------------
// Return-type aliases — preserve the prior shapes one-for-one so
// downstream consumers don't see a change in the value-object signature.
// ---------------------------------------------------------------------------

type TerminalManagerReturn = ReturnType<typeof useTerminalManager>;
type ZoneLayoutReturn = ReturnType<typeof useZoneLayout>;
type StateTrackingReturn = ReturnType<typeof useSessionStateTracking>;
type SnapshotsReturn = ReturnType<typeof useOutputSnapshots>;
type FindingsReturn = ReturnType<typeof useTerminalFindings>;
type ShellIntegrationReturn = ReturnType<typeof useShellIntegration>;
type WorkflowGenReturn = ReturnType<typeof useWorkflowGeneration>;
type AnalysisReturn = ReturnType<typeof useAnalysis>;
type FindingsActionsReturn = ReturnType<typeof useFindingsActions>;
type SessionManagerReturn = ReturnType<typeof useSessionManager>;
type FileConflictsReturn = ReturnType<typeof useFileConflicts>;
type SessionPersistenceReturn = ReturnType<typeof useSessionPersistence>;

/**
 * Flat value-object exposed by `useTerminalSession()`. The shape is the
 * union of the 4 prior context values; field names are PRESERVED to
 * keep downstream callsites mechanical to migrate.
 *
 * The two large `extends` clauses (`TerminalManagerReturn`,
 * `StateTrackingReturn`) flatten the prior TerminalCore + SessionState
 * surfaces so every field shows up directly on the value-object instead
 * of nested under `terminalManager.*` / `stateTracking.*`. Behavior
 * matches the prior consumers, which previously destructured these
 * fields off `useTerminalCore()` / `useSessionState()` directly.
 */
export interface TerminalSessionContextValue
  extends TerminalManagerReturn,
    StateTrackingReturn {
  // — TerminalCore extras —
  pageId: string;
  zoneLayout: ZoneLayoutReturn;
  terminalRefs: React.MutableRefObject<Map<string, RefObject<TerminalInstanceHandle | null>>>;
  pendingProfileSessionsRef: React.MutableRefObject<ZoneSessionInfo[] | null>;

  // — SessionState extras —
  unreadZones: ReturnType<typeof useUnreadTracking>["unreadZones"];
  snapshots: SnapshotsReturn;
  processOutputRef: React.MutableRefObject<((tabId: string, text: string) => void) | undefined>;
  activeFindings: FindingsReturn["activeFindings"];
  allFindings: FindingsReturn["allFindings"];

  // — ShellInfra —
  fileConflicts: FileConflictsReturn;
  /** Per-tab lock state keyed by tab.id. */
  fileLockStates: Record<string, LockState>;
  /** Per-tab incoming yield request queues keyed by holder tab.id. */
  pendingYieldRequests: Record<string, IncomingYieldRequest[]>;
  /** Per-tab incoming long-wait signals keyed by WAITER tab.id. */
  pendingLongWaitSignals: Record<string, IncomingLongWaitSignal[]>;
  sessionPersistence: SessionPersistenceReturn;

  // — AiFeatures —
  shellIntegration: ShellIntegrationReturn;
  workflowGen: WorkflowGenReturn;
  analysis: AnalysisReturn;
  findingsActions: FindingsActionsReturn;
  sessionManager: SessionManagerReturn;
  transcriptSessions: ReturnType<typeof useTranscriptSessions>["sessions"];
  sessionsLoading: boolean;
  refreshSessions: () => void;
  loadMessages: ReturnType<typeof useTranscriptSessions>["loadMessages"];
  sessionSummary: string | null;
  sessionSummaryLoading: boolean;
  handleSummarizeSession: (
    messages: Array<{ msg_type: string; text: string }>,
  ) => Promise<void>;
  getScrollback: (tabId: string, maxLines?: number) => string;
  getActiveSelection: () => string;
}

export const TerminalSessionContext =
  createContext<TerminalSessionContextValue | null>(null);

interface TerminalSessionProviderProps {
  pageId: string;
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  children: ReactNode;
}

/**
 * The collapsed provider. Owns every piece of state previously split
 * across TerminalCore + SessionState + ShellInfra + AiFeatures. Wire
 * order matches the prior provider nesting precisely so the underlying
 * hooks see the same input timing.
 */
export function TerminalSessionProvider({
  pageId,
  onNavigateToBuilder,
  onNavigateToActive,
  children,
}: TerminalSessionProviderProps) {
  // ---- TerminalCore ----
  const terminalManager = useTerminalManager(pageId);
  const { tabs, activeId, setActiveId, updateTab, renameTab, createTerminal } =
    terminalManager;

  const tabIds = useMemo(() => tabs.map((t) => t.id), [tabs]);
  const zoneLayout = useZoneLayout(tabIds, pageId);

  // Sync focused zone → active tab.
  useEffect(() => {
    if (
      zoneLayout.focusedTabId &&
      zoneLayout.focusedTabId !== activeId &&
      tabs.some((t) => t.id === zoneLayout.focusedTabId)
    ) {
      setActiveId(zoneLayout.focusedTabId);
    }
  }, [zoneLayout.focusedTabId, activeId, setActiveId, tabs]);

  // Terminal refs map — create refs for new tabs, clean up stale ones.
  const terminalRefs = useRef<Map<string, RefObject<TerminalInstanceHandle | null>>>(
    new Map(),
  );

  useEffect(() => {
    const map = terminalRefs.current;
    for (const tab of tabs) {
      if (!map.has(tab.id)) {
        map.set(tab.id, createRef<TerminalInstanceHandle>());
      }
    }
    for (const key of map.keys()) {
      if (!tabs.some((t) => t.id === key)) {
        map.delete(key);
      }
    }
  }, [tabs]);

  // Pending Claude sessions to resume after a zone profile load settles.
  const pendingProfileSessionsRef = useRef<ZoneSessionInfo[] | null>(null);

  useEffect(() => {
    const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
    const sessions = pendingProfileSessionsRef.current;
    if (!sessions || sessions.length === 0) return;

    const isWindows = navigator.platform.startsWith("Win");
    const buildResumeCmd = (sessionId: string, configDir: string | undefined) => {
      const base = `claude --resume ${sessionId}`;
      if (!configDir) return `${base}\r`;
      return isWindows
        ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; ${base}\r`
        : `CLAUDE_CONFIG_DIR="${configDir}" ${base}\r`;
    };

    // Process only sessions whose zone now has an assignment; leave the
    // rest in the ref for the next assignments tick.
    const remaining: ZoneSessionInfo[] = [];
    for (const s of sessions) {
      const tabId = zoneLayout.assignments[s.zoneIndex];
      if (tabId && SESSION_ID_RE.test(s.claudeSessionId)) {
        updateTab(tabId, {
          claudeSessionId: s.claudeSessionId,
          claudeConfigDir: s.claudeConfigDir,
        });
        writeWhenReady(
          terminalRefs.current,
          tabId,
          buildResumeCmd(s.claudeSessionId, s.claudeConfigDir),
          {
            onTimeout: (id) =>
              console.warn(
                `[TerminalSession] profile resume: terminal ref for ${id} never became ready`,
              ),
          },
        );
      } else {
        remaining.push(s);
      }
    }
    pendingProfileSessionsRef.current = remaining.length > 0 ? remaining : null;
  }, [zoneLayout.assignments, updateTab]);

  // ---- SessionState ----
  const processOutputRef = useRef<((tabId: string, text: string) => void) | undefined>(
    undefined,
  );

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

  const { unreadZones } = useUnreadTracking(
    zoneLayout.focusedZone,
    zoneLayout.assignments,
    stateTracking.lastOutputLines,
  );

  const snapshots = useOutputSnapshots(stateTracking.lastOutputLines);

  const { processOutput, activeFindings, allFindings } = useTerminalFindings(
    activeId ?? null,
  );

  // Wire findings processOutput into stateTracking's processOutputRef.
  useEffect(() => {
    processOutputRef.current = processOutput;
  }, [processOutput]);

  // ---- ShellInfra ----
  const fileConflicts = useFileConflicts();
  const {
    lockStates: fileLockStates,
    pendingYieldRequests,
    pendingLongWaitSignals,
  } = useFileLockTracking(tabs);
  const sessionPersistence = useSessionPersistence(pageId);

  // ---- AiFeatures ----
  // Cross-hook ref wiring — shellIntegration needs setRightPanelMode
  // from workflowGen, but workflowGen is defined after shellIntegration.
  // Refs break the cycle, same pattern as the prior AiFeaturesProvider.
  const rightPanelModeSetterRef = useRef<
    React.Dispatch<
      React.SetStateAction<
        | "transcript"
        | "workflow"
        | "analysis"
        | "findings"
        | "file-ownership"
        | null
      >
    >
  >(() => {});
  const selectedSessionSetterRef = useRef<
    React.Dispatch<React.SetStateAction<string | null>>
  >(() => {});

  const shellIntegration = useShellIntegration({
    tabs,
    updateTab,
    renameTab,
    createTerminal,
    setSessionStates: stateTracking.setSessionStates,
    terminalRefs,
    setRightPanelMode: (v) => rightPanelModeSetterRef.current(v as never),
    setSelectedTranscriptSessionId: (v) =>
      selectedSessionSetterRef.current(v as never),
  });

  const getScrollback = useCallback(
    (tabId: string, maxLines = 500): string => {
      const ref = terminalRefs.current.get(tabId);
      return ref?.current?.getScrollback?.(maxLines) ?? "";
    },
    [terminalRefs],
  );

  const getActiveSelection = useCallback((): string => {
    if (!activeId) return "";
    const ref = terminalRefs.current.get(activeId);
    return ref?.current?.getSelection?.() ?? "";
  }, [activeId, terminalRefs]);

  const {
    sessions: transcriptSessions,
    loading: sessionsLoading,
    refresh: refreshSessions,
    loadMessages,
  } = useTranscriptSessions();

  // `desktopNotify` was previously read off TransitionEffectsContext
  // (which lived ABOVE AiFeaturesContext in the prior provider tree).
  // Post-collapse, TerminalSession is the OUTERMOST provider and
  // TransitionEffects depends on it — so we can't reach in. The flag
  // is persisted in `instanceStorage` ("zone-desktop-notify") by the
  // same hook that owns it (`useStateTransitionEffects`); reading
  // directly here keeps the wire-up identical. Reactive to changes
  // through a `storage`-style poll would be over-engineering — the
  // operator toggling the setting can wait for the next session-
  // manager re-render (which fires on every session-state delta
  // anyway via the upstream sessionStates dep).
  const desktopNotifyFlag = instanceStorage.getItem("zone-desktop-notify") === "true";

  const sessionManager = useSessionManager({
    tabs,
    sessionStates: stateTracking.sessionStates,
    staleTabs: stateTracking.staleTabs,
    transcriptSessions,
    sessionsLoading,
    desktopNotify: desktopNotifyFlag,
    onRefreshSessions: refreshSessions,
    onResumeSession: shellIntegration.handleResumeSession,
    onSelectSession: (sessionId: string) => {
      selectedSessionSetterRef.current(sessionId);
      rightPanelModeSetterRef.current("transcript" as never);
    },
  });

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

  // Session summary (AI Tier 2).
  const [sessionSummary, setSessionSummary] = useState<string | null>(null);
  const [sessionSummaryLoading, setSessionSummaryLoading] = useState(false);

  const handleSummarizeSession = useCallback(
    async (messages: Array<{ msg_type: string; text: string }>) => {
      setSessionSummaryLoading(true);
      setSessionSummary(null);
      try {
        const text = messages
          .map(
            (m) => `${m.msg_type === "user" ? "User" : "Assistant"}: ${m.text}`,
          )
          .join("\n\n");
        const result = await invoke<{
          success: boolean;
          data?: unknown;
          message?: string;
        }>("analyze_session_summary", { input: text });
        if (result.success && result.data) {
          const data = result.data as
            | Array<{ type?: string; content?: string }>
            | { panels?: Array<{ content?: string }> };
          let summaryContent: string | null = null;

          if (Array.isArray(data)) {
            const markdownPanel = data.find(
              (p) => p.type === "markdown" || p.content,
            );
            summaryContent = markdownPanel?.content ?? null;
          } else if (data && "panels" in data && Array.isArray(data.panels)) {
            summaryContent = data.panels[0]?.content ?? null;
          }

          setSessionSummary(
            summaryContent || "Summary generated but no content available.",
          );
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

  // Wire the cross-hook refs now that workflowGen is defined.
  useEffect(() => {
    rightPanelModeSetterRef.current = workflowGen.setRightPanelMode;
    selectedSessionSetterRef.current = workflowGen.setSelectedTranscriptSessionId;
  });

  // Memoize the value object. Keys in deps list mirror the prior
  // 4-provider memo signals so re-renders are stable.
  const value = useMemo<TerminalSessionContextValue>(
    () => ({
      // TerminalCore
      ...terminalManager,
      pageId,
      zoneLayout,
      terminalRefs,
      pendingProfileSessionsRef,
      // SessionState (spread)
      ...stateTracking,
      unreadZones,
      snapshots,
      processOutputRef,
      activeFindings,
      allFindings,
      // ShellInfra
      fileConflicts,
      fileLockStates,
      pendingYieldRequests,
      pendingLongWaitSignals,
      sessionPersistence,
      // AiFeatures
      shellIntegration,
      workflowGen,
      analysis,
      findingsActions,
      sessionManager,
      transcriptSessions,
      sessionsLoading,
      refreshSessions,
      loadMessages,
      sessionSummary,
      sessionSummaryLoading,
      handleSummarizeSession,
      getScrollback,
      getActiveSelection,
    }),
    [
      terminalManager,
      pageId,
      zoneLayout,
      stateTracking,
      unreadZones,
      snapshots,
      activeFindings,
      allFindings,
      fileConflicts,
      fileLockStates,
      pendingYieldRequests,
      pendingLongWaitSignals,
      sessionPersistence,
      shellIntegration,
      workflowGen,
      analysis,
      findingsActions,
      sessionManager,
      transcriptSessions,
      sessionsLoading,
      refreshSessions,
      loadMessages,
      sessionSummary,
      sessionSummaryLoading,
      handleSummarizeSession,
      getScrollback,
      getActiveSelection,
    ],
  );

  return (
    <TerminalSessionContext.Provider value={value}>
      {children}
    </TerminalSessionContext.Provider>
  );
}

/**
 * The new single hook. Replaces the previous four:
 *
 *   - useTerminalCore()
 *   - useSessionState()  (terminal-page variant — NOT the hooks/ one)
 *   - useShellInfra()
 *   - useAiFeatures()
 *
 * Throws when called outside a `TerminalSessionProvider`.
 */
export function useTerminalSession(): TerminalSessionContextValue {
  const ctx = useContext(TerminalSessionContext);
  if (!ctx) {
    throw new Error(
      "useTerminalSession must be used within a TerminalSessionProvider",
    );
  }
  return ctx;
}
