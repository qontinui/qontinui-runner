/**
 * IssuesPanel.tsx
 *
 * Panel component that displays detected issues during AI-assisted automation.
 * Shows a summary of issues by status and severity, with expandable details.
 */

import { useState, useCallback } from "react";
import {
  AlertTriangle,
  CheckCircle,
  Clock,
  XCircle,
  ChevronDown,
  ChevronRight,
  FileText,
  Image,
  Terminal,
  TestTube,
  Brain,
  HelpCircle,
  Trash2,
  Bot,
  Loader2,
} from "lucide-react";
import {
  SEVERITY_CONFIG,
  STATUS_CONFIG,
  SOURCE_TYPE_LABELS,
  IssueSourceType,
} from "../types/issues";
import { useIssues, useAiTaskPolling } from "../hooks";
import type { UnifiedStatus } from "../services/UnifiedReportService";
import { issueTracker } from "../services/IssueTracker";

// Component is self-contained with expand/collapse functionality
type IssuesPanelProps = Record<string, never>;

/** Icon for source types */
const SOURCE_ICONS: Record<IssueSourceType, React.ReactNode> = {
  log: <FileText className="w-3 h-3" />,
  screenshot: <Image className="w-3 h-3" />,
  console: <Terminal className="w-3 h-3" />,
  test_output: <TestTube className="w-3 h-3" />,
  ai_analysis: <Brain className="w-3 h-3" />,
  other: <HelpCircle className="w-3 h-3" />,
};

/** Status icons */
const STATUS_ICONS: Record<string, React.ReactNode> = {
  detected: <AlertTriangle className="w-4 h-4" />,
  in_progress: <Clock className="w-4 h-4" />,
  resolved: <CheckCircle className="w-4 h-4" />,
  skipped: <XCircle className="w-4 h-4" />,
};

/** Build the analysis prompt for AI to analyze and fix issues */
function buildAnalyzePrompt(issuesJson: string): string {
  return `You are analyzing detected issues from a qontinui-runner session.

## Your Task

1. **Analyze** each issue to determine what type of finding it is
2. **Fix** code bugs, security issues, test issues, and documentation problems automatically
3. **Categorize** all findings appropriately using the structured output format
4. **Report** your findings in a categorized, structured format

## Current Issues

${issuesJson}

## Output Format

For EACH issue, output a structured finding using this format:

\`\`\`
[FINDING:category_id:severity]
Title: Brief title
Description: What was found and what was done (if fixed)
File: path/to/file (if applicable)
Line: line number (if applicable)
Resolution: What you fixed (if you fixed it)
[/FINDING]
\`\`\`

## Categories to Use

**Auto-fixable (fix these automatically):**
- \`code_bug\` - Actual code bugs → FIX IT, then report with Resolution field
- \`security\` - Security vulnerabilities → FIX IT, then report with Resolution field
- \`test_issue\` - Test code problems → FIX IT, then report with Resolution field
- \`documentation\` - Doc issues → FIX IT, then report with Resolution field

**Not code bugs (categorize appropriately):**
- \`already_fixed\` - Was fixed in a previous session
- \`expected_behavior\` - This is intentional/by design, not a bug
- \`config_issue\` - Configuration or environment problem (user must handle)
- \`runtime_issue\` - Operational issue, not a code bug
- \`data_migration\` - Requires database admin intervention
- \`enhancement\` - Improvement suggestion (use :needs_input if user decision required)
- \`todo\` - TODO item that needs user decision (use :needs_input)
- \`performance\` - Performance issue (use :needs_input for trade-off decisions)
- \`warning\` - Something to be aware of, no action needed

**When user input is needed:**
Add \`:needs_input\` to the marker and include Question/Options fields:
\`\`\`
[FINDING:enhancement:medium:needs_input]
Title: Feature suggestion
Description: Explanation
Question: What should we do?
Options: Option A | Option B | Option C
[/FINDING]
\`\`\`

## Severity Levels
- \`critical\` - System-breaking, security vulnerabilities, data loss
- \`high\` - Major functionality broken
- \`medium\` - Should be addressed soon
- \`low\` - Minor issues
- \`info\` - Informational only

## Instructions

1. Read the relevant files to understand each issue
2. For auto-fixable categories: Make the fix in the code, then output the finding with a Resolution field
3. For non-actionable items: Categorize them appropriately so users understand why no action is needed
4. For items needing user input: Use the :needs_input modifier with a clear question

Work autonomously. Fix what you can, categorize everything appropriately.`;
}

export function IssuesPanel(_props: IssuesPanelProps) {
  // Use unified issues hook for reactive data
  const { items: issues, summary, resolveItem, updateItemStatus, clearSession } = useIssues();

  const [expandedIssues, setExpandedIssues] = useState<Set<string>>(new Set());
  const [isAnalyzing, setIsAnalyzing] = useState(false);

  const toggleExpanded = useCallback((issueId: string) => {
    setExpandedIssues((prev) => {
      const next = new Set(prev);
      if (next.has(issueId)) {
        next.delete(issueId);
      } else {
        next.add(issueId);
      }
      return next;
    });
  }, []);

  const handleStatusChange = useCallback(
    (issueId: string, newStatus: "in_progress" | "resolved" | "skipped") => {
      if (newStatus === "resolved") {
        resolveItem(issueId, "Manually marked as resolved");
      } else {
        updateItemStatus(issueId, newStatus as UnifiedStatus);
      }
    },
    [resolveItem, updateItemStatus],
  );

  const handleClearAll = useCallback(() => {
    clearSession();
  }, [clearSession]);

  // AI task polling hook for analyze and fix
  const { triggerTask: triggerAnalyzeAndFix } = useAiTaskPolling({
    onComplete: () => {
      setIsAnalyzing(false);
      // The AI output will contain [ISSUE:RESOLVED] and [ISSUE:REMOVED] markers
      // that are processed by IssueTracker.processLine() in AiOutputTab
    },
    onError: (error) => {
      console.error("Failed to analyze issues:", error);
      setIsAnalyzing(false);
    },
  });

  const handleAnalyzeAndFix = useCallback(async () => {
    setIsAnalyzing(true);

    const currentIssues = issueTracker.getSessionIssues();

    // Build prompt with issues data (only include relevant fields)
    const issuesJson = JSON.stringify(
      currentIssues.map((i) => ({
        id: i.id,
        type: i.type,
        severity: i.severity,
        title: i.title,
        description: i.description,
        file: i.file,
        line: i.line,
        source: i.source,
        status: i.status,
      })),
      null,
      2,
    );

    const prompt = buildAnalyzePrompt(issuesJson);

    // Use async task triggering with polling
    await triggerAnalyzeAndFix({
      name: "ai-analysis",
      content: prompt,
      max_sessions: 1,
      display_prompt: "Analyze and fix detected issues",
      timeout_seconds: 600,
    });
  }, [triggerAnalyzeAndFix]);

  if (issues.length === 0) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground">
        <AlertTriangle className="w-12 h-12 mb-4 opacity-50" />
        <p className="text-lg font-medium">No Issues Detected</p>
        <p className="text-sm mt-2">Issues found by AI will appear here during automation</p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      {/* Summary Stats */}
      {summary && (
        <div className="flex-shrink-0 p-4 border-b border-border">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-semibold">Session Summary</h3>
            <div className="flex items-center gap-2">
              <button
                onClick={handleAnalyzeAndFix}
                disabled={isAnalyzing || issues.length === 0}
                className="flex items-center gap-1.5 px-2.5 py-1 text-xs bg-purple-500 hover:bg-purple-600 disabled:bg-gray-600 disabled:cursor-not-allowed text-white rounded transition-colors"
                title="AI will analyze issues, fix what's fixable, and remove invalid issues"
              >
                {isAnalyzing ? (
                  <>
                    <Loader2 className="w-3 h-3 animate-spin" />
                    <span>Analyzing...</span>
                  </>
                ) : (
                  <>
                    <Bot className="w-3 h-3" />
                    <span>Analyze & Fix</span>
                  </>
                )}
              </button>
              <button
                onClick={handleClearAll}
                className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
                title="Clear all issues"
              >
                <Trash2 className="w-3 h-3" />
                Clear
              </button>
            </div>
          </div>

          {/* Status counts */}
          <div className="grid grid-cols-4 gap-2 mb-3">
            <div className="flex flex-col items-center p-2 rounded bg-red-500/10">
              <span className="text-lg font-bold text-red-400">{summary.byStatus.detected}</span>
              <span className="text-xs text-muted-foreground">Detected</span>
            </div>
            <div className="flex flex-col items-center p-2 rounded bg-yellow-500/10">
              <span className="text-lg font-bold text-yellow-400">
                {summary.byStatus.in_progress}
              </span>
              <span className="text-xs text-muted-foreground">In Progress</span>
            </div>
            <div className="flex flex-col items-center p-2 rounded bg-green-500/10">
              <span className="text-lg font-bold text-green-400">{summary.byStatus.resolved}</span>
              <span className="text-xs text-muted-foreground">Resolved</span>
            </div>
            <div className="flex flex-col items-center p-2 rounded bg-gray-500/10">
              <span className="text-lg font-bold text-gray-400">{summary.byStatus.skipped}</span>
              <span className="text-xs text-muted-foreground">Skipped</span>
            </div>
          </div>

          {/* Severity breakdown */}
          <div className="flex gap-4 text-xs">
            {(["critical", "high", "medium", "low"] as const).map((severity) => {
              const count = summary.bySeverity[severity];
              if (count === 0) return null;
              const config = SEVERITY_CONFIG[severity];
              return (
                <div key={severity} className="flex items-center gap-1">
                  <span className={`w-2 h-2 rounded-full ${config.bgColor}`} />
                  <span className={config.color}>{count}</span>
                  <span className="text-muted-foreground">{config.label}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Issues List */}
      <div className="flex-1 overflow-auto p-2">
        <div className="space-y-2">
          {issues.map((issue) => {
            const isExpanded = expandedIssues.has(issue.id);
            const severityConfig = SEVERITY_CONFIG[issue.severity];
            const statusConfig = STATUS_CONFIG[issue.status];

            return (
              <div
                key={issue.id}
                className={`border rounded-lg overflow-hidden ${severityConfig.bgColor} border-${issue.severity === "critical" ? "red" : issue.severity === "high" ? "orange" : issue.severity === "medium" ? "yellow" : "blue"}-500/30`}
              >
                {/* Issue Header */}
                <div
                  className="flex items-center gap-2 p-3 cursor-pointer hover:bg-black/5 transition-colors"
                  onClick={() => toggleExpanded(issue.id)}
                >
                  {/* Expand/Collapse */}
                  {isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  )}

                  {/* Status Icon */}
                  <span className={statusConfig.color}>{STATUS_ICONS[issue.status]}</span>

                  {/* Title */}
                  <span className="flex-1 font-medium text-sm truncate">{issue.title}</span>

                  {/* Severity Badge */}
                  <span
                    className={`px-2 py-0.5 text-xs rounded ${severityConfig.bgColor} ${severityConfig.color}`}
                  >
                    {severityConfig.label}
                  </span>

                  {/* Source indicator */}
                  {issue.originalIssue?.source && (
                    <span
                      className="flex items-center gap-1 text-xs text-muted-foreground"
                      title={SOURCE_TYPE_LABELS[issue.originalIssue.source.type]}
                    >
                      {SOURCE_ICONS[issue.originalIssue.source.type]}
                    </span>
                  )}
                </div>

                {/* Expanded Details */}
                {isExpanded && (
                  <div className="px-3 pb-3 border-t border-border/50">
                    {/* Description */}
                    <div className="mt-2">
                      <p className="text-sm text-foreground/80 whitespace-pre-wrap">
                        {issue.description}
                      </p>
                    </div>

                    {/* Location */}
                    {(issue.file || issue.line) && (
                      <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                        <FileText className="w-3 h-3" />
                        <span>
                          {issue.file}
                          {issue.line && `:${issue.line}`}
                        </span>
                      </div>
                    )}

                    {/* Source */}
                    {issue.originalIssue?.source && (
                      <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                        {SOURCE_ICONS[issue.originalIssue.source.type]}
                        <span>
                          Found in:{" "}
                          {issue.originalIssue.source.description ||
                            SOURCE_TYPE_LABELS[issue.originalIssue.source.type]}
                          {issue.originalIssue.source.path &&
                            ` (${issue.originalIssue.source.path})`}
                          {issue.originalIssue.source.line_range &&
                            ` lines ${issue.originalIssue.source.line_range[0]}-${issue.originalIssue.source.line_range[1]}`}
                        </span>
                      </div>
                    )}

                    {/* Resolution */}
                    {issue.resolution && (
                      <div className="mt-2 p-2 rounded bg-green-500/10 text-sm">
                        <span className="font-medium text-green-400">Resolution: </span>
                        <span className="text-foreground/80">{issue.resolution}</span>
                      </div>
                    )}

                    {/* Timestamps */}
                    <div className="mt-2 flex gap-4 text-xs text-muted-foreground">
                      <span>Detected: {new Date(issue.detectedAt).toLocaleTimeString()}</span>
                      {issue.resolvedAt && (
                        <span>Resolved: {new Date(issue.resolvedAt).toLocaleTimeString()}</span>
                      )}
                    </div>

                    {/* Actions */}
                    {issue.status !== "resolved" && issue.status !== "skipped" && (
                      <div className="mt-3 flex gap-2">
                        {issue.status === "detected" && (
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              handleStatusChange(issue.id, "in_progress");
                            }}
                            className="px-2 py-1 text-xs bg-yellow-500/20 text-yellow-400 rounded hover:bg-yellow-500/30 transition-colors"
                          >
                            Mark In Progress
                          </button>
                        )}
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleStatusChange(issue.id, "resolved");
                          }}
                          className="px-2 py-1 text-xs bg-green-500/20 text-green-400 rounded hover:bg-green-500/30 transition-colors"
                        >
                          Mark Resolved
                        </button>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleStatusChange(issue.id, "skipped");
                          }}
                          className="px-2 py-1 text-xs bg-gray-500/20 text-gray-400 rounded hover:bg-gray-500/30 transition-colors"
                        >
                          Skip
                        </button>
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
