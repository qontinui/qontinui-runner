/**
 * BottomBar Component
 *
 * Bottom status bar showing elapsed time, current action, and connection status.
 */

import {
  Wifi,
  WifiOff,
  Monitor,
  Globe,
  Globe2,
  Bot,
  CheckCircle,
  AlertTriangle,
  Lightbulb,
  CheckSquare,
  BookOpen,
  Cog,
  Activity,
  Terminal,
  FileCode,
  GitBranch,
  Plug,
  ListOrdered,
  LayoutDashboard,
  Clock,
  Scan,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui";
import type { ActivityType } from "../../types/dashboard/activity-types";
import { ACTIVITY_DISPLAY_CONFIG } from "../../types/dashboard/activity-types";
import { getStatusColors, getAccentColors } from "@/design-system";
import { useSharedElapsedTime, useRunnerConnectionStatus } from "@/hooks";
import { useSharedStepDataRaw } from "@/contexts";
import type { OrchestratorAgent } from "../shared/AiMessageDisplay";
import { ORCHESTRATOR_AGENT_DISPLAY } from "../shared/AiMessageDisplay";

/**
 * Props for BottomBar component.
 */
export interface BottomBarProps {
  /** Currently active activity type */
  activeActivity?: ActivityType | null;
  /** Description of current action */
  currentAction?: string | null;
  /** Whether any activity is running */
  isRunning?: boolean;
  /** Current orchestrator agent (if using orchestrated workflow) */
  currentOrchestratorAgent?: OrchestratorAgent;
  /** Task start time (ms since epoch) for elapsed time display */
  taskStartTime?: number | null;
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
  shell_command: Terminal,
  api_request: Globe2,
  script: FileCode,
  workflow_ref: GitBranch,
  mcp_call: Plug,
  execution_timeline: ListOrdered,
  flow_execution: GitBranch,
  canvas: LayoutDashboard,
  trace_viewer: Scan,
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

/**
 * Format elapsed seconds into human-readable string.
 */
function formatElapsedTime(totalSeconds: number): string {
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const mins = Math.floor(totalSeconds / 60);
  const secs = totalSeconds % 60;
  if (mins < 60) return `${mins}m ${secs}s`;
  const hours = Math.floor(mins / 60);
  const remainMins = mins % 60;
  return `${hours}h ${remainMins}m`;
}

export function BottomBar({
  currentAction,
  activeActivity,
  isRunning = false,
  currentOrchestratorAgent,
  taskStartTime = null,
}: BottomBarProps) {
  const elapsedSec = useSharedElapsedTime(taskStartTime ?? null);
  const { isConnected, latencyMs } = useRunnerConnectionStatus();
  const { lastFetchedAt } = useSharedStepDataRaw();
  const dataAgeSec = useSharedElapsedTime(lastFetchedAt);
  const isStale = dataAgeSec > 10;
  const connColors = getStatusColors(isConnected ? "success" : "error");

  // Get activity display info
  const activityInfo = activeActivity ? ACTIVITY_DISPLAY_CONFIG[activeActivity] : null;
  const ActivityIcon = activeActivity ? activityIcons[activeActivity] : null;

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
      {/* Left: Elapsed Time */}
      <div className="flex items-center gap-2 w-[30%]">
        <Clock className="h-4 w-4 text-muted-foreground" />
        <span
          data-content-role="metric"
          data-content-label="elapsed time"
          className="text-sm text-foreground font-medium font-mono"
        >
          {formatElapsedTime(elapsedSec)}
        </span>
      </div>

      {/* Center: Orchestrator Agent + Active Activity + Current Action */}
      <div className="flex-1 flex items-center justify-center gap-3 px-4 min-w-0">
        {/* Orchestrator Agent Badge */}
        {currentOrchestratorAgent && AgentIcon && agentColors && isRunning && (
          <Badge
            data-content-role="badge"
            data-content-label="orchestrator agent"
            className={cn(
              agentColors.bg,
              agentColors.text,
              "border",
              agentColors.border,
              "shrink-0",
            )}
          >
            <AgentIcon className="mr-1 h-3 w-3" />
            {agentConfig?.label}
          </Badge>
        )}

        {/* Activity + Action */}
        <div className="flex items-center gap-2 min-w-0">
          {activeActivity && ActivityIcon && isRunning && (
            <div className="flex items-center gap-1.5 shrink-0">
              <ActivityIcon className="w-4 h-4 text-muted-foreground" />
              <span
                data-content-role="label"
                data-content-label="activity type"
                className="text-xs text-muted-foreground"
              >
                {activityInfo?.displayName}:
              </span>
            </div>
          )}
          <p className="text-sm text-foreground font-medium truncate">
            {currentAction || (isRunning ? "Processing..." : "Waiting for action...")}
          </p>
        </div>
      </div>

      {/* Right: Freshness + Connection Status */}
      <div className="flex items-center justify-end gap-2 w-[20%]">
        {lastFetchedAt && (
          <span
            data-content-role="metric"
            data-content-label="data freshness"
            className={cn("text-xs", isStale ? "text-amber-400" : "text-muted-foreground")}
          >
            {dataAgeSec}s ago
          </span>
        )}
        <Badge
          data-content-role="status"
          data-content-label="connection status"
          className={cn(connColors.bg, connColors.text, "border", connColors.border)}
        >
          {isConnected ? (
            <Wifi className="mr-1.5 h-3 w-3" />
          ) : (
            <WifiOff className="mr-1.5 h-3 w-3" />
          )}
          {isConnected ? (latencyMs !== null ? `${latencyMs}ms` : "Connected") : "Disconnected"}
        </Badge>
      </div>
    </div>
  );
}
