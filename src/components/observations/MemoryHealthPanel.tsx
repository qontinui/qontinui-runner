/**
 * MemoryHealthPanel.tsx
 *
 * Dashboard panel showing memory system health: total observations, mental models,
 * decay queue, consolidation log, and manual consolidation trigger.
 */

import { useState, useCallback, useEffect } from "react";
import { Brain, RefreshCw, Loader2, Activity, Archive, Zap, Clock } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { getAccentColors, getStatusColors } from "@/design-system";

// ============================================================================
// Types
// ============================================================================

interface MemoryHealth {
  totalObservations: number;
  totalMentalModels: number;
  decayQueueSize: number;
  avgImportance: number | null;
  avgAccessCount: number | null;
  lastConsolidation: string | null;
}

interface ConsolidationLogEntry {
  id: number;
  startedAt: string;
  completedAt: string | null;
  observationsScanned: number;
  groupsFound: number;
  modelsCreated: number;
  observationsMerged: number;
  observationsDecayed: number;
  observationsArchived: number;
  error: string | null;
}

interface ConsolidationStats {
  observationsScanned: number;
  groupsFound: number;
  modelsCreated: number;
  observationsMerged: number;
  observationsDecayed: number;
  observationsArchived: number;
}

// ============================================================================
// Component
// ============================================================================

export function MemoryHealthPanel() {
  const [health, setHealth] = useState<MemoryHealth | null>(null);
  const [log, setLog] = useState<ConsolidationLogEntry[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [isConsolidating, setIsConsolidating] = useState(false);
  const [consolidationResult, setConsolidationResult] = useState<ConsolidationStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  const accent = getAccentColors("violet");
  const accentBlue = getAccentColors("blue");
  const statusError = getStatusColors("error");
  const statusSuccess = getStatusColors("complete");
  const statusWarning = getStatusColors("running");

  const fetchData = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const base = getApiBase();
      const [healthRes, logRes] = await Promise.all([
        tracedFetch(`${base}/memory/health`),
        tracedFetch(`${base}/memory/consolidation-log?max_results=10`),
      ]);

      if (healthRes.ok) {
        const healthJson = await healthRes.json();
        if (healthJson.success) setHealth(healthJson.data);
      }
      if (logRes.ok) {
        const logJson = await logRes.json();
        if (logJson.success) setLog(logJson.data || []);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to fetch memory health");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  const handleConsolidate = useCallback(async () => {
    setIsConsolidating(true);
    setConsolidationResult(null);
    setError(null);
    try {
      const base = getApiBase();
      const res = await tracedFetch(`${base}/memory/consolidate`, {
        method: "POST",
      });
      const json = await res.json();
      if (json.success) {
        setConsolidationResult(json.data);
        fetchData();
      } else {
        setError(json.error || "Consolidation failed");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to trigger consolidation");
    } finally {
      setIsConsolidating(false);
    }
  }, [fetchData]);

  if (isLoading) {
    return (
      <div className="flex items-center gap-2 p-4 text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        Loading memory health...
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Brain className={`h-5 w-5 ${accent.text}`} />
          <h3 className="text-sm font-semibold">Memory Health</h3>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleConsolidate}
            disabled={isConsolidating}
            className={`flex items-center gap-1 rounded px-2 py-1 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50 ${accent.text}`}
          >
            {isConsolidating ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : (
              <Zap className="h-3 w-3" />
            )}
            Consolidate
          </button>
          <button
            onClick={fetchData}
            className="rounded p-1 text-muted-foreground transition-colors hover:bg-muted"
          >
            <RefreshCw className="h-3 w-3" />
          </button>
        </div>
      </div>

      {/* Error */}
      {error && (
        <div className={`rounded-md px-3 py-2 text-xs ${statusError.bg} ${statusError.text}`}>
          {error}
        </div>
      )}

      {/* Consolidation result */}
      {consolidationResult && (
        <div className={`rounded-md px-3 py-2 text-xs ${statusSuccess.bg} ${statusSuccess.text}`}>
          Consolidation complete: {consolidationResult.modelsCreated} models created,{" "}
          {consolidationResult.observationsMerged} merged,{" "}
          {consolidationResult.observationsArchived} archived
        </div>
      )}

      {/* Stats grid */}
      {health && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <StatCard
            icon={<Activity className="h-4 w-4" />}
            label="Observations"
            value={health.totalObservations}
            colorClasses={accent}
          />
          <StatCard
            icon={<Brain className="h-4 w-4" />}
            label="Mental Models"
            value={health.totalMentalModels}
            colorClasses={accentBlue}
          />
          <StatCard
            icon={<Archive className="h-4 w-4" />}
            label="Decay Queue"
            value={health.decayQueueSize}
            colorClasses={
              health.decayQueueSize > 50 ? getAccentColors("amber") : getAccentColors("zinc")
            }
          />
          <StatCard
            icon={<Clock className="h-4 w-4" />}
            label="Last Consolidation"
            value={
              health.lastConsolidation ? formatRelativeTime(health.lastConsolidation) : "Never"
            }
            colorClasses={getAccentColors("zinc")}
          />
        </div>
      )}

      {/* Consolidation log */}
      {log.length > 0 && (
        <div>
          <h4 className="mb-2 text-xs font-medium text-muted-foreground">
            Recent Consolidation Runs
          </h4>
          <div className="space-y-1">
            {log.map((entry) => (
              <div
                key={entry.id}
                className="flex items-center justify-between rounded-md bg-muted/30 px-3 py-1.5 text-xs"
              >
                <span className="text-muted-foreground">{formatRelativeTime(entry.startedAt)}</span>
                <div className="flex items-center gap-3">
                  {entry.error ? (
                    <span className={statusError.text}>Error: {entry.error.slice(0, 40)}</span>
                  ) : (
                    <>
                      <span>{entry.modelsCreated} models</span>
                      <span className="text-muted-foreground">
                        {entry.observationsScanned} scanned
                      </span>
                      <span className="text-muted-foreground">
                        {entry.observationsArchived} archived
                      </span>
                    </>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Sub-components
// ============================================================================

function StatCard({
  icon,
  label,
  value,
  colorClasses,
}: {
  icon: React.ReactNode;
  label: string;
  value: number | string;
  colorClasses: { text: string; bg: string };
}) {
  return (
    <div className="rounded-lg border p-3">
      <div className="flex items-center gap-1.5 text-muted-foreground">
        <span className={colorClasses.text}>{icon}</span>
        <span className="text-[10px] uppercase tracking-wider">{label}</span>
      </div>
      <div className={`mt-1 text-lg font-semibold ${colorClasses.text}`}>{value}</div>
    </div>
  );
}

// ============================================================================
// Helpers
// ============================================================================

function formatRelativeTime(isoString: string): string {
  const date = new Date(isoString);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);

  if (diffMins < 1) return "just now";
  if (diffMins < 60) return `${diffMins}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;
  if (diffDays < 7) return `${diffDays}d ago`;
  return date.toLocaleDateString();
}
