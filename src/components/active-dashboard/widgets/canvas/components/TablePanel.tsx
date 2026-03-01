/**
 * TablePanel Component
 *
 * Renders a data table with optional column sorting.
 */

import { useState } from "react";
import { ArrowUpDown } from "lucide-react";
import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

type CellValue = string | number | boolean | null;

export function TablePanel({ data, size }: CanvasPanelComponentProps) {
  const columns = (data.columns as string[]) ?? [];
  const rows = (data.rows as CellValue[][]) ?? [];
  const sortable = (data.sortable as boolean) ?? false;

  const [sortCol, setSortCol] = useState<number | null>(null);
  const [sortAsc, setSortAsc] = useState(true);

  if (columns.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No table data</p>;
  }

  const handleSort = (colIdx: number) => {
    if (!sortable) return;
    if (sortCol === colIdx) {
      setSortAsc(!sortAsc);
    } else {
      setSortCol(colIdx);
      setSortAsc(true);
    }
  };

  const sortedRows =
    sortCol !== null
      ? [...rows].sort((a, b) => {
          const aVal = a[sortCol] ?? "";
          const bVal = b[sortCol] ?? "";
          const cmp = String(aVal).localeCompare(String(bVal), undefined, { numeric: true });
          return sortAsc ? cmp : -cmp;
        })
      : rows;

  const isCompact = size === "compact";

  return (
    <div className="overflow-auto">
      <table className="w-full text-left border-collapse">
        <thead>
          <tr className="border-b border-border">
            {columns.map((col, idx) => (
              <th
                key={idx}
                className={cn(
                  "text-xs font-semibold text-muted-foreground uppercase tracking-wide",
                  isCompact ? "px-2 py-1" : "px-3 py-2",
                  sortable && "cursor-pointer hover:text-foreground select-none",
                )}
                onClick={() => handleSort(idx)}
              >
                <span className="flex items-center gap-1">
                  {col}
                  {sortable && sortCol === idx && <ArrowUpDown className="h-3 w-3" />}
                </span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {sortedRows.map((row, rowIdx) => (
            <tr
              key={rowIdx}
              className="border-b border-border/30 last:border-b-0 hover:bg-muted/20"
            >
              {row.map((cell, cellIdx) => (
                <td
                  key={cellIdx}
                  className={cn("text-sm text-foreground", isCompact ? "px-2 py-1" : "px-3 py-1.5")}
                >
                  {cell === null ? (
                    <span className="text-muted-foreground italic">null</span>
                  ) : typeof cell === "boolean" ? (
                    <span className={cell ? "text-green-500" : "text-red-500"}>
                      {cell ? "true" : "false"}
                    </span>
                  ) : (
                    String(cell)
                  )}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length === 0 && (
        <p className="text-sm text-muted-foreground italic text-center py-4">No rows</p>
      )}
    </div>
  );
}
