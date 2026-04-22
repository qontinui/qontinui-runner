/**
 * ExecutionReport.tsx
 *
 * Main component for displaying categorized execution reports.
 * Shows a summary of findings grouped by category with filtering and actions.
 */

import { useState, useMemo, useCallback, useEffect, useRef } from "react";
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
  Activity,
  ToggleLeft,
  ToggleRight,
  Wrench,
  X,
  Search,
  ExternalLink,
} from "lucide-react";
import { getStatusColors, getAccentColors } from "@/design-system";
import type { AccentColor } from "@/design-system";
import type { ExecutionReport as ExecutionReportType, Finding } from "../../types/findings";
import { findingsTracker, getVisibleCategories, getCategoryById } from "../../services";
import { CategorySection } from "./CategorySection";
import { useAiTaskPolling, useSearchEvents } from "../../hooks";
import type { SearchEventResult, SearchEventSourceTable } from "../../hooks";
import { useRunSelectionOptional } from "../../contexts/RunSelectionContext";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

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
  { label: string; colorKey: string; icon: React.ComponentType<{ className?: string }> }
> = {
  running: { label: "Running", colorKey: "running", icon: Loader2 },
  completed: { label: "Completed", colorKey: "success", icon: CheckCircle },
  paused_for_input: { label: "Needs Input", colorKey: "paused", icon: AlertCircle },
  failed: { label: "Failed", colorKey: "error", icon: AlertCircle },
  cancelled: { label: "Cancelled", colorKey: "cancelled", icon: AlertCircle },
};

type FilterMode = "all" | "actionable" | "needs_input" | "resolved";

const SEVERITY_OPTIONS: { value: string; label: string }[] = [
  { value: "all", label: "All Severities" },
  { value: "critical", label: "Critical" },
  { value: "high", label: "High" },
  { value: "medium", label: "Medium" },
  { value: "low", label: "Low" },
  { value: "info", label: "Info" },
];

/**
 * Design-system accent tokens for the four source-table badges.
 * Uses `getAccentColors(...)` at render time — no hardcoded hex.
 */
const SOURCE_TABLE_META: Record<SearchEventSourceTable, { label: string; accent: AccentColor }> = {
  deferred_questions: { label: "HITL", accent: "purple" },
  error_events: { label: "Error", accent: "red" },
  observations: { label: "Observation", accent: "blue" },
  activity_timeline: { label: "Activity", accent: "slate" },
};

/** Format an ISO timestamp as `YYYY-MM-DD HH:mm` in local time. */
function formatTimestamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function ExecutionReport({
  report,
  onAnalyzeFinding,
  onResolveFinding,
  onProvideInput,
  onDismissFinding,
  onContinue,
}: ExecutionReportProps) {
  const [filterMode, setFilterMode] = useState<FilterMode>("all");
  const [severityFilter, setSeverityFilter] = useState<string>("all");
  const [categoryFilter, setCategoryFilter] = useState<string>("all");
  const [showFilterMenu, setShowFilterMenu] = useState(false);
  const [showSeverityMenu, setShowSeverityMenu] = useState(false);
  const [showCategoryMenu, setShowCategoryMenu] = useState(false);
  const [processingFindingId, setProcessingFindingId] = useState<string | null>(null);
  const [liveFindings, setLiveFindings] = useState<Finding[]>([]);
  const [isAnalyzingAll, setIsAnalyzingAll] = useState(false);
  const [autoFixEnabled, setAutoFixEnabled] = useState(false);
  const [autoFixLoading, setAutoFixLoading] = useState(false);

  // Full-text search state
  const [searchQuery, setSearchQuery] = useState("");
  const [hitlOnly, setHitlOnly] = useState(true);
  const [snippetModal, setSnippetModal] = useState<SearchEventResult | null>(null);

  // Use RunSelectionContext if available
  const runSelection = useRunSelectionOptional();
  const selectedRun = runSelection?.selectedRun;

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

  // Load auto-fix setting on mount
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    const loadAutoFixSetting = async () => {
      try {
        const response = await tracedFetch(`${getApiBase()}/session/auto-fix`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled && result.success) {
          setAutoFixEnabled(result.data?.enabled ?? false);
        }
      } catch (error) {
        if (!cancelled) {
          console.error("Failed to load auto-fix setting:", error);
        }
      }
    };
    loadAutoFixSetting();
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Toggle auto-fix setting
  const toggleAutoFix = useCallback(async () => {
    if (autoFixLoading) return;

    setAutoFixLoading(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/session/auto-fix`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ enabled: !autoFixEnabled }),
      });
      const result = await response.json();
      if (result.success) {
        setAutoFixEnabled(result.data?.enabled ?? !autoFixEnabled);
      }
    } catch (error) {
      console.error("Failed to toggle auto-fix setting:", error);
    } finally {
      setAutoFixLoading(false);
    }
  }, [autoFixEnabled, autoFixLoading]);

  // Use report findings if available, otherwise use live findings
  const findings = report?.findings || liveFindings;

  // ---------------------------------------------------------------------------
  // Full-text search (Tauri `search_events` command)
  // ---------------------------------------------------------------------------
  const trimmedSearch = searchQuery.trim();
  const isSearching = trimmedSearch.length > 0;

  const searchResultsQuery = useSearchEvents({
    query: searchQuery,
    enabled: isSearching,
  });

  // Apply source-table filter client-side. Default: HITL queue only.
  const visibleSearchResults = useMemo<SearchEventResult[]>(() => {
    const raw = searchResultsQuery.data ?? [];
    return hitlOnly ? raw.filter((r) => r.source_table === "deferred_questions") : raw;
  }, [searchResultsQuery.data, hitlOnly]);

  // Ref for the scrollable findings list (so we can query for finding cards)
  const findingsListRef = useRef<HTMLDivElement | null>(null);

  const handleJumpToFinding = useCallback(
    (result: SearchEventResult) => {
      const existing = findings.find((f) => f.id === result.record_id);
      if (existing && findingsListRef.current) {
        // Finding cards carry data-finding-id (see FindingCard.tsx).
        const escapedId =
          typeof CSS !== "undefined" && "escape" in CSS
            ? CSS.escape(existing.id)
            : existing.id.replace(/"/g, '\\"');
        const el = findingsListRef.current.querySelector<HTMLElement>(
          `[data-finding-id="${escapedId}"]`,
        );
        if (el) {
          el.scrollIntoView({ behavior: "smooth", block: "center" });
          el.classList.add("ring-2", "ring-primary");
          window.setTimeout(() => el.classList.remove("ring-2", "ring-primary"), 1600);
          return;
        }
      }
      // No local finding card — open the inline modal.
      setSnippetModal(result);
    },
    [findings],
  );

  // Get visible categories
  const categories = useMemo(() => getVisibleCategories(), []);

  // Build dynamic category options from current findings
  const categoryOptions = useMemo(() => {
    const uniqueCategoryIds = new Set(findings.map((f) => f.categoryId));
    const options: { value: string; label: string }[] = [{ value: "all", label: "All Categories" }];
    for (const catId of uniqueCategoryIds) {
      const category = getCategoryById(catId);
      options.push({ value: catId, label: category?.name ?? catId });
    }
    return options;
  }, [findings]);

  // Count of active filters (not counting "all" as active)
  const activeFilterCount = useMemo(() => {
    let count = 0;
    if (filterMode !== "all") count++;
    if (severityFilter !== "all") count++;
    if (categoryFilter !== "all") count++;
    return count;
  }, [filterMode, severityFilter, categoryFilter]);

  // Clear all filters
  const clearAllFilters = useCallback(() => {
    setFilterMode("all");
    setSeverityFilter("all");
    setCategoryFilter("all");
  }, []);

  // Filter findings based on all filter criteria
  const filteredFindings = useMemo(() => {
    let result = findings;

    // Mode filter
    switch (filterMode) {
      case "actionable":
        result = result.filter(
          (f) => f.actionable && f.status !== "resolved" && f.status !== "wont_fix",
        );
        break;
      case "needs_input":
        result = result.filter((f) => f.status === "needs_input");
        break;
      case "resolved":
        result = result.filter((f) => f.status === "resolved");
        break;
    }

    // Severity filter
    if (severityFilter !== "all") {
      result = result.filter((f) => f.severity === severityFilter);
    }

    // Category filter
    if (categoryFilter !== "all") {
      result = result.filter((f) => f.categoryId === categoryFilter);
    }

    return result;
  }, [findings, filterMode, severityFilter, categoryFilter]);

  // Group findings by category
  const findingsByCategory = useMemo(() => {
    const grouped = new Map<string, Finding[]>();
    for (const finding of filteredFindings) {
      const existing = grouped.get(finding.categoryId) || [];
      grouped.set(finding.categoryId, [...existing, finding]);
    }
    return grouped;
  }, [filteredFindings]);

  // Total (unfiltered) findings count by category - used for "N of M" display
  const totalFindingsByCategory = useMemo(() => {
    const grouped = new Map<string, number>();
    for (const finding of findings) {
      grouped.set(finding.categoryId, (grouped.get(finding.categoryId) || 0) + 1);
    }
    return grouped;
  }, [findings]);

  // Whether any filter is active (for showing "N of M" badges)
  const isFilterActive = activeFilterCount > 0;

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
      maxSessions: 1,
      displayPrompt: `Fixing ${autoFixableFindings.length} auto-fixable findings`,
      timeoutSeconds: 600,
    });
  }, [autoFixableFindings, triggerAnalyzeAll]);

  // Handle clear all findings
  const handleClearAll = useCallback(() => {
    findingsTracker.clearAll();
  }, []);

  const statusInfo = report ? statusLabels[report.status] : null;
  const StatusIcon = statusInfo?.icon || FileText;

  // No run selected state (when context is present but no run selected)
  if (runSelection && !selectedRun) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground p-8">
        <Activity className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Run Selected</p>
        <p className="text-sm mt-2 text-center max-w-md">
          Select a run from the Run Dashboard to view findings.
        </p>
      </div>
    );
  }

  // Empty state is rendered inline below (in the main return) so the header
  // region — title, status, summary stats, AND the full-text search panel —
  // remains visible and interactive even when there are no live findings.
  // Search spans historical events unrelated to the currently-selected run's
  // live findings, so reviewers must be able to search in the empty state.
  const hasFindings = findings.length > 0;

  return (
    <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
      {/* Header */}
      <div className="shrink-0 border-b border-border p-4 space-y-3">
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
          {statusInfo &&
            (() => {
              const statusColors = getStatusColors(statusInfo.colorKey);
              return (
                <div
                  className={`flex items-center gap-2 px-3 py-1.5 rounded-lg ${statusColors.bg} ${statusColors.text}`}
                >
                  <StatusIcon
                    className={`w-4 h-4 ${report?.status === "running" ? "animate-spin" : ""}`}
                  />
                  <span className="text-sm font-medium">{statusInfo.label}</span>
                </div>
              );
            })()}
        </div>

        {/* Summary Stats */}
        <div className="flex items-center gap-4 text-sm">
          <div className="flex items-center gap-2 px-3 py-1.5 bg-muted/30 rounded-lg">
            <span className="text-muted-foreground">Total:</span>
            <span className="font-semibold">{summary.total}</span>
          </div>
          {summary.actionable > 0 &&
            (() => {
              const colors = getAccentColors("amber");
              return (
                <div
                  className={`flex items-center gap-2 px-3 py-1.5 rounded-lg ${colors.bg} ${colors.text}`}
                >
                  <span>Actionable:</span>
                  <span className="font-semibold">{summary.actionable}</span>
                </div>
              );
            })()}
          {summary.needsInput > 0 &&
            (() => {
              const colors = getAccentColors("purple");
              return (
                <div
                  className={`flex items-center gap-2 px-3 py-1.5 rounded-lg ${colors.bg} ${colors.text}`}
                >
                  <span>Needs Input:</span>
                  <span className="font-semibold">{summary.needsInput}</span>
                </div>
              );
            })()}
          {summary.resolved > 0 &&
            (() => {
              const colors = getAccentColors("green");
              return (
                <div
                  className={`flex items-center gap-2 px-3 py-1.5 rounded-lg ${colors.bg} ${colors.text}`}
                >
                  <span>Resolved:</span>
                  <span className="font-semibold">{summary.resolved}</span>
                </div>
              );
            })()}
          {summary.informational > 0 &&
            (() => {
              const colors = getAccentColors("slate");
              return (
                <div
                  className={`flex items-center gap-2 px-3 py-1.5 rounded-lg ${colors.bg} ${colors.text}`}
                >
                  <span>Info:</span>
                  <span className="font-semibold">{summary.informational}</span>
                </div>
              );
            })()}
        </div>

        {/* Full-text Search */}
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <div className="relative flex-1">
              <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground pointer-events-none" />
              <input
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                placeholder="Search events…"
                aria-label="Search events"
                data-testid="search-events-input"
                className="w-full bg-muted/50 hover:bg-muted focus:bg-muted rounded-lg pl-8 pr-8 py-1.5 text-sm outline-none focus:ring-2 focus:ring-primary/40 transition-colors"
              />
              {searchQuery && (
                <button
                  type="button"
                  onClick={() => setSearchQuery("")}
                  aria-label="Clear search"
                  data-testid="search-events-clear"
                  className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                >
                  <X className="w-3.5 h-3.5" />
                </button>
              )}
            </div>
            {/* Source-filter toggle: HITL only vs. All events */}
            <div className="flex items-center bg-muted/50 rounded-lg p-0.5 text-xs">
              <button
                type="button"
                onClick={() => setHitlOnly(true)}
                aria-pressed={hitlOnly}
                data-testid="search-events-hitl-only"
                className={`px-2.5 py-1 rounded-md transition-colors ${
                  hitlOnly
                    ? "bg-primary/15 text-primary"
                    : "text-muted-foreground hover:text-foreground"
                }`}
                title="Show only deferred-question (HITL) results"
              >
                HITL queue only
              </button>
              <button
                type="button"
                onClick={() => setHitlOnly(false)}
                aria-pressed={!hitlOnly}
                data-testid="search-events-all-events"
                className={`px-2.5 py-1 rounded-md transition-colors ${
                  !hitlOnly
                    ? "bg-primary/15 text-primary"
                    : "text-muted-foreground hover:text-foreground"
                }`}
                title="Show results from all event sources"
              >
                All events
              </button>
            </div>
          </div>
          <p className="text-[11px] text-muted-foreground pl-8">
            Searches across deferred questions, errors, observations, and activity (last 7 days)
          </p>
        </div>

        {/* Filter and Actions */}
        <div className="flex items-center justify-between">
          {/* Filter Dropdowns */}
          <div className="flex items-center gap-2">
            {/* Mode Filter Dropdown */}
            <div className="relative">
              <button
                onClick={() => {
                  setShowFilterMenu(!showFilterMenu);
                  setShowSeverityMenu(false);
                  setShowCategoryMenu(false);
                }}
                className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm transition-colors ${
                  filterMode !== "all"
                    ? "bg-primary/15 text-primary hover:bg-primary/20"
                    : "bg-muted/50 hover:bg-muted"
                }`}
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
                        filterMode === option.value ? "bg-muted font-medium" : ""
                      }`}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Severity Filter Dropdown */}
            <div className="relative">
              <button
                onClick={() => {
                  setShowSeverityMenu(!showSeverityMenu);
                  setShowFilterMenu(false);
                  setShowCategoryMenu(false);
                }}
                className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm transition-colors ${
                  severityFilter !== "all"
                    ? "bg-primary/15 text-primary hover:bg-primary/20"
                    : "bg-muted/50 hover:bg-muted"
                }`}
              >
                <span>
                  {severityFilter === "all"
                    ? "Severity"
                    : (SEVERITY_OPTIONS.find((o) => o.value === severityFilter)?.label ??
                      severityFilter)}
                </span>
                <ChevronDown className="w-4 h-4" />
              </button>

              {showSeverityMenu && (
                <div className="absolute top-full left-0 mt-1 w-40 bg-popover border border-border rounded-lg shadow-lg z-50">
                  {SEVERITY_OPTIONS.map((option) => (
                    <button
                      key={option.value}
                      onClick={() => {
                        setSeverityFilter(option.value);
                        setShowSeverityMenu(false);
                      }}
                      className={`w-full text-left px-3 py-2 text-sm hover:bg-muted transition-colors ${
                        severityFilter === option.value ? "bg-muted font-medium" : ""
                      }`}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Category Filter Dropdown */}
            <div className="relative">
              <button
                onClick={() => {
                  setShowCategoryMenu(!showCategoryMenu);
                  setShowFilterMenu(false);
                  setShowSeverityMenu(false);
                }}
                className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm transition-colors ${
                  categoryFilter !== "all"
                    ? "bg-primary/15 text-primary hover:bg-primary/20"
                    : "bg-muted/50 hover:bg-muted"
                }`}
              >
                <span>
                  {categoryFilter === "all"
                    ? "Category"
                    : (categoryOptions.find((o) => o.value === categoryFilter)?.label ??
                      categoryFilter)}
                </span>
                <ChevronDown className="w-4 h-4" />
              </button>

              {showCategoryMenu && (
                <div className="absolute top-full left-0 mt-1 w-48 bg-popover border border-border rounded-lg shadow-lg z-50 max-h-64 overflow-y-auto">
                  {categoryOptions.map((option) => (
                    <button
                      key={option.value}
                      onClick={() => {
                        setCategoryFilter(option.value);
                        setShowCategoryMenu(false);
                      }}
                      className={`w-full text-left px-3 py-2 text-sm hover:bg-muted transition-colors ${
                        categoryFilter === option.value ? "bg-muted font-medium" : ""
                      }`}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Active Filter Count Badge + Clear */}
            {activeFilterCount > 0 && (
              <button
                onClick={clearAllFilters}
                className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs text-muted-foreground hover:text-foreground bg-muted/50 hover:bg-muted rounded-lg transition-colors"
                title="Clear all filters"
              >
                <span className="flex items-center justify-center w-4 h-4 rounded-full bg-primary/20 text-primary text-[10px] font-bold">
                  {activeFilterCount}
                </span>
                <X className="w-3 h-3" />
                <span>Clear</span>
              </button>
            )}
          </div>

          {/* Action Buttons */}
          <div className="flex items-center gap-2">
            {/* Auto-Fix Toggle */}
            <button
              onClick={toggleAutoFix}
              disabled={autoFixLoading}
              className={`flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-lg transition-colors ${
                autoFixEnabled
                  ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} hover:bg-blue-500/30`
                  : "bg-muted/50 text-muted-foreground hover:bg-muted"
              } ${autoFixLoading ? "opacity-50" : ""}`}
              title={
                autoFixEnabled
                  ? "Auto-fix enabled: AI will automatically fix issues"
                  : "Auto-fix disabled"
              }
            >
              <Wrench className="w-4 h-4" />
              {autoFixLoading ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : autoFixEnabled ? (
                <ToggleRight className="w-4 h-4" />
              ) : (
                <ToggleLeft className="w-4 h-4" />
              )}
              <span>Auto-Fix</span>
            </button>

            {/* Analyze & Fix All Button */}
            {summary.autoFixable > 0 && (
              <button
                onClick={handleAnalyzeAll}
                disabled={isAnalyzingAll}
                className={`flex items-center gap-1.5 px-3 py-1.5 text-sm ${getAccentColors("purple").bgSolid} hover:bg-purple-600 disabled:bg-muted disabled:cursor-not-allowed text-white rounded-lg transition-colors`}
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

      {/* Scrollable content — search results or live findings.
          When there are no live findings and no active search query, we render
          the empty-state message in the same slot the findings list would
          occupy, so the header + search region stays visible and usable. */}
      {isSearching ? (
        <SearchResultsList
          query={trimmedSearch}
          isFetching={searchResultsQuery.isFetching}
          error={searchResultsQuery.error}
          results={visibleSearchResults}
          onJump={handleJumpToFinding}
        />
      ) : !hasFindings ? (
        <div
          ref={findingsListRef}
          className="flex-1 flex flex-col items-center justify-center text-muted-foreground p-8"
        >
          <FileText className="w-12 h-12 mb-4 opacity-50" />
          <p className="text-lg font-medium">No Findings Yet</p>
          <p className="text-sm mt-2 text-center">
            Run an AI analysis to detect and categorize findings.
            <br />
            Findings will be grouped by category with action recommendations.
          </p>
          <p className="text-xs mt-4 text-center max-w-sm opacity-80">
            Use the search box above to look across deferred questions, errors, observations, and
            activity from the last 7 days.
          </p>
        </div>
      ) : (
        <div ref={findingsListRef} className="flex-1 min-h-0 overflow-y-auto p-4 space-y-4">
          {categories.map((category) => {
            const categoryFindings = findingsByCategory.get(category.id) || [];
            if (categoryFindings.length === 0) return null;

            return (
              <CategorySection
                key={category.id}
                category={category}
                findings={categoryFindings}
                totalFindingsCount={
                  totalFindingsByCategory.get(category.id) ?? categoryFindings.length
                }
                isFilterActive={isFilterActive}
                isHighlighted={categoryFilter === category.id}
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
                totalFindingsCount={
                  totalFindingsByCategory.get(categoryId) ?? categoryFindings.length
                }
                isFilterActive={isFilterActive}
                isHighlighted={categoryFilter === categoryId}
                onAnalyze={handleAnalyze}
                onResolve={onResolveFinding}
                onProvideInput={handleProvideInput}
                onDismiss={handleDismiss}
                processingFindingId={processingFindingId}
              />
            ))}
        </div>
      )}

      {/* Snippet modal — shown when Jump is clicked for a record not present locally */}
      {snippetModal && <SnippetModal result={snippetModal} onClose={() => setSnippetModal(null)} />}
    </div>
  );
}

// =============================================================================
// Search result rendering (inline sub-components)
// =============================================================================

interface SearchResultsListProps {
  query: string;
  isFetching: boolean;
  error: Error | null;
  results: SearchEventResult[];
  onJump: (result: SearchEventResult) => void;
}

function SearchResultsList({ query, isFetching, error, results, onJump }: SearchResultsListProps) {
  return (
    <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-2">
      {isFetching && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="w-4 h-4 animate-spin" />
          <span>Searching…</span>
        </div>
      )}

      {error &&
        !isFetching &&
        (() => {
          const errColors = getAccentColors("red");
          return (
            <div
              className={`flex items-start gap-2 rounded-lg p-3 text-sm ${errColors.bg} ${errColors.text} ${errColors.border} border`}
            >
              <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
              <div>
                <div className="font-medium">Search failed</div>
                <div className="text-xs opacity-80">{error.message}</div>
              </div>
            </div>
          );
        })()}

      {!isFetching && !error && results.length === 0 && (
        <div className="text-sm text-muted-foreground py-6 text-center">
          No events matched {`"${query}"`}.
        </div>
      )}

      {results.map((result, idx) => (
        <SearchResultCard
          key={`${result.source_table}:${result.record_id}:${idx}`}
          result={result}
          onJump={onJump}
        />
      ))}
    </div>
  );
}

interface SearchResultCardProps {
  result: SearchEventResult;
  onJump: (result: SearchEventResult) => void;
}

function SearchResultCard({ result, onJump }: SearchResultCardProps) {
  const meta = SOURCE_TABLE_META[result.source_table];
  const badge = getAccentColors(meta.accent);
  const isHitl = result.source_table === "deferred_questions";

  return (
    <div className="rounded-lg border border-border bg-muted/20 hover:bg-muted/40 transition-colors p-3">
      <div className="flex items-start gap-3">
        <span
          className={`shrink-0 inline-flex items-center px-2 py-0.5 rounded text-[11px] font-medium ${badge.bg} ${badge.text} ${badge.border} border`}
        >
          {meta.label}
        </span>
        <p
          className="flex-1 text-sm text-foreground overflow-hidden"
          style={{
            display: "-webkit-box",
            WebkitLineClamp: 3,
            WebkitBoxOrient: "vertical",
          }}
          title={result.snippet}
        >
          {result.snippet}
        </p>
        <div className="shrink-0 flex flex-col items-end gap-1 text-xs text-muted-foreground">
          <span className="font-mono">{formatTimestamp(result.ts)}</span>
          <span className="opacity-70">rank {result.score.toFixed(2)}</span>
          {isHitl && (
            <button
              type="button"
              onClick={() => onJump(result)}
              className="mt-1 inline-flex items-center gap-1 px-2 py-0.5 rounded bg-primary/15 text-primary hover:bg-primary/25 transition-colors"
              title="Jump to finding or open snippet"
            >
              <ExternalLink className="w-3 h-3" />
              Jump to finding
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

interface SnippetModalProps {
  result: SearchEventResult;
  onClose: () => void;
}

function SnippetModal({ result, onClose }: SnippetModalProps) {
  const meta = SOURCE_TABLE_META[result.source_table];
  const badge = getAccentColors(meta.accent);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-background/70 backdrop-blur-sm p-4"
      role="dialog"
      aria-modal="true"
      onClick={onClose}
    >
      <div
        className="max-w-lg w-full bg-popover border border-border rounded-lg shadow-lg p-4 space-y-3"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span
              className={`inline-flex items-center px-2 py-0.5 rounded text-[11px] font-medium ${badge.bg} ${badge.text} ${badge.border} border`}
            >
              {meta.label}
            </span>
            <span className="text-xs text-muted-foreground font-mono">
              {formatTimestamp(result.ts)}
            </span>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="text-muted-foreground hover:text-foreground"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        <div className="text-xs text-muted-foreground">
          Record ID: <span className="font-mono text-foreground">{result.record_id}</span>
        </div>
        <pre className="text-sm text-foreground whitespace-pre-wrap break-words max-h-72 overflow-y-auto bg-muted/30 rounded p-3">
          {result.snippet}
        </pre>
        <p className="text-xs text-muted-foreground">
          No matching finding card is currently in view. The snippet above is shown as-is.
        </p>
      </div>
    </div>
  );
}
