/**
 * ExecutionStatusPanel Component
 *
 * Main panel that provides real-time visibility into agentic features
 * during task execution. Displays status for task routing, retries,
 * memory compression, and lifecycle hooks.
 */

import { useState } from "react";
import {
  Activity,
  Brain,
  RefreshCw,
  Archive,
  Webhook,
  ChevronDown,
  Circle,
  Trash2,
  Radio,
} from "lucide-react";
import { Card, CardHeader, CardTitle, CardContent } from "../ui/Card";
import { Badge } from "../ui/Badge";
import { ScrollArea } from "../ui/ScrollArea";
import { RoutingStatusSection } from "./RoutingStatusSection";
import { RetryStatusSection } from "./RetryStatusSection";
import { CompressionStatusSection } from "./CompressionStatusSection";
import { HookExecutionSection } from "./HookExecutionSection";
import { type ExecutionStatus } from "../../types/executionStatus";
import { getStatusColors, getAccentColors } from "@/design-system";

interface ExecutionStatusPanelProps {
  /** Execution status data */
  status: ExecutionStatus;
  /** Whether connected to events */
  isConnected: boolean;
  /** Callback to clear status */
  onClear?: () => void;
  /** Optional className for custom styling */
  className?: string;
}

interface CollapsibleSectionProps {
  title: string;
  icon: React.ReactNode;
  children: React.ReactNode;
  defaultCollapsed?: boolean;
  badge?: React.ReactNode;
}

function CollapsibleSection({
  title,
  icon,
  children,
  defaultCollapsed = false,
  badge,
}: CollapsibleSectionProps) {
  const [isCollapsed, setIsCollapsed] = useState(defaultCollapsed);

  return (
    <div className="border-b border-border/50 last:border-b-0">
      <button
        onClick={() => setIsCollapsed(!isCollapsed)}
        className="w-full flex items-center justify-between px-4 py-3 hover:bg-muted/30 transition-colors"
        aria-expanded={!isCollapsed}
      >
        <div className="flex items-center gap-2">
          <span className="text-primary">{icon}</span>
          <span className="text-sm font-medium">{title}</span>
          {badge}
        </div>
        <ChevronDown
          className={`w-4 h-4 text-muted-foreground transition-transform ${
            !isCollapsed ? "rotate-180" : ""
          }`}
        />
      </button>
      {!isCollapsed && <div className="px-4 pb-4">{children}</div>}
    </div>
  );
}

/** Get status color and label */
function getStatusInfo(status: ExecutionStatus["status"]) {
  switch (status) {
    case "running":
      return {
        color: getStatusColors("running"),
        label: "Running",
        dotClass: "bg-green-500 animate-pulse",
      };
    case "completed":
      return {
        color: getStatusColors("success"),
        label: "Completed",
        dotClass: "bg-green-500",
      };
    case "failed":
      return {
        color: getStatusColors("error"),
        label: "Failed",
        dotClass: "bg-red-500",
      };
    case "paused":
      return {
        color: getStatusColors("warning"),
        label: "Paused",
        dotClass: "bg-amber-500",
      };
    case "idle":
    default:
      return {
        color: { bg: "bg-muted/50", text: "text-muted-foreground", border: "border-muted" },
        label: "Idle",
        dotClass: "bg-muted-foreground",
      };
  }
}

export function ExecutionStatusPanel({
  status,
  isConnected,
  onClear,
  className = "",
}: ExecutionStatusPanelProps) {
  const statusInfo = getStatusInfo(status.status);
  const hasRouting = status.routing.decision !== null;
  const hasRetries = status.retry.state.errorHistory.length > 0;
  const hasCompression = status.compression.currentTokenCount !== null;
  const hasHooks = status.hooks.executionHistory.length > 0;

  return (
    <Card className={`flex flex-col h-full ${className}`}>
      {/* Header */}
      <CardHeader className="border-b border-border/50 flex-shrink-0">
        <div className="flex items-center justify-between w-full">
          <CardTitle className="flex items-center gap-2">
            <Activity className="w-4 h-4 text-primary" />
            <span>Execution Status</span>
          </CardTitle>
          <div className="flex items-center gap-2">
            {/* Connection Status */}
            <div
              className="flex items-center gap-1.5"
              title={isConnected ? "Connected to events" : "Not connected"}
            >
              <Radio
                className={`w-3 h-3 ${isConnected ? "text-green-500" : "text-muted-foreground"}`}
              />
            </div>

            {/* Clear Button */}
            {onClear && (
              <button
                onClick={onClear}
                className="p-1 hover:bg-muted/50 rounded transition-colors"
                title="Clear status"
              >
                <Trash2 className="w-3.5 h-3.5 text-muted-foreground" />
              </button>
            )}
          </div>
        </div>
      </CardHeader>

      {/* Status Summary */}
      <div className="px-4 py-3 border-b border-border/50 bg-muted/20 flex-shrink-0">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {/* Status Badge */}
            <Badge className={`${statusInfo.color.bg} ${statusInfo.color.text}`}>
              <Circle className={`w-2 h-2 mr-1.5 ${statusInfo.dotClass}`} />
              {statusInfo.label}
            </Badge>

            {/* Task Name */}
            {status.taskName && (
              <span className="text-sm text-muted-foreground truncate max-w-[150px]">
                {status.taskName}
              </span>
            )}
          </div>

          {/* Iteration */}
          {status.iteration > 0 && (
            <span className="text-xs text-muted-foreground">Iteration {status.iteration}</span>
          )}
        </div>

        {/* Quick Stats */}
        <div className="flex items-center gap-4 mt-2 text-xs">
          {hasRouting && status.routing.decision && (
            <span className="flex items-center gap-1 text-muted-foreground">
              <Brain className="w-3 h-3" />
              {status.routing.decision.selectedModel.includes("haiku")
                ? "Haiku"
                : status.routing.decision.selectedModel.includes("sonnet")
                  ? "Sonnet"
                  : status.routing.decision.selectedModel.includes("opus")
                    ? "Opus"
                    : "Model"}
            </span>
          )}
          {hasRetries && (
            <span
              className={`flex items-center gap-1 ${status.retry.exhausted ? getAccentColors("red").text : "text-muted-foreground"}`}
            >
              <RefreshCw className="w-3 h-3" />
              {status.retry.state.attempt} retries
            </span>
          )}
          {hasCompression && status.compression.currentTokenCount && (
            <span
              className={`flex items-center gap-1 ${status.compression.compressionImminent ? getAccentColors("amber").text : "text-muted-foreground"}`}
            >
              <Archive className="w-3 h-3" />
              {Math.round(status.compression.thresholdPercentage)}%
            </span>
          )}
          {hasHooks && (
            <span className="flex items-center gap-1 text-muted-foreground">
              <Webhook className="w-3 h-3" />
              {status.hooks.executionHistory.length} hooks
            </span>
          )}
        </div>
      </div>

      {/* Scrollable Content */}
      <ScrollArea className="flex-1">
        <div className="divide-y divide-border/50">
          {/* Task Routing Section */}
          <CollapsibleSection
            title="Task Routing"
            icon={<Brain className="w-4 h-4" />}
            badge={
              hasRouting && status.routing.decision ? (
                <Badge variant="info" size="sm">
                  {status.routing.decision.complexity}
                </Badge>
              ) : undefined
            }
          >
            <RoutingStatusSection routing={status.routing} />
          </CollapsibleSection>

          {/* Retry Status Section */}
          <CollapsibleSection
            title="Retry Status"
            icon={<RefreshCw className="w-4 h-4" />}
            badge={
              hasRetries ? (
                <Badge variant={status.retry.exhausted ? "danger" : "warning"} size="sm">
                  {status.retry.state.attempt} / {status.retry.maxRetries}
                </Badge>
              ) : undefined
            }
          >
            <RetryStatusSection retry={status.retry} />
          </CollapsibleSection>

          {/* Memory Compression Section */}
          <CollapsibleSection
            title="Memory Compression"
            icon={<Archive className="w-4 h-4" />}
            badge={
              hasCompression ? (
                <Badge
                  variant={status.compression.compressionImminent ? "warning" : "muted"}
                  size="sm"
                >
                  {Math.round(status.compression.thresholdPercentage)}%
                </Badge>
              ) : undefined
            }
          >
            <CompressionStatusSection compression={status.compression} />
          </CollapsibleSection>

          {/* Lifecycle Hooks Section */}
          <CollapsibleSection
            title="Lifecycle Hooks"
            icon={<Webhook className="w-4 h-4" />}
            defaultCollapsed={true}
            badge={
              hasHooks ? (
                <Badge variant="muted" size="sm">
                  {status.hooks.executionHistory.filter((h) => h.success).length} /{" "}
                  {status.hooks.executionHistory.length}
                </Badge>
              ) : undefined
            }
          >
            <HookExecutionSection hooks={status.hooks} />
          </CollapsibleSection>
        </div>
      </ScrollArea>

      {/* Footer */}
      {status.lastUpdated > 0 && (
        <div className="px-4 py-2 border-t border-border/50 bg-muted/10 flex-shrink-0">
          <span className="text-[10px] text-muted-foreground">
            Last updated: {new Date(status.lastUpdated).toLocaleTimeString()}
          </span>
        </div>
      )}
    </Card>
  );
}
