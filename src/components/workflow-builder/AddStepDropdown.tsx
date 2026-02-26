/**
 * AddStepDropdown.tsx
 *
 * Dropdown menu for adding steps to a workflow.
 * Shows core step types at the top, then a "Templates" section
 * with pre-configured steps grouped by category.
 */

import React, { useRef, useEffect, useState, useMemo } from "react";
import {
  Plus,
  ChevronDown,
  ChevronRight,
  Terminal,
  MessageSquare,
  TestTube2,
  Monitor,
  AlertTriangle,
  Workflow,
  CheckCircle2,
  ShieldCheck,
  Layers,
  Bot,
  Globe,
  Zap,
  Compass,
  FileText,
  GitCompare,
  LayoutTemplate,
} from "lucide-react";
import type { WorkflowPhase, UnifiedStep } from "../../types";
import { STEP_TYPES, generateStepId } from "../../types";
import {
  getTemplatesForPhase,
  TEMPLATE_CATEGORIES,
  type StepTemplate,
  type StepTemplateCategory,
} from "./step-templates";

// =============================================================================
// Icon and Color Mapping
// =============================================================================

/**
 * Icon map for step types and template icons.
 */
const ICON_MAP: Record<string, React.ElementType> = {
  Terminal,
  MessageSquare,
  TestTube2,
  Monitor,
  Bot,
  AlertTriangle,
  Workflow,
  CheckCircle2,
  ShieldCheck,
  Layers,
  Globe,
  Zap,
  Compass,
  FileText,
  GitCompare,
  LayoutTemplate,
};

const COLOR_MAP: Record<string, string> = {
  gray: "text-gray-400",
  cyan: "text-cyan-400",
  amber: "text-amber-400",
  green: "text-green-400",
  emerald: "text-emerald-400",
  indigo: "text-indigo-400",
  violet: "text-violet-400",
  blue: "text-blue-400",
  purple: "text-purple-400",
  red: "text-red-400",
  teal: "text-teal-400",
  sky: "text-sky-400",
  pink: "text-pink-400",
  orange: "text-orange-400",
};

// =============================================================================
// Component
// =============================================================================

interface AddStepDropdownProps {
  /** If provided, only show steps for this phase */
  filterPhase?: WorkflowPhase;
  /** Called when a step is selected */
  onAddStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  /** Whether the dropdown is open */
  isOpen: boolean;
  /** Called to close the dropdown */
  onClose: () => void;
  /** Called when the "Workflow" step type is clicked -- opens the picker */
  onOpenWorkflowPicker?: (phase: WorkflowPhase) => void;
}

export function AddStepDropdown({
  filterPhase,
  onAddStep,
  isOpen,
  onClose,
  onOpenWorkflowPicker,
}: AddStepDropdownProps) {
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [showTemplates, setShowTemplates] = useState(false);

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        onClose();
      }
    }

    if (isOpen) {
      document.addEventListener("mousedown", handleClickOutside);
      return () => document.removeEventListener("mousedown", handleClickOutside);
    }
  }, [isOpen, onClose]);

  // Reset templates view when dropdown closes
  useEffect(() => {
    if (!isOpen) {
      setShowTemplates(false);
    }
  }, [isOpen]);

  // Determine which phase to use
  const phase = filterPhase || "setup";

  // Get templates filtered by phase, grouped by category
  const templatesByCategory = useMemo(() => {
    const phaseTemplates = getTemplatesForPhase(phase);
    const grouped: Partial<Record<StepTemplateCategory, StepTemplate[]>> = {};
    for (const template of phaseTemplates) {
      if (!grouped[template.category]) {
        grouped[template.category] = [];
      }
      grouped[template.category]!.push(template);
    }
    return grouped;
  }, [phase]);

  const categoryOrder = Object.keys(TEMPLATE_CATEGORIES) as StepTemplateCategory[];
  const hasTemplates = Object.keys(templatesByCategory).length > 0;

  // Handle step creation
  const handleStepClick = (stepType: string) => {
    // Workflow steps open the picker instead of creating directly
    if (stepType === "workflow") {
      onOpenWorkflowPicker?.(phase);
      onClose();
      return;
    }

    const id = generateStepId();

    let step: UnifiedStep;

    switch (stepType) {
      case "command":
        step = {
          id,
          type: "command",
          phase: phase as "setup" | "verification" | "completion",
          name: "Command",
          mode: "shell",
          command: "",
          fail_on_error: true,
        };
        break;

      case "prompt":
        step = {
          id,
          type: "prompt",
          phase: phase as "setup" | "verification" | "agentic" | "completion",
          name: phase === "agentic" ? "Prompt" : "AI Task",
          content: "",
        };
        break;

      case "ui_bridge":
        step = {
          id,
          type: "ui_bridge",
          phase: phase as "setup" | "verification" | "completion",
          name: "UI Bridge",
          action: "snapshot",
        };
        break;

      default:
        return;
    }

    onAddStep(step, phase);
    onClose();
  };

  // Handle template selection
  const handleTemplateClick = (template: StepTemplate) => {
    const step = template.createStep(phase);
    onAddStep(step, phase);
    onClose();
  };

  // Get step types for the current phase
  const steps = STEP_TYPES[phase] || [];

  if (!isOpen) return null;

  return (
    <div
      ref={dropdownRef}
      className="absolute z-50 w-80 rounded-lg border border-zinc-700 bg-zinc-800 shadow-xl overflow-hidden"
    >
      <div className="py-1">
        <div className="px-3 py-2 text-xs font-medium text-zinc-500 uppercase tracking-wider border-b border-zinc-700">
          Add Step
        </div>

        <div className="max-h-[28rem] overflow-y-auto py-1">
          {/* Core step types */}
          {steps.map((stepType) => {
            const Icon = ICON_MAP[stepType.icon] || Plus;
            const colorClass = COLOR_MAP[stepType.color] || "text-zinc-400";

            return (
              <button
                key={stepType.type}
                onClick={() => handleStepClick(stepType.type)}
                className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-left hover:bg-zinc-700/50 transition-colors"
              >
                <Icon className={`w-4 h-4 flex-shrink-0 ${colorClass}`} />
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-zinc-200">{stepType.label}</div>
                  <div className="text-xs text-zinc-500">{stepType.description}</div>
                </div>
              </button>
            );
          })}

          {/* Templates section */}
          {hasTemplates && (
            <>
              {/* Divider */}
              <div className="my-1 border-t border-zinc-700" />

              {/* Templates toggle header */}
              <button
                onClick={() => setShowTemplates(!showTemplates)}
                className="w-full flex items-center gap-2 px-3 py-2 text-xs font-medium text-zinc-400 hover:text-zinc-300 hover:bg-zinc-700/30 transition-colors uppercase tracking-wider"
              >
                {showTemplates ? (
                  <ChevronDown className="w-3 h-3" />
                ) : (
                  <ChevronRight className="w-3 h-3" />
                )}
                <LayoutTemplate className="w-3 h-3" />
                <span>Templates</span>
                <span className="text-zinc-600 normal-case tracking-normal font-normal ml-1">
                  Pre-configured steps
                </span>
              </button>

              {/* Template items grouped by category */}
              {showTemplates && (
                <div className="pb-1">
                  {categoryOrder.map((catKey) => {
                    const templates = templatesByCategory[catKey];
                    if (!templates || templates.length === 0) return null;

                    const catMeta = TEMPLATE_CATEGORIES[catKey];
                    const CatIcon = ICON_MAP[catMeta.icon] || Plus;
                    const catColor = COLOR_MAP[catMeta.color] || "text-zinc-400";

                    return (
                      <div key={catKey}>
                        {/* Category header */}
                        <div className="flex items-center gap-2 px-3 py-1.5 mt-1">
                          <CatIcon className={`w-3 h-3 ${catColor}`} />
                          <span className="text-xs font-medium text-zinc-500">{catMeta.label}</span>
                        </div>

                        {/* Templates in this category */}
                        {templates.map((template) => {
                          const TplIcon = ICON_MAP[template.icon] || Plus;
                          const tplColor = COLOR_MAP[template.color] || "text-zinc-400";

                          return (
                            <button
                              key={template.id}
                              onClick={() => handleTemplateClick(template)}
                              className="w-full flex items-center gap-3 pl-6 pr-3 py-2 text-sm text-left hover:bg-zinc-700/50 transition-colors"
                            >
                              <TplIcon className={`w-3.5 h-3.5 flex-shrink-0 ${tplColor}`} />
                              <div className="flex-1 min-w-0">
                                <div className="font-medium text-zinc-300 text-xs">
                                  {template.name}
                                </div>
                                <div className="text-xs text-zinc-600 truncate">
                                  {template.description}
                                </div>
                              </div>
                            </button>
                          );
                        })}
                      </div>
                    );
                  })}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Button Component
// =============================================================================

interface AddStepButtonProps {
  onClick: () => void;
  variant?: "default" | "compact" | "phase";
  phaseLabel?: string;
}

export function AddStepButton({ onClick, variant = "default", phaseLabel }: AddStepButtonProps) {
  if (variant === "compact") {
    return (
      <button
        onClick={onClick}
        className="flex items-center gap-1 px-2 py-1 text-sm rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-300 transition-colors"
      >
        <Plus className="w-4 h-4" />
        <span>Add</span>
      </button>
    );
  }

  if (variant === "phase" && phaseLabel) {
    return (
      <button
        onClick={onClick}
        className="w-full flex items-center justify-center gap-2 p-2 rounded-md bg-zinc-700/50 hover:bg-zinc-700 text-zinc-400 hover:text-zinc-200 transition-colors text-sm"
      >
        <Plus className="w-4 h-4" />
        <span>Add {phaseLabel} Step</span>
      </button>
    );
  }

  return (
    <button
      onClick={onClick}
      className="flex items-center gap-2 px-4 py-2 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors"
    >
      <Plus className="w-4 h-4" />
      <span>Add Step</span>
      <ChevronDown className="w-4 h-4" />
    </button>
  );
}
