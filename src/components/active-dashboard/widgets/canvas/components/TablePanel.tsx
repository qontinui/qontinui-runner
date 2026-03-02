/**
 * TablePanel Component
 *
 * Renders a data table with optional column sorting and row filtering.
 */

import { useState, useMemo } from "react";
import { ArrowUpDown, Search } from "lucide-react";
import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

type CellValue = string | number | boolean | null;

export function TablePanel({ data, size }: CanvasPanelComponentProps) {
  const columns = (data.columns as string[]) ?? [];
  const rows = (data.rows as CellValue[][]) ?? [];
  const sortable = (data.sortable as boolean) ?? false;

  const [sortCol, setSortCol] = useState<number | null>(null);
  const [sortAsc, setSortAsc] = useState(true);
  const [searchTerm, setSearchTerm] = useState("");

  const sortedRows =
    sortCol !== null
      ? [...rows].sort((a, b) => {
          const aVal = a[sortCol] ?? "";
          const bVal = b[sortCol] ?? "";
          const cmp = String(aVal).localeCompare(String(bVal), undefined, { numeric: true });
          return sortAsc ? cmp : -cmp;
        })
      : rows;

  const filteredRows = useMemo(() => {
    if (!searchTerm.trim()) return sortedRows;
    const term = searchTerm.toLowerCase();
    return sortedRows.filter((row) =>
      row.some((cell) =>
        String(cell ?? "")
          .toLowerCase()
          .includes(term),
      ),
    );
  }, [sortedRows, searchTerm]);

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

  const isCompact = size === "compact";
  const showSearch = rows.length > 5;
  const isFiltering = searchTerm.trim().length > 0;

  return (
    <div className="space-y-2">
      {/* Search input */}
      {showSearch && (
        <div className="relative">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <input
            type="text"
            placeholder="Filter rows..."
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            className="w-full pl-7 pr-2 py-1.5 text-xs bg-muted/30 border border-border/50 rounded-md text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary/50"
          />
        </div>
      )}

      {/* Filtered row count */}
      {isFiltering && (
        <div className="text-[10px] text-muted-foreground">
          Showing {filteredRows.length} of {rows.length} rows
        </div>
      )}

      {/* Table */}
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
            {filteredRows.map((row, rowIdx) => (
              <tr
                key={rowIdx}
                className="border-b border-border/30 last:border-b-0 hover:bg-muted/20"
              >
                {row.map((cell, cellIdx) => (
                  <td
                    key={cellIdx}
                    className={cn(
                      "text-sm text-foreground",
                      isCompact ? "px-2 py-1" : "px-3 py-1.5",
                    )}
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
        {filteredRows.length === 0 && rows.length > 0 && (
          <p className="text-sm text-muted-foreground italic text-center py-4">No matching rows</p>
        )}
        {rows.length === 0 && (
          <p className="text-sm text-muted-foreground italic text-center py-4">No rows</p>
        )}
      </div>
    </div>
  );
}
