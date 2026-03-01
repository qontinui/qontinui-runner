/**
 * ExecutionStatusSummary Component
 *
 * Compact summary widget for displaying execution status in the Active Dashboard sidebar.
 * Shows quick stats for routing, retries, compression, and hooks.
 */

import { Brain, RefreshCw, Archive, Webhook, Circle, Activity } from "lucide-react";
import { Badge } from "../../../ui/Badge";
import { getAccentColors } from "@/design-system";
import { getStatusInfo } from "./utils";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { ExecutionStatusWidgetData } from "./types";

interface ExecutionStatusSummaryProps extends BaseWidgetProps {
  data: ExecutionStatusWidgetData;
}

export function ExecutionStatusSummary({ data }: ExecutionStatusSummaryProps) {
  const { status } = data;
  const statusInfo = getStatusInfo(status.status);
  const hasRouting = status.routing.decision !== null;
  const hasRetries = status.retry.state.errorHistory.length > 0;
  const hasCompression = status.compression.currentTokenCount !== null;
  const hasHooks = status.hooks.executionHistory.length > 0;

  // Count active features
  const activeFeatures = [hasRouting, hasRetries, hasCompression, hasHooks].filter(Boolean).length;

  // Empty state: no active workflow
  if (status.status === "idle" && activeFeatures === 0) {
    return (
      <div className="text-center py-2">
        <Activity className="h-4 w-4 mx-auto mb-1 text-muted-foreground opacity-50" />
        <span className="text-xs text-muted-foreground">No active workflow</span>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* Status Row */}
      <div className="flex items-center justify-between">
        <Badge className={`${statusInfo.color.bg} ${statusInfo.color.text}`} size="sm">
          <Circle className={`w-2 h-2 mr-1.5 ${statusInfo.dotClass}`} />
          {statusInfo.label}
        </Badge>
        {status.iteration > 0 && (
          <span className="text-[10px] text-muted-foreground">Iter {status.iteration}</span>
        )}
      </div>

      {/* Feature Stats Grid */}
      <div className="grid grid-cols-2 gap-2">
        {/* Routing */}
        <div className="flex items-center gap-1.5 text-[11px]">
          <Brain className="w-3 h-3 text-muted-foreground" />
          {hasRouting && status.routing.decision ? (
            <span className="text-foreground">{status.routing.decision.complexity}</span>
          ) : (
            <span className="text-muted-foreground">No routing</span>
          )}
        </div>

        {/* Retries */}
        <div className="flex items-center gap-1.5 text-[11px]">
          <RefreshCw className="w-3 h-3 text-muted-foreground" />
          {hasRetries ? (
            <span
              className={status.retry.exhausted ? getAccentColors("red").text : "text-foreground"}
            >
              {status.retry.state.attempt} retries
            </span>
          ) : (
            <span className="text-muted-foreground">No retries</span>
          )}
        </div>

        {/* Compression */}
        <div className="flex items-center gap-1.5 text-[11px]">
          <Archive className="w-3 h-3 text-muted-foreground" />
          {hasCompression && status.compression.currentTokenCount ? (
            <span
              className={
                status.compression.compressionImminent
                  ? getAccentColors("amber").text
                  : "text-foreground"
              }
            >
              {Math.round(status.compression.thresholdPercentage)}%
            </span>
          ) : (
            <span className="text-muted-foreground">-</span>
          )}
        </div>

        {/* Hooks */}
        <div className="flex items-center gap-1.5 text-[11px]">
          <Webhook className="w-3 h-3 text-muted-foreground" />
          {hasHooks ? (
            <span className="text-foreground">
              {status.hooks.executionHistory.filter((h) => h.success).length}/
              {status.hooks.executionHistory.length}
            </span>
          ) : (
            <span className="text-muted-foreground">No hooks</span>
          )}
        </div>
      </div>

      {/* Active Features Count */}
      {activeFeatures > 0 && (
        <div className="text-[10px] text-muted-foreground text-center pt-1 border-t border-border/30">
          {activeFeatures} active feature{activeFeatures !== 1 ? "s" : ""}
        </div>
      )}
    </div>
  );
}
