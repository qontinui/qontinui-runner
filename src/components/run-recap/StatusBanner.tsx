/**
 * StatusBanner Component
 *
 * Displays the overall status of a task run with duration.
 */

import { CheckCircle2, XCircle, Clock, Activity, Calendar } from "lucide-react";
import { formatDuration } from "@/lib/formatting";

interface StatusBannerProps {
  status: string;
  goalAchieved?: boolean;
  duration?: number;
  startTime?: string;
  endTime?: string;
}

export function StatusBanner({ status, goalAchieved, duration, startTime, endTime }: StatusBannerProps) {
  const isSuccess = status === "complete" || status === "completed";
  const isFailed = status === "failed";
  const isRunning = status === "running";

  let bgColor = "bg-muted";
  let textColor = "text-muted-foreground";
  let icon = <Clock className="w-5 h-5" />;
  let label = "Unknown Status";

  if (isSuccess) {
    bgColor = "bg-green-500/10";
    textColor = "text-green-500";
    icon = <CheckCircle2 className="w-5 h-5" />;
    label = goalAchieved ? "Goal Achieved" : "Run Completed Successfully";
  } else if (isFailed) {
    bgColor = "bg-red-500/10";
    textColor = "text-red-500";
    icon = <XCircle className="w-5 h-5" />;
    label = "Run Failed";
  } else if (isRunning) {
    bgColor = "bg-blue-500/10";
    textColor = "text-blue-500";
    icon = <Activity className="w-5 h-5 animate-pulse" />;
    label = "Running";
  }

  const formatTime = (isoString?: string) => {
    if (!isoString) return null;
    try {
      return new Date(isoString).toLocaleString();
    } catch {
      return null;
    }
  };

  const formattedStartTime = formatTime(startTime);
  const formattedEndTime = formatTime(endTime);

  return (
    <div data-ui-id="recap-status-banner" className={`${bgColor} ${textColor} rounded-lg p-4 space-y-3`}>
      {/* Main status row */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          {icon}
          <span data-ui-id="recap-status-label" className="font-medium">{label}</span>
          <span
            data-ui-id="recap-goal-indicator"
            className={`text-xs px-2 py-0.5 rounded-full ${
              goalAchieved === true
                ? "bg-green-500/20 text-green-400"
                : goalAchieved === false
                  ? "bg-amber-500/20 text-amber-400"
                  : "bg-muted text-muted-foreground"
            }`}
          >
            {goalAchieved === true ? "Goal ✓" : goalAchieved === false ? "Goal ✗" : "Goal: Pending"}
          </span>
        </div>
        <div data-ui-id="recap-duration" className="flex items-center gap-2 text-sm">
          <Clock className="w-4 h-4" />
          <span>Duration: {duration !== undefined ? formatDuration(duration) : "In progress..."}</span>
        </div>
      </div>

      {/* Timestamps row */}
      {(formattedStartTime || formattedEndTime) && (
        <div data-ui-id="recap-timestamps" className="flex flex-wrap items-center gap-4 text-sm opacity-80">
          {formattedStartTime && (
            <div data-ui-id="recap-start-time" className="flex items-center gap-2">
              <Calendar className="w-4 h-4" />
              <span>Started: {formattedStartTime}</span>
            </div>
          )}
          {formattedEndTime && (
            <div data-ui-id="recap-end-time" className="flex items-center gap-2">
              <Calendar className="w-4 h-4" />
              <span>Ended: {formattedEndTime}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default StatusBanner;
