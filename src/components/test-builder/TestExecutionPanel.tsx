/**
 * Test Execution Panel
 *
 * Bottom panel for running tests and viewing results.
 */

import { useState } from "react";
import {
  Play,
  CheckCircle2,
  XCircle,
  Clock,
  AlertTriangle,
  ChevronDown,
  ChevronUp,
  Terminal,
  Image,
  Loader2,
} from "lucide-react";
import { useTestBuilder } from "./TestBuilderContext";
import { getStatusColors, getAccentColors } from "@/design-system";

export function TestExecutionPanel() {
  const { selectedTest, state, executeTest } = useTestBuilder();
  const [isExpanded, setIsExpanded] = useState(true);
  const [selectedScreenshot, setSelectedScreenshot] = useState<string | null>(null);

  // Handle running the test
  const handleRunTest = async () => {
    if (!selectedTest) return;
    await executeTest(selectedTest.id);
  };

  // Get status icon
  const getStatusIcon = () => {
    if (state.isExecuting) {
      return <Loader2 className={`w-5 h-5 animate-spin ${getAccentColors("blue").text}`} />;
    }
    if (!state.lastResult) {
      return <Clock className="w-5 h-5 text-muted-foreground" />;
    }
    switch (state.lastResult.status) {
      case "passed":
        return <CheckCircle2 className={`w-5 h-5 ${getStatusColors("success").icon}`} />;
      case "failed":
        return <XCircle className={`w-5 h-5 ${getStatusColors("error").icon}`} />;
      case "error":
        return <AlertTriangle className={`w-5 h-5 ${getStatusColors("error").icon}`} />;
      case "timeout":
        return <Clock className={`w-5 h-5 ${getStatusColors("warning").icon}`} />;
      default:
        return <Clock className="w-5 h-5 text-muted-foreground" />;
    }
  };

  // Get status text
  const getStatusText = () => {
    if (state.isExecuting) {
      return "Running...";
    }
    if (!state.lastResult) {
      return "Ready";
    }
    switch (state.lastResult.status) {
      case "passed":
        return "Passed";
      case "failed":
        return "Failed";
      case "error":
        return "Error";
      case "timeout":
        return "Timeout";
      default:
        return state.lastResult.status;
    }
  };

  // Get status color
  const getStatusColor = () => {
    if (state.isExecuting) {
      return getAccentColors("blue").text;
    }
    if (!state.lastResult) {
      return "text-muted-foreground";
    }
    switch (state.lastResult.status) {
      case "passed":
        return getStatusColors("success").text;
      case "failed":
      case "error":
        return getStatusColors("error").text;
      case "timeout":
        return getStatusColors("warning").text;
      default:
        return "text-muted-foreground";
    }
  };

  return (
    <div className="bg-card border-t border-border/50">
      {/* Collapsible header */}
      <div
        className="flex items-center justify-between px-4 py-2 cursor-pointer hover:bg-muted/20 transition-colors"
        onClick={() => setIsExpanded(!isExpanded)}
      >
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2">
            <Terminal className="w-4 h-4 text-muted-foreground" />
            <span data-content-role="heading" className="text-sm font-medium">
              Execution
            </span>
          </div>

          {/* Status indicator */}
          <div className="flex items-center gap-2">
            {getStatusIcon()}
            <span
              data-content-role="status"
              data-content-label="execution status"
              className={`text-sm font-medium ${getStatusColor()}`}
            >
              {getStatusText()}
            </span>
          </div>

          {/* Duration */}
          {state.lastResult && (
            <span
              data-content-role="metric"
              data-content-label="execution duration"
              className="text-xs text-muted-foreground"
            >
              {state.lastResult.duration_ms}ms
            </span>
          )}

          {/* Assertions */}
          {state.lastResult && (
            <span
              data-content-role="metric"
              data-content-label="assertions count"
              className="text-xs text-muted-foreground"
            >
              {state.lastResult.assertions_passed}/
              {state.lastResult.assertions_passed + state.lastResult.assertions_failed} assertions
            </span>
          )}
        </div>

        <div className="flex items-center gap-2">
          {/* Run button */}
          <button
            className={`
              px-3 py-1.5 text-sm font-medium rounded-md flex items-center gap-1.5
              transition-colors
              ${
                selectedTest && !state.isExecuting
                  ? "bg-green-600 text-white hover:bg-green-700"
                  : "bg-muted text-muted-foreground cursor-not-allowed"
              }
            `}
            onClick={(e) => {
              e.stopPropagation();
              handleRunTest();
            }}
            disabled={!selectedTest || state.isExecuting}
            data-ui-id="test-builder-run-test-btn"
          >
            {state.isExecuting ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Play className="w-4 h-4" />
            )}
            Run Test
          </button>

          {/* Expand/collapse */}
          {isExpanded ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronUp className="w-4 h-4 text-muted-foreground" />
          )}
        </div>
      </div>

      {/* Expanded content */}
      {isExpanded && (
        <div className="border-t border-border/50 max-h-64 overflow-y-auto">
          {!selectedTest ? (
            <div className="p-8 text-center text-muted-foreground text-sm">
              Select a test to run
            </div>
          ) : !state.lastResult && !state.isExecuting ? (
            <div className="p-8 text-center text-muted-foreground text-sm">
              Click "Run Test" to execute the selected test
            </div>
          ) : state.isExecuting ? (
            <div className="p-8 text-center">
              <Loader2 className="w-8 h-8 animate-spin text-primary mx-auto mb-2" />
              <p className="text-sm text-muted-foreground">Running test...</p>
            </div>
          ) : state.lastResult ? (
            <div className="p-4 space-y-4">
              {/* Error message */}
              {state.lastResult.error && (
                <div
                  className={`p-3 ${getStatusColors("error").bg} border ${getStatusColors("error").border} rounded-md`}
                >
                  <div className="flex items-start gap-2">
                    <AlertTriangle
                      className={`w-4 h-4 ${getStatusColors("error").icon} flex-shrink-0 mt-0.5`}
                    />
                    <div>
                      <p className={`text-sm font-medium ${getStatusColors("error").text}`}>
                        Error
                      </p>
                      <pre className="text-xs text-red-400 mt-1 whitespace-pre-wrap font-mono">
                        {state.lastResult.error}
                      </pre>
                    </div>
                  </div>
                </div>
              )}

              {/* Output */}
              {state.lastResult.output && (
                <div>
                  <h4 className="text-xs font-medium text-muted-foreground mb-2 flex items-center gap-1">
                    <Terminal className="w-3 h-3" />
                    Output
                  </h4>
                  <pre className="text-xs bg-muted/30 p-3 rounded-md overflow-x-auto font-mono max-h-32 overflow-y-auto">
                    {state.lastResult.output}
                  </pre>
                </div>
              )}

              {/* Screenshots */}
              {state.lastResult.screenshots.length > 0 && (
                <div>
                  <h4 className="text-xs font-medium text-muted-foreground mb-2 flex items-center gap-1">
                    <Image className="w-3 h-3" />
                    Screenshots ({state.lastResult.screenshots.length})
                  </h4>
                  <div className="flex gap-2 flex-wrap">
                    {state.lastResult.screenshots.map((screenshot, i) => (
                      <div
                        key={i}
                        className="w-24 h-16 bg-muted rounded-md overflow-hidden cursor-pointer hover:ring-2 hover:ring-primary/50"
                        onClick={() => setSelectedScreenshot(screenshot)}
                      >
                        <img
                          src={`file://${screenshot}`}
                          alt={`Screenshot ${i + 1}`}
                          className="w-full h-full object-cover"
                          onError={(e) => {
                            (e.target as HTMLImageElement).style.display = "none";
                          }}
                        />
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* Structured output */}
              {state.lastResult.structured_output && (
                <div>
                  <h4 className="text-xs font-medium text-muted-foreground mb-2">
                    Structured Output
                  </h4>
                  <pre className="text-xs bg-muted/30 p-3 rounded-md overflow-x-auto font-mono max-h-32 overflow-y-auto">
                    {JSON.stringify(state.lastResult.structured_output, null, 2)}
                  </pre>
                </div>
              )}
            </div>
          ) : null}

          {/* Error from context */}
          {state.error && (
            <div className="p-4 border-t border-border/50">
              <div
                className={`p-3 ${getStatusColors("error").bg} border ${getStatusColors("error").border} rounded-md`}
              >
                <p className={`text-sm ${getStatusColors("error").text}`}>{state.error}</p>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Screenshot Modal */}
      {selectedScreenshot && (
        <div
          className="fixed inset-0 bg-black/80 z-50 flex items-center justify-center p-8"
          onClick={() => setSelectedScreenshot(null)}
        >
          <div className="relative max-w-[90vw] max-h-[90vh]">
            <img
              src={`file://${selectedScreenshot}`}
              alt="Screenshot"
              className="max-w-full max-h-[90vh] object-contain rounded-lg shadow-2xl"
              onClick={(e) => e.stopPropagation()}
            />
            <button
              className="absolute top-2 right-2 p-2 bg-black/50 hover:bg-black/70 rounded-full text-white"
              onClick={() => setSelectedScreenshot(null)}
            >
              <XCircle className="w-6 h-6" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
