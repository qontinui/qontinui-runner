/**
 * SpecWorkflowBuilder
 *
 * Main component for building workflows from test specification files.
 * Handles both snapshot (Tier 1) and navigation (Tier 2) specs.
 *
 * Flow:
 * 1. Load SpecConfig JSON (.spec.uibridge.json or legacy migrated)
 * 2. Select which spec groups to include as verification steps
 * 3. Configure agentic prompt for failure recovery
 * 4. Preview and apply the generated workflow
 */

import { useState, useCallback, useMemo } from "react";
import { FileJson, CheckSquare, Bot, Eye } from "lucide-react";
import { SpecFileLoader } from "./SpecFileLoader";
import { SpecSelector } from "./SpecSelector";
import { NavigationGraphPreview } from "./NavigationGraphPreview";
import { AgenticPromptEditor } from "./AgenticPromptEditor";
import { WorkflowPreview } from "./WorkflowPreview";
import type { SpecConfig, GeneratorSpecMetadata } from "./types";
import type {
  UnifiedWorkflow,
  SetupStep,
  VerificationStep,
  PromptStep,
} from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";

type Step = "load" | "select" | "configure" | "preview";

interface SpecWorkflowBuilderProps {
  onApplyWorkflow?: (workflow: UnifiedWorkflow) => void;
}

export function SpecWorkflowBuilder({ onApplyWorkflow }: SpecWorkflowBuilderProps) {
  const [currentStep, setCurrentStep] = useState<Step>("load");
  const [loadedData, setLoadedData] = useState<SpecConfig | null>(null);
  const [fileName, setFileName] = useState<string>("");
  const [selectedSpecIds, setSelectedSpecIds] = useState<Set<string>>(new Set());
  const [agenticPrompt, setAgenticPrompt] = useState("");
  const [maxIterations, setMaxIterations] = useState(3);
  const [elementSource, setElementSource] = useState<"control" | "external">("control");

  // Extract generator metadata from SpecConfig
  const genMeta = useMemo<GeneratorSpecMetadata | undefined>(
    () => loadedData?.metadata as GeneratorSpecMetadata | undefined,
    [loadedData],
  );

  const generatorType = genMeta?.generatorType;
  const states = useMemo(() => genMeta?.states || [], [genMeta]);
  const transitions = useMemo(() => genMeta?.transitions || [], [genMeta]);

  // Handle file load
  const handleLoad = useCallback((config: SpecConfig) => {
    setLoadedData(config);
    const meta = config.metadata as GeneratorSpecMetadata | undefined;
    const genType = meta?.generatorType || "spec";
    setFileName(`${genType}.spec.uibridge.json`);
    // Auto-select all groups with critical assertions
    const criticalIds = new Set(
      config.groups
        .filter((g) => g.assertions.some((a) => a.enabled && a.severity === "critical"))
        .map((g) => g.id),
    );
    setSelectedSpecIds(criticalIds);
    // Determine element source: prefer explicit metadata, otherwise default to "control"
    const rawMeta = config.metadata as Record<string, unknown> | undefined;
    const explicitSource = rawMeta?.elementSource as "control" | "external" | undefined;
    setElementSource(explicitSource || "control");
    setCurrentStep("select");
  }, []);

  // Toggle spec selection
  const toggleSpec = useCallback((specId: string) => {
    setSelectedSpecIds((prev) => {
      const next = new Set(prev);
      if (next.has(specId)) next.delete(specId);
      else next.add(specId);
      return next;
    });
  }, []);

  // Select all / deselect all
  const selectAll = useCallback(() => {
    if (!loadedData) return;
    setSelectedSpecIds(new Set(loadedData.groups.map((g) => g.id)));
  }, [loadedData]);

  const deselectAll = useCallback(() => {
    setSelectedSpecIds(new Set());
  }, []);

  // Build the full UnifiedWorkflow
  const workflow: UnifiedWorkflow | null = useMemo(() => {
    if (!loadedData) return null;

    const now = new Date().toISOString();
    const isNavigation = generatorType === "navigation";
    const workflowName = isNavigation ? "Navigation Verification" : "Snapshot Verification";

    // Setup steps
    const setupSteps: SetupStep[] = [];
    if (isNavigation && transitions.length > 0) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const explorationMeta = (genMeta as any)?.explorationMetadata;
      const pageUrl = explorationMeta?.targetUrl || states[0]?.pageUrl || "";
      if (pageUrl) {
        setupSteps.push({
          id: crypto.randomUUID(),
          type: "command",
          phase: "setup",
          name: `Navigate to ${pageUrl}`,
          mode: "shell",
          command: `playwright test --headed -g 'navigate' -- --base-url='${pageUrl}'`,
        } satisfies SetupStep);
      }
    }

    // Verification steps: each selected group becomes a prompt-based verification step
    const selectedGroups = loadedData.groups.filter((g) => selectedSpecIds.has(g.id));
    const verificationSteps: VerificationStep[] = selectedGroups.map((group) => {
      const enabledAssertions = group.assertions.filter((a) => a.enabled);
      const assertionDescriptions = enabledAssertions
        .map((a) => `- ${a.description} [${a.severity}]`)
        .join("\n");

      return {
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "verification",
        name: group.name,
        content: `${group.description}\n\nAssertions:\n${assertionDescriptions}`,
      } satisfies VerificationStep;
    });

    // Agentic steps
    const agenticSteps: PromptStep[] = [];
    if (agenticPrompt || selectedGroups.length > 0) {
      const defaultPrompt =
        agenticPrompt ||
        `Some verification steps failed. Analyze the failures and fix the issues. The test specifications describe what the application should look like and how it should behave.`;
      agenticSteps.push({
        id: crypto.randomUUID(),
        type: "prompt",
        phase: "agentic",
        name: "Fix Verification Failures",
        content: defaultPrompt,
      });
    }

    // Completion steps
    const completionSteps = [createSummaryStep()];

    return {
      id: crypto.randomUUID(),
      name: workflowName,
      description: `Auto-generated from ${generatorType || "spec"} specifications. ${selectedGroups.length} groups selected with ${selectedGroups.reduce((sum, g) => sum + g.assertions.filter((a) => a.enabled).length, 0)} total assertions.`,
      setup_steps: setupSteps,
      verification_steps: verificationSteps,
      agentic_steps: agenticSteps,
      completion_steps: completionSteps,
      max_iterations: maxIterations,
      category: "spec-generated",
      tags: [generatorType || "spec", "auto-generated"].filter(Boolean) as string[],
      created_at: now,
      modified_at: now,
    };
  }, [
    loadedData,
    selectedSpecIds,
    agenticPrompt,
    maxIterations,
    elementSource,
    generatorType,
    genMeta,
    states,
    transitions,
  ]);

  const steps: { id: Step; label: string; icon: React.ReactNode }[] = [
    { id: "load", label: "Load Specs", icon: <FileJson className="w-4 h-4" /> },
    { id: "select", label: "Select", icon: <CheckSquare className="w-4 h-4" /> },
    { id: "configure", label: "Configure", icon: <Bot className="w-4 h-4" /> },
    { id: "preview", label: "Preview", icon: <Eye className="w-4 h-4" /> },
  ];

  return (
    <div className="flex flex-col h-full">
      {/* Step indicator */}
      <div className="flex items-center gap-1 px-4 py-3 border-b border-neutral-700 bg-neutral-800/50">
        {steps.map((step, i) => (
          <div key={step.id} className="flex items-center">
            <button
              onClick={() => {
                if (step.id === "load" || loadedData) setCurrentStep(step.id);
              }}
              disabled={step.id !== "load" && !loadedData}
              className={`flex items-center gap-2 px-3 py-1.5 text-xs rounded-md transition-colors ${
                currentStep === step.id
                  ? "bg-emerald-500/20 text-emerald-400 border border-emerald-500/30"
                  : "text-neutral-400 hover:text-neutral-200 disabled:opacity-30"
              }`}
            >
              {step.icon}
              {step.label}
            </button>
            {i < steps.length - 1 && <span className="text-neutral-600 mx-1">&rsaquo;</span>}
          </div>
        ))}

        {loadedData && (
          <span className="text-xs text-neutral-500 ml-auto">
            {generatorType === "snapshot"
              ? "Snapshot"
              : generatorType === "navigation"
                ? "Navigation"
                : "Spec"}{" "}
            &middot; {loadedData.groups.length} groups
          </span>
        )}
      </div>

      {/* Content */}
      <div className="flex-1 min-h-0 overflow-hidden">
        {currentStep === "load" && (
          <SpecFileLoader onLoad={handleLoad} currentFile={fileName || undefined} />
        )}

        {currentStep === "select" && loadedData && (
          <div className="flex flex-col h-full">
            {/* Navigation graph preview for Tier 2 */}
            {generatorType === "navigation" && states.length > 0 && (
              <div className="border-b border-neutral-700 max-h-[200px] overflow-auto">
                <NavigationGraphPreview states={states} transitions={transitions} />
              </div>
            )}
            <div className="flex-1 min-h-0">
              <SpecSelector
                specs={loadedData.groups}
                selectedIds={selectedSpecIds}
                onToggle={toggleSpec}
                onSelectAll={selectAll}
                onDeselectAll={deselectAll}
              />
            </div>
            <div className="px-4 py-3 border-t border-neutral-700 bg-neutral-800/50">
              <button
                onClick={() => setCurrentStep("configure")}
                disabled={selectedSpecIds.size === 0}
                className="px-4 py-2 text-sm font-medium bg-emerald-600 text-white rounded-md hover:bg-emerald-700 disabled:opacity-50 transition-colors"
              >
                Continue with {selectedSpecIds.size} groups
              </button>
            </div>
          </div>
        )}

        {currentStep === "configure" && (
          <div className="flex flex-col h-full">
            <div className="flex-1 overflow-auto">
              <AgenticPromptEditor
                prompt={agenticPrompt}
                onPromptChange={setAgenticPrompt}
                maxIterations={maxIterations}
                onMaxIterationsChange={setMaxIterations}
                elementSource={elementSource}
                onElementSourceChange={setElementSource}
              />
            </div>
            <div className="px-4 py-3 border-t border-neutral-700 bg-neutral-800/50">
              <button
                onClick={() => setCurrentStep("preview")}
                className="px-4 py-2 text-sm font-medium bg-emerald-600 text-white rounded-md hover:bg-emerald-700 transition-colors"
              >
                Preview Workflow
              </button>
            </div>
          </div>
        )}

        {currentStep === "preview" && workflow && (
          <WorkflowPreview
            workflow={workflow}
            onApply={onApplyWorkflow ? () => onApplyWorkflow(workflow) : undefined}
          />
        )}
      </div>
    </div>
  );
}
