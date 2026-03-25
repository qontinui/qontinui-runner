/**
 * CrossRunPatternsPanel.tsx
 *
 * Displays cross-run patterns (recurring findings, fix oscillations)
 * from the knowledge graph, with occurrence counts and trend data.
 */

import { AlertTriangle, TrendingUp, RefreshCw } from "lucide-react";
import { useCrossRunPatterns, type CrossRunPattern } from "../../hooks/useGraphAnalytics";

interface CrossRunPatternsPanelProps {
  workflowName?: string;
}

function PatternBadge({ type }: { type: string }) {
  const isOscillation = type === "fix_oscillation";
  return (
    <span
      className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium ${
        isOscillation
          ? "bg-orange-500/10 text-orange-500"
          : "bg-yellow-500/10 text-yellow-500"
      }`}
    >
      {isOscillation ? <RefreshCw className="w-3 h-3" /> : <TrendingUp className="w-3 h-3" />}
      {isOscillation ? "Fix Oscillation" : "Recurring Finding"}
    </span>
  );
}

function PatternRow({ pattern }: { pattern: CrossRunPattern }) {
  const lastSeen = new Date(pattern.last_seen_at).toLocaleDateString();
  return (
    <div className="flex items-center justify-between py-2 border-b border-border last:border-0">
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <PatternBadge type={pattern.pattern_type} />
          <span className="text-sm font-mono truncate">{pattern.signature_hash.slice(0, 12)}</span>
        </div>
        {pattern.affected_components && (
          <p className="text-xs text-muted-foreground mt-0.5 truncate">
            {pattern.affected_components}
          </p>
        )}
      </div>
      <div className="flex items-center gap-4 text-sm">
        <span className="font-semibold tabular-nums">{pattern.occurrence_count}x</span>
        <span className="text-xs text-muted-foreground">{lastSeen}</span>
        <span
          className={`text-xs px-1.5 py-0.5 rounded ${
            pattern.status === "active"
              ? "bg-red-500/10 text-red-500"
              : pattern.status === "resolved"
                ? "bg-green-500/10 text-green-500"
                : "bg-muted text-muted-foreground"
          }`}
        >
          {pattern.status}
        </span>
      </div>
    </div>
  );
}

export function CrossRunPatternsPanel({ workflowName }: CrossRunPatternsPanelProps) {
  const { data: patterns, isLoading, error } = useCrossRunPatterns(workflowName);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <AlertTriangle className="w-4 h-4" />
        Cross-Run Patterns
      </h3>

      {isLoading && (
        <div className="text-center text-muted-foreground py-8">Loading patterns...</div>
      )}

      {error && (
        <div className="text-center text-destructive py-8">
          Failed to load patterns: {String(error)}
        </div>
      )}

      {patterns && patterns.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          No cross-run patterns detected
        </div>
      )}

      {patterns && patterns.length > 0 && (
        <div className="divide-y divide-border">
          {patterns.map((p) => (
            <PatternRow key={p.id} pattern={p} />
          ))}
        </div>
      )}
    </div>
  );
}
