/**
 * RoutingStatusSection Component
 *
 * Displays the task routing status including model selection,
 * complexity assessment, and the factors that influenced the decision.
 */

import { Brain, Zap, Scale, Sparkles, Info } from "lucide-react";
import { Badge } from "../ui/Badge";
import { Progress } from "../ui/Progress";
import {
  type RoutingStatus,
  type TaskComplexity,
  getComplexityDisplayName,
} from "../../types/executionStatus";
import { getAccentColors } from "@/design-system";

interface RoutingStatusSectionProps {
  routing: RoutingStatus;
}

/** Get icon for complexity level */
function getComplexityIcon(complexity: TaskComplexity) {
  switch (complexity) {
    case "simple":
      return <Zap className="w-4 h-4" />;
    case "medium":
      return <Scale className="w-4 h-4" />;
    case "complex":
      return <Sparkles className="w-4 h-4" />;
  }
}

/** Get color classes for complexity level */
function getComplexityColors(complexity: TaskComplexity) {
  switch (complexity) {
    case "simple":
      return getAccentColors("green");
    case "medium":
      return getAccentColors("amber");
    case "complex":
      return getAccentColors("purple");
  }
}

/** Get model display name */
function getModelDisplayName(model: string): string {
  if (model.includes("haiku")) return "Haiku";
  if (model.includes("sonnet")) return "Sonnet";
  if (model.includes("opus")) return "Opus";
  return model.split("-")[0] || model;
}

export function RoutingStatusSection({ routing }: RoutingStatusSectionProps) {
  const { enabled, decision, config } = routing;

  if (!enabled) {
    return (
      <div className="space-y-3">
        <div className="flex items-center gap-2 text-muted-foreground">
          <Info className="w-4 h-4" />
          <span className="text-sm">Task routing is disabled</span>
        </div>
        <p className="text-xs text-muted-foreground">
          Enable intelligent task routing in settings to automatically select the best model for
          each task.
        </p>
      </div>
    );
  }

  if (!decision) {
    return (
      <div className="space-y-3">
        <div className="flex items-center gap-2 text-muted-foreground">
          <Brain className="w-4 h-4" />
          <span className="text-sm">Waiting for task...</span>
        </div>
        <div className="text-xs text-muted-foreground">
          <p>Available models:</p>
          <ul className="mt-1 space-y-0.5 ml-4 list-disc">
            <li>Simple: {getModelDisplayName(config.simpleModel)}</li>
            <li>Medium: {getModelDisplayName(config.mediumModel)}</li>
            <li>Complex: {getModelDisplayName(config.complexModel)}</li>
          </ul>
        </div>
      </div>
    );
  }

  const complexityColors = getComplexityColors(decision.complexity);
  const confidencePercent = Math.round(decision.confidence * 100);

  return (
    <div className="space-y-4">
      {/* Selected Model */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Brain className="w-4 h-4 text-primary" />
          <span className="text-sm font-medium">Selected Model</span>
        </div>
        <Badge
          className={`${complexityColors.bg} ${complexityColors.text} ${complexityColors.border}`}
        >
          {getComplexityIcon(decision.complexity)}
          <span className="ml-1">{getModelDisplayName(decision.selectedModel)}</span>
        </Badge>
      </div>

      {/* Complexity Assessment */}
      <div className="space-y-2">
        <div className="flex items-center justify-between text-sm">
          <span className="text-muted-foreground">Complexity</span>
          <span className={`font-medium ${complexityColors.text}`}>
            {getComplexityDisplayName(decision.complexity)}
          </span>
        </div>

        {/* Confidence Bar */}
        <div className="space-y-1">
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>Confidence</span>
            <span>{confidencePercent}%</span>
          </div>
          <Progress value={confidencePercent} className="h-1.5" />
        </div>
      </div>

      {/* Context Info */}
      {(decision.fileCount !== undefined || decision.criteriaCount !== undefined) && (
        <div className="flex gap-4 text-xs text-muted-foreground">
          {decision.fileCount !== undefined && (
            <span>
              {decision.fileCount} file{decision.fileCount !== 1 ? "s" : ""}
            </span>
          )}
          {decision.criteriaCount !== undefined && (
            <span>
              {decision.criteriaCount} criteri{decision.criteriaCount !== 1 ? "a" : "on"}
            </span>
          )}
        </div>
      )}

      {/* Factors */}
      {decision.factors.length > 0 && (
        <div className="space-y-2">
          <span className="text-xs text-muted-foreground">Decision Factors</span>
          <div className="space-y-1">
            {decision.factors.map((factor, index) => (
              <div
                key={index}
                className="text-xs bg-muted/30 rounded px-2 py-1 text-muted-foreground"
              >
                {factor}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Prompt Preview */}
      {decision.promptPreview && (
        <div className="space-y-1">
          <span className="text-xs text-muted-foreground">Task Preview</span>
          <p className="text-xs bg-muted/30 rounded px-2 py-1 text-foreground truncate">
            {decision.promptPreview}
          </p>
        </div>
      )}
    </div>
  );
}
