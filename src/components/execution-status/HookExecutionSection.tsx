/**
 * HookExecutionSection Component
 *
 * Displays lifecycle hook execution status including configured hooks,
 * execution history, and currently executing hooks.
 */

import {
  Webhook,
  Play,
  CheckCircle2,
  XCircle,
  Clock,
  Loader2,
  Terminal,
  Bell,
  FileText,
  Globe,
} from "lucide-react";
import { Badge } from "../ui/Badge";
import {
  type HookStatus,
  type HookTrigger,
  type HookExecutionResult,
  getHookTriggerDisplayName,
  formatDurationMs,
} from "../../types/executionStatus";
import { getStatusColors } from "@/design-system";

interface HookExecutionSectionProps {
  hooks: HookStatus;
}

/** Get icon for hook action type */
function getActionIcon(actionType: string) {
  switch (actionType.toLowerCase()) {
    case "command":
      return <Terminal className="w-3 h-3" />;
    case "webhook":
      return <Globe className="w-3 h-3" />;
    case "log":
      return <FileText className="w-3 h-3" />;
    case "notification":
      return <Bell className="w-3 h-3" />;
    default:
      return <Webhook className="w-3 h-3" />;
  }
}

/** Get badge variant for hook trigger */
function getTriggerBadgeVariant(
  trigger: HookTrigger,
): "default" | "muted" | "success" | "warning" | "danger" | "info" | "purple" {
  switch (trigger) {
    case "pre_execution":
    case "pre_iteration":
      return "info";
    case "post_execution":
    case "post_iteration":
      return "purple";
    case "on_error":
    case "on_verification_fail":
      return "danger";
    case "on_complete":
      return "success";
    default:
      return "muted";
  }
}

/** Format timestamp for display */
function formatTimestamp(timestamp: string): string {
  try {
    const date = new Date(timestamp);
    return date.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return timestamp;
  }
}

interface HookResultItemProps {
  result: HookExecutionResult;
}

function HookResultItem({ result }: HookResultItemProps) {
  const isSuccess = result.success;

  return (
    <div
      className={`text-xs rounded px-2 py-1.5 space-y-1 ${
        isSuccess ? "bg-muted/30" : `${getStatusColors("error").bg}`
      }`}
    >
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          {isSuccess ? (
            <CheckCircle2 className="w-3 h-3 text-green-500" />
          ) : (
            <XCircle className="w-3 h-3 text-red-500" />
          )}
          <span className="font-medium text-foreground">{result.hookName}</span>
        </div>
        <div className="flex items-center gap-2">
          <Badge variant={getTriggerBadgeVariant(result.trigger)} size="sm">
            {getHookTriggerDisplayName(result.trigger)}
          </Badge>
        </div>
      </div>

      {/* Details */}
      <div className="flex items-center gap-3 text-muted-foreground">
        <span className="flex items-center gap-1">
          <Clock className="w-3 h-3" />
          {formatDurationMs(result.durationMs)}
        </span>
        <span>{formatTimestamp(result.timestamp)}</span>
      </div>

      {/* Error Message */}
      {!isSuccess && result.error && (
        <div className={`${getStatusColors("error").text} line-clamp-2`}>{result.error}</div>
      )}

      {/* Output (truncated) */}
      {isSuccess && result.output && (
        <div className="text-muted-foreground line-clamp-2 font-mono text-[10px]">
          {result.output}
        </div>
      )}
    </div>
  );
}

export function HookExecutionSection({ hooks }: HookExecutionSectionProps) {
  const { hooks: configuredHooks, executionHistory, currentlyExecuting } = hooks;

  const hasHooks = configuredHooks.length > 0;
  const hasHistory = executionHistory.length > 0;

  // Count successes and failures
  const successCount = executionHistory.filter((h) => h.success).length;
  const failureCount = executionHistory.filter((h) => !h.success).length;

  // Find currently executing hook
  const executingHook = currentlyExecuting
    ? configuredHooks.find((h) => h.id === currentlyExecuting)
    : null;

  return (
    <div className="space-y-4">
      {/* Header with summary */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Webhook className="w-4 h-4 text-primary" />
          <span className="text-sm font-medium">Lifecycle Hooks</span>
        </div>
        {hasHistory && (
          <div className="flex items-center gap-2">
            <Badge variant="success" size="sm">
              <CheckCircle2 className="w-3 h-3 mr-0.5" />
              {successCount}
            </Badge>
            {failureCount > 0 && (
              <Badge variant="danger" size="sm">
                <XCircle className="w-3 h-3 mr-0.5" />
                {failureCount}
              </Badge>
            )}
          </div>
        )}
      </div>

      {/* Currently Executing */}
      {executingHook && (
        <div className="flex items-center gap-2 text-sm bg-primary/10 rounded px-2 py-1.5">
          <Loader2 className="w-4 h-4 text-primary animate-spin" />
          <span className="text-primary">
            Executing: <span className="font-medium">{executingHook.name}</span>
          </span>
        </div>
      )}

      {/* Configured Hooks */}
      {hasHooks && (
        <div className="space-y-2">
          <span className="text-xs text-muted-foreground">
            Configured Hooks ({configuredHooks.length})
          </span>
          <div className="grid grid-cols-2 gap-1">
            {configuredHooks.map((hook) => (
              <div
                key={hook.id}
                className={`text-xs rounded px-2 py-1 flex items-center gap-1.5 ${
                  hook.enabled ? "bg-muted/30" : "bg-muted/10 opacity-50"
                } ${currentlyExecuting === hook.id ? "ring-1 ring-primary" : ""}`}
              >
                {getActionIcon(hook.actionType)}
                <span className="truncate text-foreground">{hook.name}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Execution History */}
      {hasHistory && (
        <div className="space-y-2">
          <span className="text-xs text-muted-foreground">
            Execution History ({executionHistory.length})
          </span>
          <div className="space-y-1.5 max-h-48 overflow-y-auto">
            {executionHistory
              .slice()
              .reverse()
              .map((result, index) => (
                <HookResultItem key={index} result={result} />
              ))}
          </div>
        </div>
      )}

      {/* No Hooks */}
      {!hasHooks && (
        <div className="space-y-2">
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Webhook className="w-4 h-4" />
            <span>No lifecycle hooks configured</span>
          </div>
          <p className="text-xs text-muted-foreground">
            Configure hooks to run commands, call webhooks, or log messages at different points in
            the execution lifecycle.
          </p>
        </div>
      )}

      {/* No History */}
      {hasHooks && !hasHistory && !currentlyExecuting && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Play className="w-4 h-4" />
          <span>No hooks executed yet</span>
        </div>
      )}
    </div>
  );
}
