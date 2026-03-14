/**
 * PlaywrightTestSummary Component
 *
 * Compact summary view for playwright test widget.
 * Shows quick stats and the last few scripts with status.
 */

import { FileCode, Loader2, Hash } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getStatusColors } from "@/design-system";
import type { PlaywrightTestSummaryProps } from "./types";
import type { ScriptExecution } from "../shared/types";

/**
 * Get color for script language.
 */
function getLanguageColor(language: string): string {
  const colors: Record<string, string> = {
    python: "bg-blue-500/10 text-blue-500 border-blue-500/30",
    py: "bg-blue-500/10 text-blue-500 border-blue-500/30",
    javascript: "bg-yellow-500/10 text-yellow-500 border-yellow-500/30",
    js: "bg-yellow-500/10 text-yellow-500 border-yellow-500/30",
    typescript: "bg-blue-600/10 text-blue-600 border-blue-600/30",
    ts: "bg-blue-600/10 text-blue-600 border-blue-600/30",
    bash: "bg-green-500/10 text-green-500 border-green-500/30",
    sh: "bg-green-500/10 text-green-500 border-green-500/30",
    powershell: "bg-indigo-500/10 text-indigo-500 border-indigo-500/30",
    ps1: "bg-indigo-500/10 text-indigo-500 border-indigo-500/30",
  };
  return colors[language.toLowerCase()] || "bg-slate-500/10 text-slate-500 border-slate-500/30";
}

/**
 * Compact script row for summary view.
 */
function CompactScriptRow({ script }: { script: ScriptExecution }) {
  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={script.status} iconOnly size="sm" />
      <Badge
        className={cn(
          "text-[10px] px-1.5 py-0 font-mono border",
          getLanguageColor(script.language),
        )}
      >
        <FileCode className="h-2.5 w-2.5 mr-0.5" />
        {script.language.toUpperCase()}
      </Badge>
      <span className="text-xs text-muted-foreground truncate flex-1">{script.name}</span>
      {script.exitCode !== undefined && (
        <Badge variant={script.exitCode === 0 ? "success" : "danger"} className="text-[10px] px-1">
          <Hash className="h-2 w-2 mr-0.5" />
          {script.exitCode}
        </Badge>
      )}
    </div>
  );
}

/**
 * Compact summary widget for scripts.
 */
export function PlaywrightTestSummary({ status, data }: PlaywrightTestSummaryProps) {
  const { scripts, currentScript, stats } = data;
  const recentScripts = scripts.slice(0, 3);
  const pendingColors = getStatusColors("pending");

  return (
    <div className="space-y-3">
      {/* Quick Stats */}
      <StepStatsBar stats={stats} compact />

      {/* Recent Scripts */}
      {recentScripts.length > 0 ? (
        <div className="space-y-0.5">
          {recentScripts.map((script) => (
            <CompactScriptRow key={script.id} script={script} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No scripts executed yet</p>
      )}

      {/* Running Indicator */}
      {status === "running" && currentScript && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate", pendingColors.text)}>{currentScript.name}</span>
        </div>
      )}
    </div>
  );
}

export default PlaywrightTestSummary;
