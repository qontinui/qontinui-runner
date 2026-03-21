import { useState, useMemo } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  Loader2,
  AlertCircle,
  FileJson,
  ChevronDown,
  ChevronRight,
  Clock,
  CheckCircle,
  XCircle,
  SkipForward,
  Timer,
} from "lucide-react";
import { useTaskRunPlaywrightResults } from "../../hooks/useAiData";
import type { TaskRunPlaywrightResultDb } from "../../types/aiData";
import { getStatusColors, getAccentColors } from "@/design-system";

function getTestStatusStyle(status: string) {
  switch (status) {
    case "passed":
      return {
        bg: getStatusColors("success").bg,
        text: getStatusColors("success").text,
        icon: <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />,
      };
    case "failed":
      return {
        bg: getStatusColors("error").bg,
        text: getStatusColors("error").text,
        icon: <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />,
      };
    case "skipped":
      return {
        bg: "bg-muted",
        text: "text-muted-foreground",
        icon: <SkipForward className="w-4 h-4 text-muted-foreground" />,
      };
    case "timeout":
      return {
        bg: getAccentColors("yellow").bg,
        text: getAccentColors("yellow").text,
        icon: <Timer className={`w-4 h-4 ${getAccentColors("yellow").text}`} />,
      };
    default:
      return {
        bg: "bg-muted",
        text: "text-muted-foreground",
        icon: <Clock className="w-4 h-4 text-muted-foreground" />,
      };
  }
}

export function PlaywrightResultsDisplay({ taskRunId }: { taskRunId: string }) {
  const { data: playwrightData, isLoading, error } = useTaskRunPlaywrightResults(taskRunId);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const getImageSrc = useMemo(() => {
    return (path: string) => {
      try {
        return convertFileSrc(path);
      } catch {
        return `file://${path}`;
      }
    };
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

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin mr-2" />
        Loading Playwright results...
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

  if (!playwrightData || playwrightData.results.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <FileJson className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No Playwright test results for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4 px-3 py-2 bg-muted/30 rounded-md">
        <span data-content-role="label" className="text-sm font-medium">
          Test Results:
        </span>
        <span
          data-content-role="metric"
          data-content-label="tests passed"
          className={`flex items-center gap-1 text-sm ${getStatusColors("success").text}`}
        >
          <CheckCircle className="w-4 h-4" />
          {playwrightData.passed} passed
        </span>
        <span
          data-content-role="metric"
          data-content-label="tests failed"
          className={`flex items-center gap-1 text-sm ${getStatusColors("error").text}`}
        >
          <XCircle className="w-4 h-4" />
          {playwrightData.failed} failed
        </span>
        <span
          data-content-role="metric"
          data-content-label="total tests"
          className="text-xs text-muted-foreground ml-auto"
        >
          {playwrightData.count} total tests
        </span>
      </div>

      <div className="space-y-2">
        {playwrightData.results.map((result: TaskRunPlaywrightResultDb) => {
          const statusStyle = getTestStatusStyle(result.status);
          const isExpanded = expandedIds.has(result.id);
          const hasExpandableContent =
            result.error_message ||
            result.console_output ||
            result.stdout ||
            result.stderr ||
            result.failure_screenshot_path ||
            result.page_snapshot;

          return (
            <div key={result.id} className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => hasExpandableContent && toggleExpanded(result.id)}
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
                {statusStyle.icon}
                <span className="font-medium flex-1 truncate">{result.test_name}</span>
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${statusStyle.bg} ${statusStyle.text}`}
                >
                  {result.status}
                </span>
                {result.duration_ms !== null && result.duration_ms !== undefined && (
                  <span className="text-xs text-muted-foreground">
                    {(result.duration_ms / 1000).toFixed(2)}s
                  </span>
                )}
              </button>
              {isExpanded && hasExpandableContent && (
                <div className="px-4 py-3 bg-muted/30 border-t border-border space-y-3">
                  {result.spec_file && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Spec file:</span>{" "}
                      <span className="font-mono">{result.spec_file}</span>
                    </div>
                  )}
                  {(result.assertions_passed > 0 || result.assertions_failed > 0) && (
                    <div className="text-xs">
                      <span className="font-medium text-muted-foreground">Assertions:</span>{" "}
                      <span className={getStatusColors("success").text}>
                        {result.assertions_passed} passed
                      </span>
                      {result.assertions_failed > 0 && (
                        <>
                          {" / "}
                          <span className={getStatusColors("error").text}>
                            {result.assertions_failed} failed
                          </span>
                        </>
                      )}
                    </div>
                  )}
                  {result.error_message && (
                    <div>
                      <div className={`text-xs font-medium ${getStatusColors("error").text} mb-1`}>
                        Error Message
                      </div>
                      <pre
                        className={`text-xs ${getStatusColors("error").bg} ${getStatusColors("error").text} p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {result.error_message}
                      </pre>
                    </div>
                  )}
                  {result.console_output && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Console Output
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {result.console_output}
                      </pre>
                    </div>
                  )}
                  {result.stdout && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Standard Output
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap">
                        {result.stdout}
                      </pre>
                    </div>
                  )}
                  {result.stderr && (
                    <div>
                      <div className={`text-xs font-medium ${getAccentColors("yellow").text} mb-1`}>
                        Standard Error
                      </div>
                      <pre
                        className={`text-xs ${getAccentColors("yellow").bg} p-2 rounded overflow-x-auto max-h-32 overflow-y-auto whitespace-pre-wrap`}
                      >
                        {result.stderr}
                      </pre>
                    </div>
                  )}
                  {result.page_snapshot && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Page Snapshot
                      </div>
                      <pre className="text-xs bg-background p-2 rounded overflow-x-auto max-h-48 overflow-y-auto whitespace-pre-wrap">
                        {result.page_snapshot}
                      </pre>
                    </div>
                  )}
                  {result.failure_screenshot_path && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-1">
                        Failure Screenshot
                      </div>
                      <div className="text-xs text-muted-foreground mb-2 font-mono truncate">
                        {result.failure_screenshot_path}
                      </div>
                      <img
                        src={getImageSrc(result.failure_screenshot_path)}
                        alt="Failure screenshot"
                        className="max-w-full max-h-64 object-contain rounded border border-border"
                        onError={(e) => {
                          (e.target as HTMLImageElement).style.display = "none";
                        }}
                      />
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
