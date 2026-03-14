/**
 * CommandSummary Component
 *
 * Compact summary view for the unified command widget.
 * Shows quick stats with mode breakdown and the last few commands.
 */

import { Terminal, Loader2, Hash, CheckSquare, FlaskConical } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { ModeBadge } from "../shared/ModeBadge";
import { getStatusColors, getAccentColors } from "@/design-system";
import type { CommandSummaryProps, CommandExecution, CommandMode } from "./types";

function getModeDisplay(mode?: CommandMode) {
  switch (mode) {
    case "shell":
      return { icon: Terminal, label: "SH", color: "slate" };
    case "check":
    case "check_group":
      return { icon: CheckSquare, label: "CHK", color: "blue" };
    case "test":
      return { icon: FlaskConical, label: "TST", color: "indigo" };
    default:
      return { icon: Terminal, label: "CMD", color: "slate" };
  }
}

function CompactCommandRow({ command }: { command: CommandExecution }) {
  const modeDisplay = getModeDisplay(command.mode);
  const modeColors = getAccentColors(modeDisplay.color);
  const ModeIcon = modeDisplay.icon;

  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={command.status} iconOnly size="sm" />
      <Badge
        className={cn(
          "text-[10px] px-1.5 py-0 font-mono border",
          modeColors.bg,
          modeColors.text,
          modeColors.border,
        )}
      >
        <ModeIcon className="h-2.5 w-2.5 mr-0.5" />
        {modeDisplay.label}
      </Badge>
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

export function CommandSummary({ status, data }: CommandSummaryProps) {
  const { commands, currentCommand, stats, statsByMode } = data;
  const recentCommands = commands.slice(0, 3);
  const pendingColors = getStatusColors("pending");
  const errorColors = getStatusColors("error");

  // Compute mode counts for breakdown badges
  const shellCount = statsByMode.shell?.total ?? 0;
  const checkCount = (statsByMode.check?.total ?? 0) + (statsByMode.check_group?.total ?? 0);
  const testCount = statsByMode.test?.total ?? 0;

  return (
    <div className="space-y-3">
      <StepStatsBar stats={stats} compact />

      {/* Mode breakdown badges + fail count */}
      {(shellCount > 0 || checkCount > 0 || testCount > 0 || stats.failed > 0) && (
        <div className="flex items-center gap-1.5 flex-wrap">
          {shellCount > 0 && <ModeBadge label="SH" color="slate" count={shellCount} />}
          {checkCount > 0 && <ModeBadge label="CHK" color="blue" count={checkCount} />}
          {testCount > 0 && <ModeBadge label="TST" color="purple" count={testCount} />}
          {stats.failed > 0 && (
            <span className={cn("text-xs font-medium ml-auto", errorColors.text)}>
              {stats.failed} failed
            </span>
          )}
        </div>
      )}

      {recentCommands.length > 0 ? (
        <div className="space-y-0.5">
          {recentCommands.map((command) => (
            <CompactCommandRow key={command.id} command={command} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No commands executed yet</p>
      )}

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

export default CommandSummary;
