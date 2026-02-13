/**
 * ComparisonResults.tsx
 *
 * Displays AI-generated comparison results with a summary,
 * severity stats bar, filterable spec card list, and workflow generation.
 */

import { useState, useMemo } from "react";
import { AlertCircle, AlertTriangle, Info, Filter, Loader2, Bot, Play } from "lucide-react";
import { cn } from "@/lib/utils";
import { ComparisonSpecCard, type ComparisonSpec } from "./ComparisonSpecCard";

export interface ComparisonResult {
  specs: ComparisonSpec[];
  summary: string;
  totalDifferences: number;
  criticalCount: number;
  majorCount: number;
  minorCount: number;
  infoCount: number;
}

interface ComparisonResultsProps {
  result: ComparisonResult | null;
  isComparing: boolean;
  error: string | null;
  onGenerateWorkflow?: (selectedSpecIds: string[]) => void;
  isGeneratingWorkflow?: boolean;
}

type SeverityFilter = "all" | "critical" | "major" | "minor" | "info";
type TypeFilter =
  | "all"
  | "missing_element"
  | "extra_element"
  | "different_text"
  | "different_structure"
  | "layout_mismatch";

export function ComparisonResults({
  result,
  isComparing,
  error,
  onGenerateWorkflow,
  isGeneratingWorkflow,
}: ComparisonResultsProps) {
  const [severityFilter, setSeverityFilter] = useState<SeverityFilter>("all");
  const [typeFilter, setTypeFilter] = useState<TypeFilter>("all");
  const [selectedSpecIds, setSelectedSpecIds] = useState<Set<string>>(new Set());

  const filteredSpecs = useMemo(() => {
    if (!result) return [];
    return result.specs.filter((spec) => {
      if (severityFilter !== "all" && spec.severity !== severityFilter) return false;
      if (typeFilter !== "all" && spec.type !== typeFilter) return false;
      return true;
    });
  }, [result, severityFilter, typeFilter]);

  const toggleSelect = (id: string) => {
    setSelectedSpecIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const selectAll = () => {
    setSelectedSpecIds(new Set(filteredSpecs.map((s) => s.id)));
  };

  const selectNone = () => {
    setSelectedSpecIds(new Set());
  };

  // Loading state
  if (isComparing) {
    return (
      <div className="flex flex-col items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-brand-primary mb-3" />
        <p className="text-sm text-text-muted">Comparing snapshots with AI...</p>
        <p className="text-xs text-text-muted/70 mt-1">This may take a moment</p>
      </div>
    );
  }

  // Error state
  if (error) {
    return (
      <div className="rounded-lg bg-error/10 border border-error/30 p-4">
        <div className="flex items-start gap-2">
          <AlertCircle className="h-4 w-4 text-error shrink-0 mt-0.5" />
          <div>
            <p className="text-sm font-medium text-error">Comparison Failed</p>
            <p className="text-xs text-text-muted mt-1">{error}</p>
          </div>
        </div>
      </div>
    );
  }

  // Empty state
  if (!result) {
    return (
      <div className="flex flex-col items-center justify-center py-12">
        <Bot className="h-10 w-10 text-text-muted/30 mb-3" />
        <p className="text-sm text-text-muted">No comparison results yet</p>
        <p className="text-xs text-text-muted/70 mt-1">
          Take snapshots and click "Compare with AI"
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Summary */}
      <div className="rounded-lg border border-border bg-surface-raised/50 p-4">
        <h3 className="text-sm font-semibold text-text-primary mb-2">AI Summary</h3>
        <p className="text-xs text-text-muted leading-relaxed">{result.summary}</p>
      </div>

      {/* Stats bar */}
      <div className="flex gap-3">
        <StatBadge
          label="Critical"
          count={result.criticalCount}
          color="text-error"
          bg="bg-error/10"
          icon={AlertCircle}
        />
        <StatBadge
          label="Major"
          count={result.majorCount}
          color="text-amber-400"
          bg="bg-amber-500/10"
          icon={AlertTriangle}
        />
        <StatBadge
          label="Minor"
          count={result.minorCount}
          color="text-blue-400"
          bg="bg-blue-500/10"
          icon={Info}
        />
        <StatBadge
          label="Info"
          count={result.infoCount}
          color="text-text-muted"
          bg="bg-surface-hover"
          icon={Info}
        />
      </div>

      {/* Filters */}
      <div className="flex items-center gap-3 flex-wrap">
        <div className="flex items-center gap-1.5">
          <Filter className="h-3.5 w-3.5 text-text-muted" />
          <span className="text-[10px] uppercase tracking-wider text-text-muted">Severity:</span>
        </div>
        {(["all", "critical", "major", "minor", "info"] as SeverityFilter[]).map((s) => (
          <button
            key={s}
            onClick={() => setSeverityFilter(s)}
            className={cn(
              "rounded-md px-2 py-1 text-[10px] font-medium transition-colors capitalize",
              severityFilter === s
                ? "bg-brand-primary/10 text-brand-primary"
                : "text-text-muted hover:bg-surface-hover hover:text-text-primary",
            )}
          >
            {s}
          </button>
        ))}
        <span className="text-text-muted/30">|</span>
        <span className="text-[10px] uppercase tracking-wider text-text-muted">Type:</span>
        <select
          value={typeFilter}
          onChange={(e) => setTypeFilter(e.target.value as TypeFilter)}
          className="rounded-md border border-border bg-surface-raised px-2 py-1 text-[10px] text-text-primary"
        >
          <option value="all">All Types</option>
          <option value="missing_element">Missing Element</option>
          <option value="extra_element">Extra Element</option>
          <option value="different_text">Different Text</option>
          <option value="different_structure">Different Structure</option>
          <option value="layout_mismatch">Layout Mismatch</option>
        </select>
      </div>

      {/* Select all / none + count */}
      <div className="flex items-center justify-between">
        <span className="text-xs text-text-muted">
          {filteredSpecs.length} difference{filteredSpecs.length !== 1 ? "s" : ""}
          {selectedSpecIds.size > 0 && ` (${selectedSpecIds.size} selected)`}
        </span>
        <div className="flex items-center gap-2">
          <button
            onClick={selectAll}
            className="text-[10px] text-text-muted hover:text-text-primary"
          >
            Select All
          </button>
          <span className="text-[10px] text-text-muted">/</span>
          <button
            onClick={selectNone}
            className="text-[10px] text-text-muted hover:text-text-primary"
          >
            None
          </button>
        </div>
      </div>

      {/* Spec cards */}
      <div className="space-y-2">
        {filteredSpecs.map((spec) => (
          <ComparisonSpecCard
            key={spec.id}
            spec={spec}
            selected={selectedSpecIds.has(spec.id)}
            onToggleSelect={toggleSelect}
          />
        ))}
      </div>

      {/* Generate Workflow button */}
      {onGenerateWorkflow && selectedSpecIds.size > 0 && (
        <button
          onClick={() => onGenerateWorkflow(Array.from(selectedSpecIds))}
          disabled={isGeneratingWorkflow}
          className="w-full flex items-center justify-center gap-2 rounded-lg bg-gradient-to-r from-brand-success to-brand-success/80 py-3 text-sm font-medium text-white hover:from-brand-success/90 hover:to-brand-success/70 disabled:opacity-50 transition-all"
        >
          {isGeneratingWorkflow ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" />
              Generating Workflow...
            </>
          ) : (
            <>
              <Play className="h-4 w-4" />
              Generate Workflow from {selectedSpecIds.size} Spec
              {selectedSpecIds.size !== 1 ? "s" : ""}
            </>
          )}
        </button>
      )}
    </div>
  );
}

function StatBadge({
  label,
  count,
  color,
  bg,
  icon: Icon,
}: {
  label: string;
  count: number;
  color: string;
  bg: string;
  icon: React.ComponentType<{ className?: string }>;
}) {
  return (
    <div className={cn("flex items-center gap-1.5 rounded-lg px-3 py-2", bg)}>
      <Icon className={cn("h-3.5 w-3.5", color)} />
      <span className={cn("text-sm font-bold", color)}>{count}</span>
      <span className="text-xs text-text-muted">{label}</span>
    </div>
  );
}
