/**
 * WorkflowBuilderTab.tsx
 *
 * Main component for the unified Workflow Builder.
 * Organizes steps into three phases: Setup, Verification, Agentic.
 */

import React, { useState, useCallback, useEffect, useRef } from "react";
import {
  Plus,
  Save,
  Play,
  Square,
  Settings,
  FileText,
  RotateCcw,
  Loader2,
  FolderOpen,
  Search,
  Sparkles,
} from "lucide-react";
import type {
  WorkflowPhase,
  UnifiedStep,
  SetupStep,
  VerificationStep,
  AgenticStep,
  CompletionStep,
  UnifiedWorkflow,
  SavedPrompt,
  SavedShellCommand,
  LogSourceSelection,
} from "../../types";
import { PHASE_INFO, generateStepId } from "../../types";
import { useGlobalLogSources } from "../../hooks/useGlobalLogSources";
import { WorkflowBuilderProvider, useWorkflowBuilder } from "./WorkflowBuilderContext";
import { PhaseSection } from "./PhaseSection";
import { StepItem } from "./StepItem";
import { AddStepDropdown, AddStepButton } from "./AddStepDropdown";
import { StepConfigPanel } from "./StepConfigPanel";
import { PromptLibraryPicker } from "./PromptLibraryPicker";
import { ShellCommandLibraryPicker } from "./ShellCommandLibraryPicker";
import { AiGenerateWorkflowModal } from "./AiGenerateWorkflowModal";
import { PageTutorialMenu } from "../tutorial";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

// =============================================================================
// Empty State Component
// =============================================================================

function EmptyState({ onAddStep }: { onAddStep: (phase: WorkflowPhase) => void }) {
  return (
    <div className="flex flex-col items-center justify-center py-12 px-4 text-center">
      <FileText className="w-12 h-12 text-zinc-600 mb-4" />
      <h3 className="text-lg font-medium text-zinc-300 mb-2">Start Building</h3>
      <p className="text-sm text-zinc-500 mb-6 max-w-md">
        Add steps to build your workflow. Execution order:
        <br />
        <span className="text-blue-400">Setup</span> (once) {"->"} [
        <span className="text-green-400">Verification</span> {"<->"}{" "}
        <span className="text-amber-400">Agentic</span>] (loop) {"->"}{" "}
        <span className="text-purple-400">Completion</span> (once)
      </p>

      <div className="flex flex-wrap gap-3 justify-center">
        <button
          data-tutorial-id="setup-phase"
          onClick={() => onAddStep("setup")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-blue-500/20 hover:bg-blue-500/30 text-blue-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Setup</span>
        </button>
        <button
          data-tutorial-id="verification-phase"
          onClick={() => onAddStep("verification")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-green-500/20 hover:bg-green-500/30 text-green-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Verification</span>
        </button>
        <button
          data-tutorial-id="agentic-phase"
          onClick={() => onAddStep("agentic")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-amber-500/20 hover:bg-amber-500/30 text-amber-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Agentic</span>
        </button>
        <button
          data-tutorial-id="completion-phase"
          onClick={() => onAddStep("completion")}
          className="flex items-center gap-2 px-4 py-2 rounded-md bg-purple-500/20 hover:bg-purple-500/30 text-purple-400 transition-colors"
        >
          <Plus className="w-4 h-4" />
          <span>Completion</span>
        </button>
      </div>
    </div>
  );
}

// =============================================================================
// Settings Panel Component
// =============================================================================

interface SettingsPanelProps {
  nameInputRef?: React.RefObject<HTMLInputElement | null>;
}

function SettingsPanel({ nameInputRef }: SettingsPanelProps) {
  const { state, updateWorkflow, features } = useWorkflowBuilder();
  const { workflow } = state;
  const { settings: logSourceSettings } = useGlobalLogSources();

  // Get current log source selection value for display
  const getLogSourceDisplayValue = (): string => {
    const selection = workflow.log_source_selection;
    if (!selection || selection === "default") return "default";
    if (selection === "ai") return "ai";
    if (selection === "all") return "all";
    if (typeof selection === "object" && "profile_id" in selection) {
      return `profile:${selection.profile_id}`;
    }
    return "default";
  };

  // Handle log source selection change
  const handleLogSourceChange = (value: string) => {
    let selection: LogSourceSelection;
    if (value === "default") {
      selection = "default";
    } else if (value === "ai") {
      selection = "ai";
    } else if (value === "all") {
      selection = "all";
    } else if (value.startsWith("profile:")) {
      selection = { profile_id: value.replace("profile:", "") };
    } else {
      selection = "default";
    }
    updateWorkflow({ log_source_selection: selection });
  };

  return (
    <div className="p-4 border-t border-zinc-700 space-y-4">
      {/* Always show name and description */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Name</label>
        <input
          ref={nameInputRef}
          type="text"
          value={workflow.name}
          onChange={(e) => updateWorkflow({ name: e.target.value })}
          placeholder="Workflow name..."
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Description</label>
        <textarea
          value={workflow.description}
          onChange={(e) => updateWorkflow({ description: e.target.value })}
          placeholder="What does this workflow do?"
          rows={2}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 resize-none"
        />
      </div>

      {/* Show iteration settings when there are agentic steps */}
      {features.showIterationSettings && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Max Iterations</label>
          <input
            type="number"
            value={workflow.max_iterations ?? 10}
            onChange={(e) => updateWorkflow({ max_iterations: parseInt(e.target.value) || 10 })}
            min={1}
            max={100}
            className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">
            Maximum number of verification {"<->"} agentic loops
          </p>
        </div>
      )}

      {/* Show provider/model settings when there are agentic steps */}
      {features.showIterationSettings && (
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Provider</label>
            <select
              value={workflow.provider ?? ""}
              onChange={(e) => updateWorkflow({ provider: e.target.value || undefined })}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            >
              <option value="">Default</option>
              <option value="claude_cli">Claude CLI</option>
              <option value="gemini_api">Gemini API</option>
            </select>
          </div>
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Model</label>
            <select
              value={workflow.model ?? ""}
              onChange={(e) => updateWorkflow({ model: e.target.value || undefined })}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            >
              <option value="">Default</option>
              <option value="claude-sonnet-4">Claude Sonnet 4</option>
              <option value="claude-opus-4">Claude Opus 4</option>
              <option value="gemini-2.5-pro">Gemini 2.5 Pro</option>
            </select>
          </div>
        </div>
      )}

      {/* AI Summary toggle - show when workflow has any AI prompts */}
      {features.hasAiPrompts && (
        <div className="flex items-center justify-between py-2 px-3 bg-zinc-800/50 rounded-md">
          <div>
            <label className="block text-sm font-medium text-zinc-300">AI Summary</label>
            <p className="text-xs text-zinc-500">
              Generate an AI summary of the workflow execution
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={!workflow.skip_ai_summary}
            onClick={() => updateWorkflow({ skip_ai_summary: !workflow.skip_ai_summary })}
            className={`
              relative inline-flex h-6 w-11 flex-shrink-0 cursor-pointer rounded-full border-2 border-transparent
              transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500/50
              ${!workflow.skip_ai_summary ? "bg-blue-600" : "bg-zinc-600"}
            `}
          >
            <span
              className={`
                pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0
                transition duration-200 ease-in-out
                ${!workflow.skip_ai_summary ? "translate-x-5" : "translate-x-0"}
              `}
            />
          </button>
        </div>
      )}

      {/* Log Source Selection */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Log Sources</label>
        <select
          value={getLogSourceDisplayValue()}
          onChange={(e) => handleLogSourceChange(e.target.value)}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
        >
          <option value="default">Default (use global setting)</option>
          <option value="ai">AI-based selection</option>
          <option value="all">All enabled sources</option>
          {logSourceSettings?.profiles.map((profile) => (
            <option key={profile.id} value={`profile:${profile.id}`}>
              Profile: {profile.name}
            </option>
          ))}
        </select>
        <p className="text-xs text-zinc-500 mt-1">
          Which log sources to include when running this workflow
        </p>
      </div>

      {/* Category and tags */}
      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Category</label>
          <input
            type="text"
            value={workflow.category}
            onChange={(e) => updateWorkflow({ category: e.target.value })}
            placeholder="general"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          />
        </div>
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Tags</label>
          <input
            type="text"
            value={workflow.tags.join(", ")}
            onChange={(e) =>
              updateWorkflow({
                tags: e.target.value
                  .split(",")
                  .map((t) => t.trim())
                  .filter(Boolean),
              })
            }
            placeholder="tag1, tag2"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50"
          />
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Main Content Component
// =============================================================================

function WorkflowBuilderContent({
  onOpenLibrary,
  onNavigateToActive,
}: {
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}) {
  const {
    state,
    isEmpty,
    hasUnsavedChanges,
    addStep,
    removeStep,
    moveStep,
    selectStep,
    getSelectedStep,
    showAddDropdown,
    resetToNew,
    saveWorkflow,
    loadWorkflow,
    updateWorkflow,
  } = useWorkflowBuilder();

  const selectedStep = getSelectedStep();

  const [showSettings, setShowSettings] = useState(false);
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const [dropdownPhase, setDropdownPhase] = useState<WorkflowPhase | null>(null);

  // Prompt library picker state
  const [promptPickerOpen, setPromptPickerOpen] = useState(false);
  const [promptPickerPhase, setPromptPickerPhase] = useState<WorkflowPhase>("setup");

  // Shell command library picker state
  const [shellCommandPickerOpen, setShellCommandPickerOpen] = useState(false);
  const [shellCommandPickerPhase, setShellCommandPickerPhase] = useState<WorkflowPhase>("setup");

  // AI generate workflow modal state
  const [aiGenerateModalOpen, setAiGenerateModalOpen] = useState(false);

  // Ref for focusing the name input when creating new workflow
  const nameInputRef = useRef<HTMLInputElement | null>(null);

  // Workflow library state
  const [savedWorkflows, setSavedWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [workflowsLoading, setWorkflowsLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");

  // Handle creating a new workflow with visual feedback
  const handleNewWorkflow = useCallback(() => {
    resetToNew();
    setShowSettings(true);
    // Focus the name input after state updates
    setTimeout(() => {
      nameInputRef.current?.focus();
      nameInputRef.current?.select();
    }, 50);
  }, [resetToNew]);

  const accentColors = getAccentColors("green");

  // Fetch saved workflows
  const fetchWorkflows = useCallback(async () => {
    setWorkflowsLoading(true);
    try {
      const response = await fetch(`${API_BASE}/unified-workflows`);
      const result = await response.json();
      if (result.success && result.data) {
        setSavedWorkflows(result.data);
      } else if (Array.isArray(result)) {
        setSavedWorkflows(result);
      } else {
        setSavedWorkflows([]);
      }
    } catch (error) {
      console.error("Failed to fetch workflows:", error);
    } finally {
      setWorkflowsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchWorkflows();
  }, [fetchWorkflows]);

  // Refresh workflow list after save
  useEffect(() => {
    if (!state.isSaving && state.workflow.id) {
      fetchWorkflows();
    }
  }, [state.isSaving, state.workflow.id, fetchWorkflows]);

  // Filter workflows
  const filteredWorkflows = savedWorkflows.filter((w) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      w.name.toLowerCase().includes(query) ||
      w.description?.toLowerCase().includes(query) ||
      w.category?.toLowerCase().includes(query)
    );
  });

  // Select a workflow for editing
  const selectWorkflow = async (workflow: UnifiedWorkflow) => {
    await loadWorkflow(workflow.id);
  };

  // Handle save
  const handleSave = useCallback(async () => {
    const success = await saveWorkflow();
    if (success) {
      console.log("Workflow saved successfully");
    }
  }, [saveWorkflow]);

  // Execution state
  const [isExecuting, setIsExecuting] = useState(false);
  const [executionError, setExecutionError] = useState<string | null>(null);
  const [executionSuccess, setExecutionSuccess] = useState<string | null>(null);

  // Handle run workflow
  const handleRun = useCallback(async () => {
    if (isExecuting) return;

    // Check if workflow has steps
    if (isEmpty) {
      setExecutionError("Cannot run an empty workflow. Add some steps first.");
      return;
    }

    // Determine workflow ID to use (may be updated after save)
    let workflowIdToRun = state.workflow.id;

    // Check if workflow is saved (must have an ID to run)
    if (!state.workflow.id || hasUnsavedChanges) {
      const shouldSave = confirm("Workflow must be saved before running. Save now?");
      if (shouldSave) {
        const savedWorkflow = await saveWorkflow();
        if (!savedWorkflow) {
          setExecutionError("Failed to save workflow. Cannot run.");
          return;
        }
        // Use the newly saved workflow's ID (state.workflow.id is stale in this closure)
        workflowIdToRun = savedWorkflow.id;
      } else {
        setExecutionError("Workflow must be saved before running.");
        return;
      }
    }

    setIsExecuting(true);
    setExecutionError(null);
    setExecutionSuccess(null);

    // Navigate to Active page IMMEDIATELY so user can see the workflow in progress
    // The Active page polls for running tasks and will show this workflow
    onNavigateToActive?.();

    try {
      console.log(
        "[WorkflowBuilder] Starting unified workflow execution:",
        workflowIdToRun,
        state.workflow.name,
      );

      // Use the unified workflow run endpoint
      const response = await fetch(`${API_BASE}/unified-workflows/${workflowIdToRun}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          monitor_index: 0, // Default to primary monitor
          timeout_seconds: 300, // 5 minute timeout
        }),
      });

      // Check HTTP response status first
      if (!response.ok) {
        const errorText = await response.text();
        let errorMsg: string;
        try {
          const errorJson = JSON.parse(errorText);
          errorMsg = errorJson.error || `HTTP ${response.status}`;
        } catch {
          errorMsg = errorText || `HTTP ${response.status}`;
        }

        // Handle 404 specifically - workflow may have been deleted
        if (response.status === 404) {
          // Clear the stale ID so the workflow will be saved as new
          updateWorkflow({ id: undefined });
          setExecutionError(
            `Workflow not found (stale ID cleared). Click Run again to save and execute.`,
          );
        } else {
          setExecutionError(`Execution failed: ${errorMsg}`);
        }
        console.error("[WorkflowBuilder] Execution failed:", errorMsg);
        return;
      }

      const result = await response.json();
      console.log("[WorkflowBuilder] Execution result:", result);

      if (result.success && result.data?.success) {
        const totalDuration = result.data.total_duration_ms || 0;
        const durationInfo = totalDuration > 0 ? ` in ${(totalDuration / 1000).toFixed(1)}s` : "";
        const successMsg = `Workflow completed successfully${durationInfo} (${result.data.successful_steps}/${result.data.total_steps} steps)`;
        setExecutionSuccess(successMsg);
        console.log(`[WorkflowBuilder] ${successMsg}`);
        // Clear success message after 8 seconds
        setTimeout(() => setExecutionSuccess(null), 8000);
      } else {
        // Find first failed step for the error message
        const failedStep = result.data?.steps?.find((s: { success: boolean }) => !s.success);
        const stepInfo = failedStep
          ? `Step "${failedStep.step_name || failedStep.step_type}" failed: `
          : "";
        const errorDetail =
          failedStep?.error || result.data?.steps?.[0]?.error || result.error || "Unknown error";
        const errorMsg = `${stepInfo}${errorDetail}`;
        setExecutionError(errorMsg);
        console.error("[WorkflowBuilder] Execution failed:", errorMsg);
        // Don't clear error message automatically - let user dismiss it
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      setExecutionError(`Failed to run workflow: ${errorMsg}`);
      console.error("[WorkflowBuilder] Failed to run workflow:", error);
    } finally {
      setIsExecuting(false);
    }
  }, [
    state.workflow,
    isEmpty,
    hasUnsavedChanges,
    saveWorkflow,
    isExecuting,
    onNavigateToActive,
    updateWorkflow,
  ]);

  // Handle stop execution
  const handleStop = useCallback(async () => {
    try {
      const response = await fetch(`${API_BASE}/stop-execution`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
      });
      const result = await response.json();
      if (result.success) {
        setIsExecuting(false);
        console.log("[WorkflowBuilder] Execution stopped");
      }
    } catch (error) {
      console.error("[WorkflowBuilder] Failed to stop execution:", error);
    }
  }, []);

  const handleAddStep = useCallback((phase: WorkflowPhase) => {
    setDropdownPhase(phase);
    setDropdownOpen(true);
  }, []);

  const handleStepAdded = useCallback(
    (step: UnifiedStep, phase: WorkflowPhase) => {
      addStep(step, phase);
      setDropdownOpen(false);
    },
    [addStep],
  );

  // Handler to open the prompt library picker
  const handleOpenPromptLibrary = useCallback((phase: WorkflowPhase) => {
    setPromptPickerPhase(phase);
    setPromptPickerOpen(true);
  }, []);

  // Handler for when a prompt is selected from the library
  const handlePromptSelected = useCallback(
    (prompt: SavedPrompt, phase: WorkflowPhase) => {
      const promptNames: Record<WorkflowPhase, string> = {
        setup: "AI Setup Task",
        verification: "AI Verification",
        agentic: "AI Prompt",
        completion: "AI Completion Task",
      };

      const step: UnifiedStep = {
        id: generateStepId(),
        type: "prompt",
        phase: phase as "setup" | "verification" | "agentic" | "completion",
        name: prompt.name || promptNames[phase],
        content: prompt.content,
        prompt_id: prompt.id,
        provider: prompt.provider ?? undefined,
        model: prompt.model ?? undefined,
        is_blocking: phase === "verification" ? true : undefined,
      };

      addStep(step, phase);
      setPromptPickerOpen(false);
    },
    [addStep],
  );

  // Handler to open the shell command library picker
  const handleOpenShellCommandLibrary = useCallback((phase: WorkflowPhase) => {
    setShellCommandPickerPhase(phase);
    setShellCommandPickerOpen(true);
  }, []);

  // Handler for when a shell command is selected from the library
  const handleShellCommandSelected = useCallback(
    (command: SavedShellCommand, phase: WorkflowPhase) => {
      const step: UnifiedStep = {
        id: generateStepId(),
        type: "shell_command",
        phase: phase as "setup" | "completion",
        name: command.name || (phase === "setup" ? "Setup Command" : "Completion Command"),
        command: command.command,
        shell_command_id: command.id,
        working_directory: command.working_directory ?? undefined,
        timeout_seconds: command.timeout_seconds,
        fail_on_error: command.fail_on_error,
      };

      addStep(step, phase);
      setShellCommandPickerOpen(false);
    },
    [addStep],
  );

  // Handler for when an AI-generated workflow is ready to load
  const handleAiWorkflowGenerated = useCallback(
    (workflow: UnifiedWorkflow) => {
      // Load the generated workflow into the builder
      // The workflow will be treated as a new workflow (not yet saved)
      resetToNew();
      updateWorkflow({
        name: workflow.name,
        description: workflow.description,
        category: workflow.category,
        tags: workflow.tags,
        setup_steps: workflow.setup_steps,
        verification_steps: workflow.verification_steps,
        agentic_steps: workflow.agentic_steps,
        completion_steps: workflow.completion_steps,
        max_iterations: workflow.max_iterations,
        provider: workflow.provider ?? undefined,
        model: workflow.model ?? undefined,
        skip_ai_summary: workflow.skip_ai_summary,
        log_source_selection: workflow.log_source_selection,
      });
      setShowSettings(true);
      console.log("[WorkflowBuilder] Loaded AI-generated workflow:", workflow.name);
    },
    [resetToNew, updateWorkflow],
  );

  const renderStep = useCallback(
    (step: UnifiedStep, index: number, phase: WorkflowPhase, steps: UnifiedStep[]) => (
      <StepItem
        key={step.id}
        step={step}
        phase={phase}
        index={index}
        isFirst={index === 0}
        isLast={index === steps.length - 1}
        isSelected={state.selectedStepId === step.id}
        onMoveUp={() => moveStep(step.id, phase, "up")}
        onMoveDown={() => moveStep(step.id, phase, "down")}
        onDelete={() => removeStep(step.id, phase)}
        onClick={() => selectStep(step.id)}
      />
    ),
    [state.selectedStepId, moveStep, removeStep, selectStep],
  );

  // Get step count for a workflow
  const getStepCount = (workflow: UnifiedWorkflow): number => {
    return (
      (workflow.setup_steps?.length || 0) +
      (workflow.verification_steps?.length || 0) +
      (workflow.agentic_steps?.length || 0) +
      (workflow.completion_steps?.length || 0)
    );
  };

  return (
    <div className="h-full flex">
      {/* Left Panel - Workflow List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <Sparkles className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Workflows
            </h2>
            <div className="flex items-center gap-1">
              <PageTutorialMenu page="unified-workflow-builder" variant="compact" />
              <button
                data-tutorial-id="ai-generate-workflow-button"
                onClick={() => setAiGenerateModalOpen(true)}
                className="p-2 rounded-lg hover:bg-neutral-800 transition-colors text-blue-400 hover:text-blue-300"
                title="Generate with AI"
              >
                <Sparkles className="w-4 h-4" />
              </button>
              <button
                data-tutorial-id="new-workflow-button"
                onClick={handleNewWorkflow}
                className="p-2 rounded-lg hover:bg-neutral-800 transition-colors"
                title="New Workflow"
              >
                <Plus className="w-4 h-4" />
              </button>
            </div>
          </div>

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search workflows..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
            />
          </div>
        </div>

        {/* Workflow List */}
        <div className="flex-1 overflow-y-auto p-2">
          {workflowsLoading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredWorkflows.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <Sparkles className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No workflows found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredWorkflows.map((workflow) => (
                <button
                  key={workflow.id}
                  onClick={() => selectWorkflow(workflow)}
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    state.workflow.id === workflow.id ? "bg-neutral-700" : "hover:bg-neutral-800"
                  }`}
                >
                  <div className="font-medium text-sm truncate">{workflow.name}</div>
                  {workflow.description && (
                    <div className="text-xs text-neutral-400 truncate mt-0.5">
                      {workflow.description}
                    </div>
                  )}
                  <div className="flex items-center gap-2 mt-1.5">
                    <span className="text-xs text-neutral-500">{getStepCount(workflow)} steps</span>
                    {workflow.category && (
                      <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-400">
                        {workflow.category}
                      </span>
                    )}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-zinc-900">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-zinc-700">
          <div className="flex items-center gap-4">
            <h1 className="text-lg font-semibold text-zinc-100">
              {state.workflow.name || "New Workflow"}
              {hasUnsavedChanges && <span className="text-zinc-500 ml-2">*</span>}
            </h1>
          </div>

          <div className="flex items-center gap-2">
            <button
              data-tutorial-id="workflow-settings"
              onClick={() => setShowSettings(!showSettings)}
              className={`
                flex items-center gap-2 px-3 py-1.5 rounded-md transition-colors
                ${showSettings ? "bg-zinc-700 text-zinc-200" : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"}
              `}
              title="Settings"
            >
              <Settings className="w-4 h-4" />
              <span className="text-sm">Settings</span>
            </button>

            <button
              data-tutorial-id="save-workflow-button"
              onClick={handleSave}
              disabled={state.isSaving || (!hasUnsavedChanges && !!state.originalWorkflow)}
              className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              title="Save workflow"
            >
              {state.isSaving ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Save className="w-4 h-4" />
              )}
              <span className="text-sm">{state.isSaving ? "Saving..." : "Save"}</span>
            </button>

            {isExecuting ? (
              <button
                onClick={handleStop}
                className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 text-white transition-colors"
                title="Stop execution"
              >
                <Square className="w-4 h-4" />
                <span className="text-sm">Stop</span>
              </button>
            ) : (
              <button
                onClick={handleRun}
                disabled={isEmpty}
                className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-blue-600 hover:bg-blue-500 text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                title="Run workflow"
              >
                <Play className="w-4 h-4" />
                <span className="text-sm">Run</span>
              </button>
            )}
          </div>
        </div>

        {/* Status display */}
        {isExecuting && (
          <div className="px-4 py-2 bg-blue-500/20 border-b border-blue-500/30 text-blue-400 text-sm flex items-center gap-2">
            <Loader2 className="w-4 h-4 animate-spin" />
            Running workflow: {state.workflow.name}...
          </div>
        )}
        {executionSuccess && !isExecuting && (
          <div className="px-4 py-2 bg-green-500/20 border-b border-green-500/30 text-green-400 text-sm">
            {executionSuccess}
          </div>
        )}
        {(state.error || executionError) && !isExecuting && (
          <div className="px-4 py-2 bg-red-500/20 border-b border-red-500/30 text-red-400 text-sm flex items-center justify-between gap-2">
            <span className="flex-1">{state.error || executionError}</span>
            <button
              onClick={() => setExecutionError(null)}
              className="text-red-400 hover:text-red-300 p-1"
              title="Dismiss"
            >
              ✕
            </button>
          </div>
        )}

        {/* Settings Panel (collapsible) */}
        {showSettings && <SettingsPanel nameInputRef={nameInputRef} />}

        {/* Main Content with Split Layout */}
        <div className="flex-1 flex overflow-hidden">
          {/* Left: Workflow Steps */}
          <div
            className={`flex-1 overflow-y-auto p-4 ${selectedStep ? "border-r border-zinc-700" : ""}`}
          >
            {isEmpty ? (
              <EmptyState onAddStep={handleAddStep} />
            ) : (
              <div className="space-y-4 max-w-3xl mx-auto">
                {/* Setup Phase */}
                <PhaseSection
                  phase="setup"
                  steps={state.workflow.setup_steps}
                  onAddStep={handleAddStep}
                  renderStep={(step, index) =>
                    renderStep(step, index, "setup", state.workflow.setup_steps)
                  }
                />

                {/* Verification Phase */}
                <PhaseSection
                  phase="verification"
                  steps={state.workflow.verification_steps}
                  onAddStep={handleAddStep}
                  renderStep={(step, index) =>
                    renderStep(step, index, "verification", state.workflow.verification_steps)
                  }
                />

                {/* Agentic Phase */}
                <PhaseSection
                  phase="agentic"
                  steps={state.workflow.agentic_steps}
                  onAddStep={handleAddStep}
                  renderStep={(step, index) =>
                    renderStep(step, index, "agentic", state.workflow.agentic_steps)
                  }
                />

                {/* Completion Phase */}
                <PhaseSection
                  phase="completion"
                  steps={state.workflow.completion_steps ?? []}
                  onAddStep={handleAddStep}
                  renderStep={(step, index) =>
                    renderStep(step, index, "completion", state.workflow.completion_steps ?? [])
                  }
                />

                {/* Global Add Step Button */}
                <div className="relative flex justify-center pt-4">
                  <AddStepButton onClick={() => handleAddStep("setup")} />
                </div>
              </div>
            )}

            {/* Add Step Dropdown */}
            {dropdownOpen && (
              <div className="fixed inset-0 z-40" onClick={() => setDropdownOpen(false)}>
                <div
                  className="absolute top-1/2 left-1/2 transform -translate-x-1/2 -translate-y-1/2"
                  onClick={(e) => e.stopPropagation()}
                >
                  <AddStepDropdown
                    filterPhase={dropdownPhase ?? undefined}
                    onAddStep={handleStepAdded}
                    isOpen={dropdownOpen}
                    onClose={() => setDropdownOpen(false)}
                    onOpenPromptLibrary={handleOpenPromptLibrary}
                    onOpenShellCommandLibrary={handleOpenShellCommandLibrary}
                  />
                </div>
              </div>
            )}

            {/* Prompt Library Picker */}
            <PromptLibraryPicker
              isOpen={promptPickerOpen}
              onClose={() => setPromptPickerOpen(false)}
              onSelect={handlePromptSelected}
              phase={promptPickerPhase}
            />

            {/* Shell Command Library Picker */}
            <ShellCommandLibraryPicker
              isOpen={shellCommandPickerOpen}
              onClose={() => setShellCommandPickerOpen(false)}
              onSelect={handleShellCommandSelected}
              phase={shellCommandPickerPhase}
            />

            {/* AI Generate Workflow Modal */}
            <AiGenerateWorkflowModal
              isOpen={aiGenerateModalOpen}
              onClose={() => setAiGenerateModalOpen(false)}
              onWorkflowGenerated={handleAiWorkflowGenerated}
            />
          </div>

          {/* Right: Step Configuration Panel */}
          {selectedStep && (
            <div className="w-96 flex-shrink-0">
              <StepConfigPanel onClose={() => selectStep(null)} />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Inner Content Wrapper (handles loading workflow by ID)
// =============================================================================

function WorkflowBuilderInner({
  editWorkflowId,
  onOpenLibrary,
  onNavigateToActive,
}: {
  editWorkflowId?: string | null;
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}) {
  const { loadWorkflow, state } = useWorkflowBuilder();

  // Load workflow if editWorkflowId is provided
  useEffect(() => {
    if (editWorkflowId && !state.isLoading) {
      loadWorkflow(editWorkflowId);
    }
  }, [editWorkflowId, loadWorkflow]); // eslint-disable-line react-hooks/exhaustive-deps

  if (state.isLoading) {
    return (
      <div className="flex items-center justify-center h-full bg-zinc-900">
        <div className="flex flex-col items-center gap-3">
          <Loader2 className="w-8 h-8 animate-spin text-zinc-400" />
          <span className="text-zinc-400">Loading workflow...</span>
        </div>
      </div>
    );
  }

  return (
    <WorkflowBuilderContent onOpenLibrary={onOpenLibrary} onNavigateToActive={onNavigateToActive} />
  );
}

// =============================================================================
// Main Export
// =============================================================================

interface WorkflowBuilderTabProps {
  editWorkflowId?: string | null;
  onOpenLibrary?: () => void;
  onNavigateToActive?: () => void;
}

export function WorkflowBuilderTab({
  editWorkflowId,
  onOpenLibrary,
  onNavigateToActive,
}: WorkflowBuilderTabProps) {
  return (
    <div data-tutorial-id="workflow-builder-nav" className="h-full">
      <WorkflowBuilderProvider>
        <WorkflowBuilderInner
          editWorkflowId={editWorkflowId}
          onOpenLibrary={onOpenLibrary}
          onNavigateToActive={onNavigateToActive}
        />
      </WorkflowBuilderProvider>
    </div>
  );
}

export default WorkflowBuilderTab;
