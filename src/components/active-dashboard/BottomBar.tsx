/**
 * BottomBar Component
 *
 * Bottom status bar showing iteration progress, current action, and connection status.
 */

import {
  Wifi,
  Monitor,
  Globe,
  Bot,
  CheckCircle,
  AlertTriangle,
  Lightbulb,
  CheckSquare,
  BookOpen,
  Cog,
  Activity,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui";
import type { ActivityType } from "../../types/dashboard/activity-types";
import { ACTIVITY_DISPLAY_CONFIG } from "../../types/dashboard/activity-types";
import { getStatusColors, getAccentColors } from "@/design-system";
import type { OrchestratorAgent } from "../shared/AiMessageDisplay";
import { ORCHESTRATOR_AGENT_DISPLAY } from "../shared/AiMessageDisplay";

/**
 * Props for BottomBar component.
 */
export interface BottomBarProps {
  /** Current iteration (1-based) */
  iteration?: number;
  /** Maximum iterations */
  maxIterations?: number;
  /** Currently active activity type */
  activeActivity?: ActivityType | null;
  /** Description of current action */
  currentAction?: string | null;
  /** Whether any activity is running */
  isRunning?: boolean;
  /** Current orchestrator agent (if using orchestrated workflow) */
  currentOrchestratorAgent?: OrchestratorAgent;
}

/**
 * Icon mapping for activity types.
 */
const activityIcons: Record<ActivityType, React.ComponentType<{ className?: string }>> = {
  gui_automation: Monitor,
  playwright: Globe,
  ai_conversation: Bot,
  verification: CheckCircle,
  findings: AlertTriangle,
  execution_status: Activity,
};

/**
 * Icon mapping for orchestrator agents.
 */
const orchestratorAgentIcons: Record<
  NonNullable<OrchestratorAgent>,
  React.ComponentType<{ className?: string }>
> = {
  planning: Lightbulb,
  verification: CheckSquare,
  knowledge: BookOpen,
  orchestrator: Cog,
  worker: Bot,
};

export function BottomBar({
  currentAction,
  iteration = 1,
  maxIterations = 1,
  activeActivity,
  isRunning = false,
  currentOrchestratorAgent,
}: BottomBarProps) {
  const iterationProgress = maxIterations > 0 ? (iteration / maxIterations) * 100 : 0;

  // Get activity display info
  const activityInfo = activeActivity ? ACTIVITY_DISPLAY_CONFIG[activeActivity] : null;
  const ActivityIcon = activeActivity ? activityIcons[activeActivity] : null;
  const blueColors = getAccentColors("blue");
  const successColors = getStatusColors("success");

  // Get orchestrator agent display info
  const agentConfig = currentOrchestratorAgent
    ? ORCHESTRATOR_AGENT_DISPLAY[currentOrchestratorAgent]
    : null;
  const AgentIcon = currentOrchestratorAgent
    ? orchestratorAgentIcons[currentOrchestratorAgent]
    : null;
  const agentColors = agentConfig ? getAccentColors(agentConfig.color) : null;

  return (
    <div className="flex h-10 items-center border-t border-border bg-card px-4">
      {/* Left: Iteration Progress */}
      <div className="flex items-center gap-2 w-[30%]">
        {/* Circular progress indicator */}
        <div className="relative h-4 w-4">
          <svg className="h-4 w-4 -rotate-90 transform">
            <circle
              cx="8"
              cy="8"
              r="6"
              stroke="currentColor"
              strokeWidth="2"
              fill="none"
              className="text-muted"
            />
            <circle
              cx="8"
              cy="8"
              r="6"
              stroke="currentColor"
              strokeWidth="2"
              fill="none"
              strokeDasharray={`${2 * Math.PI * 6}`}
              strokeDashoffset={`${2 * Math.PI * 6 * (1 - iterationProgress / 100)}`}
              className={cn("transition-all", blueColors.text)}
              strokeLinecap="round"
            />
          </svg>
        </div>
        <span className="text-sm text-foreground font-medium">
          Iteration {iteration}/{maxIterations}
        </span>
      </div>

      {/* Center: Orchestrator Agent + Active Activity + Current Action */}
      <div className="flex-1 flex items-center justify-center gap-3 px-4 min-w-0">
        {/* Orchestrator Agent Badge */}
        {currentOrchestratorAgent && AgentIcon && agentColors && isRunning && (
          <Badge
            className={cn(
              agentColors.bg,
              agentColors.text,
              "border",
              agentColors.border,
              "flex-shrink-0",
            )}
          >
            <AgentIcon className="mr-1 h-3 w-3" />
            {agentConfig?.label}
          </Badge>
        )}

        {/* Activity + Action */}
        <div className="flex items-center gap-2 min-w-0">
          {activeActivity && ActivityIcon && isRunning && (
            <div className="flex items-center gap-1.5 flex-shrink-0">
              <ActivityIcon className="w-4 h-4 text-muted-foreground" />
              <span className="text-xs text-muted-foreground">{activityInfo?.displayName}:</span>
            </div>
          )}
          <p className="text-sm text-foreground font-medium truncate">
            {currentAction || (isRunning ? "Processing..." : "Waiting for action...")}
          </p>
        </div>
      </div>

      {/* Right: Connection Status */}
      <div className="flex items-center justify-end w-[20%]">
        <Badge className={cn(successColors.bg, successColors.text, "border", successColors.border)}>
          <Wifi className="mr-1.5 h-3 w-3" />
          Connected
        </Badge>
      </div>
    </div>
  );
}
