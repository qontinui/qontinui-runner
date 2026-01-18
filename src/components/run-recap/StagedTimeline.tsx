/**
 * StagedTimeline
 *
 * Displays steps grouped by workflow stages (setup, agentic, verification, completion).
 * Uses collapsible sections with stage icons and status indicators.
 */

import { useState } from "react";
import {
  Settings,
  Bot,
  CheckSquare,
  Flag,
  ChevronDown,
  ChevronRight,
  Activity,
  CheckCircle2,
  XCircle,
  AlertCircle,
  Clock,
  Zap,
  TestTube,
  Play,
} from "lucide-react";
import {
  WORKFLOW_STAGE_CONFIG,
  type WorkflowStage,
} from "@/types/dashboard/activity-types";
import type { StageRecap, RecapStep } from "@/types/recap";

// Stage icons mapping
const STAGE_ICONS: Record<WorkflowStage, React.ElementType> = {
  setup: Settings,
  agentic: Bot,
  verification: CheckSquare,
  completion: Flag,
};

// Color classes mapping for Tailwind
const COLOR_CLASSES: Record<string, { bg: string; text: string }> = {
  blue: { bg: "bg-blue-500/10", text: "text-blue-500" },
  green: { bg: "bg-green-500/10", text: "text-green-500" },
  purple: { bg: "bg-purple-500/10", text: "text-purple-500" },
  teal: { bg: "bg-teal-500/10", text: "text-teal-500" },
};

/**
 * Format duration in milliseconds to a human-readable string.
 */
function formatDuration(ms: number | undefined): string {
  if (ms === undefined || ms === null) return "-";

  if (ms < 1000) {
    return `${ms}ms`;
  }

  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) {
    return `${seconds}s`;
  }

  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) {
    return remainingSeconds > 0 ? `${minutes}m ${remainingSeconds}s` : `${minutes}m`;
  }

  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
}

/**
 * Get the status icon component.
 */
function getStatusIcon(status: string) {
  switch (status) {
    case "success":
    case "complete":
      return <CheckCircle2 className="w-4 h-4 text-green-500" />;
    case "failed":
      return <XCircle className="w-4 h-4 text-red-500" />;
    case "running":
      return <Activity className="w-4 h-4 text-blue-500 animate-pulse" />;
    case "skipped":
    case "pending":
      return <AlertCircle className="w-4 h-4 text-yellow-500" />;
    default:
      return <Clock className="w-4 h-4 text-muted-foreground" />;
  }
}

/**
 * Get the icon for a step type.
 */
function getStepIcon(stepType: string) {
  switch (stepType) {
    case "workflow":
      return Play;
    case "action":
      return Zap;
    case "ai_session":
      return Bot;
    case "test":
    case "check":
      return TestTube;
    default:
      return Activity;
  }
}

interface StepItemProps {
  step: RecapStep;
}

function StepItem({ step }: StepItemProps) {
  const Icon = getStepIcon(step.step_type);

  return (
    <div className="flex items-center gap-3 px-3 py-2 rounded-lg hover:bg-muted/30 transition-colors">
      {/* Status icon */}
      {getStatusIcon(step.status)}

      {/* Step type icon */}
      <div className="p-1 rounded bg-muted/50">
        <Icon className="w-3 h-3 text-muted-foreground" />
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="font-medium text-sm truncate">{step.name}</span>
          {step.duration_ms !== undefined && (
            <span className="text-xs text-muted-foreground">
              ({formatDuration(step.duration_ms)})
            </span>
          )}
        </div>
        {step.summary && (
          <p className="text-xs text-muted-foreground truncate mt-0.5">{step.summary}</p>
        )}
        {step.error && <p className="text-xs text-red-400 truncate mt-0.5">{step.error}</p>}
      </div>
    </div>
  );
}

interface StageSectionProps {
  stage: StageRecap;
}

function StageSection({ stage }: StageSectionProps) {
  const [expanded, setExpanded] = useState(
    stage.status === "failed" || stage.status === "running"
  );

  const stageKey = stage.stage as WorkflowStage;
  const config = WORKFLOW_STAGE_CONFIG[stageKey];
  const Icon = STAGE_ICONS[stageKey] || Activity;
  const colorClasses = COLOR_CLASSES[config?.color] || COLOR_CLASSES.blue;

  return (
    <div className="card overflow-hidden">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full px-4 py-3 flex items-center justify-between hover:bg-muted/50 transition-colors"
      >
        <div className="flex items-center gap-3">
          <div className={`p-1.5 rounded ${colorClasses.bg}`}>
            <Icon className={`w-4 h-4 ${colorClasses.text}`} />
          </div>
          <span className="font-medium">{stage.display_name}</span>
          {getStatusIcon(stage.status)}
          {stage.iteration !== undefined && stage.iteration > 0 && (
            <span className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
              Iteration {stage.iteration}
            </span>
          )}
          <span className="text-sm text-muted-foreground">
            ({stage.steps.length} {stage.steps.length === 1 ? "step" : "steps"})
          </span>
        </div>
        <div className="flex items-center gap-2">
          {stage.duration_ms !== undefined && (
            <span className="text-sm text-muted-foreground">
              {formatDuration(stage.duration_ms)}
            </span>
          )}
          {stage.steps.length > 0 &&
            (expanded ? (
              <ChevronDown className="w-4 h-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-4 h-4 text-muted-foreground" />
            ))}
        </div>
      </button>

      {expanded && stage.steps.length > 0 && (
        <div className="border-t border-border p-2 space-y-1">
          {stage.steps.map((step, i) => (
            <StepItem key={`${step.name}-${i}`} step={step} />
          ))}
        </div>
      )}
    </div>
  );
}

interface StagedTimelineProps {
  stages: StageRecap[];
}

export function StagedTimeline({ stages }: StagedTimelineProps) {
  // Filter out empty skipped stages
  const visibleStages = stages.filter(
    (s) => s.steps.length > 0 || s.status !== "skipped"
  );

  if (visibleStages.length === 0) {
    return (
      <div className="card p-6 text-center text-muted-foreground">
        <Activity className="w-8 h-8 mx-auto mb-2 opacity-50" />
        <p>No stages recorded</p>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {visibleStages.map((stage, index) => (
        <StageSection key={`${stage.stage}-${stage.iteration ?? index}`} stage={stage} />
      ))}
    </div>
  );
}

export default StagedTimeline;
