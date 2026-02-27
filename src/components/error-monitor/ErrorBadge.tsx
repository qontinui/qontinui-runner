/**
 * ErrorBadge.tsx
 *
 * Small badge component showing error count and severity for the status bar.
 */

import { AlertCircle, Bug, AlertTriangle } from "lucide-react";
import { cn } from "../../lib/utils";
import { useErrorBadge } from "../../hooks/useErrorMonitor";
import type { ErrorSeverity } from "../../types/errorMonitor";

interface ErrorBadgeProps {
  /** Task run ID to scope errors to */
  taskRunId?: string;
  /** Click handler */
  onClick?: () => void;
  /** Optional className */
  className?: string;
}

export function ErrorBadge({ taskRunId, onClick, className }: ErrorBadgeProps) {
  const { count, hasActionable, highestSeverity } = useErrorBadge(taskRunId);

  // Don't render if no errors
  if (count === 0) return null;

  const getSeverityIcon = (severity: ErrorSeverity | null) => {
    switch (severity) {
      case "critical":
        return <AlertCircle className="w-3.5 h-3.5" />;
      case "error":
        return <Bug className="w-3.5 h-3.5" />;
      case "warning":
        return <AlertTriangle className="w-3.5 h-3.5" />;
      default:
        return <Bug className="w-3.5 h-3.5" />;
    }
  };

  const getColorClasses = (severity: ErrorSeverity | null) => {
    if (!hasActionable) {
      return "bg-muted text-muted-foreground";
    }
    switch (severity) {
      case "critical":
        return "bg-red-600/20 text-red-600";
      case "error":
        return "bg-red-500/20 text-red-500";
      case "warning":
        return "bg-yellow-500/20 text-yellow-500";
      default:
        return "bg-muted text-muted-foreground";
    }
  };

  return (
    <button
      onClick={onClick}
      className={cn(
        "flex items-center gap-1.5 px-2 py-1 rounded-full transition-colors",
        getColorClasses(highestSeverity),
        onClick && "hover:opacity-80 cursor-pointer",
        className,
      )}
      title={`${count} unresolved error${count !== 1 ? "s" : ""}`}
    >
      {getSeverityIcon(highestSeverity)}
      <span className="text-xs font-medium">{count}</span>
    </button>
  );
}

export default ErrorBadge;
