/**
 * FeedbackScoresPanel
 *
 * Displays feedback scores for an execution run. Shows a summary strip
 * (average score, count by source) followed by individual score rows with
 * colored progress bars and expandable reason text.
 */

import { useMemo, useState } from "react";
import {
  BarChart3,
  ChevronDown,
  ChevronRight,
  Loader2,
  MessageSquare,
  Plus,
  RefreshCw,
  X,
  XCircle,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import {
  useFeedbackScores,
  type FeedbackScore,
  type FeedbackScoreSource,
  type FeedbackScoresSummary,
} from "../../hooks/useFeedbackScores";

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

interface FeedbackScoresPanelProps {
  runId: string;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SOURCE_LABELS: Record<FeedbackScoreSource, string> = {
  manual: "Manual",
  automated: "Automated",
  llm_judge: "LLM Judge",
};

const SOURCE_BADGE_VARIANT: Record<FeedbackScoreSource, "info" | "success" | "purple"> = {
  manual: "info",
  automated: "success",
  llm_judge: "purple",
};

/** Returns a Tailwind bg class based on 0-1 score value. */
function scoreBarColor(value: number): string {
  if (value >= 0.8) return "bg-green-500";
  if (value >= 0.5) return "bg-yellow-500";
  return "bg-red-500";
}

/** Format score as percentage string. */
function formatScore(value: number): string {
  return `${(value * 100).toFixed(0)}%`;
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function SummaryStrip({ summary }: { summary: FeedbackScoresSummary }) {
  return (
    <div className="flex flex-wrap items-center gap-4 p-3 rounded-lg bg-muted/40 border border-border text-sm">
      {/* Average score */}
      <div className="flex items-center gap-2">
        <BarChart3 className="w-4 h-4 text-muted-foreground" />
        <span className="text-muted-foreground">Avg Score</span>
        <span className={cn("font-semibold tabular-nums", scoreBarColor(summary.averageScore).replace("bg-", "text-"))}>
          {formatScore(summary.averageScore)}
        </span>
      </div>

      {/* Divider */}
      <div className="h-4 w-px bg-border" />

      {/* Count by source */}
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-muted-foreground">{summary.totalCount} scores:</span>
        {(Object.entries(summary.countBySource) as [FeedbackScoreSource, number][])
          .filter(([, count]) => count > 0)
          .map(([source, count]) => (
            <Badge key={source} variant={SOURCE_BADGE_VARIANT[source]} size="sm">
              {SOURCE_LABELS[source]} ({count})
            </Badge>
          ))}
      </div>
    </div>
  );
}

function ScoreRow({ score }: { score: FeedbackScore }) {
  const [expanded, setExpanded] = useState(false);
  const hasReason = !!score.reason;

  return (
    <div className="rounded-lg border border-border bg-card">
      <div
        className={cn(
          "flex items-center gap-3 p-3",
          hasReason && "cursor-pointer hover:bg-muted/30 transition-colors",
        )}
        role={hasReason ? "button" : undefined}
        aria-label={hasReason ? `${expanded ? "Collapse" : "Expand"} reason for ${score.name}` : undefined}
        onClick={() => hasReason && setExpanded(!expanded)}
      >
        {/* Expand indicator */}
        <span className="w-4 shrink-0 text-muted-foreground">
          {hasReason ? (
            expanded ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )
          ) : null}
        </span>

        {/* Score name */}
        <span className="text-sm font-medium truncate min-w-0 flex-1">{score.name}</span>

        {/* Source badge */}
        <Badge variant={SOURCE_BADGE_VARIANT[score.source]} size="sm">
          {SOURCE_LABELS[score.source]}
        </Badge>

        {/* Score bar + value */}
        <div className="flex items-center gap-2 shrink-0 w-36">
          <div className="flex-1 h-2 rounded-full bg-muted overflow-hidden">
            <div
              className={cn("h-full rounded-full transition-all", scoreBarColor(score.value))}
              style={{ width: `${Math.max(score.value * 100, 2)}%` }}
            />
          </div>
          <span className="text-xs font-medium tabular-nums w-10 text-right">
            {formatScore(score.value)}
          </span>
        </div>
      </div>

      {/* Expandable reason */}
      {expanded && score.reason && (
        <div className="px-3 pb-3 pt-0 ml-7 border-t border-border">
          <div className="flex items-start gap-2 mt-2 text-sm text-muted-foreground">
            <MessageSquare className="w-3.5 h-3.5 mt-0.5 shrink-0" />
            <p className="whitespace-pre-wrap">{score.reason}</p>
          </div>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Add Score Form
// ---------------------------------------------------------------------------

interface AddScoreFormProps {
  onSubmit: (data: { name: string; value: number; reason?: string }) => void;
  onCancel: () => void;
  isSubmitting: boolean;
}

function AddScoreForm({ onSubmit, onCancel, isSubmitting }: AddScoreFormProps) {
  const [name, setName] = useState("");
  const [value, setValue] = useState(0.5);
  const [reason, setReason] = useState("");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    onSubmit({
      name: name.trim(),
      value,
      reason: reason.trim() || undefined,
    });
  };

  return (
    <form
      onSubmit={handleSubmit}
      className="rounded-lg border border-border bg-card p-3 space-y-3"
    >
      <div className="grid gap-2 sm:grid-cols-2">
        <div>
          <label htmlFor="score-name" className="block text-xs font-medium text-muted-foreground mb-1">
            Name
          </label>
          <input
            id="score-name"
            type="text"
            required
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. accuracy"
            className="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring"
          />
        </div>
        <div>
          <label htmlFor="score-value" className="block text-xs font-medium text-muted-foreground mb-1">
            Value (0 – 1)
          </label>
          <input
            id="score-value"
            type="number"
            required
            min={0}
            max={1}
            step={0.01}
            value={value}
            onChange={(e) => setValue(Number(e.target.value))}
            className="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm tabular-nums outline-none focus:ring-2 focus:ring-ring"
          />
        </div>
      </div>

      <div>
        <label htmlFor="score-reason" className="block text-xs font-medium text-muted-foreground mb-1">
          Reason <span className="text-muted-foreground/60">(optional)</span>
        </label>
        <textarea
          id="score-reason"
          rows={2}
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder="Why this score was given..."
          className="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm outline-none focus:ring-2 focus:ring-ring resize-none"
        />
      </div>

      <div className="flex items-center justify-end gap-2">
        <Button type="button" variant="ghost" size="sm" onClick={onCancel} disabled={isSubmitting}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={isSubmitting || !name.trim()}>
          {isSubmitting ? <Loader2 className="w-3.5 h-3.5 animate-spin mr-1.5" /> : null}
          Submit
        </Button>
      </div>
    </form>
  );
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export function FeedbackScoresPanel({ runId }: FeedbackScoresPanelProps) {
  const { scores, summary, loading, error, refetch, createScore } = useFeedbackScores(runId);
  const [showForm, setShowForm] = useState(false);

  const memoizedSummary = useMemo(() => summary, [summary]);

  // Loading
  if (loading) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
        <Loader2 className="w-8 h-8 animate-spin mb-3 opacity-50" />
        <p className="text-sm">Loading feedback scores...</p>
      </div>
    );
  }

  // Error
  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
        <XCircle className="w-8 h-8 mb-3 text-red-500 opacity-50" />
        <p className="text-sm font-medium">Failed to load feedback scores</p>
        <p className="text-xs mt-1">{error.message}</p>
        <Button
          variant="outline"
          size="sm"
          className="mt-3"
          onClick={() => refetch()}
        >
          <RefreshCw className="w-3.5 h-3.5 mr-1.5" />
          Retry
        </Button>
      </div>
    );
  }

  // Empty state
  if (scores.length === 0 && !showForm) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
        <BarChart3 className="w-8 h-8 mb-3 opacity-40" />
        <p className="text-sm font-medium">No Feedback Scores</p>
        <p className="text-xs mt-1">
          Scores will appear here once manual, automated, or LLM judge feedback is recorded.
        </p>
        <Button
          variant="outline"
          size="sm"
          className="mt-3"
          onClick={() => setShowForm(true)}
        >
          <Plus className="w-3.5 h-3.5 mr-1.5" />
          Add Score
        </Button>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium text-muted-foreground">Feedback Scores</h3>
        {!showForm && (
          <Button variant="outline" size="sm" onClick={() => setShowForm(true)}>
            <Plus className="w-3.5 h-3.5 mr-1.5" />
            Add Score
          </Button>
        )}
        {showForm && (
          <Button variant="ghost" size="sm" onClick={() => setShowForm(false)}>
            <X className="w-3.5 h-3.5 mr-1.5" />
            Cancel
          </Button>
        )}
      </div>

      {/* Inline add score form */}
      {showForm && (
        <AddScoreForm
          isSubmitting={createScore.isPending}
          onCancel={() => setShowForm(false)}
          onSubmit={(data) => {
            createScore.mutate(data, {
              onSuccess: () => setShowForm(false),
            });
          }}
        />
      )}

      {/* Summary */}
      {memoizedSummary && <SummaryStrip summary={memoizedSummary} />}

      {/* Individual scores */}
      <div className="space-y-2">
        {scores.map((score) => (
          <ScoreRow key={score.id} score={score} />
        ))}
      </div>
    </div>
  );
}
