import React, { useState, useEffect, useReducer, useRef } from "react";
import {
  Info,
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
import { getAccentColors } from "@/design-system";
import { DoctorHealthBadge } from "./doctor";
import { ErrorBanner, ErrorResidual } from "./ErrorBanner";
import {
  INITIAL_ERROR_BANNER_STATE,
  errorBannerReducer,
  errorBannerView,
  type ErrorEventPayload,
} from "./errorBannerState";

const RUNNER_NAME_STORAGE_KEY = "qontinui-runner-name";
const SELECTED_PROJECT_STORAGE_KEY = "qontinui-selected-project";

/** Payload of the backend's `error-cleared` event: the fault was observed gone. */
interface ErrorClearedEvent {
  error_code: string;
}

/** How long a transient `info` event stays up before it auto-hides. */
const INFO_AUTO_HIDE_MS = 5000;
/** Re-render cadence for the residual's "last seen" age. */
const RESIDUAL_TICK_MS = 15000;

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
  const [banner, dispatchBanner] = useReducer(errorBannerReducer, INITIAL_ERROR_BANNER_STATE);
  const infoTimers = useRef(new Set<ReturnType<typeof setTimeout>>());
  // Bumped only to re-render the residuals' "last seen" age; the age itself is
  // read from the clock at render time, so it is never stale.
  const [, setTick] = useState(0);
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
    const timers = infoTimers.current;
    const unlistenError = listen<ErrorEventPayload>("error", (event) => {
      const at = Date.now();
      dispatchBanner({ type: "received", error: event.payload, at });
      // Only an `info` is transient. The expiry names the occurrence it was
      // armed for, so it can never hide a later event (the reducer ignores it).
      if (event.payload.severity === "info") {
        const code = event.payload.error_code;
        const timer = setTimeout(() => {
          timers.delete(timer);
          dispatchBanner({ type: "expired", errorCode: code, at });
        }, INFO_AUTO_HIDE_MS);
        timers.add(timer);
      }
    });
    // The emitter observed the fault gone: the banner or residual for that code
    // goes away. Emitters that never report a clear leave the residual standing,
    // with its last-seen time, rather than inventing one.
    const unlistenCleared = listen<ErrorClearedEvent>("error-cleared", (event) => {
      dispatchBanner({ type: "cleared", errorCode: event.payload.error_code });
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
      unlistenCleared.then((fn) => fn());
      timers.forEach((t) => clearTimeout(t));
      timers.clear();
      window.removeEventListener("runner-name-changed", handleRunnerNameChange as EventListener);
      window.removeEventListener(
        "project-selection-changed",
        handleProjectSelectionChange as EventListener,
      );
    };
  }, []);

  const bannerView = errorBannerView(banner);
  const hasResiduals = bannerView.residuals.length > 0;
  const now = Date.now();

  // Keep the residuals' "last seen" age honest while one is on screen.
  useEffect(() => {
    if (!hasResiduals) return;
    const id = setInterval(() => setTick((t) => t + 1), RESIDUAL_TICK_MS);
    return () => clearInterval(id);
  }, [hasResiduals]);

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

        {/* A dismissed fault stays visible here until it is observed cleared */}
        {bannerView.residuals.map((f) => (
          <ErrorResidual
            key={f.error.error_code}
            error={f.error}
            lastSeenAt={f.lastSeenAt}
            now={now}
            onReopen={() => dispatchBanner({ type: "reopened", errorCode: f.error.error_code })}
          />
        ))}

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

      {/* Error Display — in flow, under the status bar, never over a pane */}
      {bannerView.banners.map((error) => (
        <ErrorBanner
          key={error.error_code}
          error={error}
          onDismiss={() => dispatchBanner({ type: "dismissed", errorCode: error.error_code })}
        />
      ))}
    </div>
  );
};

export default StatusIndicator;
