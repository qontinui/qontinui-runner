/**
 * ShellCommandWidget Component
 *
 * Full widget view for shell command execution activity.
 * Displays command list, output, exit codes, and execution stats.
 */

import { useState } from "react";
import { Terminal, FolderOpen, Hash } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea } from "../../../ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { ShellCommandWidgetProps } from "./types";
import type { ShellCommandExecution } from "../shared/types";

/**
 * Inline command detail panel that drops down below the command.
 */
function InlineCommandDetail({ command }: { command: ShellCommandExecution }) {
  return (
    <div className="border-l-2 border-primary bg-muted/30 ml-4 mr-4 mb-2 rounded-b overflow-hidden">
      {/* Command header */}
      <div className="px-4 py-2 border-b border-border/50 bg-muted/20">
        <div className="flex items-center gap-2">
          <Terminal className="h-3.5 w-3.5 text-muted-foreground" />
          <code className="font-mono text-xs text-foreground break-all">{command.command}</code>
        </div>
        {command.workingDirectory && (
          <div className="flex items-center gap-1 mt-1">
            <FolderOpen className="h-3 w-3 text-muted-foreground" />
            <span className="text-xs text-muted-foreground">{command.workingDirectory}</span>
          </div>
        )}
      </div>

      {/* Output panels */}
      <div className="p-3 space-y-2 max-h-[300px] overflow-y-auto">
        {command.stdout && (
          <StepOutputPanel
            title="Standard Output"
            content={command.stdout}
            contentType="text"
            accentColor="slate"
            showLineNumbers
            maxHeight="max-h-[200px]"
          />
        )}
        {command.stderr && (
          <StepOutputPanel
            title="Standard Error"
            content={command.stderr}
            contentType="error"
            accentColor="red"
            showLineNumbers
            maxHeight="max-h-[150px]"
          />
        )}
        {!command.stdout && !command.stderr && command.status !== "running" && (
          <div className="text-center text-muted-foreground text-xs py-4">No output captured</div>
        )}
        {command.status === "running" && (
          <div className="text-center text-muted-foreground text-xs py-4">Command is running...</div>
        )}
      </div>
    </div>
  );
}

/**
 * Command row component showing command details.
 * Clicking toggles the inline detail view below.
 */
function CommandRow({
  command,
  isExpanded,
  onToggle,
}: {
  command: ShellCommandExecution;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const slateColors = getAccentColors("slate");
  const errorColors = getStatusColors("error");
  const isActive = command.status === "running";
  const pendingColors = getStatusColors("pending");

  return (
    <div className="flex flex-col">
      <div
        className={cn(
          "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors cursor-pointer",
          isActive
            ? cn(pendingColors.border, pendingColors.bg)
            : isExpanded
              ? "border-primary bg-muted/50"
              : "border-transparent hover:bg-muted/30",
        )}
        onClick={onToggle}
      >
        {/* Status icon */}
        <StepStatusBadge status={command.status} iconOnly size="md" />

        {/* Command badge */}
        <Badge
          className={cn(
            "text-xs flex-shrink-0 font-mono border",
            slateColors.bg,
            slateColors.text,
            slateColors.border,
          )}
        >
          <Terminal className="h-3 w-3 mr-1" />
          CMD
        </Badge>

        {/* Command content */}
        <div className="flex-1 min-w-0">
          <p className="font-mono text-sm text-foreground truncate">{command.command}</p>
          {command.workingDirectory && (
            <div className="flex items-center gap-1 mt-0.5">
              <FolderOpen className="h-3 w-3 text-muted-foreground" />
              <span className="text-xs text-muted-foreground truncate">
                {command.workingDirectory}
              </span>
            </div>
          )}
          {command.error && (
            <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{command.error}</p>
          )}
        </div>

        {/* Exit code and duration */}
        <div className="flex items-center gap-2 flex-shrink-0">
          {command.exitCode !== undefined && (
            <Badge variant={command.exitCode === 0 ? "success" : "danger"} className="text-[10px]">
              <Hash className="h-2.5 w-2.5 mr-0.5" />
              {command.exitCode}
            </Badge>
          )}
          {command.durationMs !== undefined && (
            <span className="font-mono text-xs text-muted-foreground">
              {command.durationMs < 1000
                ? `${command.durationMs}ms`
                : `${(command.durationMs / 1000).toFixed(1)}s`}
            </span>
          )}
        </div>
      </div>

      {/* Inline detail panel - shown when expanded */}
      {isExpanded && <InlineCommandDetail command={command} />}
    </div>
  );
}

/**
 * Full Shell Command widget component.
 */
export function ShellCommandWidget({ isSummary, data, className }: ShellCommandWidgetProps) {
  const [expandedCommandId, setExpandedCommandId] = useState<string | null>(null);

  // If summary mode, don't render (use ShellCommandSummary instead)
  if (isSummary) {
    return null;
  }

  // Toggle expansion - clicking same command collapses it
  const handleToggle = (commandId: string) => {
    setExpandedCommandId((prev) => (prev === commandId ? null : commandId));
  };

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar */}
      <StepStatsBar stats={data.stats} />

      {/* Command list header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
        <h3 className="text-sm font-semibold text-foreground">Commands</h3>
        <Badge variant="muted" className="text-xs">
          {data.commands.length}
        </Badge>
      </div>

      {/* Command list - full width with inline details */}
      <ScrollArea className="flex-1">
        <div className="flex flex-col-reverse">
          {data.commands.slice(0, 50).map((command) => (
            <CommandRow
              key={command.id}
              command={command}
              isExpanded={command.id === expandedCommandId}
              onToggle={() => handleToggle(command.id)}
            />
          ))}
        </div>
        {data.commands.length === 0 && (
          <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
            No commands executed yet
          </div>
        )}
      </ScrollArea>
    </div>
  );
}

export default ShellCommandWidget;
