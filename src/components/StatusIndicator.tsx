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
import { instanceStorage } from "@/lib/instance-storage";
import type { BackgroundActivity, ActivityType } from "../hooks/useBackgroundActivities";
import { getSeverityColors, getAccentColors } from "@/design-system";
import { DoctorHealthBadge } from "./doctor";

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

// Helper to get color for activity type using design system
const getActivityColor = (type: ActivityType) => {
  const colorMap: Record<ActivityType, string> = {
    extraction: "blue",
    ai: "amber",
    sync: "green",
  };
  return getAccentColors(colorMap[type] || "slate").text;
};

const StatusIndicator: React.FC<StatusIndicatorProps> = ({
  pythonStatus,
  executionActive,
  backgroundActivities = [],
}) => {
  const [error, setError] = useState<ErrorEvent | null>(null);
  const [showError, setShowError] = useState(false);
  const [isBeta] = useState(true);
  const [runnerName, setRunnerName] = useState<string>(() => {
    return instanceStorage.getItem(RUNNER_NAME_STORAGE_KEY) || "";
  });
  const [projectName, setProjectName] = useState<string | null>(() => {
    const parsed = instanceStorage.getJSON<{ selectedProjectName?: string } | null>(
      SELECTED_PROJECT_STORAGE_KEY,
      null,
    );
    return parsed?.selectedProjectName || null;
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

  // Use design system for severity colors
  const getErrorSeverityClasses = (severity: string) => {
    const colors = getSeverityColors(severity);
    return `${colors.bg} ${colors.text} ${colors.border}`;
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
      {/* Status Bar */}
      <div className="flex items-center gap-4 px-4 py-2 bg-card border-b border-border">
        {/* Title */}
        <div className="flex items-center gap-2 pr-3 border-r border-border">
          <div
            className={`w-2 h-2 rounded-full ${pythonStatus === "running" ? getAccentColors("green").bgSolid : "bg-muted-foreground"}`}
          />
          <span
            data-content-role="heading"
            data-content-level="1"
            className="text-sm font-bold text-foreground"
          >
            Qontinui Runner
          </span>
          {isBeta && (
            <span
              data-content-role="badge"
              className="px-1.5 py-0.5 text-[10px] font-semibold bg-purple-500/20 text-purple-400 rounded"
            >
              BETA
            </span>
          )}
        </div>

        {/* Runner Name */}
        {runnerName && (
          <div className="flex items-center gap-2 pr-3 border-r border-border">
            <Tag className="w-4 h-4 text-primary" />
            <span
              data-content-role="label"
              data-content-label="runner name"
              className="text-sm font-medium text-foreground"
            >
              {runnerName}
            </span>
          </div>
        )}

        {/* Selected Project */}
        {projectName && (
          <div className="flex items-center gap-2 pr-3 border-r border-border">
            <FolderKanban className={`w-4 h-4 ${getAccentColors("blue").text}`} />
            <span
              data-content-role="label"
              data-content-label="project name"
              className="text-sm font-medium text-foreground"
            >
              {projectName}
            </span>
          </div>
        )}

        {/* Doctor Health Badge - Shows when AI processes appear stuck */}
        <DoctorHealthBadge />

        {/* Active Processes Section - Only shown when there is activity */}
        {(executionActive || backgroundActivities.length > 0) && (
          <>
            <div className="h-4 w-px bg-border" />
            <div className="flex items-center gap-3">
              {/* GUI Automation - Only shown when active */}
              {executionActive && (
                <div
                  className={`flex items-center gap-1.5 px-2 py-0.5 rounded-full ${getAccentColors("blue").bg}`}
                  title="GUI Automation workflow is running"
                >
                  <Play className={`w-3.5 h-3.5 animate-pulse ${getAccentColors("blue").text}`} />
                  <span
                    data-content-role="status"
                    data-content-label="GUI automation active"
                    className={`text-xs font-medium ${getAccentColors("blue").text}`}
                  >
                    GUI Automation
                  </span>
                </div>
              )}

              {/* Background Activities */}
              {backgroundActivities.map((activity) => {
                const Icon = getActivityIcon(activity.type);
                const colorClass = getActivityColor(activity.type);
                return (
                  <div
                    key={activity.id}
                    className="flex items-center gap-1.5 px-2 py-0.5 bg-card rounded-full"
                    title={activity.detail || activity.label}
                  >
                    <Icon className={`w-3.5 h-3.5 ${colorClass} animate-pulse`} />
                    <span
                      data-content-role="status"
                      className="text-xs font-medium text-foreground truncate max-w-[200px]"
                    >
                      {activity.label}
                    </span>
                    {activity.progress !== undefined && (
                      <span
                        data-content-role="metric"
                        data-content-label={`${activity.label} progress`}
                        className="text-xs text-muted-foreground"
                      >
                        {activity.progress}%
                      </span>
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
          className={`fixed top-20 right-4 max-w-md z-40 p-4 rounded-lg shadow-lg border ${getErrorSeverityClasses(error.severity)}`}
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
