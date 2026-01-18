/**
 * VerificationWidget Component
 *
 * Full widget for displaying verification test results.
 * Shows a prominent pass/fail indicator, test details, evidence, and error messages.
 * Answers the question: "Did the fix work?"
 */

import { useState } from "react";
import {
  CheckCircle,
  XCircle,
  Clock,
  Image,
  FileText,
  Loader2,
  ChevronDown,
  ChevronRight,
  Terminal,
  Globe,
  Monitor,
  FlaskConical,
  SkipForward,
  AlertCircle,
} from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge, ScrollArea } from "../../../ui";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type {
  VerificationData,
  VerificationEvidence,
  VerificationTestType,
  VerificationStatus,
} from "./types";
import { useVerificationDataWithStatus } from "./useVerificationData";
import { getStatusColors, getAccentColors } from "@/design-system";

/**
 * Get icon for test type.
 */
function TestTypeIcon({
  type,
  className,
}: {
  type: VerificationTestType | null;
  className?: string;
}) {
  const iconClass = cn("h-4 w-4", className);

  switch (type) {
    case "repo_test":
      return <FlaskConical className={iconClass} />;
    case "playwright":
      return <Globe className={iconClass} />;
    case "gui_automation":
      return <Monitor className={iconClass} />;
    default:
      return <FlaskConical className={iconClass} />;
  }
}

/**
 * Get display label for test type.
 */
function getTestTypeLabel(type: VerificationTestType | null): string {
  switch (type) {
    case "repo_test":
      return "Repository Test";
    case "playwright":
      return "Playwright Test";
    case "gui_automation":
      return "GUI Automation";
    default:
      return "Test";
  }
}

/**
 * Format duration for display.
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

/**
 * Large pass/fail indicator - the main visual element of this widget.
 */
function StatusIndicator({ status }: { status: VerificationStatus }) {
  const pendingColors = getStatusColors("pending");
  const _runningColors = getStatusColors("running");
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pausedColors = getStatusColors("paused");

  const config = {
    pending: {
      icon: Clock,
      bgClass: "bg-muted/30",
      iconClass: "text-muted-foreground",
      borderClass: "border-muted",
      label: "Pending",
      sublabel: "Waiting for verification",
    },
    running: {
      icon: Loader2,
      bgClass: pendingColors.bg,
      iconClass: cn(pendingColors.text, "animate-spin"),
      borderClass: pendingColors.border,
      label: "Verifying",
      sublabel: "Running verification test...",
    },
    passed: {
      icon: CheckCircle,
      bgClass: successColors.bg,
      iconClass: successColors.text,
      borderClass: successColors.border,
      label: "Passed",
      sublabel: "Verification successful",
    },
    failed: {
      icon: XCircle,
      bgClass: errorColors.bg,
      iconClass: errorColors.text,
      borderClass: errorColors.border,
      label: "Failed",
      sublabel: "Verification unsuccessful",
    },
    skipped: {
      icon: SkipForward,
      bgClass: pausedColors.bg,
      iconClass: pausedColors.text,
      borderClass: pausedColors.border,
      label: "Skipped",
      sublabel: "Verification was skipped",
    },
  };

  const { icon: Icon, bgClass, iconClass, borderClass, label, sublabel } = config[status];

  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center py-8 px-4 border-b",
        bgClass,
        borderClass,
      )}
    >
      <Icon className={cn("h-16 w-16 mb-3", iconClass)} />
      <span className="text-2xl font-bold">{label}</span>
      <span className="text-sm text-muted-foreground">{sublabel}</span>
    </div>
  );
}

/**
 * Test details section showing what was tested.
 */
function TestDetails({ data }: { data: VerificationData }) {
  if (!data.testName && !data.testType && !data.description) {
    return null;
  }

  return (
    <div className="px-4 py-3 border-b border-border">
      <div className="flex items-start gap-3">
        <TestTypeIcon type={data.testType} className="text-muted-foreground mt-0.5" />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            {data.testType && (
              <Badge variant="muted" size="sm">
                {getTestTypeLabel(data.testType)}
              </Badge>
            )}
            {data.durationMs > 0 && (
              <span className="text-xs text-muted-foreground font-mono">
                {formatDuration(data.durationMs)}
              </span>
            )}
          </div>
          {data.testName && <p className="text-sm font-medium truncate">{data.testName}</p>}
          {data.description && (
            <p className="text-sm text-muted-foreground mt-1">{data.description}</p>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Error message section.
 */
function ErrorSection({ error }: { error: string }) {
  const errorColors = getStatusColors("error");
  return (
    <div className={cn("px-4 py-3 border-b", errorColors.bg, errorColors.border)}>
      <div className="flex items-start gap-2">
        <AlertCircle className={cn("h-4 w-4 flex-shrink-0 mt-0.5", errorColors.text)} />
        <div className="flex-1 min-w-0">
          <span className={cn("text-sm font-medium", errorColors.text)}>Error Details</span>
          <p className={cn("text-sm mt-1 whitespace-pre-wrap break-words", errorColors.text)}>
            {error}
          </p>
        </div>
      </div>
    </div>
  );
}

/**
 * Single evidence item display.
 */
function EvidenceItem({ evidence }: { evidence: VerificationEvidence }) {
  const [expanded, setExpanded] = useState(false);

  const iconMap = {
    screenshot: Image,
    log: FileText,
    dom_snapshot: Terminal,
    console: Terminal,
  };

  const labelMap = {
    screenshot: "Screenshot",
    log: "Log Output",
    dom_snapshot: "DOM Snapshot",
    console: "Console Output",
  };

  const Icon = iconMap[evidence.type] || FileText;
  const label = labelMap[evidence.type] || "Evidence";

  const hasExpandableContent = evidence.content || evidence.type === "screenshot";

  return (
    <div className="border-b border-border/50 last:border-b-0">
      <button
        onClick={() => hasExpandableContent && setExpanded(!expanded)}
        className={cn(
          "w-full flex items-center gap-2 px-4 py-2 text-left",
          hasExpandableContent && "hover:bg-muted/30 transition-colors",
        )}
        disabled={!hasExpandableContent}
      >
        {hasExpandableContent &&
          (expanded ? (
            <ChevronDown className="h-3 w-3 text-muted-foreground" />
          ) : (
            <ChevronRight className="h-3 w-3 text-muted-foreground" />
          ))}
        <Icon className="h-4 w-4 text-muted-foreground" />
        <span className="text-sm flex-1">{label}</span>
        <span className="text-xs text-muted-foreground">
          {new Date(evidence.timestamp).toLocaleTimeString()}
        </span>
      </button>

      {expanded && evidence.content && (
        <div className="px-4 py-2 bg-muted/20">
          <pre className="text-xs font-mono text-muted-foreground whitespace-pre-wrap overflow-auto max-h-32">
            {evidence.content}
          </pre>
        </div>
      )}

      {expanded && evidence.type === "screenshot" && evidence.path && (
        <div className="px-4 py-2 bg-muted/20">
          <div className="aspect-video bg-muted rounded border border-border overflow-hidden max-w-md">
            <img
              src={`file://${evidence.path}`}
              alt="Verification screenshot"
              className="w-full h-full object-contain"
              onError={(e) => {
                (e.target as HTMLImageElement).style.display = "none";
              }}
            />
          </div>
          <p className="text-xs text-muted-foreground mt-1 truncate">{evidence.path}</p>
        </div>
      )}
    </div>
  );
}

/**
 * Evidence section with collapsible items.
 */
function EvidenceSection({ evidence }: { evidence: VerificationEvidence[] }) {
  const [sectionExpanded, setSectionExpanded] = useState(false);

  if (evidence.length === 0) return null;

  return (
    <div className="border-t border-border">
      <button
        onClick={() => setSectionExpanded(!sectionExpanded)}
        className="w-full flex items-center gap-2 px-4 py-2 hover:bg-muted/30 transition-colors"
      >
        {sectionExpanded ? (
          <ChevronDown className="h-4 w-4" />
        ) : (
          <ChevronRight className="h-4 w-4" />
        )}
        <FileText className="h-4 w-4 text-muted-foreground" />
        <span className="text-sm font-medium">Evidence</span>
        <Badge variant="muted" size="sm">
          {evidence.length}
        </Badge>
      </button>

      {sectionExpanded && (
        <ScrollArea className="max-h-48">
          <div className="flex flex-col">
            {evidence.map((item, idx) => (
              <EvidenceItem key={`${item.type}-${idx}`} evidence={item} />
            ))}
          </div>
        </ScrollArea>
      )}
    </div>
  );
}

/**
 * VerificationWidget - Full widget component for active dashboard.
 */
export function VerificationWidget({ isActive, className }: BaseWidgetProps) {
  const { data, isLoading } = useVerificationDataWithStatus();

  // Note: teal not in design system, using cyan as closest match
  const cyanColors = getAccentColors("cyan");

  return (
    <div className={cn("flex flex-col h-full", isActive ? cyanColors.border : "", className)}>
      {isLoading ? (
        <div className="flex items-center justify-center p-8">
          <Loader2 className={cn("h-6 w-6 animate-spin", cyanColors.text)} />
        </div>
      ) : (
        <>
          {/* Prominent Pass/Fail Indicator */}
          <StatusIndicator status={data.status} />

          {/* Test Details */}
          <TestDetails data={data} />

          {/* Error Message (if failed) */}
          {data.error && <ErrorSection error={data.error} />}

          {/* Evidence Section */}
          <EvidenceSection evidence={data.evidence} />
        </>
      )}
    </div>
  );
}

export default VerificationWidget;
