/**
 * SpecsPage — Spec Viewer/Editor
 *
 * Main page for viewing, editing, and managing project specifications.
 * Three-panel layout:
 * - Left: Spec tree (page specs, architecture, API, data, dependency, constraint)
 * - Center: Detail panel (spec overview or group/assertion detail with inline editing)
 * - Right: AI Chat panel (spec review, creation, gap analysis, workflow building)
 *
 * Features:
 * 1. Spec editing UI (inline editing of assertions, groups)
 * 2. AI-driven spec modifications (AI proposes changes, user reviews)
 * 3. Spec file loading from disk (all spec formats)
 * 4. Spec creation from scratch (via AI chat or templates)
 * 5. Spec-to-workflow pipeline (build verification workflows from specs)
 */

import { useEffect, useCallback, useReducer, useState } from "react";
import { ShieldCheck, FlaskConical, FileJson, X, CheckCircle2, AlertTriangle } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { useSpecsState } from "./useSpecsState";
import { ConnectionBar } from "./ConnectionBar";
import { SpecTree } from "./SpecTree";
import { SpecDetailPanel } from "./SpecDetailPanel";
import { SpecChatPanel } from "./SpecChatPanel";
import {
  buildSpecWorkflow,
  type SpecConfig as BuildSpecConfig,
} from "@/lib/workflow-builder/buildSpecWorkflow";
import { buildSpecBrief } from "@/lib/workflow-builder/buildSpecBrief";
import { generateFromBrief } from "@/lib/workflow-builder/generateFromBrief";
import { getApiBase } from "@/lib/runner-api";
import { instanceStorage } from "@/lib/instance-storage";
import { createSummaryStep } from "@/types/unified-workflow";
import { useKnownIssues } from "@/hooks/useKnownIssues";
import { SpecExperimentationDashboard } from "@/components/specs/SpecExperimentationDashboard";
import { ContractBuilder } from "./ContractBuilder";
import type { LoadedSpec } from "./types";
import { compileStateMachineFromSpecs } from "@/lib/compile-state-machine";
import { persistCompiledStateMachine } from "@/lib/persist-compiled-state-machine";
import { persistQuarantinedCompilation } from "@/lib/persist-quarantined-compilation";
import type { SpecConfig } from "@/lib/spec-prompt-builder";
import { useSpecSync } from "@/hooks/useSpecSync";

// ============================================================================
// AI spec-generation flag — persisted via instanceStorage
// ============================================================================

const AI_GENERATION_STORAGE_KEY = "specs.useAiGeneration";

// Default: AI path on. Verified end-to-end on the Terminal spec at commit
// 19d9422de (6 setup steps + 14 verifications + 1 agentic, category
// "spec-generated", tags ["spec","auto-generated"]). Users can flip back to
// the deterministic builder via the "Generate with AI (spec brief)" toggle
// in ConnectionBar — the toggle persists via instanceStorage, so
// `instanceStorage.setItem("specs.useAiGeneration", "false")` rolls back
// per-instance without a code change.
function readAiGenerationFlag(): boolean {
  return instanceStorage.getJSON<boolean>(AI_GENERATION_STORAGE_KEY, true) !== false;
}

function writeAiGenerationFlag(value: boolean): void {
  instanceStorage.setJSON(AI_GENERATION_STORAGE_KEY, value);
}

// ============================================================================
// SpecsPage reducer
// ============================================================================

type GenerateSpecRequest = {
  expectedSpecId: string;
  label: string;
  description: string;
} | null;

interface SpecsPageState {
  isSavingWorkflow: boolean;
  forcePromptOnly: boolean;
  includeRegressionChecks: boolean;
  viewMode: "editor" | "experimentation" | "contracts";
  generateSpecRequest: GenerateSpecRequest;
}

type SpecsPageAction =
  | { type: "SET_SAVING_WORKFLOW"; value: boolean }
  | { type: "TOGGLE_FORCE_PROMPT" }
  | { type: "TOGGLE_REGRESSION_CHECKS" }
  | { type: "SET_VIEW_MODE"; value: "editor" | "experimentation" | "contracts" }
  | { type: "SET_GENERATE_SPEC_REQUEST"; value: GenerateSpecRequest };

function specsPageReducer(state: SpecsPageState, action: SpecsPageAction): SpecsPageState {
  switch (action.type) {
    case "SET_SAVING_WORKFLOW":
      return { ...state, isSavingWorkflow: action.value };
    case "TOGGLE_FORCE_PROMPT":
      return { ...state, forcePromptOnly: !state.forcePromptOnly };
    case "TOGGLE_REGRESSION_CHECKS":
      return { ...state, includeRegressionChecks: !state.includeRegressionChecks };
    case "SET_VIEW_MODE":
      return { ...state, viewMode: action.value };
    case "SET_GENERATE_SPEC_REQUEST":
      return { ...state, generateSpecRequest: action.value };
  }
}

const SPECS_PAGE_INITIAL: SpecsPageState = {
  isSavingWorkflow: false,
  forcePromptOnly: false,
  includeRegressionChecks: false,
  viewMode: "editor",
  generateSpecRequest: null,
};

// ============================================================================
// SpecsPage component
// ============================================================================

/** Save a workflow via the API and navigate to builder */
async function saveWorkflowAndNavigate(
  workflowPayload: Record<string, unknown>,
  onNavigate?: (id: string) => void,
) {
  const response = await fetch(`${getApiBase()}/unified-workflows`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(workflowPayload),
  });
  if (!response.ok) {
    console.error("[Specs] HTTP error saving workflow:", response.status, response.statusText);
    return;
  }
  const data = await response.json();
  if (data.success && data.data?.id && onNavigate) {
    onNavigate(data.data.id);
  } else if (!data.success) {
    console.error("[Specs] Failed to save workflow:", data.error);
  }
}

// ============================================================================
// SpecsPageHeader -- extracted sub-component for page header
// ============================================================================

function SpecsPageHeader({
  editMode,
  viewMode,
  onSetViewMode,
}: {
  editMode: boolean;
  viewMode: "editor" | "experimentation" | "contracts";
  onSetViewMode: (mode: "editor" | "experimentation" | "contracts") => void;
}) {
  return (
    <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
      <ShieldCheck className="w-5 h-5 text-purple-400" />
      <h1 className="text-lg font-semibold">Specs</h1>
      <span className="text-xs px-1.5 py-0.5 rounded bg-purple-500/20 text-purple-400 border border-purple-500/30 font-medium">
        {editMode ? "editor" : "viewer"}
      </span>
      <div className="ml-auto flex items-center gap-1">
        <button
          onClick={() => onSetViewMode("editor")}
          className={`px-2 py-1 text-xs rounded ${
            viewMode === "editor"
              ? "bg-zinc-700 text-zinc-100"
              : "text-zinc-500 hover:text-zinc-300"
          }`}
        >
          Editor
        </button>
        <button
          onClick={() => onSetViewMode("contracts")}
          className={`px-2 py-1 text-xs rounded flex items-center gap-1 ${
            viewMode === "contracts"
              ? "bg-zinc-700 text-zinc-100"
              : "text-zinc-500 hover:text-zinc-300"
          }`}
        >
          <FileJson className="w-3 h-3" />
          Contracts
        </button>
        <button
          onClick={() => onSetViewMode("experimentation")}
          className={`px-2 py-1 text-xs rounded flex items-center gap-1 ${
            viewMode === "experimentation"
              ? "bg-zinc-700 text-zinc-100"
              : "text-zinc-500 hover:text-zinc-300"
          }`}
        >
          <FlaskConical className="w-3 h-3" />
          Experimentation
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// SyncProgressBanner — shows sync progress inline below ConnectionBar
// ============================================================================

function SyncProgressBanner({
  phase,
  progress,
  results,
  warnings,
  onCancel,
  onDismiss,
}: {
  phase: import("@/hooks/useSpecSync").SyncPhase;
  progress: import("@/hooks/useSpecSync").SyncProgress;
  results: import("@/hooks/useSpecSync").SyncResults;
  warnings: string[];
  onCancel: () => void;
  onDismiss: () => void;
}) {
  if (phase === "idle") return null;

  const isDone = phase === "done";
  const pct = progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0;

  const phaseLabel =
    phase === "analyzing"
      ? "Analyzing specs..."
      : phase === "syncing"
        ? `Syncing ${progress.current + 1}/${progress.total}`
        : phase === "compiling"
          ? "Compiling state machine..."
          : "Sync complete";

  return (
    <div className="border-b border-border bg-teal-500/5 px-4 py-2">
      <div className="flex items-center gap-3">
        {/* Phase label */}
        <span className="text-xs font-medium text-teal-400 shrink-0">{phaseLabel}</span>

        {/* Progress bar */}
        {!isDone && progress.total > 0 && (
          <div className="flex-1 max-w-xs h-1.5 bg-white/5 rounded-full overflow-hidden">
            <div
              className="h-full bg-teal-500 rounded-full transition-all duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
        )}

        {/* Current spec */}
        {!isDone && progress.currentSpec && (
          <span className="text-[10px] text-muted-foreground truncate max-w-[200px]">
            {progress.currentSpec}
          </span>
        )}

        {/* Done summary */}
        {isDone && (
          <div className="flex items-center gap-3 text-[10px]">
            {results.updated.length > 0 && (
              <span className="flex items-center gap-1 text-green-400">
                <CheckCircle2 className="w-3 h-3" />
                {results.updated.length} updated
              </span>
            )}
            {results.skipped.length > 0 && (
              <span className="text-muted-foreground">{results.skipped.length} skipped</span>
            )}
            {results.failed.length > 0 && (
              <span className="flex items-center gap-1 text-red-400">
                <AlertTriangle className="w-3 h-3" />
                {results.failed.length} failed
              </span>
            )}
            {warnings.length > 0 && (
              <span className="text-amber-400">{warnings.length} warnings</span>
            )}
          </div>
        )}

        {/* Cancel / Dismiss */}
        <button
          onClick={isDone ? onDismiss : onCancel}
          className="ml-auto shrink-0 p-0.5 rounded hover:bg-white/10 text-muted-foreground hover:text-foreground transition-colors"
          title={isDone ? "Dismiss" : "Cancel sync"}
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

interface SpecsPageProps {
  onNavigateToWorkflowBuilder?: (workflowId: string) => void;
}

export function SpecsPage({ onNavigateToWorkflowBuilder }: SpecsPageProps) {
  const state = useSpecsState();
  const [pageState, dispatch] = useReducer(specsPageReducer, SPECS_PAGE_INITIAL);
  const {
    isSavingWorkflow,
    forcePromptOnly,
    includeRegressionChecks,
    viewMode,
    generateSpecRequest,
  } = pageState;
  const { issues: knownIssues, loadIssuesForSpec } = useKnownIssues();

  // AI spec-generation toggle (persisted across reloads via instanceStorage).
  const [useAiSpecGeneration, setUseAiSpecGeneration] = useState<boolean>(() =>
    readAiGenerationFlag(),
  );
  const toggleUseAiSpecGeneration = useCallback(() => {
    setUseAiSpecGeneration((prev) => {
      const next = !prev;
      writeAiGenerationFlag(next);
      return next;
    });
  }, []);

  // Dedicated progress state for the AI path (generation can take 30+ seconds).
  const [isGeneratingWithAi, setIsGeneratingWithAi] = useState(false);

  useUIComponent({
    id: "specs-page",
    name: "Specs Page",
    description:
      "View, edit, and manage project specifications with AI-driven sync and state machine compilation",
    actions: [
      {
        id: "toggle-ai-generation",
        label: "Toggle AI spec-generation flag",
        description:
          "Toggle the useAiGeneration flag (same as clicking the AI toggle button). " +
          "Invoke via POST /ui-bridge/control/component/specs-page/action/toggle-ai-generation",
        handler: () => {
          toggleUseAiSpecGeneration();
        },
      },
    ],
    state: () => ({
      useAiSpecGeneration,
    }),
  });

  // Spec sync — batch update all stale specs with AI merge mode
  const specSync = useSpecSync(state.specs, state.addSpec);

  // Triage resolution context — set when user clicks "Update Spec" in SpecTriageView
  const [triageContext, setTriageContext] = useState<{
    specId: string;
    groupId: string;
  } | null>(null);

  // Auto-load bundled specs on first mount only (skip if restored from cache)
  useEffect(() => {
    if (!state.restoredFromCache) {
      state.loadBundledSpecs();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Load known issues when a spec is selected (for regression checks)
  useEffect(() => {
    if (state.selectedSpec?.kind === "page-spec") {
      const pageUrl = (state.selectedSpec.config as { metadata?: { pageUrl?: string } })?.metadata
        ?.pageUrl;
      loadIssuesForSpec(state.selectedSpec.specId, pageUrl);
    }
  }, [state.selectedSpec, loadIssuesForSpec]);

  // AI path: build a GenerationBrief, run the AI pipeline, persist the returned
  // workflow, and navigate to the builder. On any failure, logs and returns —
  // does NOT silently fall back to the deterministic path.
  const runAiGeneration = useCallback(
    async (
      briefInput: Parameters<typeof buildSpecBrief>[0],
      extraTags?: string[],
    ): Promise<void> => {
      setIsGeneratingWithAi(true);
      try {
        const brief = buildSpecBrief(briefInput);
        const response = await generateFromBrief(brief);

        if (!response.success) {
          console.error(
            "[Specs] AI workflow generation failed:",
            response.message || "(no message)",
          );
          return;
        }

        // Mirror the shape returned by `generate_workflow_standalone`:
        // `data = { success, error?, workflow? }`. We follow the same idiom as
        // `useWorkflowGeneration.ts` and read the workflow from `data.workflow`.
        const data = response.data as
          | { workflow?: Record<string, unknown>; error?: string }
          | null
          | undefined;
        const workflow = data?.workflow;
        if (!workflow) {
          console.error(
            "[Specs] AI generation returned no workflow:",
            data?.error || response.message || "(no details)",
          );
          return;
        }

        // The Rust backend persists the workflow server-side and returns the
        // DB id on `workflow.id`. Navigate to it directly — no re-POST. When
        // the caller supplied `extraTags`, we still need a re-POST to apply
        // them (rare path; triage fix is the only current caller).
        const workflowId = typeof workflow.id === "string" ? workflow.id : "";
        if (!extraTags || extraTags.length === 0) {
          if (workflowId) {
            onNavigateToWorkflowBuilder?.(workflowId);
          } else {
            console.error("[Specs] AI-generated workflow has no id; cannot navigate");
          }
          return;
        }

        const existingTags = Array.isArray(workflow.tags) ? (workflow.tags as unknown[]) : [];
        const payload: Record<string, unknown> = {
          ...workflow,
          reflection_mode: true,
          tags: [...existingTags, ...extraTags],
        };
        // Strip the server-assigned id so saveWorkflowAndNavigate creates a
        // new row with the merged tags rather than colliding on primary key.
        delete payload.id;
        await saveWorkflowAndNavigate(payload, onNavigateToWorkflowBuilder);
      } catch (err) {
        console.error("[Specs] Error during AI workflow generation:", err);
      } finally {
        setIsGeneratingWithAi(false);
      }
    },
    [onNavigateToWorkflowBuilder],
  );

  // Build workflow from any spec type, save via API, navigate to builder
  const handleBuildWorkflow = useCallback(
    async (spec?: LoadedSpec) => {
      const target = spec || state.selectedSpec;
      if (!target || isSavingWorkflow || isGeneratingWithAi) return;

      if (target.kind === "page-spec") {
        const activeIssues = knownIssues.filter((i) => i.status === "active");
        const specConfig = target.config as unknown as BuildSpecConfig;
        const workflowName = `Spec: ${target.config.description || target.specId}`;

        // AI path — flag-gated. Does NOT fall back to deterministic on error.
        if (useAiSpecGeneration) {
          const pageUrl = (target.config as { metadata?: { pageUrl?: string } })?.metadata?.pageUrl;
          await runAiGeneration({
            specConfig,
            workflowName,
            pageUrl,
          });
          return;
        }

        const workflow = buildSpecWorkflow({
          specConfig,
          workflowName,
          forcePromptOnly,
          includeRegressionChecks: includeRegressionChecks && activeIssues.length > 0,
          knownIssues: includeRegressionChecks ? activeIssues : [],
        });

        dispatch({ type: "SET_SAVING_WORKFLOW", value: true });
        try {
          await saveWorkflowAndNavigate(
            { ...workflow, reflection_mode: true },
            onNavigateToWorkflowBuilder,
          );
        } catch (err) {
          console.error("[Specs] Error saving workflow:", err);
        } finally {
          dispatch({ type: "SET_SAVING_WORKFLOW", value: false });
        }
      } else {
        // For non-page specs, generate a prompt-based workflow
        const kindLabel = target.kind.replace("-", " ");
        const specContext = JSON.stringify(target.config, null, 2);

        dispatch({ type: "SET_SAVING_WORKFLOW", value: true });
        try {
          const setupPrompt = `Implement the following ${kindLabel} specification:\n\n\`\`\`json\n${specContext}\n\`\`\``;
          const workflow = {
            name: `Build: ${(target.config as { description?: string }).description || target.specId}`,
            description: `Auto-generated from ${kindLabel} spec: ${target.specId}`,
            category: "spec-generated",
            tags: ["spec", "auto-generated", kindLabel],
            setup_steps: [],
            verification_steps: [
              {
                id: crypto.randomUUID(),
                type: "prompt",
                phase: "verification",
                name: `Verify ${kindLabel}`,
                content: `Verify the implementation matches the ${kindLabel} specification. Check each item in the spec against the actual codebase.`,
              },
            ],
            agentic_steps: [
              {
                id: crypto.randomUUID(),
                type: "prompt",
                phase: "agentic",
                name: `Implement ${kindLabel}`,
                content: setupPrompt,
              },
            ],
            completion_steps: [createSummaryStep()],
            max_iterations: 3,
            reflection_mode: true,
          };

          await saveWorkflowAndNavigate(workflow, onNavigateToWorkflowBuilder);
        } catch (err) {
          console.error("[Specs] Error saving workflow:", err);
        } finally {
          dispatch({ type: "SET_SAVING_WORKFLOW", value: false });
        }
      }
    },
    [
      state.selectedSpec,
      isSavingWorkflow,
      isGeneratingWithAi,
      onNavigateToWorkflowBuilder,
      forcePromptOnly,
      includeRegressionChecks,
      knownIssues,
      useAiSpecGeneration,
      runAiGeneration,
    ],
  );

  // Compile all specs' stateMachine sections into a runtime state machine
  const handleCompileStateMachine = useCallback(() => {
    const allSpecs = state.specs.map((s) => s.config as SpecConfig);
    const compilation = compileStateMachineFromSpecs(allSpecs);

    if (!compilation.compiled) {
      // Quarantined — persist the record and refuse to load into the engine.
      void persistQuarantinedCompilation(compilation.quarantine);
      console.warn(
        `[Specs] State machine compilation quarantined: ${compilation.quarantine.conflicts.length} element conflicts (record ${compilation.quarantine.id})`,
        compilation.quarantine.conflicts,
      );
      return;
    }

    const { stateMachine, stats } = compilation;

    // Load into the engine via the bridge
    const bridge = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
      | Record<string, unknown>
      | undefined;
    if (bridge?.loadStateMachine) {
      const json = JSON.stringify(stateMachine);
      const result = (bridge.loadStateMachine as (json: string) => Record<string, unknown>)(json);
      console.warn(
        `[Specs] State machine compiled: ${stats.statesCompiled} states, ${stats.transitionsCompiled} transitions from ${stats.specsProcessed} specs`,
        result,
      );
    } else {
      // loadStateMachine not available — dispatch config-changed event as fallback
      console.warn(
        `[Specs] State machine compiled: ${stats.statesCompiled} states, ${stats.transitionsCompiled} transitions (engine not available — loadStateMachine not found on bridge)`,
      );
    }

    // Best-effort: also persist to the backend DB so the compiled machine is
    // available via the HTTP API (and survives runner restarts).
    void persistCompiledStateMachine(stateMachine);

    if (stats.warnings.length > 0) {
      console.warn("[Specs] Compilation warnings:", stats.warnings);
    }
  }, [state.specs]);

  // Triage: "Update Spec" — trigger merge review in chat panel
  const handleTriageUpdateSpec = useCallback(
    (groupId: string) => {
      if (!state.selectedSpec || state.selectedSpec.kind !== "page-spec") return;
      setTriageContext({ specId: state.selectedSpec.specId, groupId });
    },
    [state.selectedSpec],
  );

  // Triage: "Fix Code" — create a scoped workflow for the broken group
  const handleTriageFixCode = useCallback(
    async (groupId: string) => {
      if (
        !state.selectedSpec ||
        state.selectedSpec.kind !== "page-spec" ||
        isSavingWorkflow ||
        isGeneratingWithAi
      )
        return;

      const specConfig = state.selectedSpec.config as unknown as BuildSpecConfig;
      const groupName =
        specConfig.groups.find((g: { id: string }) => g.id === groupId)?.name || groupId;

      const selectedGroupIds = new Set([groupId]);
      const agenticPrompt = `These assertions in group "${groupName}" were previously passing but now fail. Investigate the regression and fix the code to make them pass again.`;
      const workflowName = `Fix: ${specConfig.description || state.selectedSpec.specId} — ${groupName}`;

      // AI path — flag-gated. Does NOT fall back on error.
      if (useAiSpecGeneration) {
        const pageUrl = (state.selectedSpec.config as { metadata?: { pageUrl?: string } })?.metadata
          ?.pageUrl;
        await runAiGeneration(
          {
            specConfig,
            selectedGroupIds,
            pageUrl,
            workflowName,
            additionalInstructions: agenticPrompt,
          },
          ["triage-fix"],
        );
        return;
      }

      const workflow = buildSpecWorkflow({
        specConfig,
        selectedGroupIds,
        agenticPrompt,
        workflowName,
      });

      dispatch({ type: "SET_SAVING_WORKFLOW", value: true });
      try {
        const response = await fetch(`${getApiBase()}/unified-workflows`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            name: workflow.name,
            description: workflow.description,
            category: workflow.category,
            tags: [...(workflow.tags || []), "triage-fix"],
            setupSteps: workflow.setupSteps,
            verificationSteps: workflow.verificationSteps,
            agenticSteps: workflow.agenticSteps,
            completionSteps: workflow.completionSteps,
            maxIterations: workflow.maxIterations,
            reflection_mode: true,
          }),
        });

        const data = await response.json();
        if (data.success && data.data?.id && onNavigateToWorkflowBuilder) {
          onNavigateToWorkflowBuilder(data.data.id);
        }
      } catch (err) {
        console.error("[Specs] Error creating fix workflow:", err);
      } finally {
        dispatch({ type: "SET_SAVING_WORKFLOW", value: false });
      }
    },
    [
      state.selectedSpec,
      isSavingWorkflow,
      isGeneratingWithAi,
      onNavigateToWorkflowBuilder,
      useAiSpecGeneration,
      runAiGeneration,
    ],
  );

  return (
    <div className="h-full flex flex-col">
      <SpecsPageHeader
        editMode={state.editMode}
        viewMode={viewMode}
        onSetViewMode={(mode) => dispatch({ type: "SET_VIEW_MODE", value: mode })}
      />

      {viewMode === "contracts" ? (
        <div className="flex-1 min-h-0">
          <ContractBuilder />
        </div>
      ) : viewMode === "experimentation" ? (
        <div className="flex-1 overflow-y-auto">
          <SpecExperimentationDashboard />
        </div>
      ) : (
        <>
          {/* Connection bar */}
          <ConnectionBar
            connection={state.connection}
            isLoading={state.isLoading || isSavingWorkflow || isGeneratingWithAi}
            stats={state.stats}
            editMode={state.editMode}
            hasSelectedSpec={!!state.selectedSpec}
            selectedSpecKind={state.selectedSpec?.kind || null}
            onLoadBundled={state.loadBundledSpecs}
            onDiscover={state.discoverFromApp}
            onLoadFromFile={state.loadFromFile}
            onSaveToFile={state.saveToFile}
            forcePromptOnly={forcePromptOnly}
            onToggleForcePromptOnly={() => dispatch({ type: "TOGGLE_FORCE_PROMPT" })}
            includeRegressionChecks={includeRegressionChecks}
            onToggleRegressionChecks={() => dispatch({ type: "TOGGLE_REGRESSION_CHECKS" })}
            regressionIssueCount={knownIssues.filter((i) => i.status === "active").length}
            useAiSpecGeneration={useAiSpecGeneration}
            onToggleUseAiSpecGeneration={toggleUseAiSpecGeneration}
            isGeneratingWithAi={isGeneratingWithAi}
            onBuildWorkflow={() => handleBuildWorkflow()}
            onCompileStateMachine={handleCompileStateMachine}
            onSyncAllSpecs={specSync.startSync}
            isSyncing={specSync.isSyncing}
            syncProgress={specSync.state.progress}
            onCancelSync={specSync.cancel}
            onToggleEditMode={() => state.setEditMode(!state.editMode)}
          />

          {/* Sync progress banner */}
          {specSync.state.phase !== "idle" && (
            <SyncProgressBanner
              phase={specSync.state.phase}
              progress={specSync.state.progress}
              results={specSync.state.results}
              warnings={specSync.state.warnings}
              onCancel={specSync.cancel}
              onDismiss={specSync.reset}
            />
          )}

          {/* Main content: tree + detail + chat */}
          <div className="flex-1 flex min-h-0">
            {/* Left sidebar: spec tree */}
            <div className="w-64 shrink-0 border-r border-border overflow-y-auto">
              <div className="px-3 py-2 border-b border-border">
                <h2 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  Specifications
                </h2>
              </div>
              <SpecTree
                tree={state.tree}
                expandedNodes={state.expandedNodes}
                selection={state.selection}
                onToggle={state.toggleNode}
                onSelect={state.selectNode}
              />
            </div>

            {/* Center: detail panel */}
            <div className="flex-1 min-w-0">
              <SpecDetailPanel
                selectedSpec={state.selectedSpec}
                selectedGroup={state.selectedGroup}
                selectionType={state.selection.type}
                editMode={state.editMode}
                unspeccedPageInfo={
                  state.selection.type === "unspecced-page"
                    ? {
                        label: state.selection.label,
                        description: state.selection.description,
                        expectedSpecId: state.selection.expectedSpecId,
                      }
                    : null
                }
                onToggleAssertion={state.toggleAssertion}
                onRemoveAssertion={state.removeAssertion}
                onAddAssertion={state.addAssertion}
                onAddGroup={state.addGroup}
                onRemoveGroup={state.removeGroup}
                onUpdateSetupActions={state.updateSetupActions}
                onClearSelection={() => state.setSelection({ type: "none" })}
                onGenerateSpec={
                  state.selection.type === "unspecced-page"
                    ? () => {
                        const sel = state.selection as {
                          type: "unspecced-page";
                          expectedSpecId: string;
                          label: string;
                          description: string;
                        };
                        dispatch({
                          type: "SET_GENERATE_SPEC_REQUEST",
                          value: {
                            expectedSpecId: sel.expectedSpecId,
                            label: sel.label,
                            description: sel.description,
                          },
                        });
                      }
                    : undefined
                }
                onUpdateSpec={handleTriageUpdateSpec}
                onFixCode={handleTriageFixCode}
              />
            </div>

            {/* Right: AI Chat panel */}
            <div className="flex-[0.5] min-w-[280px] border-l border-border flex flex-col">
              <SpecChatPanel
                selectedSpec={state.selectedSpec}
                onAddSpec={state.addSpec}
                onBuildWorkflow={handleBuildWorkflow}
                generateSpecRequest={generateSpecRequest}
                onGenerateSpecHandled={() =>
                  dispatch({ type: "SET_GENERATE_SPEC_REQUEST", value: null })
                }
                triageContext={triageContext}
                onTriageContextHandled={() => setTriageContext(null)}
              />
            </div>
          </div>
        </>
      )}
    </div>
  );
}
