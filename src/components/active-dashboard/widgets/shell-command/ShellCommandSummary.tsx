/**
 * ShellCommandSummary Component
 *
 * Compact summary view for shell command widget.
 * Shows quick stats and the last few commands with status.
 */

import { Terminal, Loader2, Hash, Variable } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge } from "../../../ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getStatusColors, getAccentColors } from "@/design-system";
import type { ShellCommandSummaryProps } from "./types";
import type { ShellCommandExecution } from "../shared/types";

/**
 * Check if command used variable expansion.
 */
function hasVariables(command: ShellCommandExecution): boolean {
  return Boolean(command.templateCommand && command.resolvedVariables && Object.keys(command.resolvedVariables).length > 0);
}

/**
 * Compact command row for summary view.
 */
function CompactCommandRow({ command }: { command: ShellCommandExecution }) {
  const slateColors = getAccentColors("slate");
  const purpleColors = getAccentColors("purple");
  const hasVars = hasVariables(command);

  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={command.status} iconOnly size="sm" />
      <Badge
        className={cn(
          "text-[10px] px-1.5 py-0 font-mono border",
          slateColors.bg,
          slateColors.text,
          slateColors.border,
        )}
      >
        <Terminal className="h-2.5 w-2.5 mr-0.5" />
        CMD
      </Badge>
      {/* Variables indicator */}
      {hasVars && command.resolvedVariables && (
        <Badge
          className={cn(
            "text-[9px] px-1 py-0 border",
            purpleColors.bg,
            purpleColors.text,
            purpleColors.border,
          )}
          title={`${Object.keys(command.resolvedVariables).length} variable(s)`}
        >
          <Variable className="h-2 w-2" />
        </Badge>
      )}
      <span className="text-xs text-muted-foreground truncate flex-1 font-mono">
        {command.command}
      </span>
      {command.exitCode !== undefined && (
        <Badge variant={command.exitCode === 0 ? "success" : "danger"} className="text-[10px] px-1">
          <Hash className="h-2 w-2 mr-0.5" />
          {command.exitCode}
        </Badge>
      )}
    </div>
  );
}

/**
 * Compact summary widget for shell commands.
 */
export function ShellCommandSummary({ status, data }: ShellCommandSummaryProps) {
  const { commands, currentCommand, stats } = data;
  const recentCommands = commands.slice(0, 3);
  const pendingColors = getStatusColors("pending");

  return (
    <div className="space-y-3">
      {/* Quick Stats */}
      <StepStatsBar stats={stats} compact />

      {/* Recent Commands */}
      {recentCommands.length > 0 ? (
        <div className="space-y-0.5">
          {recentCommands.map((command) => (
            <CompactCommandRow key={command.id} command={command} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No commands executed yet</p>
      )}

      {/* Running Indicator */}
      {status === "running" && currentCommand && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate font-mono", pendingColors.text)}>
            {currentCommand.command}
          </span>
        </div>
      )}
    </div>
  );
}

export default ShellCommandSummary;
