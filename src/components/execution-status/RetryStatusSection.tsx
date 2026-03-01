/**
 * RetryStatusSection Component
 *
 * Displays retry status including current attempt number,
 * error history, next retry delay, and feedback injection status.
 */

import { RefreshCw, AlertCircle, Clock, MessageSquare, CheckCircle2, XCircle } from "lucide-react";
import { Badge } from "../ui/Badge";
import { Progress } from "../ui/Progress";
import { type RetryStatus, formatDurationMs } from "../../types/executionStatus";
import { getStatusColors, getAccentColors } from "@/design-system";

interface RetryStatusSectionProps {
  retry: RetryStatus;
}

export function RetryStatusSection({ retry }: RetryStatusSectionProps) {
  const { enabled, maxRetries, feedbackInjection, state, nextRetryDelayMs, exhausted } = retry;

  if (!enabled) {
    return (
      <div className="space-y-3">
        <div className="flex items-center gap-2 text-muted-foreground">
          <RefreshCw className="w-4 h-4" />
          <span className="text-sm">Automatic retry is disabled</span>
        </div>
        <p className="text-xs text-muted-foreground">
          Enable automatic retry in settings to recover from transient errors.
        </p>
      </div>
    );
  }

  const currentAttempt = state.attempt + 1; // Display as 1-indexed
  const hasErrors = state.errorHistory.length > 0;
  const progressPercent = (state.attempt / maxRetries) * 100;

  return (
    <div className="space-y-4">
      {/* Attempt Progress */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <RefreshCw className="w-4 h-4 text-primary" />
            <span className="text-sm font-medium">Retry Status</span>
          </div>
          <Badge
            variant={exhausted ? "danger" : hasErrors ? "warning" : "muted"}
            className="flex items-center gap-1"
          >
            {exhausted ? (
              <XCircle className="w-3 h-3" />
            ) : hasErrors ? (
              <AlertCircle className="w-3 h-3" />
            ) : (
              <CheckCircle2 className="w-3 h-3" />
            )}
            <span>
              Attempt {currentAttempt} of {maxRetries + 1}
            </span>
          </Badge>
        </div>

        {/* Progress Bar */}
        {hasErrors && (
          <div className="space-y-1">
            <Progress
              value={progressPercent}
              className={`h-1.5 ${exhausted ? "[&>div]:bg-red-500" : ""}`}
            />
            <div className="flex items-center justify-between text-xs text-muted-foreground">
              <span>{state.attempt} retries used</span>
              <span>{maxRetries - state.attempt} remaining</span>
            </div>
          </div>
        )}
      </div>

      {/* Next Retry Delay */}
      {nextRetryDelayMs !== null && !exhausted && (
        <div className="flex items-center gap-2 text-sm">
          <Clock className="w-4 h-4 text-amber-500" />
          <span className="text-muted-foreground">Next retry in</span>
          <span className={`font-mono ${getAccentColors("amber").text}`}>
            {formatDurationMs(nextRetryDelayMs)}
          </span>
        </div>
      )}

      {/* Feedback Injection Status */}
      <div className="flex items-center gap-2 text-sm">
        <MessageSquare className="w-4 h-4 text-muted-foreground" />
        <span className="text-muted-foreground">Feedback injection</span>
        <Badge variant={feedbackInjection ? "success" : "muted"} size="sm">
          {feedbackInjection ? "Enabled" : "Disabled"}
        </Badge>
      </div>

      {/* Total Delay */}
      {state.totalDelayMs > 0 && (
        <div className="text-xs text-muted-foreground">
          Total delay: {formatDurationMs(state.totalDelayMs)}
        </div>
      )}

      {/* Last Error */}
      {state.lastError && (
        <div className="space-y-1">
          <span className="text-xs text-muted-foreground">Last Error</span>
          <div
            className={`text-xs rounded px-2 py-1.5 ${getStatusColors("error").bg} ${getStatusColors("error").text}`}
          >
            <p className="line-clamp-2">{state.lastError}</p>
          </div>
        </div>
      )}

      {/* Error History */}
      {state.errorHistory.length > 0 && (
        <div className="space-y-2">
          <span className="text-xs text-muted-foreground">
            Retry History ({state.errorHistory.length})
          </span>
          <div className="space-y-1 max-h-32 overflow-y-auto">
            {state.errorHistory
              .slice()
              .reverse()
              .map((attempt) => (
                <div
                  key={attempt.attemptNumber}
                  className="text-xs bg-muted/30 rounded px-2 py-1.5 space-y-0.5"
                >
                  <div className="flex items-center justify-between">
                    <span className="font-medium text-foreground">
                      Attempt {attempt.attemptNumber}
                    </span>
                    <div className="flex items-center gap-2">
                      {attempt.feedbackInjected && (
                        <Badge variant="info" size="sm" className="px-1 py-0">
                          <MessageSquare className="w-2.5 h-2.5" />
                        </Badge>
                      )}
                      <span className="text-muted-foreground">
                        {formatDurationMs(attempt.delayMs)} delay
                      </span>
                    </div>
                  </div>
                  <p className="text-muted-foreground line-clamp-1">{attempt.error}</p>
                </div>
              ))}
          </div>
        </div>
      )}

      {/* Exhausted Warning */}
      {exhausted && (
        <div
          className={`flex items-center gap-2 text-sm rounded px-2 py-1.5 ${getStatusColors("error").bg} ${getStatusColors("error").text}`}
        >
          <XCircle className="w-4 h-4" />
          <span>All retries exhausted</span>
        </div>
      )}

      {/* No Errors */}
      {!hasErrors && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <CheckCircle2 className="w-4 h-4 text-green-500" />
          <span>No retries needed</span>
        </div>
      )}
    </div>
  );
}
