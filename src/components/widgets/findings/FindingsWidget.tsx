/**
 * FindingsWidget Component
 *
 * Full-size widget displaying AI-detected issues (bugs, security, performance, etc.)
 * Shows severity summary, category breakdown, and scrollable list of findings.
 */

import {
  AlertTriangle,
  Bug,
  Shield,
  Zap,
  CheckCircle,
  Sparkles,
  Settings,
  FlaskConical,
  FileText,
  ExternalLink,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/ScrollArea";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import {
  useFindingsData,
  getSeverityConfig,
  getCategoryConfig,
  getStatusConfig,
} from "./useFindingsData";
import type { Finding, FindingSeverity, FindingCategory } from "./types";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Props for the FindingsWidget.
 */
type FindingsWidgetProps = BaseWidgetProps;

/**
 * Icon mapping for categories.
 */
const categoryIconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Bug,
  Shield,
  Zap,
  CheckCircle,
  Sparkles,
  Settings,
  FlaskConical,
  FileText,
  AlertTriangle,
};

/**
 * Get the icon component for a category.
 */
function getCategoryIcon(category: FindingCategory): React.ComponentType<{ className?: string }> {
  const config = getCategoryConfig(category);
  return categoryIconMap[config.icon] ?? AlertTriangle;
}

/**
 * Severity count badge component.
 */
function SeverityBadge({ severity, count }: { severity: FindingSeverity; count: number }) {
  const config = getSeverityConfig(severity);

  if (count === 0) {
    return null;
  }

  return (
    <div
      className={cn(
        "flex items-center gap-1.5 px-2 py-1 rounded-md text-xs font-medium",
        config.bgColor,
        config.borderColor,
        "border",
      )}
    >
      <span className={config.color}>{count}</span>
      <span className="text-muted-foreground">{config.label}</span>
    </div>
  );
}

/**
 * Category badge component.
 */
function CategoryBadge({ category }: { category: FindingCategory }) {
  const config = getCategoryConfig(category);
  const IconComponent = getCategoryIcon(category);

  return (
    <div className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-muted/50 text-xs text-muted-foreground">
      <IconComponent className="w-3 h-3" />
      <span>{config.label}</span>
    </div>
  );
}

/**
 * Status indicator component.
 */
function StatusIndicator({ status }: { status: Finding["status"] }) {
  const config = getStatusConfig(status);

  return <span className={cn("text-xs", config.color)}>{config.label}</span>;
}

/**
 * Single finding item in the list.
 */
function FindingItem({ finding }: { finding: Finding }) {
  const severityConfig = getSeverityConfig(finding.severity);
  const IconComponent = getCategoryIcon(finding.category);

  return (
    <div
      className={cn(
        "flex gap-3 px-4 py-3 border-b border-border/50 last:border-b-0",
        "hover:bg-muted/30 transition-colors",
      )}
    >
      {/* Severity indicator */}
      <div
        className={cn(
          "shrink-0 w-8 h-8 rounded-full flex items-center justify-center",
          severityConfig.bgColor,
        )}
      >
        <IconComponent className={cn("w-4 h-4", severityConfig.color)} />
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 mb-1">
          <span className="text-sm font-medium truncate">{finding.title}</span>
          <CategoryBadge category={finding.category} />
        </div>

        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          <StatusIndicator status={finding.status} />
          {finding.filePath && (
            <span className="truncate max-w-[200px]">
              {finding.filePath}
              {finding.lineNumber && `:${finding.lineNumber}`}
            </span>
          )}
        </div>

        {finding.description && (
          <p className="text-xs text-muted-foreground mt-1 line-clamp-2">{finding.description}</p>
        )}
      </div>
    </div>
  );
}

/**
 * Summary header showing severity counts.
 */
function SeveritySummary({ bySeverity }: { bySeverity: Record<FindingSeverity, number> }) {
  return (
    <div className="flex items-center gap-2 flex-wrap px-4 py-2 bg-muted/30 border-b border-border">
      <SeverityBadge severity="critical" count={bySeverity.critical} />
      <SeverityBadge severity="high" count={bySeverity.high} />
      <SeverityBadge severity="medium" count={bySeverity.medium} />
      <SeverityBadge severity="low" count={bySeverity.low} />
      <SeverityBadge severity="info" count={bySeverity.info} />
    </div>
  );
}

/**
 * Empty state when no findings exist.
 */
function EmptyState() {
  const successColors = getStatusColors("success");
  return (
    <div className="flex-1 flex flex-col items-center justify-center text-center p-8">
      <div
        className={cn(
          "w-16 h-16 rounded-full flex items-center justify-center mb-4",
          successColors.bg,
        )}
      >
        <CheckCircle className={cn("w-8 h-8", successColors.text)} />
      </div>
      <h3 className="text-lg font-medium mb-2">No Issues Detected</h3>
      <p className="text-sm text-muted-foreground max-w-xs">
        No issues have been detected. Findings will appear here as the AI analyzes your workflow.
      </p>
    </div>
  );
}

/**
 * FindingsWidget component.
 * Displays full findings interface when active.
 */
export function FindingsWidget({ isSummary, onNavigateToDetail, className }: FindingsWidgetProps) {
  const data = useFindingsData();

  // If summary mode, render nothing (handled by FindingsSummary)
  if (isSummary) {
    return null;
  }

  const handleViewAll = () => {
    if (onNavigateToDetail) {
      onNavigateToDetail();
    }
  };

  const amberColors = getAccentColors("amber");
  return (
    <div className={cn("flex flex-col h-full", className)}>
      {/* Severity summary */}
      {data.totalCount > 0 && <SeveritySummary bySeverity={data.bySeverity} />}

      {/* Content */}
      <ScrollArea className="flex-1">
        <div className="flex flex-col">
          {data.findings.length === 0 ? (
            <EmptyState />
          ) : (
            data.findings.map((finding) => <FindingItem key={finding.id} finding={finding} />)
          )}
        </div>
      </ScrollArea>

      {/* Footer with stats */}
      {data.totalCount > 0 && (
        <div className="px-4 py-2 border-t border-border bg-muted/30 flex items-center justify-between">
          <span className="text-xs text-muted-foreground">{data.byStatus.resolved} resolved</span>
          <button
            onClick={handleViewAll}
            className={cn("flex items-center gap-1 text-xs hover:underline", amberColors.text)}
          >
            View Details
            <ExternalLink className="w-3 h-3" />
          </button>
        </div>
      )}
    </div>
  );
}

export default FindingsWidget;
