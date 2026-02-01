/**
 * ApiRequestWidget Component
 *
 * Full widget view for API request execution activity.
 * Displays request list, response details, headers, and status codes.
 */

import { useState } from "react";
import { ArrowRight, ArrowLeft } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea, Tabs, TabsContent, TabsList, TabsTrigger } from "../../../ui";
import { StepStatsBar, StepStatusBadge, StepOutputPanel } from "../shared";
import { getStatusColors } from "@/design-system";
import type { ApiRequestWidgetProps } from "./types";
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
 * Get color for status code.
 */
function getStatusCodeVariant(code?: number): "success" | "warning" | "danger" | "muted" {
  if (!code) return "muted";
  if (code >= 200 && code < 300) return "success";
  if (code >= 300 && code < 400) return "warning";
  if (code >= 400) return "danger";
  return "muted";
}

/**
 * Request row component showing request details.
 */
function RequestRow({
  request,
  isSelected,
  onSelect,
}: {
  request: ApiRequestExecution;
  isSelected: boolean;
  onSelect: () => void;
}) {
  const errorColors = getStatusColors("error");
  const isActive = request.status === "running";
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
      <StepStatusBadge status={request.status} iconOnly size="md" />

      {/* Method badge */}
      <Badge
        className={cn("text-xs flex-shrink-0 font-mono border", getMethodColor(request.method))}
      >
        {request.method}
      </Badge>

      {/* URL and details */}
      <div className="flex-1 min-w-0">
        <p className="font-mono text-sm text-foreground truncate">{request.url}</p>
        {request.error && (
          <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{request.error}</p>
        )}
      </div>

      {/* Status code and duration */}
      <div className="flex items-center gap-2 flex-shrink-0">
        {request.statusCode !== undefined && (
          <Badge variant={getStatusCodeVariant(request.statusCode)} className="text-[10px]">
            {request.statusCode}
          </Badge>
        )}
        {request.durationMs !== undefined && (
          <span className="font-mono text-xs text-muted-foreground">
            {request.durationMs < 1000
              ? `${request.durationMs}ms`
              : `${(request.durationMs / 1000).toFixed(1)}s`}
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * Request detail panel showing full request/response.
 */
function RequestDetail({ request }: { request: ApiRequestExecution | null }) {
  if (!request) {
    return (
      <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
        Select a request to view details
      </div>
    );
  }

  const formatHeaders = (headers?: Record<string, string>): string => {
    if (!headers) return "";
    return Object.entries(headers)
      .map(([key, value]) => `${key}: ${value}`)
      .join("\n");
  };

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Request header */}
      <div className="px-4 py-3 border-b border-border bg-muted/10 flex-shrink-0">
        <div className="flex items-center gap-2">
          <Badge className={cn("font-mono border", getMethodColor(request.method))}>
            {request.method}
          </Badge>
          <code className="font-mono text-sm text-foreground truncate flex-1">{request.url}</code>
          {request.statusCode !== undefined && (
            <Badge variant={getStatusCodeVariant(request.statusCode)}>{request.statusCode}</Badge>
          )}
        </div>
      </div>

      {/* Tabs for request/response */}
      <Tabs defaultValue="response" className="flex-1 flex flex-col overflow-hidden">
        <TabsList className="px-4 pt-2 justify-start border-b border-border bg-transparent flex-shrink-0">
          <TabsTrigger value="request" className="gap-1.5">
            <ArrowRight className="h-3.5 w-3.5" />
            Request
          </TabsTrigger>
          <TabsTrigger value="response" className="gap-1.5">
            <ArrowLeft className="h-3.5 w-3.5" />
            Response
          </TabsTrigger>
        </TabsList>

        <TabsContent value="request" className="flex-1 overflow-hidden m-0 p-0">
          <ScrollArea className="h-full p-3 space-y-3">
            {request.requestHeaders && Object.keys(request.requestHeaders).length > 0 && (
              <StepOutputPanel
                title="Request Headers"
                content={formatHeaders(request.requestHeaders)}
                accentColor="orange"
                maxHeight="max-h-[150px]"
              />
            )}
            {request.requestBody && (
              <StepOutputPanel
                title="Request Body"
                content={request.requestBody}
                contentType={request.contentType?.includes("json") ? "json" : "text"}
                accentColor="orange"
                showLineNumbers
                maxHeight="max-h-[300px]"
              />
            )}
            {!request.requestHeaders && !request.requestBody && (
              <div className="text-center text-muted-foreground text-sm py-8">
                No request data available
              </div>
            )}
          </ScrollArea>
        </TabsContent>

        <TabsContent value="response" className="flex-1 overflow-hidden m-0 p-0">
          <ScrollArea className="h-full p-3 space-y-3">
            {request.responseHeaders && Object.keys(request.responseHeaders).length > 0 && (
              <StepOutputPanel
                title="Response Headers"
                content={formatHeaders(request.responseHeaders)}
                accentColor="slate"
                maxHeight="max-h-[150px]"
              />
            )}
            {request.responseBody && (
              <StepOutputPanel
                title="Response Body"
                content={request.responseBody}
                contentType={request.contentType?.includes("json") ? "json" : "text"}
                accentColor="slate"
                showLineNumbers
                maxHeight="max-h-[300px]"
              />
            )}
            {!request.responseBody && request.status !== "running" && (
              <div className="text-center text-muted-foreground text-sm py-8">
                No response data available
              </div>
            )}
            {request.status === "running" && (
              <div className="text-center text-muted-foreground text-sm py-8">
                Request in progress...
              </div>
            )}
          </ScrollArea>
        </TabsContent>
      </Tabs>
    </div>
  );
}

/**
 * Full API Request widget component.
 */
export function ApiRequestWidget({ isSummary, data, className }: ApiRequestWidgetProps) {
  const [selectedRequestId, setSelectedRequestId] = useState<string | null>(null);

  if (isSummary) {
    return null;
  }

  const selectedRequest = data.requests.find((r) => r.id === selectedRequestId) || null;

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar */}
      <StepStatsBar stats={data.stats} />

      {/* Main content - split view */}
      <div className="flex-1 flex overflow-hidden">
        {/* Request list */}
        <div className="w-1/2 border-r border-border flex flex-col">
          <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
            <h3 className="text-sm font-semibold text-foreground">Requests</h3>
            <Badge variant="muted" className="text-xs">
              {data.requests.length}
            </Badge>
          </div>
          <ScrollArea className="flex-1">
            <div className="flex flex-col-reverse">
              {data.requests.slice(0, 50).map((request) => (
                <RequestRow
                  key={request.id}
                  request={request}
                  isSelected={request.id === selectedRequestId}
                  onSelect={() => setSelectedRequestId(request.id)}
                />
              ))}
            </div>
            {data.requests.length === 0 && (
              <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
                No requests executed yet
              </div>
            )}
          </ScrollArea>
        </div>

        {/* Request detail */}
        <div className="w-1/2 flex flex-col">
          <RequestDetail request={selectedRequest} />
        </div>
      </div>
    </div>
  );
}

export default ApiRequestWidget;
