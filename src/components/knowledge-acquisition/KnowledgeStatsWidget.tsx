/**
 * KnowledgeStatsWidget — compact card showing knowledge acquisition metrics.
 *
 * Displays: total searches, cache hit rate, results ingested, provider breakdown,
 * and prompt injection detections. Auto-refreshes every 30 seconds.
 */

import {
  Brain,
  Database,
  Search,
  Shield,
  TrendingUp,
  Zap,
  AlertTriangle,
  Loader2,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useKnowledgeStats } from "@/hooks/useKnowledgeAcquisition";
import type { ProviderStats } from "@/services/knowledge-acquisition-service";

interface KnowledgeStatsWidgetProps {
  className?: string;
  compact?: boolean;
}

export function KnowledgeStatsWidget({
  className,
  compact = false,
}: KnowledgeStatsWidgetProps) {
  const { data: stats, isLoading, error } = useKnowledgeStats();

  if (isLoading) {
    return (
      <div
        className={cn(
          "bg-card rounded-lg border border-border p-3",
          className,
        )}
      >
        <div className="flex items-center gap-2 text-muted-foreground">
          <Loader2 className="w-4 h-4 animate-spin" />
          <span className="text-xs">Loading knowledge stats...</span>
        </div>
      </div>
    );
  }

  if (error || !stats) {
    return (
      <div
        className={cn(
          "bg-card rounded-lg border border-border p-3",
          className,
        )}
      >
        <div className="flex items-center gap-2 text-muted-foreground">
          <Brain className="w-4 h-4" />
          <span className="text-xs">Knowledge stats unavailable</span>
        </div>
      </div>
    );
  }

  const cacheHitPct = Math.round(stats.cache_hit_rate * 100);
  const activeProviders = Object.entries(stats.providers).filter(
    ([, p]) => (p as ProviderStats).queries > 0,
  );

  if (compact) {
    return (
      <div
        className={cn(
          "bg-card rounded-lg border border-border p-2",
          className,
        )}
      >
        <div className="flex items-center gap-3 text-xs">
          <span className="flex items-center gap-1 text-muted-foreground">
            <Search className="w-3 h-3" />
            {stats.total_searches}
          </span>
          <span className="flex items-center gap-1 text-green-400">
            <Zap className="w-3 h-3" />
            {cacheHitPct}% cache
          </span>
          <span className="flex items-center gap-1 text-blue-400">
            <Database className="w-3 h-3" />
            {stats.results_ingested}
          </span>
          {stats.injection_detections > 0 && (
            <span className="flex items-center gap-1 text-yellow-400">
              <Shield className="w-3 h-3" />
              {stats.injection_detections}
            </span>
          )}
        </div>
      </div>
    );
  }

  return (
    <div
      className={cn("bg-card rounded-lg border border-border", className)}
    >
      {/* Header */}
      <div className="px-3 py-2 border-b border-border/50 bg-muted/30">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Brain className="h-4 w-4 text-orange-400" />
            <span className="text-xs font-medium">Knowledge Acquisition</span>
          </div>
          <span className="text-[10px] text-muted-foreground">
            {formatUptime(stats.uptime_secs)}
          </span>
        </div>
      </div>

      {/* Stats Grid */}
      <div className="p-3 grid grid-cols-2 gap-3">
        <StatCell
          icon={<Search className="w-3.5 h-3.5" />}
          label="Total Searches"
          value={stats.total_searches}
          color="text-foreground"
        />
        <StatCell
          icon={<Zap className="w-3.5 h-3.5" />}
          label="Cache Hit Rate"
          value={`${cacheHitPct}%`}
          color={cacheHitPct > 50 ? "text-green-400" : "text-yellow-400"}
        />
        <StatCell
          icon={<Database className="w-3.5 h-3.5" />}
          label="Results Stored"
          value={stats.results_ingested}
          subtitle={
            stats.dedup_skipped > 0
              ? `${stats.dedup_skipped} deduped`
              : undefined
          }
          color="text-blue-400"
        />
        <StatCell
          icon={<TrendingUp className="w-3.5 h-3.5" />}
          label="Cache Hits"
          value={stats.cache_hits}
          subtitle={`of ${stats.total_searches} searches`}
          color="text-green-400"
        />
      </div>

      {/* Active Providers */}
      {activeProviders.length > 0 && (
        <div className="px-3 pb-3">
          <div className="text-[10px] uppercase tracking-wider text-muted-foreground mb-1.5">
            Active Providers
          </div>
          <div className="flex flex-wrap gap-1">
            {activeProviders.map(([name, p]) => {
              const provider = p as ProviderStats;
              return (
                <span
                  key={name}
                  className={cn(
                    "inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px]",
                    provider.failures > 0
                      ? "bg-red-500/10 text-red-400"
                      : "bg-muted text-muted-foreground",
                  )}
                >
                  {formatProviderName(name)}
                  <span className="font-mono">
                    {provider.successes}/{provider.queries}
                  </span>
                </span>
              );
            })}
          </div>
        </div>
      )}

      {/* Injection Warning */}
      {stats.injection_detections > 0 && (
        <div className="px-3 pb-3">
          <div className="flex items-center gap-1.5 text-yellow-400 text-[10px]">
            <AlertTriangle className="w-3 h-3" />
            {stats.injection_detections} prompt injection
            {stats.injection_detections !== 1 ? "s" : ""} detected
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Sub-components
// ============================================================================

function StatCell({
  icon,
  label,
  value,
  subtitle,
  color,
}: {
  icon: React.ReactNode;
  label: string;
  value: number | string;
  subtitle?: string;
  color: string;
}) {
  return (
    <div className="flex items-start gap-2">
      <div className={cn("mt-0.5", color)}>{icon}</div>
      <div>
        <div className={cn("text-sm font-semibold tabular-nums", color)}>
          {typeof value === "number" ? value.toLocaleString() : value}
        </div>
        <div className="text-[10px] text-muted-foreground leading-tight">
          {label}
        </div>
        {subtitle && (
          <div className="text-[10px] text-muted-foreground/60 leading-tight">
            {subtitle}
          </div>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Helpers
// ============================================================================

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${h}h ${m}m`;
}

function formatProviderName(name: string): string {
  const names: Record<string, string> = {
    local_knowledge: "Local",
    ui_bridge: "UI Bridge",
    duckduckgo: "DDG",
    tavily: "Tavily",
    perplexity: "Perplexity",
    sploitus: "Sploitus",
    osv: "OSV",
  };
  return names[name] || name;
}
