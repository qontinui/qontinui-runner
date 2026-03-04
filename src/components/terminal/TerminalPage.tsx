import { useEffect, useCallback, useRef, useState, createRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { TerminalInstance, type TerminalInstanceHandle, type ShellIntegrationEvent } from "./TerminalInstance";
import { TerminalTabBar } from "./TerminalTabBar";
import { TerminalActionBar } from "./TerminalActionBar";
import { TerminalNotification } from "./TerminalNotification";
import { TranscriptSessionSidebar } from "./TranscriptSessionSidebar";
import { TranscriptContentPanel } from "./TranscriptContentPanel";
import { useTranscriptSessions, type TranscriptMessage, type TranscriptSession } from "./useTranscriptSessions";
import { TerminalAnalysisPanel, type AnalysisType } from "./TerminalAnalysisPanel";
import { useTerminalManager } from "./useTerminalManager";
import { WorkflowPreviewPanel } from "@qontinui/workflow-ui";
import type { UnifiedWorkflow, CanvasPanel } from "@qontinui/shared-types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

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
}

export function TerminalPage({ onNavigateToBuilder, onNavigateToActive }: TerminalPageProps) {
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
    "transcript" | "workflow" | "analysis" | null
  >(null);

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

  // On mount: try to reconnect to existing Rust PTY sessions, else create a fresh terminal
  const didInit = useRef(false);
  useEffect(() => {
    if (didInit.current) return;
    didInit.current = true;

    (async () => {
      const reconnected = await reconnectToExistingSessions();
      if (!reconnected) {
        await createTerminal();
      }
      setInitialized(true);
    })();
  }, [reconnectToExistingSessions, createTerminal, setInitialized]);

  // Load plan content once on mount (best-effort)
  useEffect(() => {
    loadPlanContent();
  }, [loadPlanContent]);

  const handleExit = useCallback(
    (terminalId: string, exitCode: number | null) => {
      updateTab(terminalId, { isAlive: false, exitCode });
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
            ref?.current?.writeToTerminal(`claude --resume ${pending.sessionId}\r`);
          }, 50);
        }
      }
      if (event.type === "cwd") {
        updateTab(tabId, { workingDir: event.path });
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
    [updateTab],
  );

  // ── Resume Claude Code session in terminal ─────────────────────────────────

  // Tracks the tab ID and session ID awaiting the first shell prompt to send the command.
  const pendingResumeRef = useRef<{ tabId: string; sessionId: string } | null>(null);

  const handleResumeSession = useCallback(
    async (session: TranscriptSession) => {
      // Derive a short label from the session ID for the tab title
      const tabTitle = `claude ${session.session_id.slice(0, 8)}`;
      const tabId = await createTerminal(tabTitle, session.project_path);
      if (!tabId) return;

      // Close the transcript panel so the terminal is visible
      setRightPanelMode(null);
      setSelectedTranscriptSessionId(null);

      // Queue the resume command — it will be sent once the shell emits its first prompt
      pendingResumeRef.current = { tabId, sessionId: session.session_id };

      // Fallback: send after 1.5 s regardless (in case shell integration isn't active)
      setTimeout(() => {
        const pending = pendingResumeRef.current;
        if (!pending || pending.tabId !== tabId) return;
        pendingResumeRef.current = null;
        const ref = terminalRefs.current.get(tabId);
        ref?.current?.writeToTerminal(`claude --resume ${pending.sessionId}\r`);
      }, 1500);
    },
    [createTerminal],
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

  // ── Generate from latest session ──────────────────────────────────────────

  const handleGenerateFromLatestSession = useCallback(async () => {
    try {
      const result = await invoke<CommandResponse>("transcript_get_latest", {});
      if (result.success && result.data) {
        const session = result.data as { session_id: string };
        // Open sidebar so the user can see the session list
        setShowSidebar(true);
        // Load and display the session's messages
        await handleSelectTranscriptSession(session.session_id);
      } else {
        setNotification({
          message: "No Claude Code sessions found",
          type: "error",
        });
      }
    } catch (err) {
      setNotification({
        message: `Failed to detect session: ${err instanceof Error ? err.message : err}`,
        type: "error",
      });
    }
  }, [handleSelectTranscriptSession]);

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

  // ── Keyboard shortcuts ────────────────────────────────────────────────────

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Ctrl+Shift+T — new terminal
      if (e.ctrlKey && e.shiftKey && e.key === "T") {
        e.preventDefault();
        createTerminal();
        return;
      }
      // Ctrl+Shift+W — close active terminal
      if (e.ctrlKey && e.shiftKey && e.key === "W") {
        e.preventDefault();
        if (activeId) closeTerminal(activeId);
        return;
      }
      // Ctrl+Tab / Ctrl+Shift+Tab — cycle tabs
      if (e.ctrlKey && e.key === "Tab" && tabs.length > 1 && activeId) {
        e.preventDefault();
        const idx = tabs.findIndex((t) => t.id === activeId);
        const next = e.shiftKey ? (idx - 1 + tabs.length) % tabs.length : (idx + 1) % tabs.length;
        setActiveId(tabs[next].id);
      }
      // Escape — close right panel
      if (e.key === "Escape" && rightPanelMode) {
        setRightPanelMode(null);
        setSelectedTranscriptSessionId(null);
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [activeId, tabs, createTerminal, closeTerminal, setActiveId, rightPanelMode]);

  return (
    <div className="h-full flex flex-col bg-[#1a1b26]">
      <TerminalTabBar
        tabs={tabs}
        activeId={activeId}
        onSelect={setActiveId}
        onClose={closeTerminal}
        onCreate={() => createTerminal()}
        onRename={renameTab}
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
      />
      <TerminalNotification
        message={notification?.message ?? null}
        type={notification?.type ?? "success"}
        onDismiss={() => setNotification(null)}
      />

      {/* Main content: optional sidebar + terminal + optional right panel */}
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

        {/* Terminal area */}
        <div className="flex-1 relative overflow-hidden">
          {tabs.map((tab) => (
            <TerminalInstance
              key={tab.id}
              ref={terminalRefs.current.get(tab.id)}
              terminalId={tab.id}
              visible={tab.id === activeId}
              isReconnecting={tab.isReconnecting}
              onReconnected={() => markReconnected(tab.id)}
              onExit={(code) => handleExit(tab.id, code)}
              onFirstInput={(input) => handleFirstInput(tab.id, input)}
              onShellIntegration={(event) => handleShellIntegration(tab.id, event)}
            />
          ))}
          {tabs.length === 0 && (
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
        </div>

        {/* Right panel — transcript content OR workflow preview */}
        {rightPanelMode === "transcript" && selectedTranscriptSessionId && (
          <TranscriptContentPanel
            sessionId={selectedTranscriptSessionId}
            session={sessions.find((s) => s.session_id === selectedTranscriptSessionId) ?? null}
            messages={transcriptMessages}
            loading={loadingMessages}
            onGenerate={handleGenerateFromTranscript}
            onGenerateAndRun={handleGenerateAndRunFromTranscript}
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
      </div>
    </div>
  );
}
