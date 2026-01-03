import React, { useState, useEffect } from "react";
import {
  AlertCircle,
  Info,
  AlertTriangle,
  X,
  Tag,
  FolderKanban,
  Globe,
  Bot,
  Cloud,
  Play,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import type { BackgroundActivity, ActivityType } from "../hooks/useBackgroundActivities";

const RUNNER_NAME_STORAGE_KEY = "qontinui-runner-name";
const SELECTED_PROJECT_STORAGE_KEY = "qontinui-selected-project";

interface ErrorEvent {
  title: string;
  message: string;
  details?: string;
  error_code: string;
  severity: "info" | "warning" | "error" | "critical";
  recoverable: boolean;
  suggested_action?: string;
}

interface StatusIndicatorProps {
  pythonStatus: "stopped" | "running";
  configLoaded: boolean;
  executionActive: boolean;
  backgroundActivities?: BackgroundActivity[];
}

// Helper to get icon for activity type
const getActivityIcon = (type: ActivityType) => {
  switch (type) {
    case "extraction":
      return Globe;
    case "ai":
      return Bot;
    case "sync":
      return Cloud;
    default:
      return Info;
  }
};

// Helper to get color for activity type
const getActivityColor = (type: ActivityType) => {
  switch (type) {
    case "extraction":
      return "text-blue-500";
    case "ai":
      return "text-amber-500";
    case "sync":
      return "text-green-500";
    default:
      return "text-gray-500";
  }
};

const StatusIndicator: React.FC<StatusIndicatorProps> = ({
  pythonStatus,
  configLoaded,
  executionActive,
  backgroundActivities = [],
}) => {
  const [error, setError] = useState<ErrorEvent | null>(null);
  const [showError, setShowError] = useState(false);
  const [isBeta] = useState(true);
  const [runnerName, setRunnerName] = useState<string>(() => {
    return localStorage.getItem(RUNNER_NAME_STORAGE_KEY) || "";
  });
  const [projectName, setProjectName] = useState<string | null>(() => {
    const stored = localStorage.getItem(SELECTED_PROJECT_STORAGE_KEY);
    if (stored) {
      try {
        const parsed = JSON.parse(stored);
        return parsed.selectedProjectName || null;
      } catch {
        return null;
      }
    }
    return null;
  });

  useEffect(() => {
    const unlistenError = listen<ErrorEvent>("error", (event) => {
      setError(event.payload);
      setShowError(true);

      // Auto-hide info messages after 5 seconds
      if (event.payload.severity === "info") {
        setTimeout(() => setShowError(false), 5000);
      }
    });

    // Listen for runner name changes from settings
    const handleRunnerNameChange = (event: CustomEvent<string>) => {
      setRunnerName(event.detail);
    };
    window.addEventListener("runner-name-changed", handleRunnerNameChange as EventListener);

    // Listen for project selection changes from settings
    const handleProjectSelectionChange = (
      event: CustomEvent<{ projectId: string | null; projectName: string | null }>,
    ) => {
      setProjectName(event.detail.projectName);
    };
    window.addEventListener(
      "project-selection-changed",
      handleProjectSelectionChange as EventListener,
    );

    return () => {
      unlistenError.then((fn) => fn());
      window.removeEventListener("runner-name-changed", handleRunnerNameChange as EventListener);
      window.removeEventListener(
        "project-selection-changed",
        handleProjectSelectionChange as EventListener,
      );
    };
  }, []);

  const getSeverityColor = (severity: string) => {
    switch (severity) {
      case "info":
        return "bg-blue-100 text-blue-800 border-blue-200";
      case "warning":
        return "bg-yellow-100 text-yellow-800 border-yellow-200";
      case "error":
        return "bg-red-100 text-red-800 border-red-200";
      case "critical":
        return "bg-red-200 text-red-900 border-red-300";
      default:
        return "bg-gray-100 text-gray-800 border-gray-200";
    }
  };

  const getSeverityIcon = (severity: string) => {
    switch (severity) {
      case "info":
        return <Info className="w-5 h-5" />;
      case "warning":
        return <AlertTriangle className="w-5 h-5" />;
      case "error":
        return <AlertCircle className="w-5 h-5" />;
      case "critical":
        return <AlertCircle className="w-5 h-5" />;
      default:
        return <Info className="w-5 h-5" />;
    }
  };

  return (
    <div className="relative">
      {/* Beta Badge */}
      {isBeta && (
        <div className="fixed top-4 right-4 z-50">
          <span className="px-3 py-1 text-xs font-semibold bg-gradient-to-r from-purple-500 to-indigo-500 text-white rounded-full shadow-lg">
            BETA
          </span>
        </div>
      )}

      {/* Status Bar */}
      <div className="flex items-center gap-4 px-4 py-2 bg-gray-50 border-b border-gray-200">
        {/* Title */}
        <div className="flex items-center gap-2 pr-3 border-r border-gray-300">
          <div
            className={`w-2 h-2 rounded-full ${pythonStatus === "running" ? "bg-green-500" : "bg-gray-400"}`}
          />
          <span className="text-sm font-bold text-gray-800">Qontinui Runner</span>
        </div>

        {/* Runner Name */}
        {runnerName && (
          <div className="flex items-center gap-2 pr-3 border-r border-gray-300">
            <Tag className="w-4 h-4 text-primary" />
            <span className="text-sm font-medium text-gray-800">{runnerName}</span>
          </div>
        )}

        {/* Selected Project */}
        {projectName && (
          <div className="flex items-center gap-2 pr-3 border-r border-gray-300">
            <FolderKanban className="w-4 h-4 text-blue-500" />
            <span className="text-sm font-medium text-gray-800">{projectName}</span>
          </div>
        )}

        <div className="flex items-center gap-2">
          <div
            className={`w-2 h-2 rounded-full ${configLoaded ? "bg-green-500" : "bg-gray-400"}`}
          />
          <span className="text-sm text-gray-600">
            Config: {configLoaded ? "Loaded" : "Not Loaded"}
          </span>
        </div>

        {/* Active Processes Section - Only shown when there is activity */}
        {(executionActive || backgroundActivities.length > 0) && (
          <>
            <div className="h-4 w-px bg-gray-300" />
            <div className="flex items-center gap-3">
              {/* GUI Automation - Only shown when active */}
              {executionActive && (
                <div
                  className="flex items-center gap-1.5 px-2 py-0.5 bg-blue-100 rounded-full"
                  title="GUI Automation workflow is running"
                >
                  <Play className="w-3.5 h-3.5 text-blue-600 animate-pulse" />
                  <span className="text-xs font-medium text-blue-700">GUI Automation</span>
                </div>
              )}

              {/* Background Activities */}
              {backgroundActivities.map((activity) => {
                const Icon = getActivityIcon(activity.type);
                const colorClass = getActivityColor(activity.type);
                return (
                  <div
                    key={activity.id}
                    className="flex items-center gap-1.5 px-2 py-0.5 bg-gray-100 rounded-full"
                    title={activity.detail || activity.label}
                  >
                    <Icon className={`w-3.5 h-3.5 ${colorClass} animate-pulse`} />
                    <span className="text-xs font-medium text-gray-700">{activity.label}</span>
                    {activity.progress !== undefined && (
                      <span className="text-xs text-gray-500">{activity.progress}%</span>
                    )}
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>

      {/* Error Display */}
      {showError && error && (
        <div
          className={`fixed top-20 right-4 max-w-md z-40 p-4 rounded-lg shadow-lg border ${getSeverityColor(error.severity)}`}
        >
          <div className="flex items-start gap-3">
            <div className="flex-shrink-0 mt-0.5">{getSeverityIcon(error.severity)}</div>
            <div className="flex-1">
              <div className="flex justify-between items-start">
                <h3 className="font-semibold">{error.title}</h3>
                <button
                  onClick={() => setShowError(false)}
                  className="ml-2 p-1 hover:bg-black/10 rounded"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>
              <p className="mt-1 text-sm">{error.message}</p>
              {error.details && (
                <details className="mt-2">
                  <summary className="text-xs cursor-pointer hover:underline">
                    Technical Details
                  </summary>
                  <pre className="mt-1 text-xs bg-black/10 p-2 rounded overflow-x-auto">
                    {error.details}
                  </pre>
                </details>
              )}
              {error.suggested_action && (
                <p className="mt-2 text-sm font-medium">💡 {error.suggested_action}</p>
              )}
              <p className="mt-2 text-xs opacity-60">Error Code: {error.error_code}</p>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default StatusIndicator;
