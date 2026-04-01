/**
 * RunCostBreakdown
 *
 * Displays per-run LLM cost breakdown: totals strip at top,
 * per-model breakdown table sorted by cost descending.
 */

import { useMemo } from "react";
import { DollarSign, Loader2, XCircle } from "lucide-react";
import { useRunCostSummary, type ModelCostBreakdown } from "../../hooks/useRunCostSummary";

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

interface RunCostBreakdownProps {
  runId: string;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Format USD cost to a readable string. */
function formatCost(usd: number): string {
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}

/** Format token count with commas. */
function formatTokens(count: number): string {
  return count.toLocaleString();
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function TotalsStrip({
  totalCost,
  tokensInput,
  tokensOutput,
  actionCount,
}: {
  totalCost: number;
  tokensInput: number;
  tokensOutput: number;
  actionCount: number;
}) {
  return (
    <div className="flex flex-wrap items-center gap-4 p-3 rounded-lg bg-muted/40 border border-border text-sm">
      <div className="flex items-center gap-2">
        <DollarSign className="w-4 h-4 text-muted-foreground" />
        <span className="text-muted-foreground">Total Cost</span>
        <span className="font-semibold tabular-nums">{formatCost(totalCost)}</span>
      </div>

      <div className="h-4 w-px bg-border" />

      <div className="flex items-center gap-2">
        <span className="text-muted-foreground">Tokens In</span>
        <span className="font-medium tabular-nums">{formatTokens(tokensInput)}</span>
      </div>

      <div className="h-4 w-px bg-border" />

      <div className="flex items-center gap-2">
        <span className="text-muted-foreground">Tokens Out</span>
        <span className="font-medium tabular-nums">{formatTokens(tokensOutput)}</span>
      </div>

      <div className="h-4 w-px bg-border" />

      <div className="flex items-center gap-2">
        <span className="text-muted-foreground">LLM Actions</span>
        <span className="font-medium tabular-nums">{actionCount}</span>
      </div>
    </div>
  );
}

function ModelRow({ model }: { model: ModelCostBreakdown }) {
  return (
    <tr className="border-b border-border last:border-b-0 hover:bg-muted/30 transition-colors">
      <td className="py-2 px-3 text-sm font-medium">{model.model}</td>
      <td className="py-2 px-3 text-sm text-muted-foreground">{model.provider}</td>
      <td className="py-2 px-3 text-sm tabular-nums text-right">
        {formatTokens(model.tokens_input)}
      </td>
      <td className="py-2 px-3 text-sm tabular-nums text-right">
        {formatTokens(model.tokens_output)}
      </td>
      <td className="py-2 px-3 text-sm tabular-nums text-right font-medium">
        {formatCost(model.cost_usd)}
      </td>
      <td className="py-2 px-3 text-sm tabular-nums text-right">{model.action_count}</td>
    </tr>
  );
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export function RunCostBreakdown({ runId }: RunCostBreakdownProps) {
  const { data, loading, error } = useRunCostSummary(runId);

  const sortedModels = useMemo(() => {
    if (!data?.per_model) return [];
    return [...data.per_model].sort((a, b) => b.cost_usd - a.cost_usd);
  }, [data]);

  // Loading
  if (loading) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
        <Loader2 className="w-8 h-8 animate-spin mb-3 opacity-50" />
        <p className="text-sm">Loading cost breakdown...</p>
      </div>
    );
  }

  // Error
  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
        <XCircle className="w-8 h-8 mb-3 text-red-500 opacity-50" />
        <p className="text-sm font-medium">Failed to load cost breakdown</p>
        <p className="text-xs mt-1">{error.message}</p>
      </div>
    );
  }

  // Empty state
  if (!data || data.llm_action_count === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
        <DollarSign className="w-8 h-8 mb-3 opacity-40" />
        <p className="text-sm font-medium">No LLM Costs</p>
        <p className="text-xs mt-1">This run did not include any LLM actions.</p>
      </div>
    );
  }

  return (
    <div className="space-y-3" data-tutorial-id="run-cost-breakdown">
      {/* Totals */}
      <TotalsStrip
        totalCost={data.total_cost_usd}
        tokensInput={data.total_tokens_input}
        tokensOutput={data.total_tokens_output}
        actionCount={data.llm_action_count}
      />

      {/* Per-model breakdown table */}
      {sortedModels.length > 0 && (
        <div className="rounded-lg border border-border bg-card overflow-hidden">
          <table className="w-full">
            <thead>
              <tr className="border-b border-border bg-muted/30">
                <th className="py-2 px-3 text-xs font-medium text-muted-foreground text-left">
                  Model
                </th>
                <th className="py-2 px-3 text-xs font-medium text-muted-foreground text-left">
                  Provider
                </th>
                <th className="py-2 px-3 text-xs font-medium text-muted-foreground text-right">
                  Tokens In
                </th>
                <th className="py-2 px-3 text-xs font-medium text-muted-foreground text-right">
                  Tokens Out
                </th>
                <th className="py-2 px-3 text-xs font-medium text-muted-foreground text-right">
                  Cost
                </th>
                <th className="py-2 px-3 text-xs font-medium text-muted-foreground text-right">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {sortedModels.map((model) => (
                <ModelRow key={`${model.provider}-${model.model}`} model={model} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
