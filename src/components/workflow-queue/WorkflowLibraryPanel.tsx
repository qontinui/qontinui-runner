/**
 * WorkflowLibraryPanel
 *
 * Left panel showing the library of all unified workflows.
 * Includes search, category filtering, and add-to-queue functionality.
 */

import { Search, Loader2, Inbox, ChevronLeft } from "lucide-react";
import type { WorkflowWithStats } from "./types";
import { WorkflowLibraryCard } from "./WorkflowLibraryCard";

interface WorkflowLibraryPanelProps {
  workflows: WorkflowWithStats[];
  loading: boolean;
  search: string;
  onSearchChange: (value: string) => void;
  category: string;
  onCategoryChange: (value: string) => void;
  categories: string[];
  onAddToQueue: (workflow: WorkflowWithStats) => void;
  queuedIds: Set<string>;
  onCollapse: () => void;
}

export function WorkflowLibraryPanel({
  workflows,
  loading,
  search,
  onSearchChange,
  category,
  onCategoryChange,
  categories,
  onAddToQueue,
  queuedIds,
  onCollapse,
}: WorkflowLibraryPanelProps) {
  return (
    <div className="w-full border-r border-white/10 flex flex-col">
      {/* Header */}
      <div className="px-3 py-3 border-b border-white/10">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-h3 text-foreground">Workflow Library</h2>
          <button
            onClick={onCollapse}
            className="p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
            title="Collapse library"
          >
            <ChevronLeft className="w-4 h-4" />
          </button>
        </div>

        {/* Search */}
        <div className="relative mb-3">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search workflows..."
            value={search}
            onChange={(e) => onSearchChange(e.target.value)}
            className="input pl-9 w-full"
          />
        </div>

        {/* Category filter */}
        <div className="flex flex-wrap gap-2">
          {categories.map((cat) => (
            <button
              key={cat}
              onClick={() => onCategoryChange(cat)}
              className={`px-3 py-1 rounded-full text-label-sm transition-colors ${
                category === cat
                  ? "bg-cyan-500/20 text-cyan-400 border border-cyan-500/50"
                  : "bg-white/5 text-muted-foreground border border-transparent hover:bg-white/10"
              }`}
            >
              {cat === "all" ? "All" : cat.charAt(0).toUpperCase() + cat.slice(1)}
            </button>
          ))}
        </div>
      </div>

      {/* Workflow list */}
      <div className="flex-1 overflow-y-auto p-4">
        {loading ? (
          <div className="flex items-center justify-center h-32">
            <Loader2 className="w-6 h-6 text-cyan-400 animate-spin" />
          </div>
        ) : workflows.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-32 text-muted-foreground">
            <Inbox className="w-8 h-8 mb-2" />
            <p className="text-body-sm">
              {search || category !== "all"
                ? "No workflows match your filters"
                : "No workflows found"}
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {workflows.map((workflow) => (
              <WorkflowLibraryCard
                key={workflow.id}
                workflow={workflow}
                onAdd={() => onAddToQueue(workflow)}
                isQueued={queuedIds.has(workflow.id)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
