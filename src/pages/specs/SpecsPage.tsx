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

import { useEffect, useCallback, useState } from "react";
import { ShieldCheck } from "lucide-react";
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
import type { LoadedSpec } from "./types";

interface SpecsPageProps {
  onNavigateToWorkflowBuilder?: (workflowId: string) => void;
}

export function SpecsPage({ onNavigateToWorkflowBuilder }: SpecsPageProps) {
  const state = useSpecsState();
  const [isSavingWorkflow, setIsSavingWorkflow] = useState(false);
  const [forcePromptOnly, setForcePromptOnly] = useState(false);

  // Auto-load bundled specs on first mount only (skip if restored from cache)
  useEffect(() => {
    if (!state.restoredFromCache) {
      state.loadBundledSpecs();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Build workflow from any spec type, save via API, navigate to builder
  const handleBuildWorkflow = useCallback(
    async (spec?: LoadedSpec) => {
      const target = spec || state.selectedSpec;
      if (!target || isSavingWorkflow) return;

      if (target.kind === "page-spec") {
        const workflow = buildSpecWorkflow({
          specConfig: target.config as unknown as BuildSpecConfig,
          workflowName: `Spec: ${target.config.description || target.specId}`,
          forcePromptOnly,
        });

        // Save to backend and navigate to builder
        setIsSavingWorkflow(true);
        try {
          const response = await fetch(`${getApiBase()}/unified-workflows`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              name: workflow.name,
              description: workflow.description,
              category: workflow.category,
              tags: workflow.tags,
              setup_steps: workflow.setup_steps,
              verification_steps: workflow.verification_steps,
              agentic_steps: workflow.agentic_steps,
              completion_steps: workflow.completion_steps,
              max_iterations: workflow.max_iterations,
            }),
          });

          const data = await response.json();
          if (data.success && data.data?.id) {
            if (onNavigateToWorkflowBuilder) {
              onNavigateToWorkflowBuilder(data.data.id);
            }
          } else {
            console.error("[Specs] Failed to save workflow:", data.error);
          }
        } catch (err) {
          console.error("[Specs] Error saving workflow:", err);
        } finally {
          setIsSavingWorkflow(false);
        }
      } else {
        // For non-page specs, generate a prompt-based workflow
        const kindLabel = target.kind.replace("-", " ");
        const specContext = JSON.stringify(target.config, null, 2);

        setIsSavingWorkflow(true);
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
            completion_steps: [],
            max_iterations: 3,
          };

          const response = await fetch(`${getApiBase()}/unified-workflows`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(workflow),
          });

          const data = await response.json();
          if (data.success && data.data?.id) {
            if (onNavigateToWorkflowBuilder) {
              onNavigateToWorkflowBuilder(data.data.id);
            }
          } else {
            console.error("[Specs] Failed to save workflow:", data.error);
          }
        } catch (err) {
          console.error("[Specs] Error saving workflow:", err);
        } finally {
          setIsSavingWorkflow(false);
        }
      }
    },
    [state.selectedSpec, isSavingWorkflow, onNavigateToWorkflowBuilder, forcePromptOnly],
  );

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
        <ShieldCheck className="w-5 h-5 text-purple-400" />
        <h1 className="text-lg font-semibold">Specs</h1>
        <span className="text-xs px-1.5 py-0.5 rounded bg-purple-500/20 text-purple-400 border border-purple-500/30 font-medium">
          {state.editMode ? "editor" : "viewer"}
        </span>
      </div>

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
        onToggleForcePromptOnly={() => setForcePromptOnly(!forcePromptOnly)}
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
            onToggleAssertion={state.toggleAssertion}
            onRemoveAssertion={state.removeAssertion}
            onAddAssertion={state.addAssertion}
            onAddGroup={state.addGroup}
            onRemoveGroup={state.removeGroup}
          />
        </div>

        {/* Right: AI Chat panel */}
        <div className="w-80 shrink-0 border-l border-border flex flex-col">
          <SpecChatPanel
            selectedSpec={state.selectedSpec}
            onAddSpec={state.addSpec}
            onBuildWorkflow={handleBuildWorkflow}
          />
        </div>
      </div>
    </div>
  );
}
