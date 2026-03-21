import React from "react";
import type { SpanFilter, WorkflowPhase } from "./types";

interface TraceToolbarProps {
  filter: SpanFilter;
  onFilterChange: (filter: SpanFilter) => void;
  spanCount: number;
  filteredCount: number;
}

const PHASE_OPTIONS: { value: WorkflowPhase | "all"; label: string }[] = [
  { value: "all", label: "All Phases" },
  { value: "setup", label: "Setup" },
  { value: "verification", label: "Verification" },
  { value: "agentic", label: "Agentic" },
  { value: "completion", label: "Completion" },
  { value: "ai", label: "AI" },
  { value: "python", label: "Python" },
];

export const TraceToolbar: React.FC<TraceToolbarProps> = ({
  filter,
  onFilterChange,
  spanCount,
  filteredCount,
}) => {
  return (
    <div className="flex items-center gap-3 px-3 py-2 bg-zinc-900 border-b border-zinc-700 text-xs">
      {/* Search */}
      <input
        type="text"
        placeholder="Search spans..."
        value={filter.nameSearch}
        onChange={(e) => onFilterChange({ ...filter, nameSearch: e.target.value })}
        className="w-40 px-2 py-1 bg-zinc-800 border border-zinc-700 rounded text-zinc-300 placeholder:text-zinc-600 focus:outline-hidden focus:border-zinc-500"
      />

      {/* Min duration */}
      <input
        type="number"
        placeholder="Min ms"
        value={filter.minDurationMs ?? ""}
        onChange={(e) =>
          onFilterChange({
            ...filter,
            minDurationMs: e.target.value ? Number(e.target.value) : null,
          })
        }
        className="w-20 px-2 py-1 bg-zinc-800 border border-zinc-700 rounded text-zinc-300 placeholder:text-zinc-600 focus:outline-hidden focus:border-zinc-500"
      />

      {/* Phase */}
      <select
        value={filter.phase}
        onChange={(e) =>
          onFilterChange({ ...filter, phase: e.target.value as WorkflowPhase | "all" })
        }
        className="px-2 py-1 bg-zinc-800 border border-zinc-700 rounded text-zinc-300 focus:outline-hidden focus:border-zinc-500"
      >
        {PHASE_OPTIONS.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>

      {/* Status */}
      <select
        value={filter.status}
        onChange={(e) =>
          onFilterChange({
            ...filter,
            status: e.target.value as "all" | "success" | "error",
          })
        }
        className="px-2 py-1 bg-zinc-800 border border-zinc-700 rounded text-zinc-300 focus:outline-hidden focus:border-zinc-500"
      >
        <option value="all">All Status</option>
        <option value="success">Success</option>
        <option value="error">Errors</option>
      </select>

      {/* Count */}
      <span className="text-zinc-500 ml-auto">
        {filteredCount === spanCount
          ? `${spanCount} spans`
          : `${filteredCount} / ${spanCount} spans`}
      </span>
    </div>
  );
};
