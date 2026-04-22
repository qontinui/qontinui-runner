/**
 * TestResultsTab.tsx
 *
 * Displays test results for the selected task run.
 * Shows a list of test results with status, duration, and assertion counts.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Activity,
  CheckCircle,
  XCircle,
  Clock,
  AlertTriangle,
  RefreshCw,
  ChevronDown,
  ChevronRight,
  FileText,
  TestTube,
  Eye,
  Code,
  Terminal,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import type { TestResult, TestStatus, TestType, VerificationTest } from "../../types/test-builder";

// Status badge configuration
const statusConfig: Record<
  TestStatus,
  { icon: typeof CheckCircle; color: string; bgColor: string; label: string }
> = {
  pending: {
    icon: Clock,
    color: "text-slate-400",
    bgColor: "bg-slate-500/10",
    label: "Pending",
  },
  running: {
    icon: RefreshCw,
    color: "text-blue-400",
    bgColor: "bg-blue-500/10",
    label: "Running",
  },
  passed: {
    icon: CheckCircle,
    color: "text-green-400",
    bgColor: "bg-green-500/10",
    label: "Passed",
  },
  failed: {
    icon: XCircle,
    color: "text-red-400",
    bgColor: "bg-red-500/10",
    label: "Failed",
  },
  skipped: {
    icon: Activity,
    color: "text-slate-400",
    bgColor: "bg-slate-500/10",
    label: "Skipped",
  },
  error: {
    icon: AlertTriangle,
    color: "text-amber-400",
    bgColor: "bg-amber-500/10",
    label: "Error",
  },
  timeout: {
    icon: Clock,
    color: "text-amber-400",
    bgColor: "bg-amber-500/10",
    label: "Timeout",
  },
};

// Test type icons
const testTypeIcons: Record<TestType, typeof TestTube> = {
  playwright_cdp: Terminal,
  qontinui_vision: Eye,
  python_script: Code,
  repository_test: FileText,
};

// Extended TestResult with test definition info
interface TestResultWithTest extends TestResult {
  test_name?: string;
  test_type?: TestType;
  is_critical?: boolean;
}

interface TestResultsResponse {
  success: boolean;
  data?: TestResult[];
  message?: string;
}

interface TestResponse {
  success: boolean;
  data?: VerificationTest;
  message?: string;
}

export function TestResultsTab() {
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

  const [results, setResults] = useState<TestResultWithTest[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedResults, setExpandedResults] = useState<Set<string>>(new Set());

  // Fetch test results for the selected run

  const fetchResults = useCallback(async () => {
    if (!selectedRun?.id) {
      setResults([]);
      return;
    }

    setIsLoading(true);
    setError(null);

    try {
      const response = await invoke<TestResultsResponse>("list_test_results", {
        taskRunId: selectedRun.id,
        limit: 100,
      });

      if (response.success && response.data) {
        // Fetch test details for each result
        const enrichedResults = await Promise.all(
          response.data.map(async (result) => {
            try {
              const testResponse = await invoke<TestResponse>("get_test", {
                testId: result.test_id,
              });
              if (testResponse.success && testResponse.data) {
                return {
                  ...result,
                  test_name: testResponse.data.name,
                  test_type: testResponse.data.test_type,
                  is_critical: testResponse.data.is_critical,
                } as TestResultWithTest;
              }
            } catch {
              // Test may have been deleted
            }
            return result as TestResultWithTest;
          }),
        );
        setResults(enrichedResults);
      } else {
        setError(response.message || "Failed to fetch test results");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch test results");
    } finally {
      setIsLoading(false);
    }
  }, [selectedRun?.id]);

  // Fetch results when selected run changes
  useEffect(() => {
    fetchResults();
  }, [fetchResults]);

  // Toggle result expansion
  const toggleExpanded = (resultId: string) => {
    setExpandedResults((prev) => {
      const next = new Set(prev);
      if (next.has(resultId)) {
        next.delete(resultId);
      } else {
        next.add(resultId);
      }
      return next;
    });
  };

  // No run selected state
  if (runSelection && !selectedRun) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view test results.
        </p>
      </div>
    );
  }

  // Loading state
  if (isLoading) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <RefreshCw className="w-8 h-8 animate-spin mb-4" />
        <p className="text-sm">Loading test results...</p>
      </div>
    );
  }

  // Error state
  if (error) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <AlertTriangle className="w-12 h-12 mb-4 text-amber-400" />
        <p className="text-lg font-medium">Error Loading Results</p>
        <p className="text-sm mt-2 text-center max-w-md text-amber-400">{error}</p>
        <button
          onClick={fetchResults}
          className="mt-4 px-4 py-2 bg-primary text-primary-foreground rounded hover:bg-primary/90 transition-colors"
        >
          Retry
        </button>
      </div>
    );
  }

  // No results state
  if (results.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <TestTube className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Test Results</p>
        <p className="text-sm mt-2 text-center max-w-md">
          No verification tests were run during this task run.
        </p>
      </div>
    );
  }

  // Calculate summary stats
  const passedCount = results.filter((r) => r.status === "passed").length;
  const failedCount = results.filter((r) => r.status === "failed" || r.status === "error").length;
  const totalDuration = results.reduce((acc, r) => acc + (r.duration_ms || 0), 0);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Summary header */}
      <div className="shrink-0 p-4 border-b border-border bg-muted/30">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-4">
            <div className="flex items-center gap-2">
              <TestTube className="w-5 h-5 text-muted-foreground" />
              <span className="font-medium">{results.length} Tests</span>
            </div>
            <div className="flex items-center gap-3 text-sm">
              <span className="flex items-center gap-1 text-green-400">
                <CheckCircle className="w-4 h-4" />
                {passedCount} passed
              </span>
              <span className="flex items-center gap-1 text-red-400">
                <XCircle className="w-4 h-4" />
                {failedCount} failed
              </span>
              <span className="flex items-center gap-1 text-muted-foreground">
                <Clock className="w-4 h-4" />
                {(totalDuration / 1000).toFixed(2)}s total
              </span>
            </div>
          </div>
          <button
            onClick={fetchResults}
            className="p-2 text-muted-foreground hover:text-foreground hover:bg-muted rounded transition-colors"
            title="Refresh results"
          >
            <RefreshCw className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Results list */}
      <div className="flex-1 overflow-y-auto p-4 space-y-2">
        {results.map((result) => {
          const statusInfo = statusConfig[result.status];
          const StatusIcon = statusInfo.icon;
          const TypeIcon = result.test_type ? testTypeIcons[result.test_type] : TestTube;
          const isExpanded = expandedResults.has(result.id);

          return (
            <div
              key={result.id}
              className="border border-border rounded-lg bg-card overflow-hidden"
            >
              {/* Result header */}
              <button
                onClick={() => toggleExpanded(result.id)}
                className="w-full flex items-center gap-3 p-3 text-left hover:bg-muted/30 transition-colors"
              >
                {isExpanded ? (
                  <ChevronDown className="w-4 h-4 text-muted-foreground shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
                )}
                <div className={`p-1.5 rounded ${statusInfo.bgColor}`}>
                  <StatusIcon
                    className={`w-4 h-4 ${statusInfo.color} ${
                      result.status === "running" ? "animate-spin" : ""
                    }`}
                  />
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="font-medium truncate">
                      {result.test_name || result.test_id}
                    </span>
                    {result.is_critical && (
                      <span className="px-1.5 py-0.5 text-xs bg-amber-500/10 text-amber-400 rounded">
                        Critical
                      </span>
                    )}
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground mt-0.5">
                    <TypeIcon className="w-3 h-3" />
                    <span>{result.test_type || "Unknown type"}</span>
                  </div>
                </div>
                <div className="flex items-center gap-4 text-sm">
                  <span className={statusInfo.color}>{statusInfo.label}</span>
                  <span className="text-muted-foreground">
                    {result.duration_ms ? `${(result.duration_ms / 1000).toFixed(2)}s` : "-"}
                  </span>
                  <span className="text-muted-foreground">
                    {result.assertions_passed}/{result.assertions_passed + result.assertions_failed}{" "}
                    assertions
                  </span>
                </div>
              </button>

              {/* Expanded content */}
              {isExpanded && (
                <div className="border-t border-border p-4 space-y-4 bg-muted/10">
                  {/* Error message */}
                  {result.error_message && (
                    <div className="p-3 bg-red-500/10 border border-red-500/30 rounded">
                      <div className="flex items-center gap-2 text-red-400 mb-1">
                        <XCircle className="w-4 h-4" />
                        <span className="font-medium">Error</span>
                      </div>
                      <pre className="text-sm text-red-300 whitespace-pre-wrap overflow-x-auto">
                        {result.error_message}
                      </pre>
                    </div>
                  )}

                  {/* Output */}
                  {result.output && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-2">Output</div>
                      <pre className="p-3 bg-muted/30 rounded text-sm text-foreground whitespace-pre-wrap overflow-x-auto max-h-48 overflow-y-auto">
                        {result.output}
                      </pre>
                    </div>
                  )}

                  {/* Timestamps */}
                  <div className="flex items-center gap-4 text-xs text-muted-foreground">
                    {result.started_at && (
                      <span>Started: {new Date(result.started_at).toLocaleString()}</span>
                    )}
                    {result.completed_at && (
                      <span>Completed: {new Date(result.completed_at).toLocaleString()}</span>
                    )}
                  </div>

                  {/* AI Analysis */}
                  {result.ai_analysis && (
                    <div>
                      <div className="text-xs font-medium text-muted-foreground mb-2">
                        AI Analysis
                      </div>
                      <div className="p-3 bg-primary/10 border border-primary/30 rounded">
                        <pre className="text-sm text-foreground whitespace-pre-wrap">
                          {result.ai_analysis}
                        </pre>
                      </div>
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

export default TestResultsTab;
