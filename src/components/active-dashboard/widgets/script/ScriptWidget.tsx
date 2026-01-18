/**
 * ScriptWidget Component
 *
 * Full widget view for script execution activity.
 * Displays script list, code content, output, and exit codes.
 */

import { useState } from "react";
import { FileCode, Hash, FolderOpen } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea } from "../../../ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { ScriptWidgetProps } from "./types";
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
 * Script row component showing script details.
 */
function ScriptRow({
  script,
  isSelected,
  onSelect,
}: {
  script: ScriptExecution;
  isSelected: boolean;
  onSelect: () => void;
}) {
  const errorColors = getStatusColors("error");
  const isActive = script.status === "running";
  const pendingColors = getStatusColors("pending");

  return (
    <div
      className={cn(
        "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors cursor-pointer",
        isActive
          ? cn(pendingColors.border, pendingColors.bg)
          : isSelected
            ? "border-primary bg-muted/50"
            : "border-transparent hover:bg-muted/30",
      )}
      onClick={onSelect}
    >
      {/* Status icon */}
      <StepStatusBadge status={script.status} iconOnly size="md" />

      {/* Language badge */}
      <Badge
        className={cn("text-xs flex-shrink-0 font-mono border", getLanguageColor(script.language))}
      >
        <FileCode className="h-3 w-3 mr-1" />
        {script.language.toUpperCase()}
      </Badge>

      {/* Script content */}
      <div className="flex-1 min-w-0">
        <p className="font-mono text-sm text-foreground truncate">{script.name}</p>
        {script.scriptPath && (
          <div className="flex items-center gap-1 mt-0.5">
            <FolderOpen className="h-3 w-3 text-muted-foreground" />
            <span className="text-xs text-muted-foreground truncate">{script.scriptPath}</span>
          </div>
        )}
        {script.error && (
          <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{script.error}</p>
        )}
      </div>

      {/* Exit code and duration */}
      <div className="flex items-center gap-2 flex-shrink-0">
        {script.exitCode !== undefined && (
          <Badge variant={script.exitCode === 0 ? "success" : "danger"} className="text-[10px]">
            <Hash className="h-2.5 w-2.5 mr-0.5" />
            {script.exitCode}
          </Badge>
        )}
        {script.durationMs !== undefined && (
          <span className="font-mono text-xs text-muted-foreground">
            {script.durationMs < 1000
              ? `${script.durationMs}ms`
              : `${(script.durationMs / 1000).toFixed(1)}s`}
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * Script detail panel showing code and output.
 */
function ScriptDetail({ script }: { script: ScriptExecution | null }) {
  if (!script) {
    return (
      <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
        Select a script to view details
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Script header */}
      <div className="px-4 py-3 border-b border-border bg-muted/10 flex-shrink-0">
        <div className="flex items-center gap-2">
          <Badge className={cn("font-mono border", getLanguageColor(script.language))}>
            <FileCode className="h-3 w-3 mr-1" />
            {script.language.toUpperCase()}
          </Badge>
          <span className="font-mono text-sm text-foreground truncate flex-1">{script.name}</span>
          {script.exitCode !== undefined && (
            <Badge variant={script.exitCode === 0 ? "success" : "danger"}>
              Exit: {script.exitCode}
            </Badge>
          )}
        </div>
        {script.scriptPath && (
          <div className="flex items-center gap-1 mt-1">
            <FolderOpen className="h-3 w-3 text-muted-foreground" />
            <span className="text-xs text-muted-foreground">{script.scriptPath}</span>
          </div>
        )}
      </div>

      {/* Output panels */}
      <ScrollArea className="flex-1 p-3 space-y-3">
        {script.scriptContent && (
          <StepOutputPanel
            title="Script Content"
            content={script.scriptContent}
            accentColor="indigo"
            showLineNumbers
            collapsible
            defaultCollapsed={false}
            maxHeight="max-h-[200px]"
          />
        )}
        {script.stdout && (
          <StepOutputPanel
            title="Standard Output"
            content={script.stdout}
            contentType="text"
            accentColor="slate"
            showLineNumbers
            maxHeight="max-h-[250px]"
          />
        )}
        {script.stderr && (
          <StepOutputPanel
            title="Standard Error"
            content={script.stderr}
            contentType="error"
            accentColor="red"
            showLineNumbers
            maxHeight="max-h-[150px]"
          />
        )}
        {!script.scriptContent &&
          !script.stdout &&
          !script.stderr &&
          script.status !== "running" && (
            <div className="text-center text-muted-foreground text-sm py-8">No output captured</div>
          )}
        {script.status === "running" && (
          <div className="text-center text-muted-foreground text-sm py-8">Script is running...</div>
        )}
      </ScrollArea>
    </div>
  );
}

/**
 * Full Script widget component.
 */
export function ScriptWidget({ isSummary, data, className }: ScriptWidgetProps) {
  const [selectedScriptId, setSelectedScriptId] = useState<string | null>(null);

  if (isSummary) {
    return null;
  }

  const selectedScript = data.scripts.find((s) => s.id === selectedScriptId) || null;

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar */}
      <StepStatsBar stats={data.stats} />

      {/* Main content - split view */}
      <div className="flex-1 flex overflow-hidden">
        {/* Script list */}
        <div className="w-1/2 border-r border-border flex flex-col">
          <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
            <h3 className="text-sm font-semibold text-foreground">Scripts</h3>
            <Badge variant="muted" className="text-xs">
              {data.scripts.length}
            </Badge>
          </div>
          <ScrollArea className="flex-1">
            <div className="flex flex-col-reverse">
              {data.scripts.slice(0, 50).map((script) => (
                <ScriptRow
                  key={script.id}
                  script={script}
                  isSelected={script.id === selectedScriptId}
                  onSelect={() => setSelectedScriptId(script.id)}
                />
              ))}
            </div>
            {data.scripts.length === 0 && (
              <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
                No scripts executed yet
              </div>
            )}
          </ScrollArea>
        </div>

        {/* Script detail */}
        <div className="w-1/2 flex flex-col">
          <ScriptDetail script={selectedScript} />
        </div>
      </div>
    </div>
  );
}

export default ScriptWidget;
