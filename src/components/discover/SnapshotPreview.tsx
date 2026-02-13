/**
 * SnapshotPreview.tsx
 *
 * Side-by-side display of Reference and Target UI snapshots.
 * Shows element counts, component counts, structure summary, and diff stats.
 */

import { Layers, ArrowRight, Plus, Minus, Equal } from "lucide-react";
import { cn } from "@/lib/utils";

interface SnapshotSummary {
  appName: string;
  url: string;
  elementCount: number;
  componentCount: number;
  interactiveCount: number;
  /** Top-level element types and counts */
  elementTypes: Record<string, number>;
}

interface DiffStats {
  onlyInReference: number;
  onlyInTarget: number;
  inBoth: number;
}

interface SnapshotPreviewProps {
  referenceSnapshot: unknown | null;
  targetSnapshot: unknown | null;
  referenceSummary: SnapshotSummary | null;
  targetSummary: SnapshotSummary | null;
  diffStats: DiffStats | null;
  isLoading: boolean;
}

function SnapshotPanel({
  label,
  color,
  summary,
}: {
  label: string;
  color: string;
  summary: SnapshotSummary | null;
}) {
  if (!summary) {
    return (
      <div className="flex-1 rounded-lg border border-dashed border-border p-6 text-center">
        <Layers className="mx-auto h-6 w-6 text-text-muted/30 mb-2" />
        <p className="text-xs text-text-muted">No snapshot taken</p>
      </div>
    );
  }

  const topTypes = Object.entries(summary.elementTypes)
    .sort(([, a], [, b]) => b - a)
    .slice(0, 6);

  return (
    <div className="flex-1 rounded-lg border border-border bg-surface-raised/50 overflow-hidden">
      <div className={cn("border-b border-border px-3 py-2", color)}>
        <span className="text-xs font-semibold uppercase tracking-wider">{label}</span>
        <p className="text-xs text-text-muted mt-0.5 truncate">{summary.appName}</p>
      </div>
      <div className="p-3 space-y-3">
        {/* Counts row */}
        <div className="grid grid-cols-3 gap-2">
          <div className="rounded-md bg-surface-canvas p-2 text-center">
            <div className="text-lg font-bold text-text-primary">{summary.elementCount}</div>
            <div className="text-[10px] text-text-muted">Elements</div>
          </div>
          <div className="rounded-md bg-surface-canvas p-2 text-center">
            <div className="text-lg font-bold text-text-primary">{summary.componentCount}</div>
            <div className="text-[10px] text-text-muted">Components</div>
          </div>
          <div className="rounded-md bg-surface-canvas p-2 text-center">
            <div className="text-lg font-bold text-text-primary">{summary.interactiveCount}</div>
            <div className="text-[10px] text-text-muted">Interactive</div>
          </div>
        </div>

        {/* Element types */}
        {topTypes.length > 0 && (
          <div>
            <div className="mb-1.5 text-[10px] font-medium uppercase tracking-wider text-text-muted">
              Element Types
            </div>
            <div className="flex flex-wrap gap-1">
              {topTypes.map(([type, count]) => (
                <span
                  key={type}
                  className="inline-flex items-center gap-1 rounded bg-surface-canvas px-1.5 py-0.5 text-[10px] text-text-muted"
                >
                  <span className="text-text-primary font-medium">{type}</span>
                  <span className="text-text-muted/60">{count}</span>
                </span>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export function SnapshotPreview({
  referenceSnapshot: _referenceSnapshot,
  targetSnapshot: _targetSnapshot,
  referenceSummary,
  targetSummary,
  diffStats,
  isLoading,
}: SnapshotPreviewProps) {
  return (
    <div className="space-y-4">
      {/* Side-by-side snapshots */}
      <div className="flex gap-3 items-stretch">
        <SnapshotPanel label="Reference" color="text-blue-400" summary={referenceSummary} />
        <div className="flex items-center">
          <ArrowRight className="h-5 w-5 text-text-muted/40" />
        </div>
        <SnapshotPanel label="Target" color="text-amber-400" summary={targetSummary} />
      </div>

      {/* Diff stats bar */}
      {diffStats && (
        <div className="rounded-lg border border-border bg-surface-raised/50 p-3">
          <div className="mb-2 text-[10px] font-medium uppercase tracking-wider text-text-muted">
            Quick Diff
          </div>
          <div className="flex gap-4">
            <div className="flex items-center gap-1.5">
              <Minus className="h-3.5 w-3.5 text-error" />
              <span className="text-sm font-semibold text-error">{diffStats.onlyInReference}</span>
              <span className="text-xs text-text-muted">only in reference</span>
            </div>
            <div className="flex items-center gap-1.5">
              <Plus className="h-3.5 w-3.5 text-brand-success" />
              <span className="text-sm font-semibold text-brand-success">
                {diffStats.onlyInTarget}
              </span>
              <span className="text-xs text-text-muted">only in target</span>
            </div>
            <div className="flex items-center gap-1.5">
              <Equal className="h-3.5 w-3.5 text-text-muted" />
              <span className="text-sm font-semibold text-text-primary">{diffStats.inBoth}</span>
              <span className="text-xs text-text-muted">in both</span>
            </div>
          </div>
        </div>
      )}

      {isLoading && (
        <div className="text-center py-4">
          <div className="inline-flex items-center gap-2 text-xs text-text-muted">
            <div className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-brand-primary border-t-transparent" />
            Taking snapshots...
          </div>
        </div>
      )}
    </div>
  );
}

export type { SnapshotSummary, DiffStats };
