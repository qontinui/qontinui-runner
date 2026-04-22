/**
 * AiFeaturesContext
 *
 * Owns workflow generation, analysis, findings, transcript sessions,
 * session manager, shell integration, and session summary.
 * This is the largest context — it encapsulates the entire AI feature set
 * of the Terminal page.
 */

import { createContext, useState, useCallback, useRef, useMemo, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTerminalCore } from "./useTerminalCore";
import { useSessionState } from "./useSessionState";
import { useTransitionEffects } from "./useTransitionEffects";
import { useShellIntegration } from "../useShellIntegration";
import { useWorkflowGeneration } from "../useWorkflowGeneration";
import { useAnalysis } from "../useAnalysis";
import { useFindingsActions } from "../useFindingsActions";
import { useTranscriptSessions } from "../useTranscriptSessions";
import { useSessionManager } from "../useSessionManager";

export interface AiFeaturesContextValue {
  shellIntegration: ReturnType<typeof useShellIntegration>;
  workflowGen: ReturnType<typeof useWorkflowGeneration>;
  analysis: ReturnType<typeof useAnalysis>;
  findingsActions: ReturnType<typeof useFindingsActions>;
  sessionManager: ReturnType<typeof useSessionManager>;
  transcriptSessions: ReturnType<typeof useTranscriptSessions>["sessions"];
  sessionsLoading: boolean;
  refreshSessions: () => void;
  loadMessages: ReturnType<typeof useTranscriptSessions>["loadMessages"];
  sessionSummary: string | null;
  sessionSummaryLoading: boolean;
  handleSummarizeSession: (messages: Array<{ msg_type: string; text: string }>) => Promise<void>;
  getScrollback: (tabId: string, maxLines?: number) => string;
  getActiveSelection: () => string;
}

export const AiFeaturesContext = createContext<AiFeaturesContextValue | null>(null);

interface AiFeaturesProviderProps {
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
  children: ReactNode;
}

export function AiFeaturesProvider({
  onNavigateToBuilder,
  onNavigateToActive,
  children,
}: AiFeaturesProviderProps) {
  const { tabs, activeId, updateTab, renameTab, createTerminal, terminalRefs } = useTerminalCore();
  const stateTracking = useSessionState();
  const transitionEffects = useTransitionEffects();

  // Cross-hook ref wiring: shellIntegration needs setRightPanelMode from workflowGen,
  // but workflowGen is defined after shellIntegration. Refs break the cycle.
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
        const text = messages
          .map((m) => `${m.msg_type === "user" ? "User" : "Assistant"}: ${m.text}`)
          .join("\n\n");
        const result = await invoke<{ success: boolean; data?: unknown; message?: string }>(
          "analyze_session_summary",
          { input: text },
        );
        if (result.success && result.data) {
          const data = result.data as
            | Array<{ type?: string; content?: string }>
            | { panels?: Array<{ content?: string }> };
          let summaryContent: string | null = null;

          if (Array.isArray(data)) {
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

  // Wire the cross-hook refs now that workflowGen is defined

  rightPanelModeSetterRef.current = workflowGen.setRightPanelMode;

  selectedSessionSetterRef.current = workflowGen.setSelectedTranscriptSessionId;

  const value = useMemo<AiFeaturesContextValue>(
    () => ({
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

  return <AiFeaturesContext.Provider value={value}>{children}</AiFeaturesContext.Provider>;
}
