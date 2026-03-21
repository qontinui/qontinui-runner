/**
 * SpecWorkflowBuilder
 *
 * Main component for building workflows from test specification files.
 * Handles both snapshot (Tier 1) and navigation (Tier 2) specs.
 *
 * Flow:
 * 1. Load SpecConfig JSON (.spec.uibridge.json or legacy migrated)
 * 2. Select which spec groups to include as verification steps
 * 3. Configure workflow mode and agentic prompt
 * 4. Preview and apply the generated workflow
 */

import { useState, useCallback, useMemo } from "react";
import { FileJson, CheckSquare, Bot, Eye } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { SpecFileLoader } from "./SpecFileLoader";
import { SpecSelector } from "./SpecSelector";
import { NavigationGraphPreview } from "./NavigationGraphPreview";
import { AgenticPromptEditor } from "./AgenticPromptEditor";
import { WorkflowPreview } from "./WorkflowPreview";
import type { SpecConfig, GeneratorSpecMetadata } from "./types";
import type { UnifiedWorkflow } from "../../types/unified-workflow";
import {
  buildSpecWorkflow,
  type SpecConfig as BuildSpecConfig,
} from "../../lib/workflow-builder/buildSpecWorkflow";
import { buildSpecDrivenWorkflow } from "../../lib/workflow-builder/buildSpecDrivenWorkflow";

type Step = "load" | "select" | "configure" | "preview";
type WorkflowMode = "verify" | "implement" | "update";

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
  const [workflowMode, setWorkflowMode] = useState<WorkflowMode>("verify");

  // Extract generator metadata from SpecConfig
  const genMeta = useMemo<GeneratorSpecMetadata | undefined>(
    () => loadedData?.metadata as GeneratorSpecMetadata | undefined,
    [loadedData],
  );

  const generatorType = genMeta?.generatorType;
  const states = useMemo(() => genMeta?.states || [], [genMeta]);
  const transitions = useMemo(() => genMeta?.transitions || [], [genMeta]);

  // Handle file load + auto-version snapshot
  const handleLoad = useCallback((loadedConfig: SpecConfig) => {
    setLoadedData(loadedConfig);
    const meta = loadedConfig.metadata as GeneratorSpecMetadata | undefined;
    const genType = meta?.generatorType || "spec";
    setFileName(`${genType}.spec.uibridge.json`);
    const criticalIds = new Set(
      loadedConfig.groups
        .filter((g) => g.assertions.some((a) => a.enabled && a.severity === "critical"))
        .map((g) => g.id),
    );
    setSelectedSpecIds(criticalIds);
    const rawMeta = loadedConfig.metadata as Record<string, unknown> | undefined;
    const explicitSource = rawMeta?.elementSource as "control" | "external" | undefined;
    setElementSource(explicitSource || "control");
    setCurrentStep("select");
    // Auto-version: snapshot for change tracking (fire-and-forget)
    invoke("snapshot_current_spec", {
      specId: genType,
      specJson: JSON.stringify(loadedConfig),
      changeSummary: null,
      changeType: "manual",
    }).catch(() => {});
  }, []);

  const toggleSpec = useCallback((specId: string) => {
    setSelectedSpecIds((prev) => {
      const next = new Set(prev);
      if (next.has(specId)) next.delete(specId);
      else next.add(specId);
      return next;
    });
  }, []);

  const selectAll = useCallback(() => {
    if (!loadedData) return;
    setSelectedSpecIds(new Set(loadedData.groups.map((g) => g.id)));
  }, [loadedData]);

  const deselectAll = useCallback(() => {
    setSelectedSpecIds(new Set());
  }, []);

  // Build workflow based on selected mode
  const workflow: UnifiedWorkflow | null = useMemo(() => {
    if (!loadedData) return null;

    const isNavigation = generatorType === "navigation";
    const explorationMeta = genMeta?.explorationMetadata as { targetUrl?: string } | undefined;
    const pageUrl = isNavigation
      ? explorationMeta?.targetUrl || states[0]?.pageUrl || undefined
      : undefined;

    if (workflowMode === "verify") {
      return buildSpecWorkflow({
        specConfig: loadedData as unknown as BuildSpecConfig,
        selectedGroupIds: selectedSpecIds,
        agenticPrompt: agenticPrompt || undefined,
        maxIterations,
        elementSource,
        pageUrl,
        workflowName: isNavigation ? "Navigation Verification" : "Snapshot Verification",
      });
    }

    return buildSpecDrivenWorkflow({
      specConfig: loadedData as unknown as BuildSpecConfig,
      mode: workflowMode,
      elementSource,
      pageUrl,
      maxIterations: workflowMode === "implement" ? 5 : 3,
      workflowName:
        workflowMode === "implement"
          ? `Implement from Spec — ${fileName.replace(".spec.uibridge.json", "")}`
          : `Update from Spec — ${fileName.replace(".spec.uibridge.json", "")}`,
    });
  }, [
    loadedData,
    selectedSpecIds,
    agenticPrompt,
    maxIterations,
    elementSource,
    generatorType,
    genMeta,
    states,
    workflowMode,
    fileName,
  ]);

  const stepDefs: { id: Step; label: string; icon: React.ReactNode }[] = [
    { id: "load", label: "Load Specs", icon: <FileJson className="w-4 h-4" /> },
    { id: "select", label: "Select", icon: <CheckSquare className="w-4 h-4" /> },
    { id: "configure", label: "Configure", icon: <Bot className="w-4 h-4" /> },
    { id: "preview", label: "Preview", icon: <Eye className="w-4 h-4" /> },
  ];

  const modeOptions: { value: WorkflowMode; label: string; desc: string }[] = [
    { value: "verify", label: "Verify Only", desc: "Run spec assertions against existing UI" },
    {
      value: "implement",
      label: "Implement from Spec",
      desc: "Generate code to satisfy all assertions",
    },
    {
      value: "update",
      label: "Update from Changes",
      desc: "Implement only changed/added assertions",
    },
  ];

  return (
    <div className="flex flex-col h-full">
      {/* Step indicator */}
      <div className="flex items-center gap-1 px-4 py-3 border-b border-border bg-muted/50">
        {stepDefs.map((step, i) => (
          <div key={step.id} className="flex items-center">
            <button
              onClick={() => {
                if (step.id === "load" || loadedData) setCurrentStep(step.id);
              }}
              disabled={step.id !== "load" && !loadedData}
              className={`flex items-center gap-2 px-3 py-1.5 text-xs rounded-md transition-colors ${
                currentStep === step.id
                  ? "bg-emerald-500/20 text-emerald-400 border border-emerald-500/30"
                  : "text-muted-foreground hover:text-foreground disabled:opacity-30"
              }`}
            >
              {step.icon}
              {step.label}
            </button>
            {i < stepDefs.length - 1 && <span className="text-border mx-1">&rsaquo;</span>}
          </div>
        ))}

        {loadedData && (
          <span className="text-xs text-muted-foreground ml-auto">
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
            {generatorType === "navigation" && states.length > 0 && (
              <div className="border-b border-border max-h-[200px] overflow-auto">
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
            <div className="px-4 py-3 border-t border-border bg-muted/50">
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
              {/* Workflow mode selector */}
              <div className="px-4 pt-4 pb-2">
                <div className="text-xs text-muted-foreground uppercase tracking-wide mb-2">
                  Workflow Mode
                </div>
                <div className="flex gap-2">
                  {modeOptions.map((opt) => (
                    <button
                      key={opt.value}
                      onClick={() => setWorkflowMode(opt.value)}
                      className={`flex-1 px-3 py-2 text-left rounded-md border transition-colors ${
                        workflowMode === opt.value
                          ? "border-emerald-500/50 bg-emerald-500/10 text-emerald-400"
                          : "border-border bg-muted/30 text-muted-foreground hover:text-foreground"
                      }`}
                    >
                      <div className="text-sm font-medium">{opt.label}</div>
                      <div className="text-xs opacity-70 mt-0.5">{opt.desc}</div>
                    </button>
                  ))}
                </div>
              </div>

              <AgenticPromptEditor
                prompt={agenticPrompt}
                onPromptChange={setAgenticPrompt}
                maxIterations={maxIterations}
                onMaxIterationsChange={setMaxIterations}
                elementSource={elementSource}
                onElementSourceChange={setElementSource}
              />
            </div>
            <div className="px-4 py-3 border-t border-border bg-muted/50">
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
