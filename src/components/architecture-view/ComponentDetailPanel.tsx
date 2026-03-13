import { useState } from "react";
import { ChevronDown, ChevronRight, ArrowRight, ArrowLeft } from "lucide-react";
import type { ComponentDetails, FixSummary, ImpactEntry } from "@/types/architecture";

function healthColorClass(score: number): string {
  if (score > 0.7) return "text-green-400";
  if (score > 0.4) return "text-yellow-400";
  return "text-red-400";
}

function healthBgClass(score: number): string {
  if (score > 0.7) return "bg-green-500/20";
  if (score > 0.4) return "bg-yellow-500/20";
  return "bg-red-500/20";
}

function effectivenessColor(e: string | null): string {
  if (e === "effective") return "text-green-400";
  if (e === "ineffective" || e === "caused_regression") return "text-red-400";
  return "text-gray-400";
}

function formatDate(dateStr: string | null): string {
  if (!dateStr) return "N/A";
  const d = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  if (diffMins < 60) return `${diffMins}m ago`;
  const diffHours = Math.floor(diffMins / 60);
  if (diffHours < 24) return `${diffHours}h ago`;
  return `${Math.floor(diffHours / 24)}d ago`;
}

interface Props {
  details: ComponentDetails;
  loading?: boolean;
}

export function ComponentDetailPanel({ details, loading }: Props) {
  const [fixesExpanded, setFixesExpanded] = useState(true);
  const { node } = details;

  if (loading) {
    return <div className="p-4 text-sm text-muted-foreground">Loading details...</div>;
  }

  const effectivenessRate =
    node.fix_count > 0 ? Math.round((node.effective_fix_count / node.fix_count) * 100) : 100;

  return (
    <div className="flex flex-col h-full overflow-y-auto p-3 space-y-3 text-sm">
      {/* Header */}
      <div>
        <h3 className="font-semibold text-sm truncate">{node.component_path}</h3>
        <span className="text-[10px] text-muted-foreground">{node.component_type}</span>
      </div>

      {/* Health gauge */}
      <div className="flex items-center gap-2">
        <div
          className={`w-10 h-10 rounded-full flex items-center justify-center text-xs font-bold ${healthBgClass(node.health_score)}`}
        >
          <span className={healthColorClass(node.health_score)}>
            {Math.round(node.health_score * 100)}
          </span>
        </div>
        <div className="text-xs text-muted-foreground">Health Score</div>
      </div>

      {/* Metrics grid */}
      <div className="grid grid-cols-2 gap-2">
        <MetricCard label="Fixes" value={node.fix_count} />
        <MetricCard label="Errors" value={node.error_count} />
        <MetricCard label="Effectiveness" value={`${effectivenessRate}%`} />
        <MetricCard label="Velocity" value={node.change_velocity.toFixed(1)} />
        <MetricCard label="Causal Links" value={node.causal_involvement_count} />
        <MetricCard label="Last Activity" value={formatDate(node.last_activity_at)} />
      </div>

      {/* Recent fixes */}
      <div>
        <button
          onClick={() => setFixesExpanded((p) => !p)}
          className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground"
        >
          {fixesExpanded ? (
            <ChevronDown className="w-3 h-3" />
          ) : (
            <ChevronRight className="w-3 h-3" />
          )}
          Recent Fixes ({details.recent_fixes?.length ?? 0})
        </button>
        {fixesExpanded && (details.recent_fixes?.length ?? 0) > 0 && (
          <div className="mt-1 space-y-1">
            {details.recent_fixes?.map((fix: FixSummary) => (
              <div
                key={fix.id}
                className="px-2 py-1.5 bg-muted/30 rounded text-xs border border-border/30"
              >
                <div className="flex items-center justify-between">
                  <span className="truncate">{fix.fix_description}</span>
                  <span className={`text-[10px] ${effectivenessColor(fix.effectiveness)}`}>
                    {fix.effectiveness ?? "pending"}
                  </span>
                </div>
                <div className="text-[10px] text-muted-foreground mt-0.5">
                  {fix.fix_type} {fix.applied_at ? `- ${formatDate(fix.applied_at)}` : ""}
                </div>
              </div>
            ))}
          </div>
        )}
        {fixesExpanded && (details.recent_fixes?.length ?? 0) === 0 && (
          <div className="text-xs text-muted-foreground mt-1">No recent fixes</div>
        )}
      </div>

      {/* Impacts */}
      {details.impacts.length > 0 && (
        <div>
          <div className="flex items-center gap-1 text-xs font-medium text-muted-foreground mb-1">
            <ArrowRight className="w-3 h-3" />
            Impacts ({details.impacts.length})
          </div>
          <div className="space-y-0.5">
            {details.impacts.map((entry: ImpactEntry, i: number) => (
              <ImpactRow key={i} entry={entry} />
            ))}
          </div>
        </div>
      )}

      {/* Impacted by */}
      {details.impacted_by.length > 0 && (
        <div>
          <div className="flex items-center gap-1 text-xs font-medium text-muted-foreground mb-1">
            <ArrowLeft className="w-3 h-3" />
            Impacted By ({details.impacted_by.length})
          </div>
          <div className="space-y-0.5">
            {details.impacted_by.map((entry: ImpactEntry, i: number) => (
              <ImpactRow key={i} entry={entry} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function MetricCard({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="px-2 py-1.5 bg-muted/20 rounded border border-border/20">
      <div className="text-[10px] text-muted-foreground">{label}</div>
      <div className="text-xs font-medium">{value}</div>
    </div>
  );
}

function ImpactRow({ entry }: { entry: ImpactEntry }) {
  return (
    <div className="flex items-center justify-between px-2 py-1 bg-muted/20 rounded text-xs">
      <span className="truncate flex-1">{entry.component_path}</span>
      <span className="ml-1 text-[10px] px-1 py-0.5 rounded bg-muted/40 text-muted-foreground">
        {entry.strength}
      </span>
    </div>
  );
}
