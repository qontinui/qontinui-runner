/**
 * VerificationWidget Component
 *
 * Full widget for displaying verification test results.
 * Shows a list of check steps with pass/fail status, plus overall results.
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
import { StepStatsBar, StepStatusBadge } from "../shared";
import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type {
  VerificationEvidence,
  VerificationTestType,
  VerificationStatus,
  CheckStep,
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
    case "check_group":
      return <CheckCircle className={iconClass} />;
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
    case "check_group":
      return "Check Group";
    default:
      return "Test";
  }
}

/**
 * Format duration for display.
 */
function formatDuration(ms: number | undefined): string {
  if (!ms) return "";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const mins = Math.floor(ms / 60000);
  const secs = Math.floor((ms % 60000) / 1000);
  return `${mins}m ${secs}s`;
}

/**
 * Compact status indicator for the overall result.
 */
function StatusIndicatorCompact({ status }: { status: VerificationStatus }) {
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");
  const pendingColors = getStatusColors("pending");
  const pausedColors = getStatusColors("paused");

  const config = {
    pending: {
      icon: Clock,
      iconClass: "text-muted-foreground",
      label: "Pending",
      badgeClass: "bg-muted text-muted-foreground border-border",
    },
    running: {
      icon: Loader2,
      iconClass: cn(pendingColors.text, "animate-spin"),
      label: "Running",
      badgeClass: cn(pendingColors.bg, pendingColors.text, pendingColors.border),
    },
    passed: {
      icon: CheckCircle,
      iconClass: successColors.text,
      label: "Passed",
      badgeClass: cn(successColors.bg, successColors.text, successColors.border),
    },
    failed: {
      icon: XCircle,
      iconClass: errorColors.text,
      label: "Failed",
      badgeClass: cn(errorColors.bg, errorColors.text, errorColors.border),
    },
    skipped: {
      icon: SkipForward,
      iconClass: pausedColors.text,
      label: "Skipped",
      badgeClass: cn(pausedColors.bg, pausedColors.text, pausedColors.border),
    },
  };

  const { icon: Icon, iconClass, label, badgeClass } = config[status];

  return (
    <Badge className={cn("px-3 py-1.5 text-sm font-medium border", badgeClass)}>
      <Icon className={cn("h-4 w-4 mr-2", iconClass)} />
      {label}
    </Badge>
  );
}

/**
 * Check step row component.
 */
function CheckStepRow({
  step,
  isExpanded,
  onToggle,
}: {
  step: CheckStep;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const tealColors = getAccentColors("cyan"); // Using cyan as teal substitute
  const errorColors = getStatusColors("error");
  const isActive = step.status === "running";
  const pendingColors = getStatusColors("pending");

  return (
    <div className="flex flex-col">
      <div
        className={cn(
          "flex items-center gap-3 border-l-2 px-4 py-2.5 transition-colors cursor-pointer",
          isActive
            ? cn(pendingColors.border, pendingColors.bg)
            : isExpanded
              ? "border-primary bg-muted/50"
              : "border-transparent hover:bg-muted/30",
        )}
        onClick={onToggle}
      >
        {/* Expand/collapse indicator */}
        <div className="flex-shrink-0 text-muted-foreground">
          {isExpanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </div>

        {/* Status icon */}
        <StepStatusBadge status={step.status} iconOnly size="md" />

        {/* Check type badge */}
        <Badge
          className={cn(
            "text-xs flex-shrink-0 font-mono border",
            tealColors.bg,
            tealColors.text,
            tealColors.border,
          )}
        >
          <TestTypeIcon type={step.checkType} className="h-3 w-3 mr-1" />
          {getTestTypeLabel(step.checkType)}
        </Badge>

        {/* Step name */}
        <span className="flex-1 text-sm truncate text-foreground">{step.name}</span>

        {/* Duration */}
        {step.durationMs !== undefined && (
          <span className="text-xs text-muted-foreground font-mono">
            {formatDuration(step.durationMs)}
          </span>
        )}

        {/* Error indicator */}
        {step.error && (
          <span title={step.error}>
            <AlertCircle className={cn("h-3.5 w-3.5", errorColors.text)} />
          </span>
        )}
      </div>

      {/* Expanded details */}
      {isExpanded && (
        <div className="border-l-2 border-primary bg-muted/30 ml-4 mr-4 mb-2 rounded-b overflow-hidden">
          {step.output && (
            <div className="px-4 py-2 border-b border-border/50">
              <div className="flex items-center gap-2 mb-1">
                <Terminal className="h-3 w-3 text-muted-foreground" />
                <span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
                  Output
                </span>
              </div>
              <pre className="text-xs font-mono text-muted-foreground whitespace-pre-wrap overflow-auto max-h-32">
                {step.output}
              </pre>
            </div>
          )}
          {step.error && (
            <div className={cn("px-4 py-2", errorColors.bg)}>
              <div className="flex items-center gap-2 mb-1">
                <AlertCircle className={cn("h-3 w-3", errorColors.text)} />
                <span
                  className={cn(
                    "text-[10px] uppercase tracking-wide font-medium",
                    errorColors.text,
                  )}
                >
                  Error
                </span>
              </div>
              <p className={cn("text-xs whitespace-pre-wrap", errorColors.text)}>{step.error}</p>
            </div>
          )}
          {!step.output && !step.error && (
            <div className="px-4 py-3 text-center text-xs text-muted-foreground">
              No output captured
            </div>
          )}
        </div>
      )}
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
  const [expandedStepId, setExpandedStepId] = useState<string | null>(null);

  const tealColors = getAccentColors("cyan"); // Using cyan as teal substitute

  const handleToggleStep = (stepId: string) => {
    setExpandedStepId((prev) => (prev === stepId ? null : stepId));
  };

  return (
    <div className={cn("flex flex-col h-full", isActive ? tealColors.border : "", className)}>
      {isLoading ? (
        <div className="flex items-center justify-center p-8">
          <Loader2 className={cn("h-6 w-6 animate-spin", tealColors.text)} />
        </div>
      ) : (
        <>
          {/* Stats Bar (if we have check steps) */}
          {data.checkSteps.length > 0 && <StepStatsBar stats={data.stats} />}

          {/* Overall Status and Check Steps Header */}
          <div className="flex items-center justify-between border-b border-border px-4 py-3 bg-muted/10 flex-shrink-0">
            <div className="flex items-center gap-3">
              <StatusIndicatorCompact status={data.status} />
              {data.testName && (
                <span className="text-sm text-muted-foreground truncate">{data.testName}</span>
              )}
            </div>
            {data.checkSteps.length > 0 && (
              <Badge variant="muted" className="text-xs">
                {data.checkSteps.length} checks
              </Badge>
            )}
          </div>

          {/* Check Steps List */}
          {data.checkSteps.length > 0 ? (
            <ScrollArea className="flex-1">
              <div className="flex flex-col-reverse">
                {data.checkSteps.map((step) => (
                  <CheckStepRow
                    key={step.id}
                    step={step}
                    isExpanded={step.id === expandedStepId}
                    onToggle={() => handleToggleStep(step.id)}
                  />
                ))}
              </div>
            </ScrollArea>
          ) : (
            /* Legacy single-result view when no check steps */
            <div className="flex-1 overflow-auto">
              {/* Test Details */}
              {(data.testName || data.testType || data.description) && (
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
                      {data.testName && (
                        <p className="text-sm font-medium truncate">{data.testName}</p>
                      )}
                      {data.description && (
                        <p className="text-sm text-muted-foreground mt-1">{data.description}</p>
                      )}
                    </div>
                  </div>
                </div>
              )}

              {/* Error Message */}
              {data.error && (
                <div className={cn("px-4 py-3 border-b", getStatusColors("error").bg)}>
                  <div className="flex items-start gap-2">
                    <AlertCircle
                      className={cn("h-4 w-4 flex-shrink-0 mt-0.5", getStatusColors("error").text)}
                    />
                    <div className="flex-1 min-w-0">
                      <span className={cn("text-sm font-medium", getStatusColors("error").text)}>
                        Error Details
                      </span>
                      <p
                        className={cn(
                          "text-sm mt-1 whitespace-pre-wrap break-words",
                          getStatusColors("error").text,
                        )}
                      >
                        {data.error}
                      </p>
                    </div>
                  </div>
                </div>
              )}

              {/* Waiting message when no data */}
              {!data.testName && !data.description && !data.error && data.status === "pending" && (
                <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
                  <Clock className="h-8 w-8 mb-2 opacity-50" />
                  <span className="text-sm">Waiting for verification...</span>
                </div>
              )}
            </div>
          )}

          {/* Evidence Section */}
          <EvidenceSection evidence={data.evidence} />
        </>
      )}
    </div>
  );
}

export default VerificationWidget;
