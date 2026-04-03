import { useState, useCallback } from "react";
import {
  Loader2,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  CheckCircle,
  XCircle,
  Bot,
} from "lucide-react";

const PAGE_SIZE = 50;
import { useRunSelection } from "../../contexts/RunSelectionContext";
import { useTaskRunAwasSteps } from "../../hooks/useAiData";
import type { TaskRunAwasStepDb } from "../../types/aiData";
import { getStatusColors, getAccentColors } from "@/design-system";
import { CollapsibleSection } from "./CollapsibleSection";
import { parseJson, formatJson, formatTimestamp } from "./ai-data-viewer-utils";

function getStepTypeStyle(stepType: string) {
  switch (stepType) {
    case "awas_discover":
      return {
        bg: getAccentColors("blue").bg,
        text: getAccentColors("blue").text,
        label: "Discover",
      };
    case "awas_execute":
      return {
        bg: getAccentColors("green").bg,
        text: getAccentColors("green").text,
        label: "Execute",
      };
    case "awas_check_support":
      return {
        bg: getAccentColors("purple").bg,
        text: getAccentColors("purple").text,
        label: "Check Support",
      };
    case "awas_list_actions":
      return {
        bg: getAccentColors("yellow").bg,
        text: getAccentColors("yellow").text,
        label: "List Actions",
      };
    case "awas_extract_elements":
      return {
        bg: "bg-muted",
        text: "text-foreground",
        label: "Extract Elements",
      };
    default:
      return {
        bg: "bg-muted",
        text: "text-muted-foreground",
        label: stepType.replace("awas_", "").replace(/_/g, " "),
      };
  }
}

export function AwasStepsSection() {
  const { selectedRunId, selectedRun: _selectedRun } = useRunSelection();
  const [pageSize, setPageSize] = useState(PAGE_SIZE);
  const {
    data: awasData,
    isLoading,
    error,
  } = useTaskRunAwasSteps(selectedRunId, undefined, pageSize);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const loadMore = useCallback(() => {
    setPageSize((prev) => prev + PAGE_SIZE);
  }, []);

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

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Bot className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view AWAS steps</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading AWAS steps...
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

  if (!awasData || awasData.steps.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Bot className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No AWAS steps for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4 px-3 py-2 bg-muted/30 rounded-md">
        <span className="text-sm font-medium">AWAS Steps:</span>
        <span className={`flex items-center gap-1 text-sm ${getStatusColors("success").text}`}>
          <CheckCircle className="w-4 h-4" />
          {awasData.success_count} successful
        </span>
        <span className={`flex items-center gap-1 text-sm ${getStatusColors("error").text}`}>
          <XCircle className="w-4 h-4" />
          {awasData.failed_count} failed
        </span>
        <span className="text-xs text-muted-foreground ml-auto">{awasData.count} total steps</span>
      </div>

      <div className="space-y-2">
        {awasData.steps.map((step: TaskRunAwasStepDb) => {
          const stepTypeStyle = getStepTypeStyle(step.step_type);
          const isExpanded = expandedIds.has(step.id);
          const parameters = parseJson(step.parameters);
          const responseData = parseJson(step.response_data);
          const hasExpandableContent =
            step.url || step.action_id || parameters || responseData || step.error_message;

          return (
            <div key={step.id} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => hasExpandableContent && toggleExpanded(step.id)}
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
                {step.success ? (
                  <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />
                ) : (
                  <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />
                )}
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${stepTypeStyle.bg} ${stepTypeStyle.text}`}
                >
                  {stepTypeStyle.label}
                </span>
                <span className="flex-1 truncate text-sm">
                  {step.step_name || step.action_id || step.url || "Step"}
                </span>
                {step.duration_ms !== null && step.duration_ms !== undefined && (
                  <span className="text-xs text-muted-foreground">{step.duration_ms}ms</span>
                )}
              </button>
              {isExpanded && hasExpandableContent && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {step.step_name && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Step Name:</span>{" "}
                      {step.step_name}
                    </div>
                  )}
                  {step.url && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">URL:</span>{" "}
                      <span className="font-mono break-all">{step.url}</span>
                    </div>
                  )}
                  {step.action_id && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Action ID:</span>{" "}
                      <span className="font-mono">{step.action_id}</span>
                    </div>
                  )}
                  {parameters != null && (
                    <CollapsibleSection title="Parameters">
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {formatJson(parameters)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {responseData != null && (
                    <CollapsibleSection title="Response Data" defaultOpen={!step.error_message}>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap">
                        {formatJson(responseData)}
                      </pre>
                    </CollapsibleSection>
                  )}
                  {step.error_message && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {step.error_message}
                      </pre>
                    </div>
                  )}
                  <div className="text-xs text-muted-foreground">
                    Created: {formatTimestamp(step.created_at)}
                  </div>
                </div>
              )}
            </div>
          );
        })}
        {awasData.has_more && (
          <button
            onClick={loadMore}
            className="w-full py-2 text-xs text-muted-foreground hover:text-foreground flex items-center justify-center gap-1 border border-border rounded hover:bg-muted/50 transition-colors"
          >
            <ChevronDown className="w-3 h-3" />
            Show more ({awasData.count - awasData.steps.length} remaining)
          </button>
        )}
      </div>
    </div>
  );
}
