/**
 * ShellCommandWidget Component
 *
 * Full widget view for shell command execution activity.
 * Displays command list, output, exit codes, and execution stats.
 */

import { useState } from "react";
import {
  Terminal,
  FolderOpen,
  Hash,
  ChevronRight,
  ChevronDown,
  Variable,
  FileCode,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge, ScrollArea } from "@/components/ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { ShellCommandWidgetProps } from "./types";
import type { ShellCommandExecution } from "../shared/types";

/**
 * Extract the base command (first word/executable) from a command string.
 */
function extractBaseCommand(command: string): string {
  const trimmed = command.trim();
  // Handle common patterns: executable, path/to/executable, ./script.sh, etc.
  const match = trimmed.match(/^(?:\.\/|\.\\|[a-zA-Z]:\\)?([^\s|;&]+)/);
  if (match) {
    // Get just the filename without path
    const fullPath = match[1];
    const parts = fullPath.split(/[/\\]/);
    return parts[parts.length - 1];
  }
  return trimmed.split(/\s+/)[0] || trimmed;
}

/**
 * Extract key parameters/flags from a command string.
 * Returns an array of notable parameters.
 */
function extractParameters(command: string): string[] {
  const params: string[] = [];

  // Match flags like -f, --flag, /flag (Windows)
  const flagMatches = command.match(/(?:^|\s)(--?[a-zA-Z][\w-]*|\/[a-zA-Z][\w]*)/g);
  if (flagMatches) {
    // Take first few unique flags
    const uniqueFlags = [...new Set(flagMatches.map((f) => f.trim()))];
    params.push(...uniqueFlags.slice(0, 4));
  }

  return params;
}

/**
 * Check if the name is a meaningful title (not just the command itself).
 */
function hasTitle(command: ShellCommandExecution): boolean {
  if (!command.name) return false;
  // Name is meaningful if it's different from the command and not just the base command
  const baseCmd = extractBaseCommand(command.command);
  return (
    command.name !== command.command &&
    command.name.toLowerCase() !== baseCmd.toLowerCase() &&
    command.name.length > 0
  );
}

/**
 * Check if command used variable expansion (has template and resolved variables).
 */
function hasVariables(command: ShellCommandExecution): boolean {
  return Boolean(
    command.templateCommand &&
    command.resolvedVariables &&
    Object.keys(command.resolvedVariables).length > 0,
  );
}

/**
 * Inline command detail panel that drops down below the command.
 */
function InlineCommandDetail({ command }: { command: ShellCommandExecution }) {
  const hasVars = hasVariables(command);
  const purpleColors = getAccentColors("purple");

  return (
    <div className="border-l-2 border-primary bg-muted/30 ml-4 mr-4 mb-2 rounded-b overflow-hidden">
      {/* Command header */}
      <div className="px-4 py-2 border-b border-border/50 bg-muted/20">
        {/* Show template command if variables were used */}
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

        {/* Resolved command (or original if no variables) */}
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

      {/* Resolved Variables Section */}
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
          <div className="text-center text-muted-foreground text-xs py-4">
            Command is running...
          </div>
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
  const purpleColors = getAccentColors("purple");
  const errorColors = getStatusColors("error");
  const isActive = command.status === "running";
  const pendingColors = getStatusColors("pending");

  const showTitle = hasTitle(command);
  const baseCommand = extractBaseCommand(command.command);
  const parameters = extractParameters(command.command);
  const hasVars = hasVariables(command);

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
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggle();
          }
        }}
        role="button"
        tabIndex={0}
      >
        {/* Expand/collapse indicator */}
        <div className="flex-shrink-0 mt-0.5 text-muted-foreground">
          {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </div>

        {/* Status icon */}
        <StepStatusBadge status={command.status} iconOnly size="md" />

        {/* Command badge showing base command */}
        <Badge
          className={cn(
            "text-xs flex-shrink-0 font-mono border",
            slateColors.bg,
            slateColors.text,
            slateColors.border,
          )}
        >
          <Terminal className="h-3 w-3 mr-1" />
          {baseCommand}
        </Badge>

        {/* Variables indicator */}
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

        {/* Command content */}
        <div className="flex-1 min-w-0">
          {/* Title or truncated command */}
          {showTitle ? (
            <p className="text-sm font-medium text-foreground">{command.name}</p>
          ) : (
            <p className="font-mono text-sm text-foreground truncate">{command.command}</p>
          )}

          {/* Parameters */}
          {parameters.length > 0 && (
            <div className="flex flex-wrap gap-1 mt-1">
              {parameters.map((param, idx) => (
                <Badge
                  key={`${param}-${idx}`}
                  variant="muted"
                  className="text-[10px] font-mono px-1.5 py-0"
                >
                  {param}
                </Badge>
              ))}
            </div>
          )}

          {/* Working directory */}
          {command.workingDirectory && (
            <div className="flex items-center gap-1 mt-1">
              <FolderOpen className="h-3 w-3 text-muted-foreground" />
              <span className="text-xs text-muted-foreground truncate">
                {command.workingDirectory}
              </span>
            </div>
          )}

          {/* Error */}
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
        <div className="flex items-center gap-3">
          <h3 className="text-sm font-semibold text-foreground">Commands</h3>
          <span className="text-xs text-muted-foreground">Click to view full command</span>
        </div>
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
