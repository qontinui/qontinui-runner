/**
 * AISummarySection Component
 *
 * Displays the AI-generated summary with goal achievement status.
 * Allows generating a summary if one doesn't exist.
 * Parses [USER_MESSAGE]...[/USER_MESSAGE] blocks and renders them
 * as styled user message bubbles.
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
  User,
} from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { getStatusColors, getAccentColors } from "@/design-system";
import { aiDataService } from "@/services";
import { aiDataKeys } from "@/hooks/useAiData";

// ============================================================================
// User Message Parsing
// ============================================================================

/**
 * A segment of summary text, either plain text or a user message.
 */
interface SummarySegment {
  type: "text" | "user_message";
  content: string;
}

/**
 * Parse text containing [USER_MESSAGE]...[/USER_MESSAGE] markers into segments.
 * Plain text between markers is preserved as "text" segments.
 * Content inside markers is extracted as "user_message" segments.
 */
function parseSummarySegments(text: string): SummarySegment[] {
  const segments: SummarySegment[] = [];
  const regex = /\[USER_MESSAGE\]\s*([\s\S]*?)\s*\[\/USER_MESSAGE\]/g;
  let lastIndex = 0;
  let match;

  while ((match = regex.exec(text)) !== null) {
    // Add any text before this user message
    if (match.index > lastIndex) {
      const before = text.slice(lastIndex, match.index).trim();
      if (before) {
        segments.push({ type: "text", content: before });
      }
    }
    // Add the user message content
    const userContent = match[1].trim();
    if (userContent) {
      segments.push({ type: "user_message", content: userContent });
    }
    lastIndex = match.index + match[0].length;
  }

  // Add any remaining text after the last user message
  if (lastIndex < text.length) {
    const remaining = text.slice(lastIndex).trim();
    if (remaining) {
      segments.push({ type: "text", content: remaining });
    }
  }

  return segments;
}

/**
 * Check if text contains any [USER_MESSAGE] markers.
 */
function hasUserMessageMarkers(text: string): boolean {
  return text.includes("[USER_MESSAGE]");
}

/**
 * Renders a styled user message bubble.
 */
function UserMessageBubble({ content }: { content: string }) {
  const blueColors = getAccentColors("blue");

  return (
    <div
      className={`flex items-start gap-2 rounded-lg p-3 border ${blueColors.bg} ${blueColors.border}`}
    >
      <div
        className={`flex-shrink-0 w-6 h-6 rounded-full flex items-center justify-center ${blueColors.bg} ${blueColors.text}`}
      >
        <User className="w-3.5 h-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <span className={`text-xs font-semibold ${blueColors.text}`}>You</span>
        <p className="text-foreground/90 text-sm leading-relaxed whitespace-pre-wrap mt-0.5">
          {content}
        </p>
      </div>
    </div>
  );
}

/**
 * Renders summary text with user messages styled as distinct bubbles.
 * Falls back to plain text rendering when no markers are present.
 */
function SummaryContent({ text }: { text: string }) {
  const segments = useMemo(() => parseSummarySegments(text), [text]);

  // If no user messages found, render as plain text (same as before)
  if (segments.length === 1 && segments[0].type === "text") {
    return (
      <p
        data-ui-id="recap-ai-summary-text"
        className="text-foreground/90 leading-relaxed whitespace-pre-wrap"
      >
        {segments[0].content}
      </p>
    );
  }

  return (
    <div data-ui-id="recap-ai-summary-text" className="space-y-3">
      {segments.map((segment, index) =>
        segment.type === "user_message" ? (
          <UserMessageBubble key={index} content={segment.content} />
        ) : (
          <p key={index} className="text-foreground/90 leading-relaxed whitespace-pre-wrap">
            {segment.content}
          </p>
        ),
      )}
    </div>
  );
}

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

  // Determine if summary is long enough to need expand/collapse.
  // For length calculation, strip [USER_MESSAGE] markers so they don't
  // inflate the character count and cause early truncation.
  const { isLongSummary, displayText } = useMemo(() => {
    if (!aiSummary) return { isLongSummary: false, displayText: "" };
    // Calculate visible length without marker tags
    const visibleText = aiSummary
      .replace(/\[USER_MESSAGE\]/g, "")
      .replace(/\[\/USER_MESSAGE\]/g, "");
    const isLong = visibleText.length > SUMMARY_COLLAPSE_THRESHOLD;
    if (!isLong || isExpanded) {
      return { isLongSummary: isLong, displayText: aiSummary };
    }
    // When truncating, we work with the full text (including markers) but
    // need to be careful not to cut in the middle of a marker tag.
    // If the text has user message markers, show it fully when collapsed
    // to avoid cutting markers mid-tag (the SummaryContent component
    // handles the visual presentation).
    if (hasUserMessageMarkers(aiSummary)) {
      // For texts with user messages, truncate based on visible content length
      // but keep full marker structure intact
      return { isLongSummary: true, displayText: aiSummary };
    }
    // Plain text truncation (no markers)
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
                <p
                  data-ui-id="recap-summary-timestamp"
                  data-content-role="metric"
                  data-content-label="summary generation time"
                  className="text-xs text-muted-foreground"
                >
                  Generated {new Date(summaryGeneratedAt).toLocaleString()}
                </p>
              )}
            </div>
          </div>
          {/* Goal Achievement Badge */}
          {goalAchieved !== undefined && goalAchieved !== null && (
            <span
              data-ui-id="recap-goal-badge"
              data-content-role="badge"
              data-content-label="goal achievement"
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
          <SummaryContent text={displayText} />
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
              <span
                data-content-role="heading"
                data-content-level="3"
                className={`font-medium ${getAccentColors("amber").text}`}
              >
                Remaining Work
              </span>
            </div>
            <p className="text-amber-300/90 whitespace-pre-wrap">{remainingWork}</p>
          </div>
        )}
      </div>
    );
  }

  // No summary yet - show placeholder or loading state
  const isTerminal =
    status === "complete" || status === "completed" || status === "failed" || status === "stopped";

  if (isGenerating) {
    return (
      <div className="rounded-xl border border-border bg-muted/20 p-5">
        <div className="flex items-center gap-3 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin" />
          <span data-content-role="status">Generating AI summary...</span>
        </div>
      </div>
    );
  }

  if (isTerminal) {
    return (
      <div
        data-ui-id="recap-ai-summary-section"
        data-has-summary="false"
        className="rounded-xl border border-border bg-muted/20 p-5"
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3 text-muted-foreground">
            <FileText className="w-5 h-5 opacity-50" />
            <span data-ui-id="recap-ai-summary-text" data-content-role="status">
              No AI summary available for this run.
            </span>
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
        <span data-ui-id="recap-ai-summary-text" data-content-role="status">
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
