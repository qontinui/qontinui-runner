import { useState, useCallback } from "react";
import { Loader2, RefreshCw, Save } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import type { SessionRecap } from "@/lib/session-recap/types";
import { SessionSummaryCard } from "./SessionSummaryCard";
import { SessionTimeline } from "./SessionTimeline";
import { SessionGraphView } from "./SessionGraphView";

type ViewMode = "graph" | "timeline";

export function SessionRecapPage() {
  const [recap, setRecap] = useState<SessionRecap | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lookback, setLookback] = useState("3 hours");
  const [viewMode, setViewMode] = useState<ViewMode>("graph");
  const [saving, setSaving] = useState(false);

  const analyze = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await tracedFetch(`${getApiBase()}/session-recap/analyze`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ lookback }),
      });
      const json = await res.json();
      if (json.success) {
        setRecap(json.data);
      } else {
        setError(json.error ?? "Analysis failed");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Network error");
    } finally {
      setLoading(false);
    }
  }, [lookback]);

  const saveToObservations = useCallback(async () => {
    if (!recap) return;
    setSaving(true);
    try {
      const date = new Date().toISOString().split("T")[0];
      await tracedFetch(`${getApiBase()}/observations`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          topic_key: `session-recap-${date}`,
          observation_type: "session_recap",
          content: JSON.stringify(recap.summary),
          metadata: {
            total_files: recap.summary.total_files,
            total_repos: recap.summary.total_repos,
            lookback,
          },
        }),
      });
    } catch {
      // Silently fail — non-critical
    } finally {
      setSaving(false);
    }
  }, [recap, lookback]);

  return (
    <div className="h-full flex flex-col p-4 gap-3">
      {/* Toolbar */}
      <div className="flex items-center gap-2 shrink-0">
        <select
          value={lookback}
          onChange={(e) => setLookback(e.target.value)}
          className="h-8 px-2 text-xs rounded-md border border-border bg-card text-foreground"
        >
          <option value="1 hour">Last hour</option>
          <option value="3 hours">Last 3 hours</option>
          <option value="8 hours">Last 8 hours</option>
          <option value="1 day">Last day</option>
          <option value="3 days">Last 3 days</option>
          <option value="HEAD~5">Last 5 commits</option>
          <option value="HEAD~10">Last 10 commits</option>
          <option value="HEAD~20">Last 20 commits</option>
        </select>

        <button
          onClick={analyze}
          disabled={loading}
          className="h-8 px-3 text-xs font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 flex items-center gap-1.5"
        >
          {loading ? (
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
          ) : (
            <RefreshCw className="w-3.5 h-3.5" />
          )}
          Analyze
        </button>

        {recap && (
          <>
            {/* View toggle */}
            <div className="flex rounded-md border border-border overflow-hidden ml-auto">
              <button
                onClick={() => setViewMode("graph")}
                className={`px-2.5 py-1 text-xs ${viewMode === "graph" ? "bg-muted font-medium" : "hover:bg-muted/50"}`}
              >
                Graph
              </button>
              <button
                onClick={() => setViewMode("timeline")}
                className={`px-2.5 py-1 text-xs ${viewMode === "timeline" ? "bg-muted font-medium" : "hover:bg-muted/50"}`}
              >
                Timeline
              </button>
            </div>

            <button
              onClick={saveToObservations}
              disabled={saving}
              className="h-8 px-3 text-xs rounded-md border border-border hover:bg-muted flex items-center gap-1.5"
            >
              <Save className="w-3.5 h-3.5" />
              {saving ? "Saving..." : "Save to Memory"}
            </button>
          </>
        )}
      </div>

      {/* Error */}
      {error && (
        <div className="text-xs text-destructive bg-destructive/10 border border-destructive/20 rounded-md px-3 py-2">
          {error}
        </div>
      )}

      {/* Content */}
      {!recap && !loading && (
        <div className="flex-1 flex items-center justify-center text-sm text-muted-foreground">
          Click <strong className="mx-1">Analyze</strong> to generate a session recap
        </div>
      )}

      {loading && (
        <div className="flex-1 flex items-center justify-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin" />
          Analyzing git history across repos...
        </div>
      )}

      {recap && !loading && (
        <div className="flex-1 min-h-0 flex gap-3">
          {/* Left sidebar: summary */}
          <div className="w-72 shrink-0 overflow-y-auto border border-border/30 rounded-lg bg-card/30 p-3">
            <SessionSummaryCard
              summary={recap.summary}
              repos={recap.repos_touched}
              lookback={recap.timespan.lookback_spec}
            />
          </div>

          {/* Main area */}
          <div className="flex-1 min-w-0 border border-border/30 rounded-lg overflow-hidden bg-card/20">
            {viewMode === "graph" ? (
              <SessionGraphView recap={recap} />
            ) : (
              <SessionTimeline
                filesCreated={recap.files_created}
                filesModified={recap.files_modified}
                filesDeleted={recap.files_deleted}
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}
