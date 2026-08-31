/**
 * HealthScoreCard.tsx
 *
 * Gauge-style card showing the composite automation health score
 * with a breakdown of contributing factors.
 *
 * ## Unknown is a rendered state here, not a fallback value
 *
 * Every rate on `GET /ui-bridge/analytics/health-score` is nullable, and
 * `overall_score` is null whenever any of its inputs is. This card is the
 * PROJECTION of that typed unknown — it does not decide what unknown means and
 * it never substitutes a number for one.
 *
 * Concretely: a null score renders neither a `0`, nor a green "Good", nor a
 * blank card that reads as healthy. It renders an explicit "No data" state
 * that names the window queried and which inputs were missing, and it uses a
 * deliberately non-semantic muted colour so it cannot be mistaken for a
 * verdict. Before this, an empty window arrived as `overall_score: 0.70` —
 * `0.30*0 + 0.25*(1-0) + 0.25*(1-0) + 0.20`, two "no bad news" terms plus the
 * formula's base — and this card painted it yellow-green "Good".
 *
 * Audience profile `non-developer-owner`: viewers "cannot tell a good value
 * from a bad one and have no basis for choosing", so the surface must not hand
 * them one to choose from. `human-operator`: "a dashboard is not chrome when it
 * is the operator's only channel."
 */

import { Activity, AlertTriangle, HelpCircle, RefreshCw } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { withTimeout } from "@/lib/withTimeout";

interface AutomationHealthScore {
  /** null unless all three rates below are known. */
  overall_score: number | null;
  /** null when total_interactions === 0. */
  element_success_rate: number | null;
  /** null when regression_eligible_pairs === 0. */
  regression_rate: number | null;
  /** null when total_interactions === 0, INCLUDING when total_stalls > 0. */
  stall_frequency: number | null;
  /** Measured facts — always real integers, whatever the rates do. */
  total_interactions: number;
  total_elements: number;
  total_stalls: number;
  /** Coverage: the regression_rate denominator, otherwise invisible. */
  regression_eligible_pairs: number;
  /** Coverage: the window actually queried. */
  window_days: number;
  window_start_epoch_ms: number;
  /** Machine-actionable discriminator: which fields came back null. */
  unknown_fields: string[];
}

interface HealthScoreCardProps {
  days?: number;
}

const FIELD_LABELS: Record<string, string> = {
  element_success_rate: "Element Success Rate",
  regression_rate: "Regression Rate",
  stall_frequency: "Stall Frequency",
  overall_score: "Overall Score",
};

function scoreColor(score: number): string {
  if (score >= 0.9) return "text-green-500";
  if (score >= 0.7) return "text-yellow-500";
  if (score >= 0.5) return "text-orange-500";
  return "text-red-500";
}

function scoreLabel(score: number): string {
  if (score >= 0.9) return "Excellent";
  if (score >= 0.7) return "Good";
  if (score >= 0.5) return "Fair";
  return "Poor";
}

/**
 * Is there a score to colour and label at all?
 *
 * The colour/label functions above encode a verdict on a quality axis. Handing
 * either of them a stand-in for an unknown is how "no data" became a green-ish
 * "Good", so this is the only gate that may call them. `undefined` counts as
 * unknown too: a body from an older runner has no such key.
 */
export function hasScore(score: number | null | undefined): score is number {
  return typeof score === "number" && Number.isFinite(score);
}

/**
 * Format one breakdown value. A null rate renders "Unknown", never "0%".
 *
 * `Math.round((null as unknown as number) * 100)` is `0` in JavaScript, so
 * unconditional percent formatting would turn every unknown into a confident
 * 0% — the frontend re-committing the same defect the API just stopped
 * committing.
 */
export function formatMetricValue(
  value: number | null | undefined,
  format: "percent" | "count" = "percent",
): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "Unknown";
  return format === "percent" ? `${Math.round(value * 100)}%` : String(value);
}

/**
 * The human labels for the API's `unknown_fields` discriminator, minus
 * `overall_score` itself — the card is already saying the score is unknown, so
 * repeating it in the list of reasons says nothing.
 */
export function unknownInputLabels(unknownFields: string[] | undefined): string[] {
  return (unknownFields ?? [])
    .filter((f) => f !== "overall_score")
    .map((f) => FIELD_LABELS[f] ?? f);
}

function MetricRow({
  label,
  value,
  format = "percent",
}: {
  label: string;
  value: number | null;
  format?: "percent" | "count";
}) {
  const unknown = value === null || value === undefined;
  return (
    <div className="flex justify-between items-center text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span
        className={`tabular-nums font-medium ${unknown ? "text-muted-foreground italic" : ""}`}
        title={unknown ? "Not measured in this window — no denominator" : undefined}
      >
        {formatMetricValue(value, format)}
      </span>
    </div>
  );
}

export function HealthScoreCard({ days = 7 }: HealthScoreCardProps) {
  const { data, isLoading, isError, error, refetch, isFetching } = useQuery<AutomationHealthScore>({
    queryKey: ["graph-analytics", "health-score", days],
    queryFn: async ({ signal }) => {
      // iter4 B-2: bound the fetch so a hung backend (e.g. the PG pool
      // stall this remediation also fixes server-side, B-1) surfaces an
      // error+retry instead of an eternal "Loading health score…" spinner.
      // The AbortSignal cancels the in-flight request when the timeout wins.
      const res = await withTimeout(
        fetch(`http://localhost:9876/ui-bridge/analytics/health-score?days=${days}`, {
          signal,
        }),
        10_000,
        "health-score",
      );
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      return json.data;
    },
    staleTime: 30_000,
    refetchInterval: 60_000,
    retry: 1,
  });

  if (isError) {
    return (
      <div className="bg-card rounded-lg border border-border p-6">
        <div className="text-center py-4">
          <AlertTriangle className="w-8 h-8 mx-auto mb-2 text-red-500 opacity-80" />
          <p className="text-sm text-red-500">
            Failed to load health score
            {error instanceof Error ? `: ${error.message}` : ""}
          </p>
          <button
            type="button"
            onClick={() => refetch()}
            disabled={isFetching}
            className="btn-secondary mt-3 inline-flex items-center gap-2"
          >
            <RefreshCw className={`w-3.5 h-3.5 ${isFetching ? "animate-spin" : ""}`} />
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (isLoading || !data) {
    return (
      <div className="bg-card rounded-lg border border-border p-6">
        <div className="text-center text-muted-foreground py-4">Loading health score...</div>
      </div>
    );
  }

  const score = data.overall_score;
  const missing = unknownInputLabels(data.unknown_fields);
  const windowDays = data.window_days ?? days;

  return (
    <div className="bg-card rounded-lg border border-border p-6">
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-semibold flex items-center gap-2">
          <Activity className="w-4 h-4" />
          Automation Health
        </h3>
        <span className="text-xs text-muted-foreground">{windowDays}d window</span>
      </div>

      {/* Score display — or an explicit unknown. The unknown branch says WHY
          in the card itself: a muted "No data" that a reader could mistake for
          a styling choice is the same over-trust problem in a quieter font. */}
      <div className="text-center mb-6">
        {hasScore(score) ? (
          <>
            <div className={`text-5xl font-bold tabular-nums ${scoreColor(score)}`}>
              {Math.round(score * 100)}
            </div>
            <div className={`text-sm font-medium mt-1 ${scoreColor(score)}`}>
              {scoreLabel(score)}
            </div>
          </>
        ) : (
          <div data-testid="health-score-unknown">
            <HelpCircle className="w-8 h-8 mx-auto mb-2 text-muted-foreground opacity-70" />
            <div className="text-2xl font-semibold text-muted-foreground">No data</div>
            <p className="text-xs text-muted-foreground mt-2 max-w-xs mx-auto">
              No health score for the last {windowDays} days —{" "}
              {missing.length > 0
                ? `${missing.join(", ")} ${missing.length === 1 ? "was" : "were"} not measured.`
                : "an input was not measured."}{" "}
              This is not a score of zero and not a healthy window; nothing was measured to score.
            </p>
          </div>
        )}
      </div>

      {/* Breakdown. The counts below are measured facts and are shown even
          when every rate above them is unknown — total_stalls in particular
          reports its real value beside a null Stall Frequency. */}
      <div className="space-y-2 border-t border-border pt-4">
        <MetricRow label="Element Success Rate" value={data.element_success_rate} />
        <MetricRow label="Regression Rate" value={data.regression_rate} />
        <MetricRow label="Stall Frequency" value={data.stall_frequency} />
        <MetricRow label="Total Interactions" value={data.total_interactions} format="count" />
        <MetricRow label="Unique Elements" value={data.total_elements} format="count" />
        <MetricRow label="Total Stalls" value={data.total_stalls} format="count" />
      </div>
    </div>
  );
}
