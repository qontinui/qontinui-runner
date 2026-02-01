/**
 * McpCallSummary Component
 *
 * Compact summary view for MCP call widget.
 * Shows quick stats and the last few calls with status.
 */

import { Loader2 } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge } from "../../../ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getStatusColors } from "@/design-system";
import type { McpCallSummaryProps, McpCallExecution } from "./types";

/**
 * Compact call row for summary view.
 */
function CompactCallRow({ call }: { call: McpCallExecution }) {
  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={call.status} iconOnly size="sm" />
      <Badge className="text-[10px] px-1.5 py-0 font-mono bg-indigo-500/10 text-indigo-500 border border-indigo-500/30">
        {call.toolName}
      </Badge>
      <span className="text-xs text-muted-foreground truncate flex-1">
        {call.serverName || call.serverId}
      </span>
      {call.isError && (
        <Badge variant="danger" className="text-[10px] px-1">
          Error
        </Badge>
      )}
    </div>
  );
}

/**
 * Compact summary widget for MCP calls.
 */
export function McpCallSummary({ status, data }: McpCallSummaryProps) {
  const { calls, currentCall, stats } = data;
  const recentCalls = calls.slice(0, 3);
  const pendingColors = getStatusColors("pending");

  return (
    <div className="space-y-3">
      {/* Quick Stats */}
      <StepStatsBar stats={stats} compact />

      {/* Recent Calls */}
      {recentCalls.length > 0 ? (
        <div className="space-y-0.5">
          {recentCalls.map((call) => (
            <CompactCallRow key={call.id} call={call} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No MCP calls executed yet</p>
      )}

      {/* Running Indicator */}
      {status === "running" && currentCall && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate font-mono", pendingColors.text)}>
            {currentCall.toolName} ({currentCall.serverName || currentCall.serverId})
          </span>
        </div>
      )}
    </div>
  );
}

export default McpCallSummary;
