/**
 * SpecWorkflowBuilder
 *
 * Main component for building workflows from test specification files.
 * Handles both snapshot (Tier 1) and navigation (Tier 2) specs.
 *
 * Flow:
 * 1. Load TestGeneratorOutput JSON
 * 2. Select which specs to include as verification steps
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
import type { TestGeneratorOutput } from "./types";
import type {
  UnifiedWorkflow,
  ScriptStep,
  TestStep,
  PromptStep,
} from "../../types/unified-workflow";
import { createSummaryStep } from "../../types/unified-workflow";

type Step = "load" | "select" | "configure" | "preview";

interface SpecWorkflowBuilderProps {
  onApplyWorkflow?: (workflow: UnifiedWorkflow) => void;
}

export function SpecWorkflowBuilder({ onApplyWorkflow }: SpecWorkflowBuilderProps) {
  const [currentStep, setCurrentStep] = useState<Step>("load");
  const [loadedData, setLoadedData] = useState<TestGeneratorOutput | null>(null);
  const [fileName, setFileName] = useState<string>("");
  const [selectedSpecIds, setSelectedSpecIds] = useState<Set<string>>(new Set());
  const [agenticPrompt, setAgenticPrompt] = useState("");
  const [maxIterations, setMaxIterations] = useState(3);

  // Handle file load
  const handleLoad = useCallback((output: TestGeneratorOutput) => {
    setLoadedData(output);
    setFileName(`${output.generatorType}-specs.json`);
    // Auto-select all specs with critical assertions
    const criticalIds = new Set(
      output.testSpecifications
        .filter((s) => s.assertions.some((a) => a.enabled && a.severity === "critical"))
        .map((s) => s.id),
    );
    setSelectedSpecIds(criticalIds);
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
    setSelectedSpecIds(new Set(loadedData.testSpecifications.map((s) => s.id)));
  }, [loadedData]);

  const deselectAll = useCallback(() => {
    setSelectedSpecIds(new Set());
  }, []);

  // Build the full UnifiedWorkflow
  const workflow: UnifiedWorkflow | null = useMemo(() => {
    if (!loadedData) return null;

    const now = new Date().toISOString();
    const isNavigation = loadedData.generatorType === "navigation";
    const workflowName = isNavigation ? "Navigation Verification" : "Snapshot Verification";

    // Setup steps
    const setupSteps: ScriptStep[] = [];
    if (isNavigation && loadedData.transitions.length > 0) {
      const pageUrl =
        loadedData.explorationMetadata?.targetUrl ||
        loadedData.states[0]?.pageUrl ||
        "";
      if (pageUrl) {
        setupSteps.push({
          id: crypto.randomUUID(),
          type: "script",
          phase: "setup",
          name: `Navigate to ${pageUrl}`,
          target_url: pageUrl,
          code: `import { test } from '@playwright/test';\ntest('navigate', async ({ page }) => {\n  await page.goto('${pageUrl}');\n  await page.waitForLoadState('networkidle');\n});`,
          refinement_enabled: false,
        });
      }
    }

    // Verification steps: each selected spec becomes a TestStep
    const selectedSpecs = loadedData.testSpecifications.filter((s) =>
      selectedSpecIds.has(s.id),
    );
    const verificationSteps: TestStep[] = selectedSpecs.map((spec) => {
      const enabledAssertions = spec.assertions.filter((a) => a.enabled);
      const assertionDescriptions = enabledAssertions
        .map((a) => `- ${a.description} [${a.severity}]`)
        .join("\n");

      return {
        id: crypto.randomUUID(),
        type: "test",
        phase: "verification",
        name: spec.name,
        test_type: "playwright",
        is_critical: enabledAssertions.some((a) => a.severity === "critical"),
        is_blocking: true,
        description: `${spec.description}\n\nAssertions:\n${assertionDescriptions}`,
        timeout_seconds: 30,
      };
    });

    // Agentic steps
    const agenticSteps: PromptStep[] = [];
    if (agenticPrompt || selectedSpecs.length > 0) {
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
      description: `Auto-generated from ${loadedData.generatorType} test specifications. ${selectedSpecs.length} specs selected with ${selectedSpecs.reduce((sum, s) => sum + s.assertions.filter((a) => a.enabled).length, 0)} total assertions.`,
      setup_steps: setupSteps,
      verification_steps: verificationSteps,
      agentic_steps: agenticSteps,
      completion_steps: completionSteps,
      max_iterations: maxIterations,
      category: "spec-generated",
      tags: [loadedData.generatorType, "auto-generated"],
      created_at: now,
      modified_at: now,
    };
  }, [loadedData, selectedSpecIds, agenticPrompt, maxIterations]);

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
            {i < steps.length - 1 && (
              <span className="text-neutral-600 mx-1">&rsaquo;</span>
            )}
          </div>
        ))}

        {loadedData && (
          <span className="text-xs text-neutral-500 ml-auto">
            {loadedData.generatorType === "snapshot" ? "Snapshot" : "Navigation"} &middot;{" "}
            {loadedData.testSpecifications.length} specs
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
            {loadedData.generatorType === "navigation" && (
              <div className="border-b border-neutral-700 max-h-[200px] overflow-auto">
                <NavigationGraphPreview
                  states={loadedData.states}
                  transitions={loadedData.transitions}
                />
              </div>
            )}
            <div className="flex-1 min-h-0">
              <SpecSelector
                specs={loadedData.testSpecifications}
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
                Continue with {selectedSpecIds.size} specs
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
            onApply={
              onApplyWorkflow
                ? () => onApplyWorkflow(workflow)
                : undefined
            }
          />
        )}
      </div>
    </div>
  );
}
