/**
 * McpCallWidget Component
 *
 * Full widget view for MCP call execution activity.
 * Displays call list, arguments, results, and error details.
 */

import { useState } from "react";
import { Plug, ArrowRight, ArrowLeft } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea, Tabs, TabsContent, TabsList, TabsTrigger } from "../../../ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel } from "../shared";
import { getStatusColors } from "@/design-system";
import type { McpCallWidgetProps, McpCallExecution } from "./types";

/**
 * Call row component showing call details.
 */
function CallRow({
  call,
  isSelected,
  onSelect,
}: {
  call: McpCallExecution;
  isSelected: boolean;
  onSelect: () => void;
}) {
  const errorColors = getStatusColors("error");
  const isActive = call.status === "running";
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
      <StepStatusBadge status={call.status} iconOnly size="md" />

      {/* Tool name badge */}
      <Badge className="text-xs flex-shrink-0 font-mono bg-indigo-500/10 text-indigo-500 border border-indigo-500/30">
        {call.toolName}
      </Badge>

      {/* Server and details */}
      <div className="flex-1 min-w-0">
        <p className="text-sm text-foreground truncate">{call.serverName || call.serverId}</p>
        {call.error && (
          <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{call.error}</p>
        )}
      </div>

      {/* Duration and error indicator */}
      <div className="flex items-center gap-2 flex-shrink-0">
        {call.isError && (
          <Badge variant="danger" className="text-[10px]">
            Error
          </Badge>
        )}
        {call.durationMs !== undefined && (
          <span className="font-mono text-xs text-muted-foreground">
            {call.durationMs < 1000
              ? `${call.durationMs}ms`
              : `${(call.durationMs / 1000).toFixed(1)}s`}
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * Call detail panel showing arguments and results.
 */
function CallDetail({ call }: { call: McpCallExecution | null }) {
  if (!call) {
    return (
      <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
        Select a call to view details
      </div>
    );
  }

  const formatArguments = (args?: Record<string, unknown>): string => {
    if (!args) return "";
    return JSON.stringify(args, null, 2);
  };

  const formatResult = (result?: unknown): string => {
    if (result === undefined || result === null) return "";
    if (typeof result === "string") return result;
    return JSON.stringify(result, null, 2);
  };

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Call header */}
      <div className="px-4 py-3 border-b border-border bg-muted/10 flex-shrink-0">
        <div className="flex items-center gap-2">
          <Badge className="font-mono bg-indigo-500/10 text-indigo-500 border border-indigo-500/30">
            {call.toolName}
          </Badge>
          <span className="text-sm text-muted-foreground">on</span>
          <code className="font-mono text-sm text-foreground truncate flex-1">
            {call.serverName || call.serverId}
          </code>
          {call.isError && <Badge variant="danger">Error</Badge>}
        </div>
      </div>

      {/* Tabs for arguments/result */}
      <Tabs defaultValue="result" className="flex-1 flex flex-col overflow-hidden">
        <TabsList className="px-4 pt-2 justify-start border-b border-border bg-transparent flex-shrink-0">
          <TabsTrigger value="arguments" className="gap-1.5">
            <ArrowRight className="h-3.5 w-3.5" />
            Arguments
          </TabsTrigger>
          <TabsTrigger value="result" className="gap-1.5">
            <ArrowLeft className="h-3.5 w-3.5" />
            Result
          </TabsTrigger>
        </TabsList>

        <TabsContent value="arguments" className="flex-1 overflow-hidden m-0 p-0">
          <ScrollArea className="h-full p-3 space-y-3">
            {call.arguments && Object.keys(call.arguments).length > 0 ? (
              <StepOutputPanel
                title="Arguments"
                content={formatArguments(call.arguments)}
                contentType="json"
                accentColor="indigo"
                showLineNumbers
                maxHeight="max-h-[400px]"
              />
            ) : (
              <div className="text-center text-muted-foreground text-sm py-8">
                No arguments provided
              </div>
            )}
          </ScrollArea>
        </TabsContent>

        <TabsContent value="result" className="flex-1 overflow-hidden m-0 p-0">
          <ScrollArea className="h-full p-3 space-y-3">
            {call.result !== undefined ? (
              <StepOutputPanel
                title={call.isError ? "Error" : "Result"}
                content={formatResult(call.result)}
                contentType="json"
                accentColor={call.isError ? "red" : "slate"}
                showLineNumbers
                maxHeight="max-h-[400px]"
              />
            ) : call.status === "running" ? (
              <div className="text-center text-muted-foreground text-sm py-8">
                Call in progress...
              </div>
            ) : (
              <div className="text-center text-muted-foreground text-sm py-8">
                No result available
              </div>
            )}
          </ScrollArea>
        </TabsContent>
      </Tabs>
    </div>
  );
}

/**
 * Full MCP Call widget component.
 */
export function McpCallWidget({ isSummary, data, className }: McpCallWidgetProps) {
  const [selectedCallId, setSelectedCallId] = useState<string | null>(null);

  if (isSummary) {
    return null;
  }

  const selectedCall = data.calls.find((c) => c.id === selectedCallId) || null;

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar */}
      <StepStatsBar stats={data.stats} />

      {/* Main content - split view */}
      <div className="flex-1 flex overflow-hidden">
        {/* Call list */}
        <div className="w-1/2 border-r border-border flex flex-col">
          <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
            <h3 className="text-sm font-semibold text-foreground">MCP Calls</h3>
            <Badge variant="muted" className="text-xs">
              {data.calls.length}
            </Badge>
          </div>
          <ScrollArea className="flex-1">
            <div className="flex flex-col-reverse">
              {data.calls.slice(0, 50).map((call) => (
                <CallRow
                  key={call.id}
                  call={call}
                  isSelected={call.id === selectedCallId}
                  onSelect={() => setSelectedCallId(call.id)}
                />
              ))}
            </div>
            {data.calls.length === 0 && (
              <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
                No MCP calls executed yet
              </div>
            )}
          </ScrollArea>
        </div>

        {/* Call detail */}
        <div className="w-1/2 flex flex-col">
          <CallDetail call={selectedCall} />
        </div>
      </div>
    </div>
  );
}

export default McpCallWidget;
