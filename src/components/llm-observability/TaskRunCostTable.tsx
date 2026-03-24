/**
 * TaskRunCostTable.tsx
 *
 * Table showing per-task-run LLM cost breakdown, sortable by cost.
 */

import { useState, useMemo } from "react";
import { ListChecks, ArrowUpDown } from "lucide-react";
import type { TaskRunCostRow } from "./types";

interface TaskRunCostTableProps {
  data: TaskRunCostRow[];
}

type SortField = "cost" | "calls" | "started_at";
type SortDir = "asc" | "desc";

/** Format cents as dollars */
function formatCost(cents: number): string {
  return `$${(cents / 100).toFixed(2)}`;
}

/** Format token counts compactly */
function formatTokens(count: number): string {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
  return count.toLocaleString();
}

/** Truncate task run ID for display */
function truncateId(id: string): string {
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}...`;
}

/** Format date for display */
function formatDate(dateStr: string): string {
  try {
    const d = new Date(dateStr);
    return d.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return dateStr;
  }
}

export function TaskRunCostTable({ data }: TaskRunCostTableProps) {
  const [sortField, setSortField] = useState<SortField>("cost");
  const [sortDir, setSortDir] = useState<SortDir>("desc");

  const sortedData = useMemo(() => {
    return [...data].sort((a, b) => {
      let cmp = 0;
      switch (sortField) {
        case "cost":
          cmp = a.total_cost_cents - b.total_cost_cents;
          break;
        case "calls":
          cmp = a.call_count - b.call_count;
          break;
        case "started_at":
          cmp = a.started_at.localeCompare(b.started_at);
          break;
      }
      return sortDir === "asc" ? cmp : -cmp;
    });
  }, [data, sortField, sortDir]);

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setSortDir("desc");
    }
  };

  if (data.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <ListChecks className="w-4 h-4" />
          Task Run Costs
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No task run cost data available</p>
          <p className="text-xs mt-1">Costs will appear here after workflows run</p>
        </div>
      </div>
    );
  }

  const SortButton = ({ field, label }: { field: SortField; label: string }) => (
    <button
      onClick={() => handleSort(field)}
      className="flex items-center gap-1 hover:text-foreground transition-colors"
    >
      {label}
      {sortField === field && (
        <ArrowUpDown className="w-3 h-3" />
      )}
    </button>
  );

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <ListChecks className="w-4 h-4" />
        Task Run Costs
        <span className="text-xs text-muted-foreground font-normal ml-2">
          ({data.length} run{data.length !== 1 ? "s" : ""})
        </span>
      </h3>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-border text-left">
              <th className="pb-2 font-medium text-muted-foreground">Task Run</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">
                <SortButton field="cost" label="Cost" />
              </th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Input</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Output</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">
                <SortButton field="calls" label="Calls" />
              </th>
              <th className="pb-2 font-medium text-muted-foreground text-right">
                <SortButton field="started_at" label="Started" />
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {sortedData.map((row) => (
              <tr key={row.task_run_id} className="hover:bg-muted/30">
                <td className="py-2">
                  <span className="font-mono text-xs" title={row.task_run_id}>
                    {truncateId(row.task_run_id)}
                  </span>
                </td>
                <td className="py-2 text-right font-medium">{formatCost(row.total_cost_cents)}</td>
                <td className="py-2 text-right text-muted-foreground">
                  {formatTokens(row.total_input_tokens)}
                </td>
                <td className="py-2 text-right text-muted-foreground">
                  {formatTokens(row.total_output_tokens)}
                </td>
                <td className="py-2 text-right text-muted-foreground">{row.call_count}</td>
                <td className="py-2 text-right text-muted-foreground">
                  {formatDate(row.started_at)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
