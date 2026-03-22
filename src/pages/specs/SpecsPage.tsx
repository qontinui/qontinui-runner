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
import { ShieldCheck, FlaskConical, FileJson } from "lucide-react";
import { useSpecsState } from "./useSpecsState";
import { ConnectionBar } from "./ConnectionBar";
import { SpecTree } from "./SpecTree";
import { SpecDetailPanel } from "./SpecDetailPanel";
import { SpecChatPanel } from "./SpecChatPanel";
import {
  buildSpecWorkflow,
  type SpecConfig as BuildSpecConfig,
} from "@/lib/workflow-builder/buildSpecWorkflow";
import { getApiBase } from "@/lib/runner-api";
import { createSummaryStep } from "@/types/unified-workflow";
import { useKnownIssues } from "@/hooks/useKnownIssues";
import { SpecExperimentationDashboard } from "@/components/specs/SpecExperimentationDashboard";
import { ContractBuilder } from "./ContractBuilder";
import type { LoadedSpec } from "./types";

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

  // Build workflow from any spec type, save via API, navigate to builder
  const handleBuildWorkflow = useCallback(
    async (spec?: LoadedSpec) => {
      const target = spec || state.selectedSpec;
      if (!target || isSavingWorkflow) return;

      if (target.kind === "page-spec") {
        const activeIssues = knownIssues.filter((i) => i.status === "active");
        const workflow = buildSpecWorkflow({
          specConfig: target.config as unknown as BuildSpecConfig,
          workflowName: `Spec: ${target.config.description || target.specId}`,
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
      onNavigateToWorkflowBuilder,
      forcePromptOnly,
      includeRegressionChecks,
      knownIssues,
    ],
  );

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
      if (!state.selectedSpec || state.selectedSpec.kind !== "page-spec" || isSavingWorkflow)
        return;

      const specConfig = state.selectedSpec.config as unknown as BuildSpecConfig;
      const groupName =
        specConfig.groups.find((g: { id: string }) => g.id === groupId)?.name || groupId;

      const workflow = buildSpecWorkflow({
        specConfig,
        selectedGroupIds: new Set([groupId]),
        agenticPrompt: `These assertions in group "${groupName}" were previously passing but now fail. Investigate the regression and fix the code to make them pass again.`,
        workflowName: `Fix: ${specConfig.description || state.selectedSpec.specId} — ${groupName}`,
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
            setup_steps: workflow.setup_steps,
            verification_steps: workflow.verification_steps,
            agentic_steps: workflow.agentic_steps,
            completion_steps: workflow.completion_steps,
            max_iterations: workflow.max_iterations,
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
    [state.selectedSpec, isSavingWorkflow, onNavigateToWorkflowBuilder],
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
            isLoading={state.isLoading || isSavingWorkflow}
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
            onBuildWorkflow={() => handleBuildWorkflow()}
            onToggleEditMode={() => state.setEditMode(!state.editMode)}
          />

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
