/**
 * ExecutionStepItem.tsx
 *
 * A single execution step item with controls for screenshot, delay, and reordering.
 */

import {
  Camera,
  ChevronDown,
  ChevronUp,
  ClipboardCheck,
  FileText,
  MousePointer2,
  Repeat,
  Target,
  TestTube,
  Trash2,
  Workflow,
} from "lucide-react";
import type { ExecutionStep } from "./types";
import { useAiBuilder } from "./AiBuilderContext";
import { getAccentColors, getStatusColors } from "@/design-system";

/** Check if a step type is considered GUI automation (vs verification) */
function isGuiAutomationStep(type: ExecutionStep["type"]): boolean {
  return ["workflow", "state", "action", "gui_workflow"].includes(type);
}

/** Get the default isSetup value for a step type */
function getDefaultIsSetup(type: ExecutionStep["type"]): boolean {
  // GUI automation steps default to setup
  return isGuiAutomationStep(type);
}

/** Get the default runOnSubsequentIterations value for a step type */
function getDefaultRunOnSubsequent(_type: ExecutionStep["type"]): boolean {
  // All steps run on each iteration by default
  // This ensures fresh automation data for the AI to verify changes
  // Users can toggle off individual steps if they only need to run once (e.g., one-time setup)
  return true;
}

interface ExecutionStepItemProps {
  step: ExecutionStep;
  index: number;
  totalSteps: number;
}

export function ExecutionStepItem({ step, index, totalSteps }: ExecutionStepItemProps) {
  const {
    toggleStepScreenshot,
    updateScreenshotDelay,
    updateScreenshotMonitor,
    toggleStepIsSetup,
    toggleStepRunOnSubsequent,
    toggleStepTestCritical,
    moveStepUp,
    moveStepDown,
    removeStep,
  } = useAiBuilder();

  // Determine effective values (with defaults)
  const isSetup = step.isSetup ?? getDefaultIsSetup(step.type);
  const runOnSubsequent = step.runOnSubsequentIterations ?? getDefaultRunOnSubsequent(step.type);

  // Playwright steps can't be marked as "setup only" - they always run on subsequent iterations
  const canToggleSetup = isGuiAutomationStep(step.type);
  // Show "run on subsequent" toggle for all step types (except playwright which always runs)
  const showRunOnSubsequentToggle = step.type !== "playwright";

  const getStepColors = () => {
    switch (step.type) {
      case "workflow":
        return `${getAccentColors("purple").bg} ${getAccentColors("purple").border}`;
      case "gui_workflow":
        return `${getAccentColors("orange").bg} ${getAccentColors("orange").border}`;
      case "playwright":
        return `${getAccentColors("green").bg} ${getAccentColors("green").border}`;
      case "prompt":
        return `${getAccentColors("amber").bg} ${getAccentColors("amber").border}`;
      case "action":
        return `${getAccentColors("blue").bg} ${getAccentColors("blue").border}`;
      case "screenshot":
        return `${getAccentColors("cyan").bg} ${getAccentColors("cyan").border}`;
      case "test":
        return `${getAccentColors("emerald").bg} ${getAccentColors("emerald").border}`;
      default:
        return "bg-primary/5 border-primary/20";
    }
  };

  const getStepIcon = () => {
    switch (step.type) {
      case "workflow":
        return <Workflow className={`w-4 h-4 ${getAccentColors("purple").text} flex-shrink-0`} />;
      case "gui_workflow":
        return (
          <MousePointer2 className={`w-4 h-4 ${getAccentColors("orange").text} flex-shrink-0`} />
        );
      case "playwright":
        return <TestTube className={`w-4 h-4 ${getAccentColors("green").text} flex-shrink-0`} />;
      case "prompt":
        return <FileText className={`w-4 h-4 ${getAccentColors("amber").text} flex-shrink-0`} />;
      case "action":
        return (
          <MousePointer2 className={`w-4 h-4 ${getAccentColors("blue").text} flex-shrink-0`} />
        );
      case "screenshot":
        return <Camera className={`w-4 h-4 ${getAccentColors("cyan").text} flex-shrink-0`} />;
      case "test":
        return (
          <ClipboardCheck className={`w-4 h-4 ${getAccentColors("emerald").text} flex-shrink-0`} />
        );
      default:
        return <Target className="w-4 h-4 text-primary flex-shrink-0" />;
    }
  };

  return (
    <div className={`flex items-center gap-2 p-2 rounded-md border ${getStepColors()}`}>
      {/* Step number */}
      <span className="w-6 h-6 flex items-center justify-center text-xs font-medium rounded bg-background">
        {index + 1}
      </span>

      {/* Icon */}
      {getStepIcon()}

      {/* Name and badges */}
      <div className="flex-1 flex items-center gap-2 min-w-0">
        <span className="text-sm truncate">{step.name}</span>
        {/* Setup/Verification badge */}
        {canToggleSetup && (
          <button
            onClick={() => toggleStepIsSetup(step.id)}
            className={`px-1.5 py-0.5 text-xs rounded font-medium transition-colors ${
              isSetup
                ? `${getAccentColors("amber").bg} ${getAccentColors("amber").text} hover:bg-amber-500/30`
                : `${getAccentColors("green").bg} ${getAccentColors("green").text} hover:bg-green-500/30`
            }`}
            title={
              isSetup
                ? "Setup step: Brings app to target state. Click to mark as verification."
                : "Verification step: Tests functionality. Click to mark as setup."
            }
          >
            {isSetup ? "Setup" : "Verify"}
          </button>
        )}
        {/* Run on subsequent iterations toggle */}
        {showRunOnSubsequentToggle && (
          <button
            onClick={() => toggleStepRunOnSubsequent(step.id)}
            className={`px-1.5 py-0.5 text-xs rounded font-medium flex items-center gap-1 transition-colors ${
              runOnSubsequent
                ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} hover:bg-blue-500/30`
                : `bg-muted text-muted-foreground hover:bg-muted/80`
            }`}
            title={
              runOnSubsequent
                ? "Runs on ALL iterations. Click to run only on first iteration."
                : "Runs only on FIRST iteration. Click to run on all iterations."
            }
          >
            <Repeat className="w-3 h-3" />
            {runOnSubsequent ? "All" : "1st"}
          </button>
        )}
        {/* Test critical badge */}
        {step.type === "test" && (
          <button
            onClick={() => toggleStepTestCritical(step.id)}
            className={`px-1.5 py-0.5 text-xs rounded font-medium transition-colors ${
              step.testIsCritical
                ? `${getAccentColors("amber").bg} ${getAccentColors("amber").text} hover:bg-amber-500/30`
                : `bg-muted text-muted-foreground hover:bg-muted/80`
            }`}
            title={
              step.testIsCritical
                ? "Critical test: Failure will stop the workflow. Click to make non-critical."
                : "Non-critical test: Failure will be logged but won't stop the workflow. Click to make critical."
            }
          >
            {step.testIsCritical ? "Critical" : "Non-critical"}
          </button>
        )}
      </div>

      {/* Screenshot step: monitor selector and delay */}
      {step.type === "screenshot" && (
        <div className="flex items-center gap-2">
          <select
            value={step.screenshotMonitor === "all" ? "all" : (step.screenshotMonitor ?? "all")}
            onChange={(e) => {
              const val = e.target.value;
              updateScreenshotMonitor(step.id, val === "all" ? "all" : parseInt(val, 10));
            }}
            className="px-1 py-0.5 text-xs bg-background border border-border rounded"
            title="Monitor to capture"
            onClick={(e) => e.stopPropagation()}
          >
            <option value="all">All monitors</option>
            <option value="0">Monitor 0</option>
            <option value="1">Monitor 1</option>
            <option value="2">Monitor 2</option>
          </select>
          <div className="flex items-center gap-0.5" title="Delay before capture (seconds)">
            <input
              type="number"
              min={0}
              max={30}
              step={0.5}
              value={step.screenshotDelay ?? 0}
              onChange={(e) => updateScreenshotDelay(step.id, parseFloat(e.target.value) || 0)}
              className="w-12 px-1 py-0.5 text-xs text-center bg-background border border-border rounded"
              onClick={(e) => e.stopPropagation()}
            />
            <span className="text-xs text-muted-foreground">s</span>
          </div>
        </div>
      )}

      {/* Screenshot toggle for other step types */}
      {step.type !== "prompt" && step.type !== "screenshot" && (
        <div className="flex items-center gap-1">
          <button
            onClick={() => toggleStepScreenshot(step.id)}
            className={`p-1 rounded transition-colors ${
              step.takeScreenshot
                ? `${getStatusColors("success").text} hover:text-green-400`
                : "text-muted-foreground hover:text-foreground"
            }`}
            title={step.takeScreenshot ? "Screenshot enabled" : "Screenshot disabled"}
          >
            <Camera className="w-4 h-4" />
          </button>
          {step.takeScreenshot && (
            <div className="flex items-center gap-0.5" title="Screenshot delay (seconds)">
              <input
                type="number"
                min={0}
                max={30}
                step={0.5}
                value={step.screenshotDelay ?? 0}
                onChange={(e) => updateScreenshotDelay(step.id, parseFloat(e.target.value) || 0)}
                className="w-12 px-1 py-0.5 text-xs text-center bg-background border border-border rounded"
                onClick={(e) => e.stopPropagation()}
              />
              <span className="text-xs text-muted-foreground">s</span>
            </div>
          )}
        </div>
      )}

      {/* Move up */}
      <button
        onClick={() => moveStepUp(index)}
        disabled={index === 0}
        className="p-1 text-muted-foreground hover:text-foreground disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
        title="Move up"
      >
        <ChevronUp className="w-4 h-4" />
      </button>

      {/* Move down */}
      <button
        onClick={() => moveStepDown(index)}
        disabled={index === totalSteps - 1}
        className="p-1 text-muted-foreground hover:text-foreground disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
        title="Move down"
      >
        <ChevronDown className="w-4 h-4" />
      </button>

      {/* Remove */}
      <button
        onClick={() => removeStep(step.id)}
        className={`p-1 text-muted-foreground hover:${getStatusColors("error").text} transition-colors`}
        title="Remove step"
      >
        <Trash2 className="w-4 h-4" />
      </button>
    </div>
  );
}
