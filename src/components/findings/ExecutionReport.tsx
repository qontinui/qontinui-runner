/**
 * ExecutionReport.tsx
 *
 * Main component for displaying categorized execution reports.
 * Shows a summary of findings grouped by category with filtering and actions.
 */

import { useState, useMemo, useCallback, useEffect } from "react";
import {
  FileText,
  Clock,
  CheckCircle,
  AlertCircle,
  Loader2,
  Play,
  Filter,
  ChevronDown,
  Bot,
  Trash2,
} from "lucide-react";
import type { ExecutionReport as ExecutionReportType, Finding } from "../../types/findings";
import { findingsTracker, getVisibleCategories } from "../../services";
import { CategorySection } from "./CategorySection";
import { useAiTaskPolling } from "../../hooks";

/** Auto-fixable category IDs */
const AUTO_FIXABLE_CATEGORIES = ["code_bug", "security", "test_issue", "documentation"];

interface ExecutionReportProps {
  report?: ExecutionReportType | null;
  onAnalyzeFinding?: (finding: Finding) => void;
  onResolveFinding?: (finding: Finding, resolution: string) => void;
  onProvideInput?: (finding: Finding, response: string) => void;
  onDismissFinding?: (finding: Finding) => void;
  onContinue?: () => void;
}

const statusLabels: Record<
  string,
  { label: string; color: string; icon: React.ComponentType<{ className?: string }> }
> = {
  running: { label: "Running", color: "text-blue-400", icon: Loader2 },
  completed: { label: "Completed", color: "text-green-400", icon: CheckCircle },
  paused_for_input: { label: "Needs Input", color: "text-purple-400", icon: AlertCircle },
  failed: { label: "Failed", color: "text-red-400", icon: AlertCircle },
  cancelled: { label: "Cancelled", color: "text-gray-400", icon: AlertCircle },
};

type FilterMode = "all" | "actionable" | "needs_input" | "resolved";

export function ExecutionReport({
  report,
  onAnalyzeFinding,
  onResolveFinding,
  onProvideInput,
  onDismissFinding,
  onContinue,
}: ExecutionReportProps) {
  const [filterMode, setFilterMode] = useState<FilterMode>("all");
  const [showFilterMenu, setShowFilterMenu] = useState(false);
  const [processingFindingId, setProcessingFindingId] = useState<string | null>(null);
  const [liveFindings, setLiveFindings] = useState<Finding[]>([]);
  const [isAnalyzingAll, setIsAnalyzingAll] = useState(false);

  // Subscribe to live findings updates
  useEffect(() => {
    const unsubscribe = findingsTracker.subscribe((event) => {
      if (
        event.type === "finding_detected" ||
        event.type === "finding_updated" ||
        event.type === "finding_resolved" ||
        event.type === "finding_removed"
      ) {
        setLiveFindings(findingsTracker.getAllFindings());
      }
    });

    // Initialize with current findings
    setLiveFindings(findingsTracker.getAllFindings());

    return unsubscribe;
  }, []);

  // Use report findings if available, otherwise use live findings
  const findings = report?.findings || liveFindings;

  // Get visible categories
  const categories = useMemo(() => getVisibleCategories(), []);

  // Filter findings based on filter mode
  const filteredFindings = useMemo(() => {
    switch (filterMode) {
      case "actionable":
        return findings.filter(
          (f) => f.actionable && f.status !== "resolved" && f.status !== "wont_fix",
        );
      case "needs_input":
        return findings.filter((f) => f.status === "needs_input");
      case "resolved":
        return findings.filter((f) => f.status === "resolved");
      default:
        return findings;
    }
  }, [findings, filterMode]);

  // Group findings by category
  const findingsByCategory = useMemo(() => {
    const grouped = new Map<string, Finding[]>();
    for (const finding of filteredFindings) {
      const existing = grouped.get(finding.categoryId) || [];
      grouped.set(finding.categoryId, [...existing, finding]);
    }
    return grouped;
  }, [filteredFindings]);

  // Calculate summary stats
  const summary = useMemo(() => {
    const total = findings.length;
    const resolved = findings.filter((f) => f.status === "resolved").length;
    const actionable = findings.filter(
      (f) => f.actionable && f.status !== "resolved" && f.status !== "wont_fix",
    ).length;
    const needsInput = findings.filter((f) => f.status === "needs_input").length;
    const informational = findings.filter((f) => f.actionType === "informational").length;

    // Auto-fixable: actionable findings in auto-fix categories that don't need user input
    const autoFixable = findings.filter(
      (f) =>
        AUTO_FIXABLE_CATEGORIES.includes(f.categoryId) &&
        f.status !== "resolved" &&
        f.status !== "wont_fix" &&
        f.status !== "needs_input" &&
        f.actionType === "auto_fix",
    ).length;

    return { total, resolved, actionable, needsInput, informational, autoFixable };
  }, [findings]);

  // Get auto-fixable findings for bulk processing
  const autoFixableFindings = useMemo(() => {
    return findings.filter(
      (f) =>
        AUTO_FIXABLE_CATEGORIES.includes(f.categoryId) &&
        f.status !== "resolved" &&
        f.status !== "wont_fix" &&
        f.status !== "needs_input" &&
        f.actionType === "auto_fix",
    );
  }, [findings]);

  // Handle analyze action
  const handleAnalyze = useCallback(
    async (finding: Finding) => {
      if (onAnalyzeFinding) {
        setProcessingFindingId(finding.id);
        try {
          await onAnalyzeFinding(finding);
        } finally {
          setProcessingFindingId(null);
        }
      }
    },
    [onAnalyzeFinding],
  );

  // Handle dismiss action
  const handleDismiss = useCallback(
    (finding: Finding) => {
      if (onDismissFinding) {
        onDismissFinding(finding);
      } else {
        // Default: remove the finding
        findingsTracker.removeFinding(finding.id);
      }
    },
    [onDismissFinding],
  );

  // Handle provide input action
  const handleProvideInput = useCallback(
    (finding: Finding, response: string) => {
      if (onProvideInput) {
        onProvideInput(finding, response);
      } else {
        // Default: update the finding with response
        findingsTracker.provideUserResponse(finding.id, response);
      }
    },
    [onProvideInput],
  );

  // AI task polling hook for analyze all
  const { triggerTask: triggerAnalyzeAll } = useAiTaskPolling({
    onComplete: () => {
      setIsAnalyzingAll(false);
    },
    onError: (error) => {
      console.error("Failed to analyze findings:", error);
      setIsAnalyzingAll(false);
    },
  });

  // Handle analyze all auto-fixable findings
  const handleAnalyzeAll = useCallback(async () => {
    if (autoFixableFindings.length === 0) return;

    setIsAnalyzingAll(true);

    // Build a prompt with all auto-fixable findings
    const findingsJson = JSON.stringify(
      autoFixableFindings.map((f) => ({
        id: f.id,
        categoryId: f.categoryId,
        severity: f.severity,
        title: f.title,
        description: f.description,
        file: f.codeContext?.file,
        line: f.codeContext?.line,
      })),
      null,
      2,
    );

    const prompt = `You are fixing auto-fixable findings from a qontinui-runner session.

## Your Task

Fix ALL of the following findings. These are all code bugs, security issues, test issues, or documentation problems that can be fixed automatically.

## Findings to Fix

${findingsJson}

## Instructions

For EACH finding:
1. Read the relevant code file
2. Make the fix
3. Output a structured finding with the Resolution field:

\`\`\`
[FINDING:${"{category_id}"}:${"{severity}"}]
Title: ${"{original title}"}
Description: ${"{what was found}"}
File: ${"{file path}"}
Line: ${"{line number}"}
Resolution: ${"{what you fixed}"}
[/FINDING]
\`\`\`

Work through ALL findings systematically. Fix each one and report your resolution.`;

    // Use async task triggering with polling
    await triggerAnalyzeAll({
      name: "ai-analysis",
      content: prompt,
      max_sessions: 1,
      display_prompt: `Fixing ${autoFixableFindings.length} auto-fixable findings`,
      timeout_seconds: 600,
    });
  }, [autoFixableFindings, triggerAnalyzeAll]);

  // Handle clear all findings
  const handleClearAll = useCallback(() => {
    findingsTracker.clearAll();
  }, []);

  const statusInfo = report ? statusLabels[report.status] : null;
  const StatusIcon = statusInfo?.icon || FileText;

  // Empty state
  if (findings.length === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground p-8">
        <FileText className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Findings Yet</p>
        <p className="text-sm mt-2 text-center">
          Run an AI analysis to detect and categorize findings.
          <br />
          Findings will be grouped by category with action recommendations.
        </p>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 border-b border-border p-4 space-y-3">
        {/* Report Title and Status */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-primary/10 rounded-lg">
              <FileText className="w-5 h-5 text-primary" />
            </div>
            <div>
              <h2 className="font-semibold text-foreground">
                {report?.promptName || "Execution Report"}
              </h2>
              {report && (
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Clock className="w-3 h-3" />
                  {new Date(report.startedAt).toLocaleString()}
                  {report.duration && <span>({Math.round(report.duration / 1000)}s)</span>}
                </div>
              )}
            </div>
          </div>

          {/* Status Badge */}
          {statusInfo && (
            <div
              className={`flex items-center gap-2 px-3 py-1.5 rounded-lg bg-muted/50 ${statusInfo.color}`}
            >
              <StatusIcon
                className={`w-4 h-4 ${report?.status === "running" ? "animate-spin" : ""}`}
              />
              <span className="text-sm font-medium">{statusInfo.label}</span>
            </div>
          )}
        </div>

        {/* Summary Stats */}
        <div className="flex items-center gap-4 text-sm">
          <div className="flex items-center gap-2 px-3 py-1.5 bg-muted/30 rounded-lg">
            <span className="text-muted-foreground">Total:</span>
            <span className="font-semibold">{summary.total}</span>
          </div>
          {summary.actionable > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-amber-500/10 rounded-lg text-amber-400">
              <span>Actionable:</span>
              <span className="font-semibold">{summary.actionable}</span>
            </div>
          )}
          {summary.needsInput > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-purple-500/10 rounded-lg text-purple-400">
              <span>Needs Input:</span>
              <span className="font-semibold">{summary.needsInput}</span>
            </div>
          )}
          {summary.resolved > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-green-500/10 rounded-lg text-green-400">
              <span>Resolved:</span>
              <span className="font-semibold">{summary.resolved}</span>
            </div>
          )}
          {summary.informational > 0 && (
            <div className="flex items-center gap-2 px-3 py-1.5 bg-slate-500/10 rounded-lg text-slate-400">
              <span>Info:</span>
              <span className="font-semibold">{summary.informational}</span>
            </div>
          )}
        </div>

        {/* Filter and Actions */}
        <div className="flex items-center justify-between">
          {/* Filter Dropdown */}
          <div className="relative">
            <button
              onClick={() => setShowFilterMenu(!showFilterMenu)}
              className="flex items-center gap-2 px-3 py-1.5 bg-muted/50 hover:bg-muted rounded-lg text-sm transition-colors"
            >
              <Filter className="w-4 h-4" />
              <span>
                {filterMode === "all"
                  ? "All Findings"
                  : filterMode === "actionable"
                    ? "Actionable"
                    : filterMode === "needs_input"
                      ? "Needs Input"
                      : "Resolved"}
              </span>
              <ChevronDown className="w-4 h-4" />
            </button>

            {showFilterMenu && (
              <div className="absolute top-full left-0 mt-1 w-40 bg-popover border border-border rounded-lg shadow-lg z-50">
                {[
                  { value: "all", label: "All Findings" },
                  { value: "actionable", label: "Actionable" },
                  { value: "needs_input", label: "Needs Input" },
                  { value: "resolved", label: "Resolved" },
                ].map((option) => (
                  <button
                    key={option.value}
                    onClick={() => {
                      setFilterMode(option.value as FilterMode);
                      setShowFilterMenu(false);
                    }}
                    className={`w-full text-left px-3 py-2 text-sm hover:bg-muted transition-colors ${
                      filterMode === option.value ? "bg-muted" : ""
                    }`}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Action Buttons */}
          <div className="flex items-center gap-2">
            {/* Analyze & Fix All Button */}
            {summary.autoFixable > 0 && (
              <button
                onClick={handleAnalyzeAll}
                disabled={isAnalyzingAll}
                className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-purple-500 hover:bg-purple-600 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded-lg transition-colors"
                title={`Fix ${summary.autoFixable} auto-fixable findings (code bugs, security, tests, docs)`}
              >
                {isAnalyzingAll ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    <span>Fixing...</span>
                  </>
                ) : (
                  <>
                    <Bot className="w-4 h-4" />
                    <span>Fix All ({summary.autoFixable})</span>
                  </>
                )}
              </button>
            )}

            {/* Continue Button */}
            {report?.status === "paused_for_input" && onContinue && (
              <button
                onClick={onContinue}
                className="flex items-center gap-2 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
              >
                <Play className="w-4 h-4" />
                Continue with Answers
              </button>
            )}

            {/* Clear All Button */}
            <button
              onClick={handleClearAll}
              className="flex items-center gap-1 px-2 py-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
              title="Clear all findings"
            >
              <Trash2 className="w-3 h-3" />
              Clear
            </button>
          </div>
        </div>
      </div>

      {/* Scrollable Category Sections */}
      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-4">
        {categories.map((category) => {
          const categoryFindings = findingsByCategory.get(category.id) || [];
          if (categoryFindings.length === 0) return null;

          return (
            <CategorySection
              key={category.id}
              category={category}
              findings={categoryFindings}
              onAnalyze={handleAnalyze}
              onResolve={onResolveFinding}
              onProvideInput={handleProvideInput}
              onDismiss={handleDismiss}
              processingFindingId={processingFindingId}
            />
          );
        })}

        {/* Show uncategorized findings */}
        {Array.from(findingsByCategory.entries())
          .filter(([catId]) => !categories.some((c) => c.id === catId))
          .map(([categoryId, categoryFindings]) => (
            <CategorySection
              key={categoryId}
              category={{
                id: categoryId,
                name: categoryId,
                description: "Uncategorized findings",
                icon: "AlertTriangle",
                color: "slate",
                isBuiltIn: false,
                defaultActionType: "manual",
                sortOrder: 999,
              }}
              findings={categoryFindings}
              onAnalyze={handleAnalyze}
              onResolve={onResolveFinding}
              onProvideInput={handleProvideInput}
              onDismiss={handleDismiss}
              processingFindingId={processingFindingId}
            />
          ))}
      </div>
    </div>
  );
}
