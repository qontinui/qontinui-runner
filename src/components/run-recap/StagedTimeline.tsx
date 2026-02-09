/**
 * StagedTimeline
 *
 * Displays steps grouped by workflow stages (setup, agentic, verification, completion).
 * Uses collapsible sections with stage icons and status indicators.
 * Progress indicators are wrapped with error boundaries to prevent
 * parsing errors from crashing the recap view.
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
} from "lucide-react";
import { WORKFLOW_STAGE_CONFIG, type WorkflowStage } from "@/types/dashboard/activity-types";
import type { StageRecap, RecapStep } from "@/types/recap";
import { getStepIconConfigWithFallback } from "@/lib/step-icons";
import { formatDuration } from "@/lib/formatting";
import { getStatusIcon } from "@/lib/status-icons";
import { InlineProgressBar, type ProgressType, ProgressErrorBoundary } from "@/components/ui";

/**
 * Map progress type string to ProgressType for semantic coloring.
 */
function mapToProgressType(type: string | undefined): ProgressType {
  if (!type) return "default";
  const mapping: Record<string, ProgressType> = {
    file_progress: "file_progress",
    analysis_progress: "analysis_progress",
    test_progress: "test_progress",
    review_progress: "review_progress",
    iteration_progress: "iteration_progress",
  };
  return mapping[type] || "default";
}

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

interface StepItemProps {
  step: RecapStep;
  onClick?: () => void;
}

function StepItem({ step, onClick }: StepItemProps) {
  // Use icon_type if available, falling back to step_type
  const {
    icon: Icon,
    bgClass,
    textClass,
  } = getStepIconConfigWithFallback(step.icon_type, step.step_type);

  // Prefer work_summary (AI-generated) over summary (deterministic)
  const displaySummary = step.work_summary || step.summary;

  // Don't show error if it's identical to the summary (avoid duplication)
  const showError = step.error && step.error !== displaySummary;

  const isClickable = !!onClick;

  return (
    <div
      data-ui-id={`recap-staged-step-${step.step_type}`}
      className={`flex items-center gap-3 px-3 py-2 rounded-lg transition-colors ${
        isClickable ? "hover:bg-muted/50 cursor-pointer" : "hover:bg-muted/30"
      }`}
      onClick={onClick}
      role={isClickable ? "button" : undefined}
      tabIndex={isClickable ? 0 : undefined}
      onKeyDown={
        isClickable
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") onClick?.();
            }
          : undefined
      }
    >
      {/* Status icon */}
      {getStatusIcon(step.status)}

      {/* Step type icon - using shared icon config with proper colors */}
      <div className={`p-1 rounded ${bgClass}`}>
        <Icon className={`w-3 h-3 ${textClass}`} />
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
          {/* Show completed progress inline - wrapped with error boundary */}
          {step.progress && step.progress.total !== null && (
            <ProgressErrorBoundary compact componentName="StagedTimeline.InlineProgressBar">
              <InlineProgressBar
                current={step.progress.current}
                total={step.progress.total}
                progressType={mapToProgressType(step.progress.type)}
              />
            </ProgressErrorBoundary>
          )}
        </div>
        {displaySummary && (
          <p className="text-xs text-muted-foreground truncate mt-0.5">{displaySummary}</p>
        )}
        {/* Progress description if available */}
        {step.progress?.description && (
          <p className="text-xs text-muted-foreground truncate mt-0.5">
            {step.progress.description}
          </p>
        )}
        {showError && <p className="text-xs text-red-400 truncate mt-0.5">{step.error}</p>}
      </div>

      {/* Navigate hint for clickable steps */}
      {isClickable && <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />}
    </div>
  );
}

interface StageSectionProps {
  stage: StageRecap;
  onAiStepClick?: (phase: string, iteration?: number) => void;
}

function StageSection({ stage, onAiStepClick }: StageSectionProps) {
  const [expanded, setExpanded] = useState(false);

  const stageKey = stage.stage as WorkflowStage;
  const config = WORKFLOW_STAGE_CONFIG[stageKey];
  const Icon = STAGE_ICONS[stageKey] || Activity;
  const colorClasses = COLOR_CLASSES[config?.color] || COLOR_CLASSES.blue;

  return (
    <div data-ui-id={`recap-stage-section-${stage.stage}`} className="card overflow-hidden">
      <button
        data-ui-id="recap-stage-toggle-btn"
        onClick={() => setExpanded(!expanded)}
        className="w-full px-4 py-3 flex items-center justify-between hover:bg-muted/50 transition-colors"
      >
        <div className="flex items-center gap-3">
          <div className={`p-1.5 rounded ${colorClasses.bg}`}>
            <Icon className={`w-4 h-4 ${colorClasses.text}`} />
          </div>
          <span className="font-medium">{stage.display_name}</span>
          {getStatusIcon(stage.status)}
          {/* Only show iteration badge for stages that loop (verification/agentic) */}
          {(stage.stage === "verification" || stage.stage === "agentic") &&
            stage.iteration !== undefined && (
              <span
                data-ui-id={`recap-iteration-${stage.stage}-${stage.iteration}`}
                className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded"
              >
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
            <StepItem
              key={`${step.name}-${i}`}
              step={step}
              onClick={
                step.step_type === "ai_session" && onAiStepClick
                  ? () => onAiStepClick(stage.stage, stage.iteration)
                  : undefined
              }
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface StagedTimelineProps {
  stages: StageRecap[];
  onAiStepClick?: (phase: string, iteration?: number) => void;
}

// Default stage order and labels
// Note: verification comes before agentic because the typical flow is:
// setup -> verification (check initial state) -> agentic (fix issues) -> verification (re-check) -> ...
const DEFAULT_STAGES: WorkflowStage[] = ["setup", "verification", "agentic", "completion"];
const STAGE_LABELS: Record<WorkflowStage, string> = {
  setup: "Setup",
  agentic: "Agentic",
  verification: "Verification",
  completion: "Completion",
};

export function StagedTimeline({ stages, onAiStepClick }: StagedTimelineProps) {
  // Build a map of existing stages for quick lookup
  const stageMap = new Map<string, StageRecap>();
  stages.forEach((s) => {
    const key = `${s.stage}-${s.iteration ?? 0}`;
    stageMap.set(key, s);
  });

  // Collect all stages - start with actual data from backend
  const allStages: StageRecap[] = [...stages];

  // Add placeholder stages for any missing iteration 0 stages
  // (only if no stages exist for that phase at all)
  DEFAULT_STAGES.forEach((stageName) => {
    const hasAnyIterationOfStage = stages.some((s) => s.stage === stageName);
    if (!hasAnyIterationOfStage) {
      // Create a placeholder stage only if this phase never ran
      allStages.push({
        stage: stageName,
        display_name: STAGE_LABELS[stageName],
        status: "pending",
        steps: [],
        iteration: 0,
      });
    }
  });

  // Sort stages by phase order (matching backend logic in stage_builder.rs).
  // Phase order is the primary sort; timestamps are only used within the same phase+iteration.
  allStages.sort((a, b) => {
    const phaseOrder: Record<string, number> = {
      setup: 0,
      verification: 1,
      agentic: 2,
      completion: 3,
    };
    const aPhase = phaseOrder[a.stage] ?? 2;
    const bPhase = phaseOrder[b.stage] ?? 2;
    const aIter = a.iteration ?? 0;
    const bIter = b.iteration ?? 0;

    // Setup always first, completion always last
    if (aPhase === 0 || bPhase === 0 || aPhase === 3 || bPhase === 3) {
      return aPhase - bPhase;
    }

    // For verification and agentic: sort by iteration first
    if (aIter !== bIter) return aIter - bIter;

    // Same iteration: verification before agentic
    if (aPhase !== bPhase) return aPhase - bPhase;

    // Same phase and iteration: use timestamps if available
    if (a.started_at && b.started_at) {
      return new Date(a.started_at).getTime() - new Date(b.started_at).getTime();
    }
    return 0;
  });

  if (allStages.length === 0) {
    return (
      <div className="card p-6 text-center text-muted-foreground">
        <Activity className="w-8 h-8 mx-auto mb-2 opacity-50" />
        <p>No stages recorded</p>
      </div>
    );
  }

  return (
    <div data-ui-id="recap-staged-timeline" className="space-y-3">
      {allStages.map((stage, index) => (
        <StageSection
          key={`${stage.stage}-${stage.iteration ?? index}`}
          stage={stage}
          onAiStepClick={onAiStepClick}
        />
      ))}
    </div>
  );
}

export default StagedTimeline;
