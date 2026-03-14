import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import type { UnifiedWorkflow } from "@qontinui/shared-types";
import type { TranscriptMessage } from "./useTranscriptSessions";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { parsePlanMarkdown, summarizeParsedPlan } from "@/lib/workflow-builder/parsePlanMarkdown";
import { buildPlanWorkflow } from "@/lib/workflow-builder/buildPlanWorkflow";
import type { CommandResponse } from "./types";

interface GenerateWorkflowResponse {
  success: boolean;
  error?: string;
  workflow?: UnifiedWorkflow;
}

interface UseWorkflowGenerationParams {
  activeId: string | null;
  tabs: Array<{
    id: string;
    title: string;
    workingDir?: string;
    claudeSessionId?: string;
    claudeConfigDir?: string;
  }>;
  loadMessages: (sessionId: string) => Promise<TranscriptMessage[]>;
  onNavigateToBuilder?: () => void;
  onNavigateToActive?: () => void;
}

interface UseWorkflowGenerationResult {
  // State
  isGenerating: boolean;
  generatedWorkflow: UnifiedWorkflow | null;
  workflowError: string | undefined;
  notification: { message: string; type: "success" | "error" } | null;
  setNotification: React.Dispatch<
    React.SetStateAction<{ message: string; type: "success" | "error" } | null>
  >;
  latestPlanContent: string;
  planFileName: string | null;
  isPlanLoading: boolean;
  rightPanelMode: "transcript" | "workflow" | "analysis" | "findings" | null;
  setRightPanelMode: React.Dispatch<
    React.SetStateAction<"transcript" | "workflow" | "analysis" | "findings" | null>
  >;
  showSidebar: boolean;
  setShowSidebar: React.Dispatch<React.SetStateAction<boolean>>;
  selectedTranscriptSessionId: string | null;
  setSelectedTranscriptSessionId: React.Dispatch<React.SetStateAction<string | null>>;
  transcriptMessages: TranscriptMessage[];
  loadingMessages: boolean;
  // Callbacks
  runGeneration: (description: string, inlineContext: string) => Promise<void>;
  handleGenerateFromLatestSession: () => Promise<void>;
  handleGenerateFromTranscript: (desc: string, ctx: string) => Promise<void>;
  handleGenerateAndRunFromTranscript: (desc: string, ctx: string) => Promise<void>;
  handleExecute: () => Promise<void>;
  handleSaveWorkflow: () => Promise<void>;
  handleEditInBuilder: () => void;
  handleRegenerate: () => Promise<void>;
  handleBuildPlanWorkflow: (planContent: string) => void;
  handleBuildPlanFromFile: () => void;
  loadPlanContent: () => Promise<void>;
  handleSelectTranscriptSession: (sessionId: string) => Promise<void>;
}

export function useWorkflowGeneration({
  activeId,
  tabs,
  loadMessages,
  onNavigateToBuilder,
  onNavigateToActive,
}: UseWorkflowGenerationParams): UseWorkflowGenerationResult {
  // ── Generation state ────────────────────────────────────────────────────────
  const [isGenerating, setIsGenerating] = useState(false);
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

  // Plan content state
  const [latestPlanContent, setLatestPlanContent] = useState("");
  const [planFileName, setPlanFileName] = useState<string | null>(null);
  const [isPlanLoading, setIsPlanLoading] = useState(false);

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

  // Load plan content once on mount (best-effort)
  useEffect(() => {
    loadPlanContent();
  }, [loadPlanContent]);

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
    const activeTab = tabs.find((t) => t.id === activeId);

    if (activeTab?.claudeSessionId) {
      setShowSidebar(true);
      await handleSelectTranscriptSession(activeTab.claudeSessionId);
      return;
    }

    try {
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
      console.error("[useWorkflowGeneration] Failed to execute workflow:", e);
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
      console.error("[useWorkflowGeneration] Failed to save workflow:", e);
    }
  }, [generatedWorkflow]);

  const handleEditInBuilder = useCallback(() => {
    if (!generatedWorkflow) return;
    try {
      instanceStorage.setJSON("qontinui-generated-workflow", generatedWorkflow);
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

  return {
    // State
    isGenerating,
    generatedWorkflow,
    workflowError,
    notification,
    setNotification,
    latestPlanContent,
    planFileName,
    isPlanLoading,
    rightPanelMode,
    setRightPanelMode,
    showSidebar,
    setShowSidebar,
    selectedTranscriptSessionId,
    setSelectedTranscriptSessionId,
    transcriptMessages,
    loadingMessages,
    // Callbacks
    runGeneration,
    handleGenerateFromLatestSession,
    handleGenerateFromTranscript,
    handleGenerateAndRunFromTranscript,
    handleExecute,
    handleSaveWorkflow,
    handleEditInBuilder,
    handleRegenerate,
    handleBuildPlanWorkflow,
    handleBuildPlanFromFile,
    loadPlanContent,
    handleSelectTranscriptSession,
  };
}
