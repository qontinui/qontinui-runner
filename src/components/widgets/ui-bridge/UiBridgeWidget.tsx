/**
 * UiBridgeWidget Component
 *
 * Full widget view for UI Bridge step execution.
 * Shows action list with type badges and expandable details.
 */

import { useState } from "react";
import {
  Monitor,
  Navigation,
  Play,
  CheckCircle2,
  Camera,
  ChevronRight,
  ChevronDown,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge, ScrollArea } from "@/components/ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel, EmptyState } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { UiBridgeWidgetProps, UiBridgeExecution, UiBridgeActionType } from "./types";

function getActionDisplay(actionType: UiBridgeActionType) {
  switch (actionType) {
    case "navigate":
      return { icon: Navigation, label: "Navigate", color: "blue" };
    case "execute":
      return { icon: Play, label: "Execute", color: "emerald" };
    case "assert":
      return { icon: CheckCircle2, label: "Assert", color: "teal" };
    case "snapshot":
      return { icon: Camera, label: "Snapshot", color: "purple" };
    default:
      return { icon: Monitor, label: "Action", color: "slate" };
  }
}

function InlineDetail({ execution }: { execution: UiBridgeExecution }) {
  return (
    <div className="border-l-2 border-primary bg-muted/30 ml-4 mr-4 mb-2 rounded-b overflow-hidden">
      {execution.command && (
        <div className="px-4 py-2 border-b border-border/50 bg-muted/20">
          <div className="flex items-start gap-2">
            <Monitor className="h-3.5 w-3.5 text-muted-foreground shrink-0 mt-0.5" />
            <code className="font-mono text-xs text-foreground break-all whitespace-pre-wrap">
              {execution.command}
            </code>
          </div>
        </div>
      )}

      <div className="p-3 space-y-2 max-h-[300px] overflow-y-auto">
        {execution.output && (
          <StepOutputPanel
            title="Output"
            content={execution.output}
            contentType="text"
            accentColor="emerald"
            maxHeight="max-h-[200px]"
          />
        )}
        {execution.error && (
          <StepOutputPanel
            title="Error"
            content={execution.error}
            contentType="error"
            accentColor="red"
            maxHeight="max-h-[150px]"
          />
        )}
        {!execution.output && !execution.error && execution.status !== "running" && (
          <div className="text-center text-muted-foreground text-xs py-4">No output captured</div>
        )}
        {execution.status === "running" && (
          <div className="text-center text-muted-foreground text-xs py-4">Action is running...</div>
        )}
      </div>
    </div>
  );
}

function ActionRow({
  execution,
  isExpanded,
  onToggle,
}: {
  execution: UiBridgeExecution;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const actionDisplay = getActionDisplay(execution.actionType);
  const actionColors = getAccentColors(actionDisplay.color);
  const errorColors = getStatusColors("error");
  const isActive = execution.status === "running";
  const pendingColors = getStatusColors("pending");

  const ActionIcon = actionDisplay.icon;
  const panelId = `ui-bridge-output-${execution.id}`;

  return (
    <div className="flex flex-col">
      <div
        className={cn(
          "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors cursor-pointer",
          "focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
          isActive
            ? cn(pendingColors.border, pendingColors.bg)
            : isExpanded
              ? "border-primary bg-muted/50"
              : "border-transparent hover:bg-muted/30",
        )}
        onClick={onToggle}
        role="button"
        tabIndex={0}
        aria-expanded={isExpanded}
        aria-controls={panelId}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggle();
          }
        }}
      >
        <div className="shrink-0 mt-0.5 text-muted-foreground">
          {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </div>

        <StepStatusBadge status={execution.status} iconOnly size="md" />

        <Badge
          className={cn(
            "text-xs shrink-0 border",
            actionColors.bg,
            actionColors.text,
            actionColors.border,
          )}
        >
          <ActionIcon className="h-3 w-3 mr-1" />
          {actionDisplay.label}
        </Badge>

        <div className="flex-1 min-w-0">
          <p className="text-sm text-foreground truncate">{execution.name}</p>
          {execution.target && (
            <p className="text-xs text-muted-foreground truncate mt-0.5 font-mono">
              {execution.target}
            </p>
          )}
          {execution.error && (
            <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{execution.error}</p>
          )}
        </div>

        {execution.durationMs !== undefined && (
          <span className="font-mono text-xs text-muted-foreground shrink-0">
            {execution.durationMs < 1000
              ? `${execution.durationMs}ms`
              : `${(execution.durationMs / 1000).toFixed(1)}s`}
          </span>
        )}
      </div>

      {isExpanded && (
        <div id={panelId}>
          <InlineDetail execution={execution} />
        </div>
      )}
    </div>
  );
}

export function UiBridgeWidget({ isSummary, data, className }: UiBridgeWidgetProps) {
  const [expandedId, setExpandedId] = useState<string | null>(null);

  if (isSummary) return null;

  const handleToggle = (id: string) => {
    setExpandedId((prev) => (prev === id ? null : id));
  };

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      <StepStatsBar stats={data.stats} />

      {/* Assertion stats bar if there are assertions */}
      {data.assertionStats.total > 0 && (
        <div className="flex items-center gap-3 border-b border-border px-4 py-1.5 bg-muted/10 shrink-0">
          <span className="text-xs text-muted-foreground">Assertions:</span>
          <Badge variant="success" className="text-[10px]">
            {data.assertionStats.passed} passed
          </Badge>
          {data.assertionStats.failed > 0 && (
            <Badge variant="danger" className="text-[10px]">
              {data.assertionStats.failed} failed
            </Badge>
          )}
        </div>
      )}

      <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 shrink-0">
        <div className="flex items-center gap-3">
          <h3 className="text-sm font-semibold text-foreground">UI Bridge Actions</h3>
          <span className="text-xs text-muted-foreground">Click to view details</span>
        </div>
        <Badge variant="muted" className="text-xs">
          {data.executions.length}
        </Badge>
      </div>

      <ScrollArea className="flex-1">
        <div className="flex flex-col-reverse">
          {data.executions.slice(0, 50).map((execution) => (
            <ActionRow
              key={execution.id}
              execution={execution}
              isExpanded={execution.id === expandedId}
              onToggle={() => handleToggle(execution.id)}
            />
          ))}
        </div>
        {data.executions.length === 0 && (
          <EmptyState
            icon={Monitor}
            title="No UI Bridge actions yet"
            description="Actions will appear here as they execute"
          />
        )}
      </ScrollArea>
    </div>
  );
}

export default UiBridgeWidget;
