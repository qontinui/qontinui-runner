/**
 * AISummarySection Component
 *
 * Displays the AI-generated summary with goal achievement status.
 * Allows generating a summary if one doesn't exist.
 */

import { useState, useMemo } from "react";
import {
  Sparkles,
  Target,
  AlertTriangle,
  Loader2,
  FileText,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { getStatusColors, getAccentColors } from "@/design-system";
import { aiDataService } from "@/services";
import { aiDataKeys } from "@/hooks/useAiData";

interface AISummarySectionProps {
  aiSummary?: string | null;
  goalAchieved?: boolean | null;
  remainingWork?: string | null;
  summaryGeneratedAt?: string | null;
  taskRunId?: string;
  status?: string;
  onSummaryGenerated?: () => void;
}

// Threshold for showing expand/collapse (characters)
const SUMMARY_COLLAPSE_THRESHOLD = 300;

export function AISummarySection({
  aiSummary,
  goalAchieved,
  remainingWork,
  summaryGeneratedAt,
  taskRunId,
  status,
  onSummaryGenerated,
}: AISummarySectionProps) {
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isExpanded, setIsExpanded] = useState(false);
  const queryClient = useQueryClient();

  // Determine if summary is long enough to need expand/collapse
  const { isLongSummary, displayText } = useMemo(() => {
    if (!aiSummary) return { isLongSummary: false, displayText: "" };
    const isLong = aiSummary.length > SUMMARY_COLLAPSE_THRESHOLD;
    if (!isLong || isExpanded) {
      return { isLongSummary: isLong, displayText: aiSummary };
    }
    // Truncate at word boundary
    const truncated = aiSummary.slice(0, SUMMARY_COLLAPSE_THRESHOLD);
    const lastSpace = truncated.lastIndexOf(" ");
    return {
      isLongSummary: true,
      displayText: (lastSpace > 0 ? truncated.slice(0, lastSpace) : truncated) + "...",
    };
  }, [aiSummary, isExpanded]);

  const handleGenerateSummary = async () => {
    if (!taskRunId) return;

    setIsGenerating(true);
    setError(null);

    try {
      const result = await aiDataService.generateSummary(taskRunId);
      if (result.success) {
        queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRun(taskRunId) });
        queryClient.invalidateQueries({ queryKey: aiDataKeys.taskRuns() });
        onSummaryGenerated?.();
      } else {
        setError(result.error || "Failed to generate summary");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to generate summary");
    } finally {
      setIsGenerating(false);
    }
  };

  // If we have a summary, display it prominently
  if (aiSummary) {
    return (
      <div
        data-ui-id="recap-ai-summary-section"
        className="rounded-xl border-2 border-primary/30 bg-gradient-to-br from-primary/5 to-primary/10 p-5 space-y-4"
      >
        {/* Header */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-primary/20 rounded-lg">
              <Sparkles className="w-5 h-5 text-primary" />
            </div>
            <div>
              <h2 className="font-semibold text-lg text-foreground">AI Summary</h2>
              {summaryGeneratedAt && (
                <p data-ui-id="recap-summary-timestamp" className="text-xs text-muted-foreground">
                  Generated {new Date(summaryGeneratedAt).toLocaleString()}
                </p>
              )}
            </div>
          </div>
          {/* Goal Achievement Badge */}
          {goalAchieved !== undefined && goalAchieved !== null && (
            <span
              data-ui-id="recap-goal-badge"
              className={`flex items-center gap-2 px-3 py-1.5 text-sm font-medium rounded-full ${
                goalAchieved
                  ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                  : `${getAccentColors("amber").bg} ${getAccentColors("amber").text}`
              }`}
            >
              <Target className="w-4 h-4" />
              {goalAchieved ? "Goal Achieved" : "Goal Not Achieved"}
            </span>
          )}
        </div>

        {/* Summary Text */}
        <div className="space-y-2">
          <p
            data-ui-id="recap-ai-summary-text"
            className="text-foreground/90 leading-relaxed whitespace-pre-wrap"
          >
            {displayText}
          </p>
          {isLongSummary && (
            <button
              onClick={() => setIsExpanded(!isExpanded)}
              className="flex items-center gap-1 text-sm text-primary hover:text-primary/80 transition-colors"
            >
              {isExpanded ? (
                <>
                  <ChevronUp className="w-4 h-4" />
                  Show less
                </>
              ) : (
                <>
                  <ChevronDown className="w-4 h-4" />
                  Show more
                </>
              )}
            </button>
          )}
        </div>

        {/* Remaining Work (if goal not achieved) */}
        {remainingWork && !goalAchieved && (
          <div
            data-ui-id="recap-remaining-work"
            className={`p-4 rounded-lg ${getAccentColors("amber").bg} border ${getAccentColors("amber").border}`}
          >
            <div className="flex items-center gap-2 mb-2">
              <AlertTriangle className={`w-4 h-4 ${getAccentColors("amber").text}`} />
              <span className={`font-medium ${getAccentColors("amber").text}`}>Remaining Work</span>
            </div>
            <p className="text-amber-300/90 whitespace-pre-wrap">{remainingWork}</p>
          </div>
        )}
      </div>
    );
  }

  // No summary yet - show placeholder or loading state
  const isComplete = status === "complete" || status === "completed";

  if (isGenerating) {
    return (
      <div className="rounded-xl border border-border bg-muted/20 p-5">
        <div className="flex items-center gap-3 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin" />
          <span>Generating AI summary...</span>
        </div>
      </div>
    );
  }

  if (isComplete) {
    return (
      <div
        data-ui-id="recap-ai-summary-section"
        data-has-summary="false"
        className="rounded-xl border border-border bg-muted/20 p-5"
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3 text-muted-foreground">
            <FileText className="w-5 h-5 opacity-50" />
            <span data-ui-id="recap-ai-summary-text">No AI summary available for this run.</span>
          </div>
          <button
            data-ui-id="recap-generate-summary-btn"
            onClick={handleGenerateSummary}
            className="flex items-center gap-2 px-4 py-2 text-sm bg-primary/10 hover:bg-primary/20 text-primary rounded-lg transition-colors"
          >
            <Sparkles className="w-4 h-4" />
            Generate Summary
          </button>
        </div>
        {error && <p className="mt-3 text-sm text-red-400">Error: {error}</p>}
      </div>
    );
  }

  // Run is still in progress
  return (
    <div
      data-ui-id="recap-ai-summary-section"
      data-has-summary="false"
      className="rounded-xl border border-border bg-muted/20 p-5"
    >
      <div className="flex items-center gap-3 text-muted-foreground">
        <FileText className="w-5 h-5 opacity-50" />
        <span data-ui-id="recap-ai-summary-text">
          Run in progress. Summary will be available after completion.
        </span>
      </div>
      <p data-ui-id="recap-summary-timestamp" className="text-xs text-muted-foreground mt-2">
        Waiting for run to complete...
      </p>
    </div>
  );
}

export default AISummarySection;
