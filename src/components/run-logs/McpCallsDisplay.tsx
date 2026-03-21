import { useState } from "react";
import { Loader2, AlertCircle, ChevronDown, ChevronRight, Wifi } from "lucide-react";
import { useTaskRunMcpCalls } from "../../hooks/useAiData";
import type { TaskRunMcpCallDb } from "../../types/mcp-config";
import { parseJson } from "./ai-data-viewer-utils";

export function McpCallsDisplay({ taskRunId }: { taskRunId: string }) {
  const { data: mcpData, isLoading, error } = useTaskRunMcpCalls(taskRunId);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const toggleExpanded = (id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <AlertCircle className="w-8 h-8 mb-3 text-destructive opacity-50" />
        <p className="text-sm">Error loading MCP calls</p>
      </div>
    );
  }

  if (!mcpData || mcpData.calls.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Wifi className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No MCP calls for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex gap-4 text-xs text-muted-foreground">
        <span>Total: {mcpData.count}</span>
        <span className="text-green-400">Success: {mcpData.success_count}</span>
        <span className="text-red-400">Failed: {mcpData.failed_count}</span>
      </div>

      <div className="space-y-3">
        {mcpData.calls.map((call: TaskRunMcpCallDb) => {
          const isExpanded = expandedIds.has(call.id);
          const response = parseJson(call.response);
          const args = parseJson(call.arguments);
          const resolvedArgs = parseJson(call.resolved_arguments);

          return (
            <div key={call.id} className="border border-border rounded-lg overflow-hidden bg-card">
              <button
                onClick={() => toggleExpanded(call.id)}
                className="w-full flex items-center gap-3 p-3 hover:bg-muted/50 transition-colors text-left"
              >
                <div
                  className={`w-2 h-2 rounded-full shrink-0 ${
                    call.success ? "bg-green-500" : "bg-red-500"
                  }`}
                />

                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="px-2 py-0.5 text-xs font-medium rounded bg-purple-500/20 text-purple-400">
                      {call.server_name || call.server_id}
                    </span>
                    <span className="text-sm font-mono text-foreground truncate">
                      {call.tool_name}
                    </span>
                  </div>
                  {call.step_name && (
                    <p className="text-xs text-muted-foreground mt-1 truncate">
                      Step: {call.step_name}
                    </p>
                  )}
                </div>

                <span className="text-xs text-muted-foreground shrink-0">{call.duration_ms}ms</span>

                {isExpanded ? (
                  <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
                )}
              </button>

              {isExpanded && (
                <div className="border-t border-border p-3 space-y-3 bg-muted/30">
                  {Boolean(args) && (
                    <div>
                      <p className="text-xs font-medium text-muted-foreground mb-1">Arguments</p>
                      <pre className="text-xs font-mono bg-background p-2 rounded overflow-auto max-h-32 text-foreground">
                        {JSON.stringify(args, null, 2)}
                      </pre>
                    </div>
                  )}

                  {Boolean(resolvedArgs) &&
                    JSON.stringify(resolvedArgs) !== JSON.stringify(args) && (
                      <div>
                        <p className="text-xs font-medium text-muted-foreground mb-1">
                          Resolved Arguments
                        </p>
                        <pre className="text-xs font-mono bg-background p-2 rounded overflow-auto max-h-32 text-foreground">
                          {JSON.stringify(resolvedArgs, null, 2)}
                        </pre>
                      </div>
                    )}

                  {response != null && (
                    <div>
                      <p className="text-xs font-medium text-muted-foreground mb-1">
                        Response ({call.response_type})
                      </p>
                      <pre className="text-xs font-mono bg-background p-2 rounded overflow-auto max-h-48 text-foreground">
                        {typeof response === "string"
                          ? response
                          : JSON.stringify(response, null, 2)}
                      </pre>
                    </div>
                  )}

                  {call.error_message && (
                    <div>
                      <p className="text-xs font-medium text-red-400 mb-1">Error</p>
                      <p className="text-xs text-red-300 bg-red-900/20 p-2 rounded">
                        {call.error_message}
                      </p>
                    </div>
                  )}

                  <div className="flex gap-4 text-xs text-muted-foreground pt-2 border-t border-border">
                    <span>Created: {new Date(call.created_at).toLocaleString()}</span>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
