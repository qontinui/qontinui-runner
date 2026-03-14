/**
 * FindingsSummary Component
 *
 * Compact summary widget for findings.
 * Shows severity counts and actionable issues summary.
 */

import { AlertTriangle, CheckCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import { useFindingsData, getSeverityConfig, getActionableFindingsCount } from "./useFindingsData";
import type { FindingSeverity } from "./types";
import { getSeverityColors, getStatusColors, getAccentColors } from "@/design-system";

/**
 * Props for the FindingsSummary.
 */
type FindingsSummaryProps = BaseWidgetProps;

/**
 * Compact severity badge display using design system severity colors.
 */
function SeverityBadge({ severity, count }: { severity: FindingSeverity; count: number }) {
  if (count === 0) {
    return null;
  }

  const colors = getSeverityColors(severity);
  const config = getSeverityConfig(severity);

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs font-medium",
        colors.bg,
        colors.text,
      )}
    >
      {config.label}
      <span className="opacity-70">{count}</span>
    </span>
  );
}

/**
 * FindingsSummary component.
 * Displays compact findings status for the summary row.
 */
export function FindingsSummary({
  isActive,
  status: _status,
  onRequestFocus,
  className,
}: FindingsSummaryProps) {
  const data = useFindingsData();
  const actionableCount = getActionableFindingsCount(data);
  const hasIssues = actionableCount > 0;
  const hasCritical = data.bySeverity.critical > 0;
  const hasHigh = data.bySeverity.high > 0;

  // Determine the icon color based on highest severity
  const severityLevel = hasCritical ? "critical" : hasHigh ? "high" : hasIssues ? "medium" : null;

  const iconColorClass = severityLevel
    ? getSeverityColors(severityLevel).text
    : getStatusColors("success").text;

  const bgColorClass = severityLevel
    ? getSeverityColors(severityLevel).bg
    : getStatusColors("success").bg;

  return (
    <div
      className={cn(
        "flex items-center gap-3 px-3 py-2 rounded-lg border cursor-pointer",
        "transition-colors hover:bg-muted/50",
        isActive
          ? `${getAccentColors("amber").border} ${getAccentColors("amber").bg}`
          : "border-border",
        className,
      )}
      onClick={() => onRequestFocus?.()}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onRequestFocus?.();
        }
      }}
    >
      {/* Icon */}
      <div
        className={cn(
          "flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center",
          bgColorClass,
        )}
      >
        {hasIssues ? (
          <AlertTriangle className={cn("w-4 h-4", iconColorClass)} />
        ) : (
          <CheckCircle className={cn("w-4 h-4", getStatusColors("success").text)} />
        )}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Findings</span>
          {hasCritical && (
            <span
              className={cn(
                "text-xs font-medium animate-pulse",
                getSeverityColors("critical").text,
              )}
            >
              Critical
            </span>
          )}
        </div>

        {/* Status line */}
        {hasIssues ? (
          <p className="text-xs text-muted-foreground">
            {actionableCount} actionable issue{actionableCount !== 1 ? "s" : ""}
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">No issues detected</p>
        )}
      </div>

      {/* Severity badges */}
      {hasIssues && (
        <div className="flex items-center gap-1.5 flex-wrap">
          <SeverityBadge severity="critical" count={data.bySeverity.critical} />
          <SeverityBadge severity="high" count={data.bySeverity.high} />
          <SeverityBadge severity="medium" count={data.bySeverity.medium} />
          <SeverityBadge severity="low" count={data.bySeverity.low} />
          {data.byStatus.resolved > 0 && (
            <span className={cn("text-xs ml-auto", getStatusColors("success").text)}>
              {data.byStatus.resolved} resolved
            </span>
          )}
        </div>
      )}
    </div>
  );
}

export default FindingsSummary;
