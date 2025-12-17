/**
 * RagLogTab.tsx
 *
 * Tab component for displaying RAG (embedding) processing logs and status.
 * Shows real-time progress of embedding generation and results.
 */

import { Database, CheckCircle, XCircle, Loader2, Play, RefreshCw } from "lucide-react";

export type RagProcessingStatus = "idle" | "processing" | "completed" | "failed";

export interface RagLogEntry {
  id: string;
  timestamp: Date;
  type: "info" | "progress" | "success" | "error" | "warning";
  message: string;
  details?: {
    projectId?: string;
    percent?: number;
    elementsProcessed?: number;
    totalElements?: number;
    stateImageId?: string;
    stateImageName?: string;
  };
}

export interface RagProcessingState {
  status: RagProcessingStatus;
  projectId: string | null;
  projectName: string | null;
  percent: number;
  elementsProcessed: number;
  totalElements: number;
  lastError: string | null;
  logs: RagLogEntry[];
}

interface RagLogTabProps {
  state: RagProcessingState;
  onStartProcessing: () => void;
  onClearLogs: () => void;
  canStartProcessing: boolean;
}

export function RagLogTab({
  state,
  onStartProcessing,
  onClearLogs,
  canStartProcessing,
}: RagLogTabProps) {
  const { status, projectName, percent, elementsProcessed, totalElements, logs, lastError } = state;

  const getStatusIcon = () => {
    switch (status) {
      case "processing":
        return <Loader2 className="w-5 h-5 animate-spin text-blue-500" />;
      case "completed":
        return <CheckCircle className="w-5 h-5 text-green-500" />;
      case "failed":
        return <XCircle className="w-5 h-5 text-red-500" />;
      default:
        return <Database className="w-5 h-5 text-muted-foreground" />;
    }
  };

  const getStatusText = () => {
    switch (status) {
      case "processing":
        return `Processing embeddings... ${percent}%`;
      case "completed":
        return `Completed - ${elementsProcessed} embeddings generated`;
      case "failed":
        return `Failed: ${lastError || "Unknown error"}`;
      default:
        return "Ready to process";
    }
  };

  const getLogEntryClass = (type: RagLogEntry["type"]) => {
    switch (type) {
      case "error":
        return "text-red-500";
      case "success":
        return "text-green-500";
      case "warning":
        return "text-yellow-500";
      case "progress":
        return "text-blue-500";
      default:
        return "text-foreground";
    }
  };

  return (
    <div className="h-full flex flex-col">
      {/* Status Header */}
      <div className="flex items-center justify-between p-4 border-b border-border bg-muted/30">
        <div className="flex items-center gap-3">
          {getStatusIcon()}
          <div>
            <div className="font-medium">{getStatusText()}</div>
            {projectName && (
              <div className="text-sm text-muted-foreground">Project: {projectName}</div>
            )}
          </div>
        </div>

        <div className="flex items-center gap-2">
          <button
            onClick={onStartProcessing}
            disabled={!canStartProcessing || status === "processing"}
            className={`
              flex items-center gap-2 px-3 py-1.5 text-sm font-medium rounded-md
              ${
                canStartProcessing && status !== "processing"
                  ? "bg-primary text-primary-foreground hover:bg-primary/90"
                  : "bg-muted text-muted-foreground cursor-not-allowed"
              }
            `}
          >
            {status === "processing" ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Processing...
              </>
            ) : (
              <>
                <Play className="w-4 h-4" />
                Generate Embeddings
              </>
            )}
          </button>

          <button
            onClick={onClearLogs}
            disabled={logs.length === 0}
            className="flex items-center gap-2 px-3 py-1.5 text-sm font-medium rounded-md bg-muted hover:bg-muted/80 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <RefreshCw className="w-4 h-4" />
            Clear
          </button>
        </div>
      </div>

      {/* Progress Bar (when processing) */}
      {status === "processing" && (
        <div className="px-4 py-2 bg-muted/20">
          <div className="flex items-center justify-between text-sm mb-1">
            <span>
              {elementsProcessed} / {totalElements || "?"} elements
            </span>
            <span>{percent}%</span>
          </div>
          <div className="w-full bg-muted rounded-full h-2">
            <div
              className="bg-primary h-2 rounded-full transition-all duration-300"
              style={{ width: `${percent}%` }}
            />
          </div>
        </div>
      )}

      {/* Log Entries */}
      <div className="flex-1 overflow-y-auto p-4">
        {logs.length === 0 ? (
          <div className="text-center text-muted-foreground py-8">
            <Database className="w-12 h-12 mx-auto mb-3 opacity-50" />
            <p className="font-medium">No RAG processing logs yet</p>
            <p className="text-sm mt-1">
              {canStartProcessing
                ? "Click 'Generate Embeddings' to start processing RAG-enabled StateImages"
                : "Load a configuration with RAG-enabled StateImages to start"}
            </p>
          </div>
        ) : (
          <div className="space-y-2 font-mono text-sm">
            {logs.map((entry) => (
              <div
                key={entry.id}
                className={`flex items-start gap-3 p-2 rounded hover:bg-muted/50 ${getLogEntryClass(entry.type)}`}
              >
                <span className="text-muted-foreground whitespace-nowrap">
                  {entry.timestamp.toLocaleTimeString()}
                </span>
                <span className="flex-1">{entry.message}</span>
                {entry.details?.percent !== undefined && (
                  <span className="text-muted-foreground">{entry.details.percent}%</span>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
