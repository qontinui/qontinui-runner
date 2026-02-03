/**
 * FlowExecutionMonitor.tsx
 *
 * Real-time execution monitoring component for Flow Designer.
 * Displays step progress, timing, context/variables, and step outputs.
 */

import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  Play,
  Pause,
  StepForward,
  Square,
  Clock,
  CheckCircle,
  XCircle,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  Eye,
  Timer,
} from "lucide-react";
// FlowState and StepExecution types available from ../../types/flow if needed

// Flow execution event types matching Rust FlowEvent
interface FlowEvent {
  type: string;
  instance_id: string;
  flow_id?: string;
  flow_name?: string;
  step_id?: string;
  step_name?: string;
  step_type?: string;
  success?: boolean;
  outputs?: Record<string, unknown>;
  error?: string;
  duration_ms?: number;
  total_steps?: number;
  prompt?: string;
  options?: string[];
  completed?: number;
  total?: number;
}

interface FlowExecutionMonitorProps {
  instanceId: string | null;
  flowId: string | null;
  onStepHighlight?: (stepId: string | null) => void;
  onPause?: () => void;
  onResume?: () => void;
  onCancel?: () => void;
  onStepInto?: () => void;
}

interface ExecutionState {
  status: "idle" | "running" | "paused" | "waiting_for_input" | "completed" | "failed";
  currentStepId: string | null;
  startTime: number | null;
  elapsedMs: number;
  stepHistory: StepHistoryEntry[];
  context: Record<string, unknown>;
  error: string | null;
}

interface StepHistoryEntry {
  stepId: string;
  stepName: string;
  stepType: string;
  success: boolean;
  durationMs: number;
  outputs: Record<string, unknown>;
  error: string | null;
  timestamp: number;
}

export function FlowExecutionMonitor({
  instanceId,
  flowId: _flowId,
  onStepHighlight,
  onPause,
  onResume,
  onCancel,
  onStepInto,
}: FlowExecutionMonitorProps) {
  const [executionState, setExecutionState] = useState<ExecutionState>({
    status: "idle",
    currentStepId: null,
    startTime: null,
    elapsedMs: 0,
    stepHistory: [],
    context: {},
    error: null,
  });

  const [expandedSections, setExpandedSections] = useState({
    history: true,
    context: false,
    outputs: true,
  });

  const [selectedHistoryIndex, setSelectedHistoryIndex] = useState<number | null>(null);

  // Update elapsed time while running
  useEffect(() => {
    if (executionState.status !== "running" || !executionState.startTime) return;

    const interval = setInterval(() => {
      setExecutionState((prev) => ({
        ...prev,
        elapsedMs: Date.now() - (prev.startTime || Date.now()),
      }));
    }, 100);

    return () => clearInterval(interval);
  }, [executionState.status, executionState.startTime]);

  // Listen to Tauri events
  useEffect(() => {
    if (!instanceId) return;

    const unlistenPromises: Promise<() => void>[] = [];

    // Flow started event
    unlistenPromises.push(
      listen<FlowEvent>("flow-event", (event) => {
        const data = event.payload;
        if (data.instance_id !== instanceId) return;

        switch (data.type) {
          case "flow_started":
            setExecutionState({
              status: "running",
              currentStepId: null,
              startTime: Date.now(),
              elapsedMs: 0,
              stepHistory: [],
              context: {},
              error: null,
            });
            break;

          case "step_started":
            setExecutionState((prev) => ({
              ...prev,
              currentStepId: data.step_id || null,
            }));
            onStepHighlight?.(data.step_id || null);
            break;

          case "step_completed":
            setExecutionState((prev) => {
              const newHistory = [
                ...prev.stepHistory,
                {
                  stepId: data.step_id || "",
                  stepName: data.step_name || data.step_id || "",
                  stepType: data.step_type || "unknown",
                  success: data.success ?? true,
                  durationMs: data.duration_ms || 0,
                  outputs: data.outputs || {},
                  error: data.error || null,
                  timestamp: Date.now(),
                },
              ];

              // Merge outputs into context
              const newContext = { ...prev.context };
              if (data.outputs && data.step_id) {
                for (const [key, value] of Object.entries(data.outputs)) {
                  newContext[`${data.step_id}.${key}`] = value;
                }
              }

              return {
                ...prev,
                stepHistory: newHistory,
                context: newContext,
              };
            });
            break;

          case "waiting_for_input":
            setExecutionState((prev) => ({
              ...prev,
              status: "waiting_for_input",
            }));
            break;

          case "flow_completed":
            setExecutionState((prev) => ({
              ...prev,
              status: data.success ? "completed" : "failed",
              currentStepId: null,
              error: data.error || null,
            }));
            onStepHighlight?.(null);
            break;

          default:
            break;
        }
      }),
    );

    return () => {
      unlistenPromises.forEach((p) => p.then((fn) => fn()));
    };
  }, [instanceId, onStepHighlight]);

  const formatDuration = (ms: number): string => {
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
    const minutes = Math.floor(ms / 60000);
    const seconds = ((ms % 60000) / 1000).toFixed(0);
    return `${minutes}m ${seconds}s`;
  };

  const toggleSection = (section: keyof typeof expandedSections) => {
    setExpandedSections((prev) => ({
      ...prev,
      [section]: !prev[section],
    }));
  };

  const getStatusColor = () => {
    switch (executionState.status) {
      case "running":
        return "text-yellow-400";
      case "completed":
        return "text-green-400";
      case "failed":
        return "text-red-400";
      case "paused":
        return "text-blue-400";
      case "waiting_for_input":
        return "text-orange-400";
      default:
        return "text-gray-400";
    }
  };

  const getStatusIcon = () => {
    switch (executionState.status) {
      case "running":
        return <Play className="w-4 h-4 animate-pulse" />;
      case "completed":
        return <CheckCircle className="w-4 h-4" />;
      case "failed":
        return <XCircle className="w-4 h-4" />;
      case "paused":
        return <Pause className="w-4 h-4" />;
      case "waiting_for_input":
        return <AlertCircle className="w-4 h-4" />;
      default:
        return <Clock className="w-4 h-4" />;
    }
  };

  if (!instanceId) {
    return (
      <div className="p-4 text-gray-400 text-sm text-center">
        No execution active. Click "Run" to start.
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full bg-gray-900 border-l border-gray-700">
      {/* Header */}
      <div className="flex items-center justify-between p-3 border-b border-gray-700">
        <div className="flex items-center gap-2">
          <span className={getStatusColor()}>{getStatusIcon()}</span>
          <span className="text-sm font-medium text-gray-200 capitalize">
            {executionState.status.replace("_", " ")}
          </span>
        </div>

        {/* Controls */}
        <div className="flex items-center gap-1">
          {executionState.status === "running" && onPause && (
            <button
              onClick={onPause}
              className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded"
              title="Pause"
            >
              <Pause className="w-4 h-4" />
            </button>
          )}
          {executionState.status === "paused" && (
            <>
              {onResume && (
                <button
                  onClick={onResume}
                  className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded"
                  title="Resume"
                >
                  <Play className="w-4 h-4" />
                </button>
              )}
              {onStepInto && (
                <button
                  onClick={onStepInto}
                  className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded"
                  title="Step Into"
                >
                  <StepForward className="w-4 h-4" />
                </button>
              )}
            </>
          )}
          {(executionState.status === "running" || executionState.status === "paused") &&
            onCancel && (
              <button
                onClick={onCancel}
                className="p-1.5 text-gray-400 hover:text-red-400 hover:bg-gray-700 rounded"
                title="Cancel"
              >
                <Square className="w-4 h-4" />
              </button>
            )}
        </div>
      </div>

      {/* Timer */}
      <div className="flex items-center gap-2 px-3 py-2 text-sm text-gray-400 border-b border-gray-700">
        <Timer className="w-4 h-4" />
        <span>{formatDuration(executionState.elapsedMs)}</span>
        <span className="text-gray-500">|</span>
        <span>
          {executionState.stepHistory.length} step
          {executionState.stepHistory.length !== 1 ? "s" : ""} completed
        </span>
      </div>

      {/* Error message */}
      {executionState.error && (
        <div className="mx-3 mt-2 p-2 bg-red-900/30 border border-red-800 rounded text-sm text-red-300">
          {executionState.error}
        </div>
      )}

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto">
        {/* Step History Section */}
        <div className="border-b border-gray-700">
          <button
            onClick={() => toggleSection("history")}
            className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800"
          >
            {expandedSections.history ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
            Step History
          </button>

          {expandedSections.history && (
            <div className="px-3 pb-3 space-y-1">
              {executionState.stepHistory.length === 0 ? (
                <div className="text-gray-500 text-sm">No steps executed yet</div>
              ) : (
                executionState.stepHistory.map((entry, index) => (
                  <button
                    key={`${entry.stepId}-${index}`}
                    onClick={() => {
                      setSelectedHistoryIndex(index);
                      setExpandedSections((prev) => ({ ...prev, outputs: true }));
                    }}
                    className={`w-full flex items-center gap-2 p-2 rounded text-left text-sm transition-colors ${
                      selectedHistoryIndex === index
                        ? "bg-blue-900/30 border border-blue-700"
                        : "bg-gray-800 hover:bg-gray-750"
                    }`}
                  >
                    {entry.success ? (
                      <CheckCircle className="w-4 h-4 text-green-400 flex-shrink-0" />
                    ) : (
                      <XCircle className="w-4 h-4 text-red-400 flex-shrink-0" />
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="text-gray-200 truncate">{entry.stepName}</div>
                      <div className="text-gray-500 text-xs">
                        {entry.stepType} · {formatDuration(entry.durationMs)}
                      </div>
                    </div>
                    <Eye className="w-4 h-4 text-gray-500" />
                  </button>
                ))
              )}
            </div>
          )}
        </div>

        {/* Selected Step Outputs Section */}
        {selectedHistoryIndex !== null && executionState.stepHistory[selectedHistoryIndex] && (
          <div className="border-b border-gray-700">
            <button
              onClick={() => toggleSection("outputs")}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800"
            >
              {expandedSections.outputs ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              Step Outputs
            </button>

            {expandedSections.outputs && (
              <div className="px-3 pb-3">
                <div className="bg-gray-800 rounded p-2 text-sm">
                  <pre className="text-gray-300 overflow-x-auto whitespace-pre-wrap">
                    {JSON.stringify(
                      executionState.stepHistory[selectedHistoryIndex].outputs,
                      null,
                      2,
                    )}
                  </pre>
                  {executionState.stepHistory[selectedHistoryIndex].error && (
                    <div className="mt-2 p-2 bg-red-900/30 border border-red-800 rounded text-red-300">
                      {executionState.stepHistory[selectedHistoryIndex].error}
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>
        )}

        {/* Context Section */}
        <div className="border-b border-gray-700">
          <button
            onClick={() => toggleSection("context")}
            className="w-full flex items-center gap-2 px-3 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800"
          >
            {expandedSections.context ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
            Context Variables
          </button>

          {expandedSections.context && (
            <div className="px-3 pb-3">
              {Object.keys(executionState.context).length === 0 ? (
                <div className="text-gray-500 text-sm">No context variables</div>
              ) : (
                <div className="bg-gray-800 rounded p-2 text-sm max-h-64 overflow-y-auto">
                  <pre className="text-gray-300 whitespace-pre-wrap">
                    {JSON.stringify(executionState.context, null, 2)}
                  </pre>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
