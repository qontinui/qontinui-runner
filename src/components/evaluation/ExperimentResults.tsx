/**
 * ExperimentResults
 *
 * Table of experiment results: input, output, scores (colored badges),
 * duration, cost. Summary stats at top: avg scores, total cost, total tokens.
 * Supports client-side pagination (20 per page) and column sorting.
 */

import { useState, useMemo } from "react";
import { RefreshCw, BarChart3, ChevronUp, ChevronDown, ChevronsUpDown, Play } from "lucide-react";
import { Badge } from "@/components/ui/Badge";
import { Card, CardContent } from "@/components/ui/Card";
import type {
  ExperimentResult,
  ExperimentResultsSummary,
  ExperimentResultScore,
} from "@/hooks/useEvaluationExperiments";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PAGE_SIZE = 20;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ExperimentResultsProps {
  results: ExperimentResult[];
  summary: ExperimentResultsSummary | null;
  isLoading: boolean;
  error: string | null;
  /** When provided, shows an "Evaluate" button per row that pre-fills the live evaluation panel. */
  onEvaluateRow?: (input: string, output: string) => void;
}

type SortField = "duration" | "cost" | "tokens" | `score:${string}`;
type SortDirection = "asc" | "desc";

interface SortState {
  field: SortField | null;
  direction: SortDirection;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function scoreVariant(value: number): "success" | "warning" | "danger" {
  if (value >= 0.8) return "success";
  if (value >= 0.5) return "warning";
  return "danger";
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function formatCost(cents: number): string {
  if (cents < 1) return `$${(cents / 100).toFixed(4)}`;
  return `$${(cents / 100).toFixed(2)}`;
}

function getScoreValue(result: ExperimentResult, scoreName: string): number {
  const score = result.scores.find((s) => s.name === scoreName);
  return score?.value ?? 0;
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function ScoreBadges({ scores }: { scores: ExperimentResultScore[] }) {
  if (scores.length === 0) return <span className="text-muted-foreground">-</span>;
  return (
    <div className="flex flex-wrap gap-1">
      {scores.map((s) => (
        <Badge key={s.name} variant={scoreVariant(s.value)} size="sm">
          {s.name}: {(s.value * 100).toFixed(0)}%
        </Badge>
      ))}
    </div>
  );
}

function SortIndicator({ field, sort }: { field: SortField; sort: SortState }) {
  if (sort.field !== field) {
    return <ChevronsUpDown className="w-3 h-3 ml-1 opacity-40 inline" />;
  }
  return sort.direction === "asc" ? (
    <ChevronUp className="w-3 h-3 ml-1 inline" />
  ) : (
    <ChevronDown className="w-3 h-3 ml-1 inline" />
  );
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

function sortResults(
  results: ExperimentResult[],
  sort: SortState,
): ExperimentResult[] {
  if (!sort.field) return results;

  const sorted = [...results].sort((a, b) => {
    let aVal: number;
    let bVal: number;

    if (sort.field === "duration") {
      aVal = a.duration_ms;
      bVal = b.duration_ms;
    } else if (sort.field === "cost") {
      aVal = a.cost_cents;
      bVal = b.cost_cents;
    } else if (sort.field === "tokens") {
      aVal = a.tokens_used;
      bVal = b.tokens_used;
    } else if (sort.field?.startsWith("score:")) {
      const scoreName = sort.field.slice(6);
      aVal = getScoreValue(a, scoreName);
      bVal = getScoreValue(b, scoreName);
    } else {
      return 0;
    }

    return aVal - bVal;
  });

  if (sort.direction === "desc") sorted.reverse();
  return sorted;
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function ExperimentResults({ results, summary, isLoading, error, onEvaluateRow }: ExperimentResultsProps) {
  const [page, setPage] = useState(0);
  const [sort, setSort] = useState<SortState>({ field: null, direction: "desc" });

  // Collect all unique score names for sortable headers
  const scoreNames = useMemo(() => {
    const names = new Set<string>();
    for (const r of results) {
      for (const s of r.scores) {
        names.add(s.name);
      }
    }
    return Array.from(names).sort();
  }, [results]);

  // Sort, then paginate
  const sorted = useMemo(() => sortResults(results, sort), [results, sort]);
  const totalPages = Math.max(1, Math.ceil(sorted.length / PAGE_SIZE));
  // Reset page if it goes out of bounds after data change
  const safePage = Math.min(page, totalPages - 1);
  const start = safePage * PAGE_SIZE;
  const pageResults = sorted.slice(start, start + PAGE_SIZE);

  function toggleSort(field: SortField) {
    setSort((prev) => {
      if (prev.field === field) {
        return { field, direction: prev.direction === "asc" ? "desc" : "asc" };
      }
      return { field, direction: "desc" };
    });
    setPage(0);
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12 text-muted-foreground">
        <RefreshCw className="w-5 h-5 animate-spin" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center py-12 text-muted-foreground">
        <p className="text-sm">Failed to load results: {error}</p>
      </div>
    );
  }

  if (results.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground gap-2">
        <BarChart3 className="w-8 h-8 opacity-50" />
        <p className="text-sm">No results yet</p>
        <p className="text-xs">Results will appear as the experiment runs</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Summary cards */}
      {summary && (
        <div className="px-4 py-3 border-b border-border/50">
          <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
            {/* Avg scores */}
            {Object.entries(summary.avg_scores).map(([name, value]) => (
              <Card key={name} className="!p-0">
                <CardContent className="!px-3 !py-2">
                  <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{name}</p>
                  <p className="text-lg font-semibold">{(value * 100).toFixed(1)}%</p>
                </CardContent>
              </Card>
            ))}
            {/* Total cost */}
            <Card className="!p-0">
              <CardContent className="!px-3 !py-2">
                <p className="text-[10px] uppercase tracking-wider text-muted-foreground">Total Cost</p>
                <p className="text-lg font-semibold">{formatCost(summary.total_cost_cents)}</p>
              </CardContent>
            </Card>
            {/* Total tokens */}
            <Card className="!p-0">
              <CardContent className="!px-3 !py-2">
                <p className="text-[10px] uppercase tracking-wider text-muted-foreground">Total Tokens</p>
                <p className="text-lg font-semibold">{summary.total_tokens.toLocaleString()}</p>
              </CardContent>
            </Card>
            {/* Results count */}
            <Card className="!p-0">
              <CardContent className="!px-3 !py-2">
                <p className="text-[10px] uppercase tracking-wider text-muted-foreground">Results</p>
                <p className="text-lg font-semibold">{summary.result_count}</p>
              </CardContent>
            </Card>
          </div>
        </div>
      )}

      {/* Results table */}
      <div className="flex-1 overflow-auto">
        <table className="w-full text-xs" data-tutorial-id="experiment-results-table">
          <thead className="sticky top-0 bg-card z-10">
            <tr className="border-b border-border/50 text-left text-muted-foreground">
              <th className="px-4 py-2 font-medium">Input</th>
              <th className="px-4 py-2 font-medium">Output</th>
              <th className="px-4 py-2 font-medium">
                Scores
                {scoreNames.length > 0 && (
                  <span className="ml-1 text-[10px] opacity-60">
                    ({scoreNames.map((name) => (
                      <button
                        key={name}
                        onClick={() => toggleSort(`score:${name}`)}
                        className="hover:text-foreground transition-colors mx-0.5 cursor-pointer"
                        title={`Sort by ${name}`}
                      >
                        {name}
                        <SortIndicator field={`score:${name}`} sort={sort} />
                      </button>
                    ))})
                  </span>
                )}
              </th>
              <th className="px-4 py-2 font-medium text-right">
                <button
                  onClick={() => toggleSort("duration")}
                  className="hover:text-foreground transition-colors cursor-pointer"
                >
                  Duration
                  <SortIndicator field="duration" sort={sort} />
                </button>
              </th>
              <th className="px-4 py-2 font-medium text-right">
                <button
                  onClick={() => toggleSort("cost")}
                  className="hover:text-foreground transition-colors cursor-pointer"
                >
                  Cost
                  <SortIndicator field="cost" sort={sort} />
                </button>
              </th>
              <th className="px-4 py-2 font-medium text-right">
                <button
                  onClick={() => toggleSort("tokens")}
                  className="hover:text-foreground transition-colors cursor-pointer"
                >
                  Tokens
                  <SortIndicator field="tokens" sort={sort} />
                </button>
              </th>
              {onEvaluateRow && <th className="px-4 py-2 font-medium text-right" />}
            </tr>
          </thead>
          <tbody>
            {pageResults.map((r) => (
              <tr key={r.id} className="border-b border-border/30 hover:bg-muted/30 transition-colors">
                <td className="px-4 py-2 align-top whitespace-pre-wrap break-words max-w-[200px]">
                  {r.input}
                </td>
                <td className="px-4 py-2 align-top whitespace-pre-wrap break-words max-w-[200px]">
                  {r.output}
                </td>
                <td className="px-4 py-2 align-top">
                  <ScoreBadges scores={r.scores} />
                </td>
                <td className="px-4 py-2 align-top text-right text-muted-foreground">
                  {formatDuration(r.duration_ms)}
                </td>
                <td className="px-4 py-2 align-top text-right text-muted-foreground">
                  {formatCost(r.cost_cents)}
                </td>
                <td className="px-4 py-2 align-top text-right text-muted-foreground">
                  {r.tokens_used.toLocaleString()}
                </td>
                {onEvaluateRow && (
                  <td className="px-4 py-2 align-top text-right">
                    <button
                      onClick={() => onEvaluateRow(r.input, r.output)}
                      className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-[10px] font-medium text-primary hover:bg-primary/10 transition-colors"
                      title="Evaluate this input/output"
                    >
                      <Play className="w-3 h-3" />
                      Evaluate
                    </button>
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Pagination */}
      {sorted.length > PAGE_SIZE && (
        <div className="flex items-center justify-between px-4 py-3 border-t border-border/50 text-xs text-muted-foreground" data-tutorial-id="experiment-results-pagination">
          <span>
            Showing {start + 1}-{Math.min(start + PAGE_SIZE, sorted.length)} of{" "}
            {sorted.length} results
          </span>
          <div className="flex items-center gap-2">
            <button
              onClick={() => setPage((p) => Math.max(0, p - 1))}
              disabled={safePage === 0}
              className="px-3 py-1.5 rounded-md border border-border hover:bg-muted transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              Previous
            </button>
            <span>
              Page {safePage + 1} of {totalPages}
            </span>
            <button
              onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
              disabled={safePage >= totalPages - 1}
              className="px-3 py-1.5 rounded-md border border-border hover:bg-muted transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              Next
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
