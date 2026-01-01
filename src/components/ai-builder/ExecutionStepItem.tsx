/**
 * ExecutionStepItem.tsx
 *
 * A single execution step item with controls for screenshot, delay, and reordering.
 */

import {
  Camera,
  ChevronDown,
  ChevronUp,
  FileText,
  MousePointer2,
  Target,
  TestTube,
  Trash2,
  Workflow,
} from "lucide-react";
import type { ExecutionStep } from "./types";
import { useAiBuilder } from "./AiBuilderContext";

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
    moveStepUp,
    moveStepDown,
    removeStep,
  } = useAiBuilder();

  const getStepColors = () => {
    switch (step.type) {
      case "workflow":
        return "bg-purple-500/5 border-purple-500/20";
      case "playwright":
        return "bg-green-500/5 border-green-500/20";
      case "prompt":
        return "bg-amber-500/5 border-amber-500/20";
      case "action":
        return "bg-blue-500/5 border-blue-500/20";
      case "screenshot":
        return "bg-cyan-500/5 border-cyan-500/20";
      default:
        return "bg-primary/5 border-primary/20";
    }
  };

  const getStepIcon = () => {
    switch (step.type) {
      case "workflow":
        return <Workflow className="w-4 h-4 text-purple-500 flex-shrink-0" />;
      case "playwright":
        return <TestTube className="w-4 h-4 text-green-500 flex-shrink-0" />;
      case "prompt":
        return <FileText className="w-4 h-4 text-amber-500 flex-shrink-0" />;
      case "action":
        return <MousePointer2 className="w-4 h-4 text-blue-500 flex-shrink-0" />;
      case "screenshot":
        return <Camera className="w-4 h-4 text-cyan-500 flex-shrink-0" />;
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

      {/* Name */}
      <span className="flex-1 text-sm truncate">{step.name}</span>

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
                ? "text-green-500 hover:text-green-400"
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
        className="p-1 text-muted-foreground hover:text-red-500 transition-colors"
        title="Remove step"
      >
        <Trash2 className="w-4 h-4" />
      </button>
    </div>
  );
}
