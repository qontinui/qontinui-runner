/**
 * FindingCard.tsx
 *
 * Component for displaying an individual finding with its category,
 * severity, status, and action buttons.
 */

import { useState } from "react";
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
import type { Finding, FindingCategory } from "../../types/findings";
import { getCategoryById, getCategoryColorClasses } from "../../services/FindingCategories";

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

const severityColors: Record<string, { bg: string; text: string; border: string }> = {
  critical: { bg: "bg-red-500/20", text: "text-red-400", border: "border-red-500/30" },
  high: { bg: "bg-orange-500/20", text: "text-orange-400", border: "border-orange-500/30" },
  medium: { bg: "bg-yellow-500/20", text: "text-yellow-400", border: "border-yellow-500/30" },
  low: { bg: "bg-blue-500/20", text: "text-blue-400", border: "border-blue-500/30" },
  info: { bg: "bg-slate-500/20", text: "text-slate-400", border: "border-slate-500/30" },
};

const statusBadges: Record<string, { label: string; color: string }> = {
  detected: { label: "New", color: "bg-blue-500" },
  in_progress: { label: "In Progress", color: "bg-amber-500" },
  needs_input: { label: "Needs Input", color: "bg-purple-500" },
  resolved: { label: "Resolved", color: "bg-green-500" },
  wont_fix: { label: "Won't Fix", color: "bg-slate-500" },
  deferred: { label: "Deferred", color: "bg-gray-500" },
};

export function FindingCard({
  finding,
  onAnalyze,
  onResolve,
  onProvideInput,
  onDismiss,
  isProcessing = false,
}: FindingCardProps) {
  const [isExpanded, setIsExpanded] = useState(false);
  const [inputValue, setInputValue] = useState("");
  const [showInputForm, setShowInputForm] = useState(false);

  const category = getCategoryById(finding.categoryId);
  const categoryColors = getCategoryColorClasses(category?.color || "slate");
  const severityColor = severityColors[finding.severity] || severityColors.info;
  const statusBadge = statusBadges[finding.status] || statusBadges.detected;

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
          <div className={`flex-shrink-0 p-2 rounded-lg ${categoryColors.bg}`}>
            <IconComponent className={`w-4 h-4 ${categoryColors.text}`} />
          </div>

          {/* Content */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap">
              {/* Title */}
              <h4 className="font-medium text-sm text-foreground truncate">{finding.title}</h4>

              {/* Status Badge */}
              <span
                className={`px-1.5 py-0.5 rounded text-[10px] font-medium text-white ${statusBadge.color}`}
              >
                {statusBadge.label}
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
          <div className="flex items-center gap-1 flex-shrink-0">
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
                className="flex items-center gap-1 px-2 py-1 text-xs bg-purple-500 text-white rounded hover:bg-purple-600 transition-colors"
                title="Provide input for this finding"
              >
                <MessageSquare className="w-3 h-3" />
                Respond
              </button>
            )}

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
                className="flex-1 px-3 py-2 text-sm bg-background border border-border rounded focus:outline-none focus:ring-2 focus:ring-primary"
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
                className="flex-1 px-3 py-2 text-sm bg-green-500/20 text-green-400 border border-green-500/30 rounded hover:bg-green-500/30 transition-colors"
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
                className="flex-1 px-3 py-2 text-sm bg-red-500/20 text-red-400 border border-red-500/30 rounded hover:bg-red-500/30 transition-colors"
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
