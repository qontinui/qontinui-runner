import { useState } from "react";
import {
  Loader2,
  AlertCircle,
  Globe,
  ChevronDown,
  ChevronRight,
  CheckCircle,
  XCircle,
} from "lucide-react";
import { useTaskRunApiRequests } from "../../hooks/useAiData";
import type { TaskRunApiRequestDb } from "../../types/aiData";
import { getStatusColors, getAccentColors } from "@/design-system";
import { CollapsibleSection } from "./CollapsibleSection";
import {
  parseJson,
  formatJson,
  prettyPrintJson,
  truncateUrl,
  truncateWithEllipsis,
} from "./ai-data-viewer-utils";

function getMethodStyle(method: string) {
  switch (method.toUpperCase()) {
    case "GET":
      return {
        bg: getAccentColors("green").bg,
        text: getAccentColors("green").text,
      };
    case "POST":
      return {
        bg: getAccentColors("blue").bg,
        text: getAccentColors("blue").text,
      };
    case "PUT":
    case "PATCH":
      return {
        bg: getAccentColors("yellow").bg,
        text: getAccentColors("yellow").text,
      };
    case "DELETE":
      return {
        bg: getAccentColors("red").bg,
        text: getAccentColors("red").text,
      };
    default:
      return { bg: "bg-muted", text: "text-muted-foreground" };
  }
}

function getStatusCodeStyle(statusCode: number) {
  if (statusCode >= 200 && statusCode < 300) {
    return {
      bg: getStatusColors("success").bg,
      text: getStatusColors("success").text,
    };
  } else if (statusCode >= 400 && statusCode < 500) {
    return {
      bg: getAccentColors("yellow").bg,
      text: getAccentColors("yellow").text,
    };
  } else if (statusCode >= 500) {
    return {
      bg: getStatusColors("error").bg,
      text: getStatusColors("error").text,
    };
  }
  return { bg: "bg-muted", text: "text-muted-foreground" };
}

export function ApiRequestsDisplay({ taskRunId }: { taskRunId: string }) {
  const { data: apiData, isLoading, error } = useTaskRunApiRequests(taskRunId);
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
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading API requests...
      </div>
    );
  }

  if (error) {
    return (
      <div className={`flex items-center justify-center py-8 ${getStatusColors("error").text}`}>
        <AlertCircle className="w-5 h-5 mr-2" />
        Error: {error.message}
      </div>
    );
  }

  if (!apiData || apiData.requests.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Globe className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No API requests for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4 px-3 py-2 bg-muted/30 rounded-md">
        <span data-content-role="label" className="text-sm font-medium">
          API Requests:
        </span>
        <span
          data-content-role="metric"
          data-content-label="API requests successful"
          className={`flex items-center gap-1 text-sm ${getStatusColors("success").text}`}
        >
          <CheckCircle className="w-4 h-4" />
          {apiData.success_count} successful
        </span>
        <span
          data-content-role="metric"
          data-content-label="API requests failed"
          className={`flex items-center gap-1 text-sm ${getStatusColors("error").text}`}
        >
          <XCircle className="w-4 h-4" />
          {apiData.failed_count} failed
        </span>
        <span
          data-content-role="metric"
          data-content-label="total API requests"
          className="text-xs text-muted-foreground ml-auto"
        >
          {apiData.count} total requests
        </span>
      </div>

      <div className="space-y-2">
        {apiData.requests.map((request: TaskRunApiRequestDb) => {
          const methodStyle = getMethodStyle(request.method);
          const statusStyle = getStatusCodeStyle(request.status_code);
          const isExpanded = expandedIds.has(request.id);

          const requestHeaders = parseJson(request.request_headers);
          const responseHeaders = parseJson(request.response_headers);
          const extractions = parseJson(request.extractions) as Array<{
            variable_name: string;
            extracted_value?: string;
            success: boolean;
          }> | null;
          const assertions = parseJson(request.assertions) as Array<{
            type: string;
            expected: string;
            actual: string;
            passed: boolean;
          }> | null;

          const hasExpandableContent =
            request.resolved_url !== request.url ||
            requestHeaders ||
            request.request_body ||
            responseHeaders ||
            request.response_body ||
            extractions ||
            assertions ||
            request.error_message;

          return (
            <div key={request.id} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => hasExpandableContent && toggleExpanded(request.id)}
                className={`w-full flex items-center gap-3 px-4 py-3 bg-card transition-colors text-left ${
                  hasExpandableContent ? "hover:bg-muted/50 cursor-pointer" : "cursor-default"
                }`}
              >
                {hasExpandableContent ? (
                  isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
                  )
                ) : (
                  <div className="w-4 h-4 shrink-0" />
                )}
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${methodStyle.bg} ${methodStyle.text}`}
                >
                  {request.method}
                </span>
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${statusStyle.bg} ${statusStyle.text}`}
                >
                  {request.status_code}
                </span>
                <span className="flex-1 truncate text-sm font-mono">
                  {truncateUrl(request.resolved_url || request.url)}
                </span>
                <span className="text-xs text-muted-foreground">{request.response_time_ms}ms</span>
              </button>
              {isExpanded && hasExpandableContent && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {request.step_name && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Step:</span>{" "}
                      {request.step_name}
                    </div>
                  )}
                  <div className="text-xs">
                    <span className="font-medium text-muted-foreground">Full URL:</span>{" "}
                    <span className="font-mono break-all">
                      {request.resolved_url || request.url}
                    </span>
                  </div>
                  {requestHeaders != null && Object.keys(requestHeaders as object).length > 0 && (
                    <CollapsibleSection title="Request Headers">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                        {formatJson(requestHeaders)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {request.request_body && (
                    <CollapsibleSection title="Request Body">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {prettyPrintJson(request.request_body)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {responseHeaders != null && Object.keys(responseHeaders as object).length > 0 && (
                    <CollapsibleSection title="Response Headers">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto">
                        {formatJson(responseHeaders)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {request.response_body && (
                    <CollapsibleSection title={`Response Body (${request.response_body_type})`}>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {(() => {
                          try {
                            if (request.response_body_type === "json") {
                              return JSON.stringify(JSON.parse(request.response_body), null, 2);
                            }
                            return truncateWithEllipsis(request.response_body, 2000);
                          } catch {
                            return truncateWithEllipsis(request.response_body, 2000);
                          }
                        })()}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {extractions && extractions.length > 0 && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Extractions ({extractions.length})
                      </div>
                      <div className="space-y-1">
                        {extractions.map((ext, i) => (
                          <div
                            key={`ext-${ext.variable_name}-${i}`}
                            className="flex items-center gap-2 text-xs"
                          >
                            {ext.success ? (
                              <CheckCircle
                                className={`w-3 h-3 ${getStatusColors("success").icon}`}
                              />
                            ) : (
                              <XCircle className={`w-3 h-3 ${getStatusColors("error").icon}`} />
                            )}
                            <span className="font-mono font-medium">{ext.variable_name}</span>
                            <span className="text-muted-foreground">=</span>
                            <span className="font-mono truncate max-w-xs">
                              {ext.extracted_value ?? "(failed)"}
                            </span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {assertions && assertions.length > 0 && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Assertions ({assertions.length})
                      </div>
                      <div className="space-y-1">
                        {assertions.map((a, i) => (
                          <div
                            key={`assert-${a.type}-${i}`}
                            className="flex items-center gap-2 text-xs flex-wrap"
                          >
                            {a.passed ? (
                              <CheckCircle
                                className={`w-3 h-3 ${getStatusColors("success").icon}`}
                              />
                            ) : (
                              <XCircle className={`w-3 h-3 ${getStatusColors("error").icon}`} />
                            )}
                            <span className="font-medium">{a.type}</span>
                            <span className="text-muted-foreground">expected:</span>
                            <span className="font-mono">{a.expected}</span>
                            <span className="text-muted-foreground">actual:</span>
                            <span className="font-mono">{a.actual}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                  {request.error_message && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {request.error_message}
                      </pre>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
