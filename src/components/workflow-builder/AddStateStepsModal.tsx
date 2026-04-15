/**
 * AddStateStepsModal.tsx
 *
 * Modal for adding verification tests and agentic instructions from a single
 * state in the loaded state machine configuration to the current workflow.
 *
 * Enables UI Bridge-based application manipulation by providing AI with
 * semantic context about application states.
 */

import React, { useState, useCallback, useEffect, useMemo } from "react";
import {
  X,
  Loader2,
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronRight,
  Image,
  MapPin,
  Type,
  Square,
  Plus,
  Eye,
  Bot,
  FileCheck,
  TestTube,
  Monitor,
  Globe,
  Info,
} from "lucide-react";
import { useStateToWorkflow } from "../../hooks/useStateToWorkflow";
import type { AddStateToWorkflowOptions } from "../../hooks/useStateToWorkflow";
import type {
  UnifiedStep,
  WorkflowPhase,
  VerificationStep,
  PromptStep,
} from "../../types/unified-workflow";

/**
 * Detected configuration type based on state content analysis
 */
type ConfigType = "visual_gui" | "ui_bridge" | "hybrid" | "unknown";

interface ConfigTypeInfo {
  type: ConfigType;
  label: string;
  description: string;
  icon: React.ElementType;
  iconColor: string;
  bgColor: string;
  borderColor: string;
  recommendedVision: boolean;
  recommendedPlaywright: boolean;
}

/**
 * Analyzes states to determine if they were created for Visual GUI automation
 * or UI Bridge extraction.
 *
 * Visual GUI indicators:
 * - Images with patterns that have imageId populated
 * - Regions with absolute screen coordinates
 * - Locations with pixel coordinates
 *
 * UI Bridge indicators:
 * - States with ocrText but no image patterns
 * - DOM-extracted content in strings
 * - Missing or empty imageId in patterns
 */
function detectConfigType(
  states: Array<{
    stateImages?: Array<{
      patterns?: Array<{ imageId?: string }>;
      ocrText?: string;
    }>;
    regions?: Array<{ x: number; y: number; width: number; height: number }>;
    locations?: Array<{ x: number; y: number }>;
    strings?: Array<{ value: string }>;
  }>,
): ConfigTypeInfo {
  let hasImagePatterns = false;
  let hasOcrTextWithoutPatterns = false;

  for (const state of states) {
    // Check for image patterns with imageId (Visual GUI indicator)
    if (state.stateImages) {
      for (const img of state.stateImages) {
        const hasPatternWithImage = img.patterns?.some((p) => p.imageId && p.imageId.trim() !== "");
        if (hasPatternWithImage) {
          hasImagePatterns = true;
        }
        // OCR text without image patterns (UI Bridge indicator)
        if (img.ocrText && !hasPatternWithImage) {
          hasOcrTextWithoutPatterns = true;
        }
      }
    }
  }

  // Determine type based on indicators
  if (hasImagePatterns && !hasOcrTextWithoutPatterns) {
    return {
      type: "visual_gui",
      label: "Visual GUI Automation",
      description:
        "This config uses image templates for visual matching. Best with qontinui_vision tests.",
      icon: Monitor,
      iconColor: "text-blue-400",
      bgColor: "bg-blue-500/10",
      borderColor: "border-blue-500/30",
      recommendedVision: true,
      recommendedPlaywright: false,
    };
  }

  if (hasOcrTextWithoutPatterns && !hasImagePatterns) {
    return {
      type: "ui_bridge",
      label: "UI Bridge Extraction",
      description: "This config uses DOM-extracted data. Best with Playwright tests.",
      icon: Globe,
      iconColor: "text-green-400",
      bgColor: "bg-green-500/10",
      borderColor: "border-green-500/30",
      recommendedVision: false,
      recommendedPlaywright: true,
    };
  }

  if (hasImagePatterns && hasOcrTextWithoutPatterns) {
    return {
      type: "hybrid",
      label: "Hybrid Config",
      description: "This config has both visual and DOM data. You can use either test type.",
      icon: Info,
      iconColor: "text-purple-400",
      bgColor: "bg-purple-500/10",
      borderColor: "border-purple-500/30",
      recommendedVision: true,
      recommendedPlaywright: true,
    };
  }

  return {
    type: "unknown",
    label: "Unknown Config Type",
    description:
      "Could not determine config type. Choose test types based on your automation target.",
    icon: AlertCircle,
    iconColor: "text-zinc-400",
    bgColor: "bg-zinc-500/10",
    borderColor: "border-zinc-500/30",
    recommendedVision: true,
    recommendedPlaywright: false,
  };
}

interface AddStateStepsModalProps {
  isOpen: boolean;
  onClose: () => void;
  onStepsAdded: (steps: {
    verificationSteps: VerificationStep[];
    agenticStep: PromptStep | null;
  }) => void;
  /** Function to add a step to the workflow */
  addStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
}

export function AddStateStepsModal({
  isOpen,
  onClose,
  onStepsAdded,
  addStep,
}: AddStateStepsModalProps) {
  const {
    states,
    isLoading,
    error,
    hasConfig,
    refreshStates,
    generateStepsForState,
    prepareStepsForWorkflow,
    getStateSummary,
  } = useStateToWorkflow();

  // Selection state
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);
  const [expandedStateId, setExpandedStateId] = useState<string | null>(null);

  // Options state
  const [includeStateStep, setIncludeStateStep] = useState(true);
  const [includeVisionTest, setIncludeVisionTest] = useState(true);
  const [includePlaywrightTest, setIncludePlaywrightTest] = useState(false);
  const [addAgenticStep, setAddAgenticStep] = useState(true);
  const [addVerificationSteps, setAddVerificationSteps] = useState(true);

  // Adding state
  const [isAdding, setIsAdding] = useState(false);

  // Detect config type based on state contents
  const configTypeInfo = useMemo(() => {
    return detectConfigType(states);
  }, [states]);

  // Refresh states when modal opens and set defaults based on config type
  useEffect(() => {
    if (isOpen) {
      refreshStates();
      setSelectedStateId(null);
      setExpandedStateId(null);
    }
  }, [isOpen, refreshStates]);

  // Update defaults when config type is detected
  useEffect(() => {
    if (states.length > 0 && configTypeInfo.type !== "unknown") {
      setIncludeVisionTest(configTypeInfo.recommendedVision);
      setIncludePlaywrightTest(configTypeInfo.recommendedPlaywright);
    }
  }, [configTypeInfo, states.length]);

  // Get the selected state
  const selectedState = useMemo(() => {
    return states.find((s) => s.id === selectedStateId) || null;
  }, [states, selectedStateId]);

  // Generate preview of steps
  const preview = useMemo(() => {
    if (!selectedStateId) return null;

    const options: AddStateToWorkflowOptions = {
      includeStateStep,
      includeVisionTest,
      includePlaywrightTest,
      addVerificationSteps,
      addAgenticStep,
    };

    return generateStepsForState(selectedStateId, options);
  }, [
    selectedStateId,
    includeStateStep,
    includeVisionTest,
    includePlaywrightTest,
    addVerificationSteps,
    addAgenticStep,
    generateStepsForState,
  ]);

  // Handle adding steps to workflow
  const handleAdd = useCallback(() => {
    if (!selectedStateId) return;

    setIsAdding(true);

    try {
      const options: AddStateToWorkflowOptions = {
        includeStateStep,
        includeVisionTest,
        includePlaywrightTest,
        addVerificationSteps,
        addAgenticStep,
      };

      const prepared = prepareStepsForWorkflow(selectedStateId, options);

      if (prepared) {
        // Add steps to workflow using the provided addStep function
        prepared.addToWorkflow(addStep);

        // Notify parent of what was added
        onStepsAdded({
          verificationSteps: prepared.verificationSteps,
          agenticStep: prepared.agenticStep,
        });

        onClose();
      }
    } finally {
      setIsAdding(false);
    }
  }, [
    selectedStateId,
    includeStateStep,
    includeVisionTest,
    includePlaywrightTest,
    addVerificationSteps,
    addAgenticStep,
    prepareStepsForWorkflow,
    addStep,
    onStepsAdded,
    onClose,
  ]);

  // Toggle state expansion to show details
  const toggleStateExpanded = useCallback((stateId: string) => {
    setExpandedStateId((prev) => (prev === stateId ? null : stateId));
  }, []);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        role="button"
        tabIndex={0}
        aria-label="Close modal"
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onClose();
        }}
      />

      {/* Modal */}
      <div className="relative w-full max-w-2xl max-h-[85vh] bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700 shrink-0">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-purple-500/20">
              <Plus className="w-5 h-5 text-purple-400" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-zinc-100">Add State Steps to Workflow</h2>
              <p className="text-sm text-zinc-400">
                Generate verification and agentic steps from a state
              </p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-md text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800 transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-6 space-y-5">
          {/* Config status */}
          {isLoading ? (
            <div className="flex items-center gap-3 p-4 bg-zinc-800/50 rounded-lg">
              <Loader2 className="w-5 h-5 animate-spin text-zinc-400" />
              <span className="text-zinc-400">Loading states from configuration...</span>
            </div>
          ) : !hasConfig ? (
            <div className="flex items-start gap-3 p-4 bg-amber-500/10 border border-amber-500/30 rounded-lg">
              <AlertCircle className="w-5 h-5 text-amber-400 shrink-0 mt-0.5" />
              <div>
                <p className="text-amber-400 font-medium">No Configuration Loaded</p>
                <p className="text-sm text-zinc-400 mt-1">
                  Please load a state machine configuration first. Go to the Configuration panel and
                  load a config file exported from qontinui-web.
                </p>
              </div>
            </div>
          ) : states.length === 0 ? (
            <div className="flex items-start gap-3 p-4 bg-amber-500/10 border border-amber-500/30 rounded-lg">
              <AlertCircle className="w-5 h-5 text-amber-400 shrink-0 mt-0.5" />
              <div>
                <p className="text-amber-400 font-medium">No States Found</p>
                <p className="text-sm text-zinc-400 mt-1">
                  The loaded configuration doesn't contain any states.
                </p>
              </div>
            </div>
          ) : (
            <>
              {/* Config Type Indicator */}
              <div
                className={`flex items-start gap-3 p-3 ${configTypeInfo.bgColor} border ${configTypeInfo.borderColor} rounded-lg`}
              >
                <configTypeInfo.icon
                  className={`w-5 h-5 ${configTypeInfo.iconColor} shrink-0 mt-0.5`}
                />
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <p className={`font-medium ${configTypeInfo.iconColor}`}>
                      {configTypeInfo.label}
                    </p>
                    <span
                      data-content-role="metric"
                      data-content-label="state count"
                      className="text-xs text-zinc-500"
                    >
                      ({states.length} state{states.length !== 1 ? "s" : ""})
                    </span>
                  </div>
                  <p className="text-sm text-zinc-400 mt-0.5">{configTypeInfo.description}</p>
                </div>
              </div>

              {/* State Selection */}
              <div>
                <label className="block text-sm font-medium text-zinc-400 mb-2">
                  Select a State
                </label>
                <div className="space-y-1 max-h-60 overflow-y-auto border border-zinc-700 rounded-lg">
                  {states.map((state) => {
                    const summary = getStateSummary(state);
                    const isSelected = selectedStateId === state.id;
                    const isExpanded = expandedStateId === state.id;

                    return (
                      <div
                        key={state.id}
                        className={`border-b border-zinc-800 last:border-b-0 ${
                          isSelected ? "bg-purple-500/10" : ""
                        }`}
                      >
                        {/* State row */}
                        <div
                          role="button"
                          tabIndex={0}
                          className={`flex items-center gap-3 px-3 py-2 cursor-pointer transition-colors ${
                            isSelected ? "bg-purple-500/20" : "hover:bg-zinc-800"
                          }`}
                          onClick={() => setSelectedStateId(state.id)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") setSelectedStateId(state.id);
                          }}
                        >
                          {/* Expand button */}
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              toggleStateExpanded(state.id);
                            }}
                            className="p-0.5 text-zinc-500 hover:text-zinc-300"
                          >
                            {isExpanded ? (
                              <ChevronDown className="w-4 h-4" />
                            ) : (
                              <ChevronRight className="w-4 h-4" />
                            )}
                          </button>

                          {/* Selection indicator */}
                          <div
                            className={`w-4 h-4 rounded-full border-2 flex items-center justify-center ${
                              isSelected ? "border-purple-500 bg-purple-500" : "border-zinc-600"
                            }`}
                          >
                            {isSelected && <CheckCircle className="w-3 h-3 text-white" />}
                          </div>

                          {/* State info */}
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="font-medium text-zinc-200 truncate">
                                {state.name}
                              </span>
                              {summary.isInitial && (
                                <span
                                  data-content-role="badge"
                                  className="text-xs px-1.5 py-0.5 rounded bg-blue-500/20 text-blue-400"
                                >
                                  Initial
                                </span>
                              )}
                              {summary.isFinal && (
                                <span
                                  data-content-role="badge"
                                  className="text-xs px-1.5 py-0.5 rounded bg-green-500/20 text-green-400"
                                >
                                  Final
                                </span>
                              )}
                            </div>
                            {state.description && (
                              <p className="text-xs text-zinc-500 truncate mt-0.5">
                                {state.description}
                              </p>
                            )}
                          </div>

                          {/* Summary badges */}
                          <div className="flex items-center gap-2 text-xs text-zinc-500">
                            {summary.imageCount > 0 && (
                              <span className="flex items-center gap-1">
                                <Image className="w-3 h-3" />
                                {summary.imageCount}
                              </span>
                            )}
                            {summary.regionCount > 0 && (
                              <span className="flex items-center gap-1">
                                <Square className="w-3 h-3" />
                                {summary.regionCount}
                              </span>
                            )}
                            {summary.locationCount > 0 && (
                              <span className="flex items-center gap-1">
                                <MapPin className="w-3 h-3" />
                                {summary.locationCount}
                              </span>
                            )}
                            {summary.stringCount > 0 && (
                              <span className="flex items-center gap-1">
                                <Type className="w-3 h-3" />
                                {summary.stringCount}
                              </span>
                            )}
                          </div>
                        </div>

                        {/* Expanded details */}
                        {isExpanded && (
                          <div className="px-10 py-3 bg-zinc-800/30 text-sm space-y-2">
                            {state.description && (
                              <p className="text-zinc-400">{state.description}</p>
                            )}
                            <div className="grid grid-cols-2 gap-2 text-xs">
                              {state.stateImages && state.stateImages.length > 0 && (
                                <div>
                                  <span className="text-zinc-500">Images:</span>
                                  <ul className="mt-1 space-y-0.5 text-zinc-400">
                                    {state.stateImages.map((img) => (
                                      <li key={img.id} className="truncate">
                                        • {img.name}
                                        {img.ocrText && (
                                          <span className="text-purple-400 ml-1">(OCR)</span>
                                        )}
                                      </li>
                                    ))}
                                  </ul>
                                </div>
                              )}
                              {state.strings && state.strings.length > 0 && (
                                <div>
                                  <span className="text-zinc-500">Strings:</span>
                                  <ul className="mt-1 space-y-0.5 text-zinc-400">
                                    {state.strings.map((str) => (
                                      <li key={str.id} className="truncate">
                                        • {str.name}: "{str.value}"
                                      </li>
                                    ))}
                                  </ul>
                                </div>
                              )}
                            </div>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>

              {/* Options */}
              {selectedState && (
                <div className="space-y-3">
                  <label className="block text-sm font-medium text-zinc-400">
                    Generation Options
                  </label>

                  {/* Verification Steps Options */}
                  <div className="p-3 bg-zinc-800/50 rounded-lg space-y-2">
                    <div className="flex items-center gap-2 text-green-400 text-sm font-medium">
                      <Eye className="w-4 h-4" />
                      Verification Steps
                    </div>

                    <label className="flex items-center justify-between p-2 rounded-md cursor-pointer hover:bg-zinc-700/50 transition-colors">
                      <div>
                        <span className="text-sm text-zinc-300">Add Verification Steps</span>
                        <p className="text-xs text-zinc-500">Add steps to verify this state</p>
                      </div>
                      <input
                        type="checkbox"
                        checked={addVerificationSteps}
                        onChange={(e) => setAddVerificationSteps(e.target.checked)}
                        className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-green-500 focus:ring-green-500/50"
                      />
                    </label>

                    {addVerificationSteps && (
                      <div className="pl-4 space-y-2">
                        <label className="flex items-center justify-between p-2 rounded-md cursor-pointer hover:bg-zinc-700/50 transition-colors">
                          <div className="flex items-center gap-2">
                            <FileCheck className="w-4 h-4 text-zinc-400" />
                            <span className="text-sm text-zinc-300">State Navigation Step</span>
                          </div>
                          <input
                            type="checkbox"
                            checked={includeStateStep}
                            onChange={(e) => setIncludeStateStep(e.target.checked)}
                            className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-green-500 focus:ring-green-500/50"
                          />
                        </label>

                        <label className="flex items-center justify-between p-2 rounded-md cursor-pointer hover:bg-zinc-700/50 transition-colors">
                          <div className="flex-1">
                            <div className="flex items-center gap-2">
                              <Eye className="w-4 h-4 text-zinc-400" />
                              <span className="text-sm text-zinc-300">
                                Vision Test (qontinui_vision)
                              </span>
                              {configTypeInfo.recommendedVision && (
                                <span
                                  data-content-role="badge"
                                  className="text-xs px-1.5 py-0.5 rounded bg-green-500/20 text-green-400"
                                >
                                  Recommended
                                </span>
                              )}
                            </div>
                            {includeVisionTest && configTypeInfo.type === "ui_bridge" && (
                              <p className="text-xs text-amber-400 mt-1 flex items-center gap-1">
                                <AlertCircle className="w-3 h-3" />
                                UI Bridge configs may lack image patterns for vision tests
                              </p>
                            )}
                          </div>
                          <input
                            type="checkbox"
                            checked={includeVisionTest}
                            onChange={(e) => setIncludeVisionTest(e.target.checked)}
                            className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-green-500 focus:ring-green-500/50"
                          />
                        </label>

                        <label className="flex items-center justify-between p-2 rounded-md cursor-pointer hover:bg-zinc-700/50 transition-colors">
                          <div className="flex-1">
                            <div className="flex items-center gap-2">
                              <TestTube className="w-4 h-4 text-zinc-400" />
                              <span className="text-sm text-zinc-300">Playwright Test</span>
                              {configTypeInfo.recommendedPlaywright && (
                                <span
                                  data-content-role="badge"
                                  className="text-xs px-1.5 py-0.5 rounded bg-green-500/20 text-green-400"
                                >
                                  Recommended
                                </span>
                              )}
                            </div>
                            {includePlaywrightTest && configTypeInfo.type === "visual_gui" && (
                              <p className="text-xs text-amber-400 mt-1 flex items-center gap-1">
                                <AlertCircle className="w-3 h-3" />
                                Visual GUI configs lack DOM selectors for Playwright tests
                              </p>
                            )}
                          </div>
                          <input
                            type="checkbox"
                            checked={includePlaywrightTest}
                            onChange={(e) => setIncludePlaywrightTest(e.target.checked)}
                            className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-green-500 focus:ring-green-500/50"
                          />
                        </label>
                      </div>
                    )}
                  </div>

                  {/* Agentic Step Options */}
                  <div className="p-3 bg-zinc-800/50 rounded-lg space-y-2">
                    <div className="flex items-center gap-2 text-amber-400 text-sm font-medium">
                      <Bot className="w-4 h-4" />
                      Agentic Step
                    </div>

                    <label className="flex items-center justify-between p-2 rounded-md cursor-pointer hover:bg-zinc-700/50 transition-colors">
                      <div>
                        <span className="text-sm text-zinc-300">Add Agentic Instruction</span>
                        <p className="text-xs text-zinc-500">
                          AI instructions with semantic context from state
                        </p>
                      </div>
                      <input
                        type="checkbox"
                        checked={addAgenticStep}
                        onChange={(e) => setAddAgenticStep(e.target.checked)}
                        className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-amber-500 focus:ring-amber-500/50"
                      />
                    </label>
                  </div>
                </div>
              )}

              {/* Preview */}
              {preview && (addVerificationSteps || addAgenticStep) && (
                <div className="space-y-2">
                  <label className="block text-sm font-medium text-zinc-400">
                    Preview ({preview.verificationSteps.length + (preview.agenticStep ? 1 : 0)}{" "}
                    steps)
                  </label>
                  <div className="p-3 bg-zinc-800/30 border border-zinc-700 rounded-lg space-y-2 max-h-40 overflow-y-auto">
                    {addVerificationSteps &&
                      preview.verificationSteps.map((step, i) => (
                        <div
                          key={`vstep-${step.name ?? i}`}
                          className="flex items-center gap-2 text-sm text-zinc-400"
                        >
                          <span className="w-5 h-5 rounded bg-green-500/20 text-green-400 flex items-center justify-center text-xs">
                            V
                          </span>
                          <span className="truncate">{step.name}</span>
                          {/* VerificationStep union lacks the `type` discriminator
                              in its TS shape (only CanonicalStep carries it);
                              cast for JSX render. */}
                          <span className="text-xs text-zinc-500 ml-auto">
                            {String((step as { type?: string }).type ?? "")}
                          </span>
                        </div>
                      ))}
                    {addAgenticStep && preview.agenticStep && (
                      <div className="flex items-center gap-2 text-sm text-zinc-400">
                        <span className="w-5 h-5 rounded bg-amber-500/20 text-amber-400 flex items-center justify-center text-xs">
                          A
                        </span>
                        <span className="truncate">{preview.agenticStep.name}</span>
                        <span className="text-xs text-zinc-500 ml-auto">prompt</span>
                      </div>
                    )}
                  </div>
                </div>
              )}

              {/* Error display */}
              {error && (
                <div className="flex items-start gap-3 p-4 bg-red-500/10 border border-red-500/30 rounded-lg">
                  <AlertCircle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
                  <p className="text-red-400 text-sm">{error}</p>
                </div>
              )}
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-zinc-700 shrink-0">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm font-medium text-zinc-400 hover:text-zinc-200 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleAdd}
            disabled={
              !selectedStateId ||
              isAdding ||
              isLoading ||
              (!addVerificationSteps && !addAgenticStep)
            }
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium bg-purple-600 hover:bg-purple-500 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isAdding ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                <span>Adding...</span>
              </>
            ) : (
              <>
                <Plus className="w-4 h-4" />
                <span>Add to Workflow</span>
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
