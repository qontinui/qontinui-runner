/**
 * useAiBuilderState.ts
 *
 * Main state management hook for the AI Builder.
 * Consolidates all state and logic from the original monolithic component.
 */

import { useState, useEffect, useMemo, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useExecution, useAutoContinue } from "../../contexts";
import type { UseProjectLogsReturn } from "../../hooks/useProjectLogs";
import { convertSourcesToRust } from "../../hooks/project-logs/types";
import type {
  ExecutionStep,
  SavedAiWorkflow,
  PromptHistoryEntry,
  WorkspacePaths,
  AiDeveloperState,
  Session,
  ResumableWorkflow,
  ResultMessage,
  StateInfo,
  ImageInfo,
  WorkflowInfo,
  PlaywrightScriptInfo,
  SavedPromptInfo,
} from "./types";
import { sessionToAiDeveloperState } from "./types";
import {
  EXECUTION_STEPS_KEY,
  GOAL_KEY,
  MAX_ITERATIONS_KEY,
  CAPTURE_INPUT_VALIDATION_KEY,
  HISTORY_KEY,
  SESSION_KEY,
  SELECTED_CONFIG_KEY,
  getDeveloperPromptTemplate,
  saveCustomPromptTemplate,
  resetPromptTemplateToDefault,
  isUsingCustomTemplate,
  DEFAULT_DEVELOPER_PROMPT_TEMPLATE,
} from "./constants";

interface UseAiBuilderStateProps {
  projectLogs: UseProjectLogsReturn;
  onNavigateToLogLocations: () => void;
  onNavigateToAiOutput?: () => void;
  editWorkflowId?: string | null;
}

export function useAiBuilderState({
  projectLogs,
  onNavigateToLogLocations,
  onNavigateToAiOutput,
  editWorkflowId,
}: UseAiBuilderStateProps) {
  const execution = useExecution();

  // Input capture for coordinate validation - persisted to localStorage
  const [captureInputValidation, setCaptureInputValidation] = useState<boolean>(() => {
    try {
      return localStorage.getItem(CAPTURE_INPUT_VALIDATION_KEY) === "true";
    } catch {
      return false;
    }
  });

  // Persist input capture setting to localStorage
  useEffect(() => {
    localStorage.setItem(CAPTURE_INPUT_VALIDATION_KEY, captureInputValidation ? "true" : "false");
  }, [captureInputValidation]);

  // Ordered execution steps - persisted to localStorage
  const [executionSteps, setExecutionSteps] = useState<ExecutionStep[]>(() => {
    try {
      const saved = localStorage.getItem(EXECUTION_STEPS_KEY);
      return saved ? JSON.parse(saved) : [];
    } catch {
      return [];
    }
  });

  // Goal - persisted to localStorage
  const [goal, setGoal] = useState(() => {
    try {
      return localStorage.getItem(GOAL_KEY) || "";
    } catch {
      return "";
    }
  });

  // Max iterations - persisted to localStorage
  const [maxIterations, setMaxIterations] = useState(() => {
    try {
      const saved = localStorage.getItem(MAX_ITERATIONS_KEY);
      return saved ? parseInt(saved, 10) : 10;
    } catch {
      return 10;
    }
  });

  // Persist execution steps to localStorage
  useEffect(() => {
    try {
      localStorage.setItem(EXECUTION_STEPS_KEY, JSON.stringify(executionSteps));
    } catch (e) {
      console.error("Failed to persist execution steps:", e);
    }
  }, [executionSteps]);

  // Persist goal to localStorage
  useEffect(() => {
    localStorage.setItem(GOAL_KEY, goal);
  }, [goal]);

  // Persist max iterations to localStorage
  useEffect(() => {
    localStorage.setItem(MAX_ITERATIONS_KEY, maxIterations.toString());
  }, [maxIterations]);

  // Workspace paths (fetched from backend for portability)
  const [workspacePaths, setWorkspacePaths] = useState<WorkspacePaths | null>(null);

  // Playwright scripts for the dropdown
  const [playwrightScripts, setPlaywrightScripts] = useState<PlaywrightScriptInfo[]>([]);

  // Saved prompts from the prompt library
  const [savedPrompts, setSavedPrompts] = useState<SavedPromptInfo[]>([]);

  // Selected stored config ID - persisted to localStorage
  const [selectedConfigId, setSelectedConfigId] = useState<string | null>(() => {
    try {
      return localStorage.getItem(SELECTED_CONFIG_KEY) || null;
    } catch {
      return null;
    }
  });

  // Persist selected config ID to localStorage
  useEffect(() => {
    if (selectedConfigId) {
      localStorage.setItem(SELECTED_CONFIG_KEY, selectedConfigId);
    } else {
      localStorage.removeItem(SELECTED_CONFIG_KEY);
    }
  }, [selectedConfigId]);

  // Save workflow dialog state
  const [showSaveDialog, setShowSaveDialog] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [saveDescription, setSaveDescription] = useState("");
  const [isSaving, setIsSaving] = useState(false);

  // Prompt template editing state
  const [isEditingPromptTemplate, setIsEditingPromptTemplate] = useState(false);
  const [editedPromptTemplate, setEditedPromptTemplate] = useState("");
  const [usingCustomTemplate, setUsingCustomTemplate] = useState(() => isUsingCustomTemplate());

  // New workflow confirmation dialog
  const [showNewConfirmDialog, setShowNewConfirmDialog] = useState(false);

  // Track the currently loaded workflow
  const [currentWorkflowId, setCurrentWorkflowId] = useState<string | null>(null);
  const [currentWorkflowName, setCurrentWorkflowName] = useState<string>("");

  // Dirty flag for unsaved changes
  const [hasUnsavedChanges, setHasUnsavedChanges] = useState(false);

  // Saved AI Workflows from the AI Workflows library
  const [savedAiWorkflows, setSavedAiWorkflows] = useState<SavedAiWorkflow[]>([]);
  const [showWorkflowsPanel, setShowWorkflowsPanel] = useState(false);

  // Session state
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(() => {
    try {
      return localStorage.getItem(SESSION_KEY) || null;
    } catch {
      return null;
    }
  });
  const [sessionState, setSessionState] = useState<AiDeveloperState | null>(null);

  // UI state
  const [showAddDropdown, setShowAddDropdown] = useState(false);
  const [isRunning, setIsRunning] = useState(false);
  const [lastResult, setLastResult] = useState<ResultMessage | null>(null);

  // Resumable workflow state
  const [resumableWorkflow, setResumableWorkflow] = useState<ResumableWorkflow | null>(null);
  const [isResuming, setIsResuming] = useState(false);

  // Context selection state - tracks which contexts are selected for the prompt
  const [manuallyAddedContextIds, setManuallyAddedContextIds] = useState<Set<string>>(new Set());
  const [disabledContextIds, setDisabledContextIds] = useState<Set<string>>(new Set());
  const [autoIncludeContexts, setAutoIncludeContexts] = useState(true);

  // AI Provider override - allows using different AI providers (e.g., Gemini for fast tasks)
  const [aiProvider, setAiProvider] = useState<string>("");
  const [aiModel, setAiModel] = useState<string>("");

  // Auto-continue settings
  const {
    enabled: autoContinueEnabled,
    loading: autoContinueLoading,
    toggle: toggleAutoContinue,
  } = useAutoContinue();

  // Claude output log (real-time visibility)
  const [, setClaudeLog] = useState<string>("");
  const [, setClaudeLogInfo] = useState<{
    totalLines: number;
    lastModified: number;
  } | null>(null);

  // Persist session ID to localStorage
  useEffect(() => {
    if (currentSessionId) {
      localStorage.setItem(SESSION_KEY, currentSessionId);
    } else {
      localStorage.removeItem(SESSION_KEY);
    }
  }, [currentSessionId]);

  // History
  const [history, setHistory] = useState<PromptHistoryEntry[]>(() => {
    try {
      const stored = localStorage.getItem(HISTORY_KEY);
      return stored ? JSON.parse(stored) : [];
    } catch {
      return [];
    }
  });

  // Parse states, images, and workflows from config
  const { states, images, workflows } = useMemo(() => {
    const stateList: StateInfo[] = [];
    const imageList: ImageInfo[] = [];
    const workflowList: WorkflowInfo[] = [];

    if (execution.config?.states && Array.isArray(execution.config.states)) {
      for (const state of execution.config.states) {
        const stateName = state.name || state.id || "Unknown";
        const stateImages: string[] = [];

        const stateImgs = state.stateImages || state.images;
        if (stateImgs && Array.isArray(stateImgs)) {
          for (const img of stateImgs) {
            const imgId = img.id || "";
            const imgName = img.name || img.id || "";
            if (imgId && imgName) {
              stateImages.push(imgName);
              imageList.push({ id: imgId, name: imgName, stateName });
            }
          }
        }
        stateList.push({
          name: stateName,
          description: state.description || "",
          images: stateImages,
        });
      }
    }

    if (execution.config?.workflows && Array.isArray(execution.config.workflows)) {
      for (const workflow of execution.config.workflows) {
        const category = workflow.category?.toLowerCase() || "";
        const visibility = workflow.visibility || "public";

        if (category === "main" && visibility !== "internal") {
          workflowList.push({
            id: workflow.id || workflow.name || "unknown",
            name: workflow.name || workflow.id || "Unknown",
            category: workflow.category,
          });
        }
      }
    }

    return { states: stateList, images: imageList, workflows: workflowList };
  }, [execution.config]);

  // Compute whether execution steps have GUI steps that require a stored config
  const hasGuiSteps = useMemo(() => {
    const guiStepTypes = ["workflow", "state", "action"];
    return executionSteps.some((step) => guiStepTypes.includes(step.type));
  }, [executionSteps]);

  // Save history to localStorage
  useEffect(() => {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history.slice(0, 20)));
  }, [history]);

  // Fetch workspace paths on mount
  useEffect(() => {
    const fetchPaths = async () => {
      try {
        const response = await invoke<{ success: boolean; data?: WorkspacePaths }>(
          "get_workspace_paths",
        );
        if (response.success && response.data) {
          setWorkspacePaths(response.data);
        }
      } catch (error) {
        console.error("Failed to fetch workspace paths:", error);
      }
    };
    fetchPaths();
  }, []);

  // Fetch Playwright scripts for the dropdown
  useEffect(() => {
    const fetchPlaywrightScripts = async () => {
      try {
        const response = await fetch("http://localhost:9876/playwright/scripts");
        const result = await response.json();
        if (result.success && result.data) {
          setPlaywrightScripts(
            result.data.map(
              (s: {
                id: string;
                name: string;
                target_url: string;
                description: string;
                script_content: string;
              }) => ({
                id: s.id,
                name: s.name,
                target_url: s.target_url,
                description: s.description,
                script_content: s.script_content,
              }),
            ),
          );
        }
      } catch (error) {
        console.error("Failed to fetch Playwright scripts:", error);
      }
    };
    fetchPlaywrightScripts();
  }, []);

  // Fetch saved prompts from the prompt library
  useEffect(() => {
    const fetchSavedPrompts = async () => {
      try {
        const response = await fetch("http://localhost:9876/prompts");
        const result = await response.json();
        if (result.success && result.data) {
          setSavedPrompts(
            result.data.map(
              (p: {
                id: string;
                name: string;
                description: string;
                content: string;
                category: string;
              }) => ({
                id: p.id,
                name: p.name,
                description: p.description,
                content: p.content,
                category: p.category,
              }),
            ),
          );
        }
      } catch (error) {
        console.error("Failed to fetch saved prompts:", error);
      }
    };
    fetchSavedPrompts();
  }, []);

  // Check for resumable workflows on mount and periodically
  useEffect(() => {
    const checkResumableWorkflow = async () => {
      try {
        const response = await fetch("http://localhost:9876/workflow/resumable");
        const result = await response.json();
        if (result.success) {
          if (result.data?.is_running !== undefined) {
            setIsRunning(result.data.is_running);
          }
          if (result.data?.has_resumable && !result.data?.is_running) {
            setResumableWorkflow({
              name: result.data.name || "Unknown Workflow",
              currentPhase: result.data.current_phase || 0,
              totalPhases: result.data.total_phases || 0,
              status: result.data.status || "unknown",
            });
          } else {
            setResumableWorkflow(null);
          }
        } else {
          setResumableWorkflow(null);
        }
      } catch (error) {
        console.error("Failed to check resumable workflow:", error);
        setResumableWorkflow(null);
      }
    };

    checkResumableWorkflow();
    const interval = setInterval(checkResumableWorkflow, 5000);
    return () => clearInterval(interval);
  }, []);

  // Handler to resume a workflow
  const handleResumeWorkflow = useCallback(async () => {
    if (!resumableWorkflow || isResuming) return;

    setIsResuming(true);
    try {
      const response = await fetch("http://localhost:9876/workflow/resume", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();

      if (result.success) {
        setResumableWorkflow(null);
        setIsRunning(true);
        setLastResult({ success: true, message: `Resuming workflow: ${resumableWorkflow.name}` });
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to resume workflow",
        });
      }
    } catch (error) {
      console.error("Failed to resume workflow:", error);
      setLastResult({
        success: false,
        message: `Failed to resume: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsResuming(false);
    }
  }, [resumableWorkflow, isResuming]);

  // Fetch saved AI workflows
  const fetchSavedAiWorkflows = useCallback(async () => {
    try {
      const response = await fetch("http://localhost:9876/ai-workflows");
      const result = await response.json();
      if (result.success && result.data) {
        setSavedAiWorkflows(result.data);
      }
    } catch (error) {
      console.error("Failed to fetch saved AI workflows:", error);
    }
  }, []);

  useEffect(() => {
    fetchSavedAiWorkflows();
  }, [fetchSavedAiWorkflows]);

  // Sync input capture setting with Python executor
  useEffect(() => {
    const syncInputCapture = async () => {
      try {
        await invoke("set_input_capture_enabled", { enabled: captureInputValidation });
        console.log("[AI_BUILDER] Input capture enabled:", captureInputValidation);
      } catch (error) {
        console.error("[AI_BUILDER] Failed to set input capture:", error);
      }
    };
    syncInputCapture();
  }, [captureInputValidation]);

  // Poll session state when there's an active session
  useEffect(() => {
    if (!currentSessionId) return;

    const pollState = async () => {
      try {
        const response = await fetch(`http://localhost:9876/sessions/${currentSessionId}`);
        const result = await response.json();

        if (result.success && result.data) {
          const session = result.data as Session;
          const state = sessionToAiDeveloperState(session);
          setSessionState(state);

          const activeStatuses = ["starting", "running", "waiting_for_continuation"];
          const isActiveStatus = activeStatuses.includes(session.status);

          let isRecent = false;
          if (session.checkpoint.started_at) {
            const startedAt = new Date(session.checkpoint.started_at).getTime();
            const tenMinutesAgo = Date.now() - 10 * 60 * 1000;
            isRecent = startedAt > tenMinutesAgo;
          }

          setIsRunning(isActiveStatus && isRecent);
        } else if (!result.success) {
          console.log("Session not found, clearing session");
          setCurrentSessionId(null);
          setSessionState(null);
          setIsRunning(false);
        }
      } catch (error) {
        console.error("Failed to read session state:", error);
      }
    };

    pollState();
    const interval = setInterval(pollState, 2000);
    return () => clearInterval(interval);
  }, [currentSessionId]);

  // Poll Claude log when there's an active session
  useEffect(() => {
    if (!currentSessionId) {
      setClaudeLog("");
      setClaudeLogInfo(null);
      return;
    }

    const pollLog = async () => {
      try {
        const response = await invoke<{
          success: boolean;
          data?: {
            content: string;
            total_lines: number;
            last_modified: number;
          };
        }>("read_claude_session_log", { sessionId: currentSessionId, tailLines: 100 });
        if (response.success && response.data) {
          setClaudeLog(response.data.content);
          setClaudeLogInfo({
            totalLines: response.data.total_lines,
            lastModified: response.data.last_modified,
          });
        }
      } catch (error) {
        console.error("Failed to read Claude log:", error);
      }
    };

    pollLog();
    const interval = setInterval(pollLog, 3000);
    return () => clearInterval(interval);
  }, [currentSessionId]);

  // Add a step to the execution list
  const addStep = useCallback(
    (
      type: "workflow" | "state" | "playwright" | "prompt",
      name: string,
      scriptId?: string,
      scriptContent?: string,
      scriptTargetUrl?: string,
      promptId?: string,
      promptContent?: string,
    ) => {
      const newStep: ExecutionStep = {
        id: crypto.randomUUID(),
        type,
        name,
        takeScreenshot: type !== "prompt",
        screenshotDelay: 0,
        playwrightScriptId: scriptId,
        playwrightScriptContent: scriptContent,
        playwrightTargetUrl: scriptTargetUrl,
        promptId,
        promptContent,
      };
      setExecutionSteps((prev) => [...prev, newStep]);
      setShowAddDropdown(false);
      setHasUnsavedChanges(true);
    },
    [],
  );

  // Add an action step
  const addActionStep = useCallback(
    (actionType: "click" | "double_click" | "right_click", imageId: string, imageName: string) => {
      const actionNames = {
        click: "Click",
        double_click: "Double Click",
        right_click: "Right Click",
      };
      const newStep: ExecutionStep = {
        id: crypto.randomUUID(),
        type: "action",
        name: `${actionNames[actionType]}: ${imageName}`,
        takeScreenshot: true,
        screenshotDelay: 0,
        actionType,
        targetImageId: imageId,
        targetImageName: imageName,
      };
      setExecutionSteps((prev) => [...prev, newStep]);
      setShowAddDropdown(false);
      setHasUnsavedChanges(true);
    },
    [],
  );

  // Add a screenshot step
  const addScreenshotStep = useCallback(() => {
    const newStep: ExecutionStep = {
      id: crypto.randomUUID(),
      type: "screenshot",
      name: "Capture Screenshot",
      takeScreenshot: true,
      screenshotDelay: 0,
      screenshotMonitor: "all",
    };
    setExecutionSteps((prev) => [...prev, newStep]);
    setShowAddDropdown(false);
    setHasUnsavedChanges(true);
  }, []);

  // Update screenshot monitor for a step
  const updateScreenshotMonitor = useCallback((stepId: string, monitor: number | "all") => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, screenshotMonitor: monitor } : s)),
    );
    setHasUnsavedChanges(true);
  }, []);

  // Remove a step
  const removeStep = useCallback((stepId: string) => {
    setExecutionSteps((prev) => prev.filter((s) => s.id !== stepId));
    setHasUnsavedChanges(true);
  }, []);

  // Toggle screenshot for a step
  const toggleStepScreenshot = useCallback((stepId: string) => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, takeScreenshot: !s.takeScreenshot } : s)),
    );
    setHasUnsavedChanges(true);
  }, []);

  // Update screenshot delay for a step
  const updateScreenshotDelay = useCallback((stepId: string, delay: number) => {
    setExecutionSteps((prev) =>
      prev.map((s) => (s.id === stepId ? { ...s, screenshotDelay: Math.max(0, delay) } : s)),
    );
    setHasUnsavedChanges(true);
  }, []);

  // Move step up
  const moveStepUp = useCallback((index: number) => {
    if (index === 0) return;
    setExecutionSteps((prev) => {
      const newSteps = [...prev];
      [newSteps[index - 1], newSteps[index]] = [newSteps[index], newSteps[index - 1]];
      return newSteps;
    });
    setHasUnsavedChanges(true);
  }, []);

  // Move step down
  const moveStepDown = useCallback((index: number) => {
    setExecutionSteps((prev) => {
      if (index === prev.length - 1) return prev;
      const newSteps = [...prev];
      [newSteps[index], newSteps[index + 1]] = [newSteps[index + 1], newSteps[index]];
      return newSteps;
    });
    setHasUnsavedChanges(true);
  }, []);

  // Generate execution instructions
  // NOTE: Execution steps (workflow, action, state, screenshot, playwright) are now
  // executed DETERMINISTICALLY by the runner before the AI session starts.
  // This function only includes prompt steps (text instructions for the AI to follow)
  // and a note about the pre-executed steps.
  const generateExecutionInstructions = useCallback(() => {
    if (executionSteps.length === 0) {
      return "No execution steps configured. Proceed directly to log analysis.";
    }

    // Separate deterministic steps from prompt steps
    const deterministicSteps = executionSteps.filter((s) => s.type !== "prompt");
    const promptSteps = executionSteps.filter((s) => s.type === "prompt");

    let instructions = "";

    // Note about deterministic steps (their results will be appended by the backend)
    if (deterministicSteps.length > 0) {
      instructions += `**Pre-Executed Steps:** The runner has already executed ${deterministicSteps.length} automation step(s) before this session started. The results are shown in the "Pre-Execution Results" section above.\n\n`;
    }

    // Include prompt step instructions (these are text for the AI to follow)
    if (promptSteps.length > 0) {
      const promptInstructions = promptSteps
        .map((step, index) => {
          const stepNum = index + 1;
          return `### Prompt Step ${stepNum}: ${step.name}

**Instructions to follow:**

${step.promptContent || "(No content)"}

---
*Follow the instructions above before proceeding.*`;
        })
        .join("\n\n");

      instructions += promptInstructions;
    }

    if (!instructions) {
      instructions =
        "All execution steps were handled by the runner. Proceed to analyze the results and logs.";
    }

    return instructions;
  }, [executionSteps]);

  // Generate developer mode prompt
  const generateDeveloperPrompt = useCallback(
    (sessionId: string) => {
      const goalStr = goal.trim() || "Verify automation works correctly";
      const workspaceEscaped = workspacePaths?.workspace_root_escaped || "{{WORKSPACE_ESCAPED}}";

      return getDeveloperPromptTemplate()
        .replace("{{SESSION_ID}}", sessionId)
        .replace(/\{\{SESSION_ID\}\}/g, sessionId)
        .replace("{{ITERATION}}", "1")
        .replace(/\{\{ITERATION\}\}/g, "1")
        .replace("{{MAX_ITERATIONS}}", String(maxIterations))
        .replace(/\{\{MAX_ITERATIONS\}\}/g, String(maxIterations))
        .replace("{{STEP_COUNT}}", String(executionSteps.length))
        .replace("{{EXECUTION_STEPS}}", generateExecutionInstructions())
        .replace("{{GOAL}}", goalStr)
        .replace(/\{\{WORKSPACE_ESCAPED\}\}/g, workspaceEscaped);
    },
    [goal, workspacePaths, maxIterations, executionSteps.length, generateExecutionInstructions],
  );

  // Generate prompt for preview
  const generatePrompt = useCallback(
    (sessionId: string) => {
      return generateDeveloperPrompt(sessionId);
    },
    [generateDeveloperPrompt],
  );

  // Copy prompt to clipboard
  const copyPrompt = useCallback(async () => {
    const previewSessionId = "preview-" + Date.now().toString(36);
    const prompt = generatePrompt(previewSessionId);
    await navigator.clipboard.writeText(prompt);
    setLastResult({ success: true, message: "Prompt copied to clipboard!" });
    setTimeout(() => setLastResult(null), 3000);
  }, [generatePrompt]);

  // Load an AI Workflow
  const loadAiWorkflow = useCallback((workflow: SavedAiWorkflow) => {
    setExecutionSteps(workflow.steps);
    setGoal(workflow.goal);
    setMaxIterations(workflow.max_iterations);
    setCaptureInputValidation(workflow.capture_input_validation);
    // Restore context state
    setManuallyAddedContextIds(new Set(workflow.context_ids || []));
    setDisabledContextIds(new Set(workflow.disabled_context_ids || []));
    setAutoIncludeContexts(workflow.auto_include_contexts ?? true);
    setShowWorkflowsPanel(false);
    setCurrentWorkflowId(workflow.id);
    setCurrentWorkflowName(workflow.name);
    setHasUnsavedChanges(false);
    setLastResult({ success: true, message: `Loaded workflow "${workflow.name}"` });
    setTimeout(() => setLastResult(null), 3000);
  }, []);

  // Load workflow by ID (when navigating from Library)
  useEffect(() => {
    if (!editWorkflowId) return;

    const loadWorkflowById = async () => {
      try {
        const response = await fetch(`http://localhost:9876/ai-workflows/${editWorkflowId}`);
        const result = await response.json();
        if (result.success && result.data) {
          loadAiWorkflow(result.data);
        } else {
          setLastResult({
            success: false,
            message: `Failed to load workflow: ${result.error || "Not found"}`,
          });
          setTimeout(() => setLastResult(null), 3000);
        }
      } catch (error) {
        setLastResult({
          success: false,
          message: `Error loading workflow: ${error instanceof Error ? error.message : String(error)}`,
        });
        setTimeout(() => setLastResult(null), 3000);
      }
    };

    loadWorkflowById();
  }, [editWorkflowId, loadAiWorkflow]);

  // Reset to new workflow
  const resetToNewWorkflow = useCallback(() => {
    setExecutionSteps([]);
    setGoal("");
    setMaxIterations(5);
    setCaptureInputValidation(false);
    // Reset context state to defaults
    setManuallyAddedContextIds(new Set());
    setDisabledContextIds(new Set());
    setAutoIncludeContexts(true);
    setCurrentWorkflowId(null);
    setCurrentWorkflowName("");
    setHasUnsavedChanges(false);
    setShowNewConfirmDialog(false);
    setLastResult({ success: true, message: "Created new workflow" });
    setTimeout(() => setLastResult(null), 3000);
  }, []);

  // Handle new workflow
  const handleNewWorkflow = useCallback(() => {
    if (hasUnsavedChanges) {
      setShowNewConfirmDialog(true);
      return;
    }
    resetToNewWorkflow();
  }, [hasUnsavedChanges, resetToNewWorkflow]);

  // Delete an AI Workflow
  const deleteAiWorkflow = useCallback(
    async (workflowId: string) => {
      try {
        const response = await fetch(`http://localhost:9876/ai-workflows/${workflowId}`, {
          method: "DELETE",
        });
        const result = await response.json();
        if (result.success) {
          await fetchSavedAiWorkflows();
          setLastResult({ success: true, message: "Workflow deleted" });
          setTimeout(() => setLastResult(null), 3000);
        } else {
          throw new Error(result.error || "Failed to delete workflow");
        }
      } catch (error) {
        setLastResult({
          success: false,
          message: `Error deleting: ${error instanceof Error ? error.message : String(error)}`,
        });
      }
    },
    [fetchSavedAiWorkflows],
  );

  // Update the currently loaded workflow
  const updateCurrentWorkflow = useCallback(async () => {
    if (!currentWorkflowId) return;

    setIsSaving(true);
    try {
      const response = await fetch(`http://localhost:9876/ai-workflows/${currentWorkflowId}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: currentWorkflowName,
          description: "",
          steps: executionSteps,
          goal: goal.trim(),
          max_iterations: maxIterations,
          persistent_session: true,
          capture_input_validation: captureInputValidation,
          category: "",
          tags: [],
          context_ids: Array.from(manuallyAddedContextIds),
          disabled_context_ids: Array.from(disabledContextIds),
          auto_include_contexts: autoIncludeContexts,
        }),
      });

      const result = await response.json();
      if (result.success) {
        setLastResult({ success: true, message: `Workflow "${currentWorkflowName}" saved!` });
        setHasUnsavedChanges(false);
        await fetchSavedAiWorkflows();
      } else {
        throw new Error(result.error || "Failed to save workflow");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error saving: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsSaving(false);
    }
    setTimeout(() => setLastResult(null), 3000);
  }, [
    currentWorkflowId,
    currentWorkflowName,
    executionSteps,
    goal,
    maxIterations,
    captureInputValidation,
    manuallyAddedContextIds,
    disabledContextIds,
    autoIncludeContexts,
    fetchSavedAiWorkflows,
  ]);

  // Save current workflow
  const handleSaveWorkflow = useCallback(async () => {
    if (currentWorkflowId) {
      await updateCurrentWorkflow();
    } else {
      setSaveName(goal.trim() || "AI Builder Workflow");
      setShowSaveDialog(true);
    }
  }, [currentWorkflowId, goal, updateCurrentWorkflow]);

  // Save as new workflow
  const saveAsNewWorkflow = useCallback(async () => {
    if (!saveName.trim()) {
      setLastResult({ success: false, message: "Please enter a name for the workflow" });
      return;
    }

    setIsSaving(true);
    try {
      const response = await fetch("http://localhost:9876/ai-workflows", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: saveName.trim(),
          description: saveDescription.trim(),
          steps: executionSteps,
          goal: goal.trim(),
          max_iterations: maxIterations,
          persistent_session: true,
          capture_input_validation: captureInputValidation,
          category: "",
          tags: [],
          context_ids: Array.from(manuallyAddedContextIds),
          disabled_context_ids: Array.from(disabledContextIds),
          auto_include_contexts: autoIncludeContexts,
        }),
      });

      const result = await response.json();
      if (result.success) {
        setLastResult({ success: true, message: `Workflow "${saveName}" saved!` });
        setShowSaveDialog(false);
        if (result.data?.id) {
          setCurrentWorkflowId(result.data.id);
          setCurrentWorkflowName(saveName.trim());
        }
        setHasUnsavedChanges(false);
        setSaveName("");
        setSaveDescription("");
        await fetchSavedAiWorkflows();
      } else {
        throw new Error(result.error || "Failed to save workflow");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error saving: ${error instanceof Error ? error.message : String(error)}`,
      });
    } finally {
      setIsSaving(false);
    }
  }, [
    saveName,
    saveDescription,
    executionSteps,
    goal,
    maxIterations,
    captureInputValidation,
    manuallyAddedContextIds,
    disabledContextIds,
    autoIncludeContexts,
    fetchSavedAiWorkflows,
  ]);

  // Run the automation
  const runAutomation = useCallback(async () => {
    console.log("[AI_BUILDER] Starting AI session...");
    setIsRunning(true);
    setLastResult(null);

    try {
      const sessionId = Date.now().toString(36) + Math.random().toString(36).substring(2, 7);
      console.log("[AI_BUILDER] Generated session ID:", sessionId);

      const prompt = generateDeveloperPrompt(sessionId);
      console.log("[AI_BUILDER] Generated prompt length:", prompt.length);

      const historyEntry: PromptHistoryEntry = {
        id: sessionId,
        timestamp: Date.now(),
        steps: [...executionSteps],
        goal: goal.trim() || "Verify automation works correctly",
      };
      setHistory((prev) => [historyEntry, ...prev]);

      // Compute context IDs to pass to the backend
      // Include manually added contexts, exclude disabled ones
      const contextIdsToSend = Array.from(manuallyAddedContextIds).filter(
        (id) => !disabledContextIds.has(id),
      );

      // Auto-add Input Validation Guide context when capture is enabled
      if (captureInputValidation && !contextIdsToSend.includes("builtin-input-validation")) {
        contextIdsToSend.push("builtin-input-validation");
      }

      console.log("[AI_BUILDER] Context IDs to send:", contextIdsToSend);
      console.log("[AI_BUILDER] Auto-include contexts:", autoIncludeContexts);

      // Generate a sensible task name - prefer workflow name, then extract 2 largest words from first sentence
      const getTaskName = (): string => {
        if (currentWorkflowName) return currentWorkflowName;
        const goalText = goal.trim();
        if (!goalText) return "AI Builder Session";

        // Get first sentence (split on . ! ? or take whole text if no sentence-ending punctuation)
        const firstSentence = goalText.split(/[.!?]/)[0].trim();

        // Extract words (alphanumeric sequences, including hyphenated words)
        const words = firstSentence.match(/[\w-]+/g) || [];
        if (words.length === 0) return "AI Builder Session";
        if (words.length === 1) return words[0];

        // Sort by length descending and take the 2 largest
        const sortedByLength = [...words].sort((a, b) => b.length - a.length);
        const largest = sortedByLength[0];
        const secondLargest = sortedByLength[1];

        return `${largest} ${secondLargest}`;
      };

      // Determine if this session uses GUI (has any execution steps that need GUI)
      const guiStepTypes = ["workflow", "state", "action", "screenshot"];
      const usesGui = executionSteps.some((step) => guiStepTypes.includes(step.type));

      // Convert ExecutionStep[] to ExecutionStepConfig[] for deterministic execution
      // The backend will execute these steps BEFORE spawning the AI session
      const executionStepsConfig = executionSteps.map((step) => ({
        type: step.type,
        name: step.name,
        actionType: step.actionType || null,
        targetImageId: step.targetImageId || null,
        targetImageName: step.targetImageName || null,
        monitorIndex: null, // TODO: Could add monitor selection per step
        takeScreenshot: step.takeScreenshot,
        screenshotDelay: step.screenshotDelay || 0,
        screenshotMonitor: step.screenshotMonitor ?? null,
        playwrightScriptId: step.playwrightScriptId || null,
        promptContent: step.promptContent || null,
      }));

      console.log("[AI_BUILDER] Starting session via unified sessions API...");
      console.log(
        "[AI_BUILDER] AI Provider:",
        aiProvider || "default",
        "Model:",
        aiModel || "default",
      );
      console.log(
        "[AI_BUILDER] Execution steps for deterministic execution:",
        executionStepsConfig.length,
      );
      const response = await fetch("http://localhost:9876/sessions/start", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: getTaskName(),
          prompt: prompt,
          continuation_prompt: null,
          total_phases: maxIterations,
          uses_gui: usesGui,
          timeout_seconds: 1800,
          // Execution steps for deterministic execution before AI spawns
          execution_steps: executionStepsConfig,
          // Log sources to capture during execution (user-defined)
          log_sources: projectLogs.config?.logSources
            ? convertSourcesToRust(projectLogs.config.logSources.filter((s) => s.enabled))
            : [],
          // Context injection parameters
          context_ids: contextIdsToSend,
          auto_include_contexts: autoIncludeContexts,
          // AI provider override
          provider: aiProvider || null,
          model: aiModel || null,
        }),
      });
      const result = await response.json();
      console.log("[AI_BUILDER] sessions/start response:", result);

      if (result.success && result.data?.session) {
        const session = result.data.session as Session;
        setCurrentSessionId(session.id);
        setLastResult({
          success: true,
          message: `AI Builder session ${session.id} started!`,
        });
        setHistory((prev) => prev.map((h) => (h.id === sessionId ? { ...h, success: true } : h)));
        // Navigate to AI Output page after successful start
        onNavigateToAiOutput?.();
      } else {
        throw new Error(result.error || "Failed to start AI Builder session");
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error: ${error instanceof Error ? error.message : String(error)}`,
      });
      setHistory((prev) => {
        const lastEntry = prev[0];
        if (lastEntry) {
          return [{ ...lastEntry, success: false }, ...prev.slice(1)];
        }
        return prev;
      });
      setIsRunning(false);
    }
  }, [
    generateDeveloperPrompt,
    executionSteps,
    goal,
    maxIterations,
    currentWorkflowName,
    manuallyAddedContextIds,
    disabledContextIds,
    autoIncludeContexts,
    captureInputValidation,
    aiProvider,
    aiModel,
    projectLogs,
    onNavigateToAiOutput,
  ]);

  // Stop the current session
  const stopSession = useCallback(async () => {
    if (!currentSessionId) return;

    try {
      const response = await fetch(`http://localhost:9876/sessions/${currentSessionId}/stop`, {
        method: "POST",
      });
      const result = await response.json();

      if (result.success) {
        setLastResult({ success: true, message: "Stop requested. Session will exit gracefully." });
        setIsRunning(false);
      } else {
        setLastResult({
          success: false,
          message: result.error || "Failed to stop session",
        });
      }
    } catch (error) {
      setLastResult({
        success: false,
        message: `Error: ${error instanceof Error ? error.message : String(error)}`,
      });
    }
  }, [currentSessionId]);

  // Load from history
  const loadFromHistory = useCallback((entry: PromptHistoryEntry) => {
    setExecutionSteps(entry.steps);
    setGoal(entry.goal);
  }, []);

  // Get history summary
  const getHistorySummary = useCallback((entry: PromptHistoryEntry): string => {
    const workflowCount = entry.steps.filter((s) => s.type === "workflow").length;
    const stateCount = entry.steps.filter((s) => s.type === "state").length;
    const playwrightCount = entry.steps.filter((s) => s.type === "playwright").length;
    const promptCount = entry.steps.filter((s) => s.type === "prompt").length;
    const parts: string[] = [];
    if (workflowCount > 0) parts.push(`${workflowCount} workflow${workflowCount > 1 ? "s" : ""}`);
    if (stateCount > 0) parts.push(`${stateCount} state${stateCount > 1 ? "s" : ""}`);
    if (playwrightCount > 0) parts.push(`${playwrightCount} test${playwrightCount > 1 ? "s" : ""}`);
    if (promptCount > 0) parts.push(`${promptCount} prompt${promptCount > 1 ? "s" : ""}`);
    return parts.join(" - ") || "No steps";
  }, []);

  // Handle save prompt template
  const handleSavePromptTemplate = useCallback(() => {
    saveCustomPromptTemplate(editedPromptTemplate);
    setUsingCustomTemplate(true);
    setIsEditingPromptTemplate(false);
    setLastResult({ success: true, message: "Custom template saved!" });
    setTimeout(() => setLastResult(null), 3000);
  }, [editedPromptTemplate]);

  // Handle reset prompt template
  const handleResetPromptTemplate = useCallback(() => {
    resetPromptTemplateToDefault();
    setUsingCustomTemplate(false);
    setEditedPromptTemplate(DEFAULT_DEVELOPER_PROMPT_TEMPLATE);
    setLastResult({ success: true, message: "Reset to default template" });
    setTimeout(() => setLastResult(null), 3000);
  }, []);

  // Clear session state for new session
  const clearSessionForNew = useCallback(() => {
    setCurrentSessionId(null);
    setSessionState(null);
    setClaudeLog("");
    setClaudeLogInfo(null);
    setLastResult(null);
  }, []);

  return {
    // Execution Steps
    executionSteps,
    setExecutionSteps,
    addStep,
    addActionStep,
    addScreenshotStep,
    removeStep,
    toggleStepScreenshot,
    updateScreenshotDelay,
    updateScreenshotMonitor,
    moveStepUp,
    moveStepDown,

    // Goal
    goal,
    setGoal: (newGoal: string) => {
      setGoal(newGoal);
      setHasUnsavedChanges(true);
    },

    // Settings
    maxIterations,
    setMaxIterations: (iterations: number) => {
      setMaxIterations(Math.max(1, Math.min(50, iterations)));
      setHasUnsavedChanges(true);
    },
    captureInputValidation,
    setCaptureInputValidation: (enabled: boolean) => {
      setCaptureInputValidation(enabled);
      setHasUnsavedChanges(true);
    },

    // Workflow Management
    currentWorkflowId,
    currentWorkflowName,
    hasUnsavedChanges,
    savedAiWorkflows,
    loadAiWorkflow,
    deleteAiWorkflow,
    handleNewWorkflow,
    handleSaveWorkflow,

    // Session State
    currentSessionId,
    sessionState,
    isRunning,
    lastResult,
    setLastResult,
    clearSessionForNew,

    // Resumable Workflow
    resumableWorkflow,
    isResuming,
    handleResumeWorkflow,

    // Auto-continue
    autoContinueEnabled,
    autoContinueLoading,
    toggleAutoContinue,

    // Actions
    runAutomation,
    stopSession,
    copyPrompt,
    generatePrompt,

    // History
    history,
    loadFromHistory,
    getHistorySummary,

    // Dropdowns/Dialogs
    showAddDropdown,
    setShowAddDropdown,
    showWorkflowsPanel,
    setShowWorkflowsPanel,
    showSaveDialog,
    setShowSaveDialog,
    showNewConfirmDialog,
    setShowNewConfirmDialog,

    // Save Dialog State
    saveName,
    setSaveName,
    saveDescription,
    setSaveDescription,
    isSaving,
    saveAsNewWorkflow,
    resetToNewWorkflow,

    // Prompt Template
    isEditingPromptTemplate,
    setIsEditingPromptTemplate,
    editedPromptTemplate,
    setEditedPromptTemplate,
    usingCustomTemplate,
    handleSavePromptTemplate,
    handleResetPromptTemplate,

    // Config Data
    states,
    images,
    workflows,
    playwrightScripts,
    savedPrompts,
    workspacePaths,

    // External Props
    projectLogs,
    onNavigateToLogLocations,

    // Config Loading
    configLoaded: execution.configLoaded,
    loadConfiguration: execution.loadConfiguration,

    // Stored Config Selection
    selectedConfigId,
    setSelectedConfigId: (configId: string | null) => {
      setSelectedConfigId(configId);
      setHasUnsavedChanges(true);
    },
    hasGuiSteps,

    // Context Selection (for prompt composition)
    manuallyAddedContextIds,
    disabledContextIds,
    autoIncludeContexts,
    setAutoIncludeContexts,
    addContext: (contextId: string) => {
      setManuallyAddedContextIds((prev) => new Set([...prev, contextId]));
    },
    removeContext: (contextId: string) => {
      setManuallyAddedContextIds((prev) => {
        const next = new Set(prev);
        next.delete(contextId);
        return next;
      });
    },
    toggleContextDisabled: (contextId: string) => {
      setDisabledContextIds((prev) => {
        const next = new Set(prev);
        if (next.has(contextId)) {
          next.delete(contextId);
        } else {
          next.add(contextId);
        }
        return next;
      });
    },

    // AI Provider Override
    aiProvider,
    setAiProvider,
    aiModel,
    setAiModel,
  };
}
