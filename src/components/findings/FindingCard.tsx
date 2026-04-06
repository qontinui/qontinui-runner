/**
 * FindingCard.tsx
 *
 * Component for displaying an individual finding with its category,
 * severity, status, and action buttons.
 */

import { useState, useEffect } from "react";
import { ShareToMobileButton } from "../ClipboardSync";
import {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
  ChevronDown,
  ChevronUp,
  Play,
  MessageSquare,
  X,
  Check,
  Clock,
  Loader2,
  ExternalLink,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import type { Finding } from "../../types/findings";
import type {
  CreateKnownIssueRequest,
  IssueCategory,
  KnownIssueSeverity,
  KnownIssue,
} from "@qontinui/shared-types";
import { getCategoryById, getCategoryColorClasses } from "../../services/FindingCategories";
import { getSeverityColors, getStatusColors, getAccentColors } from "@/design-system";

// Map finding category IDs to known issue categories
const FINDING_TO_ISSUE_CATEGORY: Record<string, IssueCategory> = {
  code_bug: "other",
  security: "other",
  performance: "performance",
  runtime_issue: "state",
  todo: "other",
  enhancement: "other",
  config_issue: "other",
  test_issue: "other",
  documentation: "other",
  warning: "other",
  data_migration: "other",
  already_fixed: "other",
  expected_behavior: "other",
};

// Map finding severity to known issue severity
const FINDING_TO_ISSUE_SEVERITY: Record<string, KnownIssueSeverity> = {
  critical: "critical",
  high: "high",
  medium: "medium",
  low: "low",
  info: "low",
};

// Map icon names to Lucide components
const iconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
};

interface FindingCardProps {
  finding: Finding;
  onAnalyze?: (finding: Finding) => void;
  onResolve?: (finding: Finding, resolution: string) => void;
  onProvideInput?: (finding: Finding, response: string) => void;
  onDismiss?: (finding: Finding) => void;
  isProcessing?: boolean;
}

// Status badge labels - colors come from design system
const statusLabels: Record<string, string> = {
  detected: "New",
  in_progress: "In Progress",
  needs_input: "Needs Input",
  resolved: "Resolved",
  wont_fix: "Won't Fix",
  deferred: "Deferred",
};

// Map finding status to design system status type
const statusTypeMap: Record<string, string> = {
  detected: "pending",
  in_progress: "running",
  needs_input: "warning",
  resolved: "success",
  wont_fix: "cancelled",
  deferred: "paused",
};

export function FindingCard({
  finding,
  onAnalyze,
  onResolve: _onResolve,
  onProvideInput,
  onDismiss,
  isProcessing = false,
}: FindingCardProps) {
  const [isExpanded, setIsExpanded] = useState(false);
  const [inputValue, setInputValue] = useState("");
  const [showInputForm, setShowInputForm] = useState(false);
  const [knownIssueState, setKnownIssueState] = useState<"idle" | "creating" | "created" | "error">(
    "idle",
  );
  const [existingIssueId, setExistingIssueId] = useState<string | null>(null);

  // Check if a known issue already exists for this finding
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const issues = await invoke<KnownIssue[]>("list_known_issues", { query: {} });
        if (cancelled) return;
        const match = issues.find(
          (issue) => issue.source_finding_ids.includes(finding.id) || issue.title === finding.title,
        );
        setExistingIssueId(match?.id ?? null);
      } catch {
        // Non-critical -- leave as null (show create button)
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [finding.id, finding.title]);

  const handleCreateKnownIssue = async () => {
    setKnownIssueState("creating");
    try {
      const request: CreateKnownIssueRequest = {
        title: finding.title,
        description: finding.description,
        category: FINDING_TO_ISSUE_CATEGORY[finding.categoryId] || "other",
        severity: FINDING_TO_ISSUE_SEVERITY[finding.severity] || "medium",
        detection_method: "ai_judgment",
        scope_type: "global",
        scope_value: null,
        provenance: "manual",
        source_finding_ids: [finding.id],
        verification_hint: finding.description.slice(0, 200),
      };
      await invoke("create_known_issue", { request });
      setKnownIssueState("created");
    } catch {
      setKnownIssueState("error");
      // Reset to idle after a brief delay so the user can retry
      setTimeout(() => setKnownIssueState("idle"), 3000);
    }
  };

  const category = getCategoryById(finding.categoryId);
  const categoryColors = getCategoryColorClasses(category?.color || "slate");
  const severityColor = getSeverityColors(finding.severity);
  const statusColors = getStatusColors(statusTypeMap[finding.status] || "pending");
  const statusLabel = statusLabels[finding.status] || "Unknown";

  const IconComponent = category ? iconMap[category.icon] || Info : Info;

  const handleProvideInput = () => {
    if (inputValue.trim() && onProvideInput) {
      onProvideInput(finding, inputValue.trim());
      setInputValue("");
      setShowInputForm(false);
    }
  };

  const canAnalyze =
    finding.actionType === "auto_fix" &&
    (finding.status === "detected" || finding.status === "in_progress");

  const needsInput = finding.status === "needs_input" && finding.pendingQuestion;

  return (
    <div
      className={`rounded-lg border ${categoryColors.border} ${categoryColors.bg} overflow-hidden transition-all`}
    >
      {/* Header */}
      <div className="p-3">
        <div className="flex items-start gap-3">
          {/* Category Icon */}
          <div className={`shrink-0 p-2 rounded-lg ${categoryColors.bg}`}>
            <IconComponent className={`w-4 h-4 ${categoryColors.text}`} />
          </div>

          {/* Content */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap">
              {/* Title */}
              <h4 className="font-medium text-sm text-foreground truncate">{finding.title}</h4>

              {/* Status Badge */}
              <span
                className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${statusColors.bg} ${statusColors.text}`}
              >
                {statusLabel}
              </span>

              {/* Severity Badge */}
              <span
                className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${severityColor.bg} ${severityColor.text} border ${severityColor.border}`}
              >
                {finding.severity}
              </span>
            </div>

            {/* Category Name */}
            <p className={`text-xs ${categoryColors.text} mt-0.5`}>
              {category?.name || finding.categoryId}
            </p>

            {/* File Context */}
            {finding.codeContext?.file && (
              <p className="text-xs text-muted-foreground mt-1 flex items-center gap-1">
                <FileText className="w-3 h-3" />
                {finding.codeContext.file}
                {finding.codeContext.line && `:${finding.codeContext.line}`}
              </p>
            )}
          </div>

          {/* Actions */}
          <div className="flex items-center gap-1 shrink-0">
            {/* Expand/Collapse */}
            <button
              onClick={() => setIsExpanded(!isExpanded)}
              className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
              title={isExpanded ? "Collapse" : "Expand"}
            >
              {isExpanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
            </button>

            {/* Analyze & Fix Button */}
            {canAnalyze && onAnalyze && (
              <button
                onClick={() => onAnalyze(finding)}
                disabled={isProcessing}
                className="flex items-center gap-1 px-2 py-1 text-xs bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 transition-colors"
                title="Analyze and fix this finding"
              >
                {isProcessing ? (
                  <Loader2 className="w-3 h-3 animate-spin" />
                ) : (
                  <Play className="w-3 h-3" />
                )}
                Fix
              </button>
            )}

            {/* Provide Input Button */}
            {needsInput && !showInputForm && (
              <button
                onClick={() => setShowInputForm(true)}
                className={`flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors ${getAccentColors("purple").bgSolid} text-white hover:opacity-90`}
                title="Provide input for this finding"
              >
                <MessageSquare className="w-3 h-3" />
                Respond
              </button>
            )}

            {/* Known Issue Button: shows linked state if issue exists, otherwise create */}
            {finding.status !== "resolved" &&
              finding.status !== "wont_fix" &&
              (existingIssueId ? (
                <span
                  className="p-1.5 rounded text-amber-400 bg-amber-500/10 cursor-default"
                  title="Known issue exists for this finding"
                >
                  <Bug className="w-4 h-4" />
                </span>
              ) : (
                <button
                  onClick={handleCreateKnownIssue}
                  disabled={knownIssueState === "creating" || knownIssueState === "created"}
                  className={`p-1.5 rounded transition-colors ${
                    knownIssueState === "created"
                      ? "text-green-400 bg-green-500/10"
                      : knownIssueState === "error"
                        ? "text-red-400 bg-red-500/10"
                        : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
                  } disabled:opacity-70`}
                  title={
                    knownIssueState === "created"
                      ? "Known issue created"
                      : knownIssueState === "error"
                        ? "Failed to create known issue"
                        : "Create Known Issue"
                  }
                >
                  {knownIssueState === "creating" ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : knownIssueState === "created" ? (
                    <Check className="w-4 h-4" />
                  ) : (
                    <Bug className="w-4 h-4" />
                  )}
                </button>
              ))}

            {/* Share to Mobile */}
            <ShareToMobileButton
              getText={() => {
                let text = `[${finding.severity}] ${finding.title}\n\n${finding.description}`;
                if (finding.codeContext?.snippet) {
                  text += `\n\nCode:\n${finding.codeContext.snippet}`;
                }
                return text;
              }}
              className="text-muted-foreground hover:text-blue-400 hover:bg-blue-500/10"
              size={16}
            />

            {/* Dismiss Button */}
            {finding.status !== "resolved" && onDismiss && (
              <button
                onClick={() => onDismiss(finding)}
                className="p-1.5 text-muted-foreground hover:text-red-400 hover:bg-red-500/10 rounded transition-colors"
                title="Dismiss this finding"
              >
                <X className="w-4 h-4" />
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Expanded Content */}
      {isExpanded && (
        <div className="px-3 pb-3 border-t border-border/50 pt-3 space-y-3">
          {/* Description */}
          <div>
            <h5 className="text-xs font-medium text-muted-foreground mb-1">Description</h5>
            <p className="text-sm text-foreground whitespace-pre-wrap">{finding.description}</p>
          </div>

          {/* Code Snippet */}
          {finding.codeContext?.snippet && (
            <div>
              <h5 className="text-xs font-medium text-muted-foreground mb-1">Code</h5>
              <pre className="text-xs bg-muted/50 p-2 rounded overflow-x-auto font-mono">
                {finding.codeContext.snippet}
              </pre>
            </div>
          )}

          {/* Resolution */}
          {finding.resolution && (
            <div>
              <h5 className="text-xs font-medium text-muted-foreground mb-1">Resolution</h5>
              <p className="text-sm text-green-400">{finding.resolution}</p>
            </div>
          )}

          {/* User Response */}
          {finding.userResponse && (
            <div>
              <h5 className="text-xs font-medium text-muted-foreground mb-1">Your Response</h5>
              <p className="text-sm text-foreground">{finding.userResponse}</p>
            </div>
          )}

          {/* Timestamps */}
          <div className="flex items-center gap-4 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <Clock className="w-3 h-3" />
              Detected: {new Date(finding.detectedAt).toLocaleString()}
            </span>
            {finding.resolvedAt && (
              <span className="flex items-center gap-1 text-green-400">
                <Check className="w-3 h-3" />
                Resolved: {new Date(finding.resolvedAt).toLocaleString()}
              </span>
            )}
          </div>

          {/* Open File Link */}
          {finding.codeContext?.file && (
            <button className="flex items-center gap-1 text-xs text-primary hover:text-primary/80 transition-colors">
              <ExternalLink className="w-3 h-3" />
              Open in editor
            </button>
          )}
        </div>
      )}

      {/* User Input Form */}
      {showInputForm && finding.pendingQuestion && (
        <div className="px-3 pb-3 border-t border-border/50 pt-3 space-y-3">
          <div>
            <h5 className="text-sm font-medium text-foreground mb-1">
              {finding.pendingQuestion.question}
            </h5>
            {finding.pendingQuestion.context && (
              <p className="text-xs text-muted-foreground mb-2">
                {finding.pendingQuestion.context}
              </p>
            )}
          </div>

          {/* Choice Options */}
          {finding.pendingQuestion.inputType === "choice" && finding.pendingQuestion.options && (
            <div className="space-y-2">
              {finding.pendingQuestion.options.map((option) => (
                <button
                  key={option.value}
                  onClick={() => {
                    setInputValue(option.value);
                    if (onProvideInput) {
                      onProvideInput(finding, option.value);
                      setShowInputForm(false);
                    }
                  }}
                  className="w-full text-left px-3 py-2 text-sm bg-muted/50 hover:bg-muted rounded border border-border/50 transition-colors"
                >
                  <div className="font-medium">{option.label}</div>
                  {option.description && (
                    <div className="text-xs text-muted-foreground mt-0.5">{option.description}</div>
                  )}
                </button>
              ))}
            </div>
          )}

          {/* Text Input */}
          {finding.pendingQuestion.inputType === "text" && (
            <div className="flex gap-2">
              <input
                type="text"
                value={inputValue}
                onChange={(e) => setInputValue(e.target.value)}
                placeholder="Enter your response..."
                className="flex-1 px-3 py-2 text-sm bg-background border border-border rounded focus:outline-hidden focus:ring-2 focus:ring-primary"
              />
              <button
                onClick={handleProvideInput}
                disabled={!inputValue.trim()}
                className="px-3 py-2 text-sm bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50 transition-colors"
              >
                Submit
              </button>
            </div>
          )}

          {/* Boolean Input */}
          {finding.pendingQuestion.inputType === "boolean" && (
            <div className="flex gap-2">
              <button
                onClick={() => {
                  if (onProvideInput) {
                    onProvideInput(finding, "yes");
                    setShowInputForm(false);
                  }
                }}
                className={`flex-1 px-3 py-2 text-sm rounded transition-colors ${getAccentColors("green").bg} ${getAccentColors("green").text} border ${getAccentColors("green").border} hover:opacity-80`}
              >
                Yes
              </button>
              <button
                onClick={() => {
                  if (onProvideInput) {
                    onProvideInput(finding, "no");
                    setShowInputForm(false);
                  }
                }}
                className={`flex-1 px-3 py-2 text-sm rounded transition-colors ${getAccentColors("red").bg} ${getAccentColors("red").text} border ${getAccentColors("red").border} hover:opacity-80`}
              >
                No
              </button>
            </div>
          )}

          {/* Cancel Button */}
          <button
            onClick={() => setShowInputForm(false)}
            className="text-xs text-muted-foreground hover:text-foreground transition-colors"
          >
            Cancel
          </button>
        </div>
      )}
    </div>
  );
}
