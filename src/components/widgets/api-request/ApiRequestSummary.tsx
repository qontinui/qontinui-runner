/**
 * ApiRequestSummary Component
 *
 * Compact summary view for API request widget.
 * Shows quick stats and the last few requests with status.
 */

import { Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getStatusColors } from "@/design-system";
import type { ApiRequestSummaryProps } from "./types";
import type { ApiRequestExecution } from "../shared/types";

/**
 * Get color for HTTP method.
 */
function getMethodColor(method: string): string {
  const colors: Record<string, string> = {
    GET: "bg-green-500/10 text-green-500 border-green-500/30",
    POST: "bg-blue-500/10 text-blue-500 border-blue-500/30",
    PUT: "bg-amber-500/10 text-amber-500 border-amber-500/30",
    PATCH: "bg-purple-500/10 text-purple-500 border-purple-500/30",
    DELETE: "bg-red-500/10 text-red-500 border-red-500/30",
  };
  return colors[method.toUpperCase()] || "bg-slate-500/10 text-slate-500 border-slate-500/30";
}

/**
 * Get variant for status code.
 */
function getStatusCodeVariant(code?: number): "success" | "warning" | "danger" | "muted" {
  if (!code) return "muted";
  if (code >= 200 && code < 300) return "success";
  if (code >= 300 && code < 400) return "warning";
  if (code >= 400) return "danger";
  return "muted";
}

/**
 * Compact request row for summary view.
 */
function CompactRequestRow({ request }: { request: ApiRequestExecution }) {
  // Extract just the path from URL for compact display
  let displayUrl = request.url;
  try {
    const url = new URL(request.url);
    displayUrl = url.pathname + url.search;
  } catch {
    // Use full URL if parsing fails
  }

  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={request.status} iconOnly size="sm" />
      <Badge
        className={cn("text-[10px] px-1.5 py-0 font-mono border", getMethodColor(request.method))}
      >
        {request.method}
      </Badge>
      <span className="text-xs text-muted-foreground truncate flex-1 font-mono">{displayUrl}</span>
      {request.statusCode !== undefined && (
        <Badge variant={getStatusCodeVariant(request.statusCode)} className="text-[10px] px-1">
          {request.statusCode}
        </Badge>
      )}
    </div>
  );
}

/**
 * Compact summary widget for API requests.
 */
export function ApiRequestSummary({ status, data }: ApiRequestSummaryProps) {
  const { requests, currentRequest, stats } = data;
  const recentRequests = requests.slice(0, 3);
  const pendingColors = getStatusColors("pending");

  return (
    <div className="space-y-3">
      {/* Quick Stats */}
      <StepStatsBar stats={stats} compact />

      {/* Recent Requests */}
      {recentRequests.length > 0 ? (
        <div className="space-y-0.5">
          {recentRequests.map((request) => (
            <CompactRequestRow key={request.id} request={request} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No requests executed yet</p>
      )}

      {/* Running Indicator */}
      {status === "running" && currentRequest && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate font-mono", pendingColors.text)}>
            {currentRequest.method} {currentRequest.url}
          </span>
        </div>
      )}
    </div>
  );
}

export default ApiRequestSummary;
