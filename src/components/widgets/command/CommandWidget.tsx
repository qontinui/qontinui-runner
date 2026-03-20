/**
 * CommandWidget Component
 *
 * Full widget view for unified command execution.
 * Supports mode filter tabs (All/Shell/Check/Test) and expandable command details.
 */

import { useState, useMemo } from "react";
import {
  Terminal,
  FolderOpen,
  Hash,
  ChevronRight,
  ChevronDown,
  Variable,
  FileCode,
  CheckSquare,
  FlaskConical,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge, ScrollArea } from "@/components/ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel, EmptyState } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { CommandWidgetProps, CommandExecution, CommandMode } from "./types";

function extractBaseCommand(command: string): string {
  const trimmed = command.trim();
  const match = trimmed.match(/^(?:\.\/|\.\\|[a-zA-Z]:\\)?([^\s|;&]+)/);
  if (match) {
    const parts = match[1].split(/[/\\]/);
    return parts[parts.length - 1];
  }
  return trimmed.split(/\s+/)[0] || trimmed;
}

function extractParameters(command: string): string[] {
  const flagMatches = command.match(/(?:^|\s)(--?[a-zA-Z][\w-]*|\/[a-zA-Z][\w]*)/g);
  if (flagMatches) {
    const uniqueFlags = [...new Set(flagMatches.map((f) => f.trim()))];
    return uniqueFlags.slice(0, 4);
  }
  return [];
}

function hasTitle(command: CommandExecution): boolean {
  if (!command.name) return false;
  const baseCmd = extractBaseCommand(command.command);
  return (
    command.name !== command.command &&
    command.name.toLowerCase() !== baseCmd.toLowerCase() &&
    command.name.length > 0
  );
}

function hasVariables(command: CommandExecution): boolean {
  return Boolean(
    command.templateCommand &&
    command.resolvedVariables &&
    Object.keys(command.resolvedVariables).length > 0,
  );
}

/** Icon and label for each command mode. */
function getModeDisplay(mode?: CommandMode) {
  switch (mode) {
    case "shell":
      return { icon: Terminal, label: "Shell", color: "slate" };
    case "check":
    case "check_group":
      return { icon: CheckSquare, label: "Check", color: "blue" };
    case "test":
      return { icon: FlaskConical, label: "Test", color: "indigo" };
    default:
      return { icon: Terminal, label: "CMD", color: "slate" };
  }
}

function InlineCommandDetail({ command }: { command: CommandExecution }) {
  const hasVars = hasVariables(command);
  const purpleColors = getAccentColors("purple");

  return (
    <div className="border-l-2 border-primary bg-muted/30 ml-4 mr-4 mb-2 rounded-b overflow-hidden">
      <div className="px-4 py-2 border-b border-border/50 bg-muted/20">
        {hasVars && command.templateCommand && (
          <div className="mb-2 pb-2 border-b border-border/30">
            <div className="flex items-center gap-2 mb-1">
              <FileCode className="h-3 w-3 text-muted-foreground flex-shrink-0" />
              <span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
                Template
              </span>
            </div>
            <code className="font-mono text-xs text-muted-foreground break-all whitespace-pre-wrap">
              {command.templateCommand}
            </code>
          </div>
        )}

        <div className="flex items-start gap-2">
          <Terminal className="h-3.5 w-3.5 text-muted-foreground flex-shrink-0 mt-0.5" />
          <div className="flex-1">
            {hasVars && (
              <span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium mr-2">
                Resolved
              </span>
            )}
            <code className="font-mono text-xs text-foreground break-all whitespace-pre-wrap">
              {command.command}
            </code>
          </div>
        </div>
        {command.workingDirectory && (
          <div className="flex items-start gap-1 mt-1">
            <FolderOpen className="h-3 w-3 text-muted-foreground flex-shrink-0 mt-0.5" />
            <span className="text-xs text-muted-foreground break-all">
              {command.workingDirectory}
            </span>
          </div>
        )}
      </div>

      {hasVars && command.resolvedVariables && (
        <div className="px-4 py-2 border-b border-border/50 bg-muted/10">
          <div className="flex items-center gap-2 mb-2">
            <Variable className="h-3.5 w-3.5 text-purple-500" />
            <span className="text-xs font-medium text-foreground">Resolved Variables</span>
            <Badge variant="muted" className="text-[10px] px-1.5 py-0">
              {Object.keys(command.resolvedVariables).length}
            </Badge>
          </div>
          <div className="space-y-1">
            {Object.entries(command.resolvedVariables).map(([name, value]) => (
              <div key={name} className="flex items-start gap-2">
                <Badge
                  className={cn(
                    "text-[10px] px-1.5 py-0 font-mono flex-shrink-0 border",
                    purpleColors.bg,
                    purpleColors.text,
                    purpleColors.border,
                  )}
                >
                  {`{{${name}}}`}
                </Badge>
                <span className="text-xs text-muted-foreground">=</span>
                <code className="font-mono text-xs text-foreground break-all">{value}</code>
              </div>
            ))}
          </div>
        </div>
      )}

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
          <div className="text-center text-muted-foreground text-xs py-4">
            Command is running...
          </div>
        )}
      </div>
    </div>
  );
}

function CommandRow({
  command,
  isExpanded,
  onToggle,
}: {
  command: CommandExecution;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const modeDisplay = getModeDisplay(command.mode);
  const modeColors = getAccentColors(modeDisplay.color);
  const purpleColors = getAccentColors("purple");
  const errorColors = getStatusColors("error");
  const isActive = command.status === "running";
  const pendingColors = getStatusColors("pending");

  const showTitle = hasTitle(command);
  const baseCommand = extractBaseCommand(command.command);
  const parameters = extractParameters(command.command);
  const hasVars = hasVariables(command);

  const ModeIcon = modeDisplay.icon;

  const panelId = `command-output-${command.id}`;

  return (
    <div className="flex flex-col">
      <div
        className={cn(
          "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors cursor-pointer",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
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
        <div className="flex-shrink-0 mt-0.5 text-muted-foreground">
          {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </div>

        <StepStatusBadge status={command.status} iconOnly size="md" />

        <Badge
          className={cn(
            "text-xs flex-shrink-0 font-mono border",
            modeColors.bg,
            modeColors.text,
            modeColors.border,
          )}
        >
          <ModeIcon className="h-3 w-3 mr-1" />
          {baseCommand}
        </Badge>

        {hasVars && command.resolvedVariables && (
          <Badge
            className={cn(
              "text-[10px] flex-shrink-0 border",
              purpleColors.bg,
              purpleColors.text,
              purpleColors.border,
            )}
            title={`${Object.keys(command.resolvedVariables).length} variable(s) resolved`}
          >
            <Variable className="h-2.5 w-2.5 mr-0.5" />
            {Object.keys(command.resolvedVariables).length}
          </Badge>
        )}

        <div className="flex-1 min-w-0">
          {showTitle ? (
            <p className="text-sm font-medium text-foreground">{command.name}</p>
          ) : (
            <p className="font-mono text-sm text-foreground truncate">{command.command}</p>
          )}

          {parameters.length > 0 && (
            <div className="flex flex-wrap gap-1 mt-1">
              {parameters.map((param, idx) => (
                <Badge key={`${param}-${idx}`} variant="muted" className="text-[10px] font-mono px-1.5 py-0">
                  {param}
                </Badge>
              ))}
            </div>
          )}

          {command.workingDirectory && (
            <div className="flex items-center gap-1 mt-1">
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

      {isExpanded && (
        <div id={panelId}>
          <InlineCommandDetail command={command} />
        </div>
      )}
    </div>
  );
}

type ModeFilter = "all" | CommandMode;

export function CommandWidget({ isSummary, data, className }: CommandWidgetProps) {
  const [expandedCommandId, setExpandedCommandId] = useState<string | null>(null);
  const [modeFilter, setModeFilter] = useState<ModeFilter>("all");

  const filteredCommands = useMemo(() => {
    if (modeFilter === "all") return data.commands;
    return data.commands.filter((c) => c.mode === modeFilter);
  }, [data.commands, modeFilter]);

  // Determine which mode tabs to show (only if multiple modes exist)
  const availableModes = useMemo(() => {
    const modes = new Set(data.commands.map((c) => c.mode).filter(Boolean));
    return modes.size > 1 ? (Array.from(modes) as CommandMode[]) : [];
  }, [data.commands]);

  if (isSummary) return null;

  const handleToggle = (commandId: string) => {
    setExpandedCommandId((prev) => (prev === commandId ? null : commandId));
  };

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      <StepStatsBar stats={data.stats} />

      {/* Mode filter tabs - only shown when multiple modes exist */}
      {availableModes.length > 0 && (
        <div
          className="flex items-center gap-1 border-b border-border px-4 py-1.5 bg-muted/10 flex-shrink-0"
          role="tablist"
          aria-label="Command mode filter"
        >
          <button
            onClick={() => setModeFilter("all")}
            className={cn(
              "px-2 py-0.5 text-xs rounded transition-colors",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
              modeFilter === "all"
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-muted",
            )}
            role="tab"
            aria-selected={modeFilter === "all"}
          >
            All ({data.commands.length})
          </button>
          {availableModes.map((mode) => {
            const display = getModeDisplay(mode);
            const count = data.statsByMode[mode]?.total || 0;
            return (
              <button
                key={mode}
                onClick={() => setModeFilter(mode)}
                className={cn(
                  "px-2 py-0.5 text-xs rounded transition-colors",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-1 focus-visible:ring-offset-background",
                  modeFilter === mode
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:text-foreground hover:bg-muted",
                )}
                role="tab"
                aria-selected={modeFilter === mode}
              >
                {display.label} ({count})
              </button>
            );
          })}
        </div>
      )}

      <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
        <div className="flex items-center gap-3">
          <h3 className="text-sm font-semibold text-foreground">Commands</h3>
          <span className="text-xs text-muted-foreground">Click to view details</span>
        </div>
        <Badge variant="muted" className="text-xs">
          {filteredCommands.length}
        </Badge>
      </div>

      <ScrollArea className="flex-1">
        <div className="flex flex-col-reverse">
          {filteredCommands.slice(0, 50).map((command) => (
            <CommandRow
              key={command.id}
              command={command}
              isExpanded={command.id === expandedCommandId}
              onToggle={() => handleToggle(command.id)}
            />
          ))}
        </div>
        {filteredCommands.length === 0 && (
          <EmptyState
            icon={Terminal}
            title="No commands executed yet"
            description="Commands will appear here as they run"
          />
        )}
      </ScrollArea>
    </div>
  );
}

export default CommandWidget;
