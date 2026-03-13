import { useState, useEffect } from "react";
import {
  GitBranch,
  RefreshCw,
  Hammer,
  Loader2,
  Boxes,
  Link2,
  Activity,
  AlertTriangle,
} from "lucide-react";
import { tracedFetch } from "@/lib/traced-fetch";
import { getApiBase } from "@/lib/runner-api";
import { useArchitectureGraph, useComponentDetails } from "@/hooks/useArchitecture";
import { ArchitectureGraphPanel } from "./ArchitectureGraphPanel";
import { ComponentDetailPanel } from "./ComponentDetailPanel";
import { TrendsPanel } from "./TrendsPanel";

export function ArchitectureView() {
  const [workflowName, setWorkflowName] = useState<string>("");
  const [workflows, setWorkflows] = useState<string[]>([]);
  const [selectedComponent, setSelectedComponent] = useState<string | null>(null);

  // Load workflow names
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/task-runs?limit=100`);
        const json = await resp.json();
        if (!cancelled && json.success && json.data) {
          const names = new Set<string>();
          for (const run of json.data) {
            if (run.workflow_name) names.add(run.workflow_name);
          }
          const sorted = Array.from(names).sort();
          setWorkflows(sorted);
          if (sorted.length > 0 && !workflowName) {
            setWorkflowName(sorted[0]);
          }
        }
      } catch {
        // Runner may not be available
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const { graph, loading, error, refresh, rebuild } = useArchitectureGraph(workflowName);
  const { details, loading: detailsLoading } = useComponentDetails(workflowName, selectedComponent);

  const stats = graph?.stats;
  const hasData = graph && graph.nodes.length > 0;

  return (
    <div className="h-full flex flex-col overflow-hidden p-4 space-y-3">
      {/* Header */}
      <div className="flex items-center justify-between flex-shrink-0">
        <div className="flex items-center gap-2">
          <GitBranch className="w-5 h-5 text-blue-400" />
          <h2 className="text-lg font-semibold">Architecture Model</h2>
        </div>
        <div className="flex items-center gap-2">
          <select
            value={workflowName}
            onChange={(e) => {
              setWorkflowName(e.target.value);
              setSelectedComponent(null);
            }}
            className="px-3 py-1.5 text-sm bg-muted border border-border rounded-md"
          >
            {workflows.length === 0 && <option value="">No workflows found</option>}
            {workflows.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
          <button
            onClick={() => rebuild()}
            disabled={loading || !workflowName}
            className="flex items-center gap-1 px-2.5 py-1.5 text-sm bg-blue-600/20 hover:bg-blue-600/30 text-blue-400 border border-blue-600/30 rounded-md transition-colors disabled:opacity-50"
            title="Rebuild architecture model from reflection data"
          >
            <Hammer className="w-3.5 h-3.5" />
            Rebuild
          </button>
          <button
            onClick={refresh}
            disabled={loading}
            className="p-1.5 rounded hover:bg-muted transition-colors"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
        </div>
      </div>

      {/* Error */}
      {error && (
        <div className="px-3 py-2 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-400">
          {error}
        </div>
      )}

      {/* Stats bar */}
      {stats && hasData && (
        <div className="flex gap-3 flex-shrink-0">
          <StatCard
            icon={<Boxes className="w-4 h-4 text-blue-400" />}
            label="Components"
            value={stats.total_components}
          />
          <StatCard
            icon={<Link2 className="w-4 h-4 text-purple-400" />}
            label="Relationships"
            value={stats.total_relationships}
          />
          <StatCard
            icon={<Activity className="w-4 h-4 text-green-400" />}
            label="Avg Health"
            value={`${Math.round(stats.avg_health_score * 100)}%`}
          />
          <StatCard
            icon={<AlertTriangle className="w-4 h-4 text-orange-400" />}
            label="Most Volatile"
            value={
              stats.most_volatile.length > 0
                ? (stats.most_volatile[0].split("/").pop() ?? "-")
                : "-"
            }
          />
        </div>
      )}

      {/* Trends panel */}
      {hasData && <TrendsPanel workflowName={workflowName} selectedComponent={selectedComponent} />}

      {/* Main content */}
      <div className="flex-1 min-h-0 flex gap-3">
        {loading && !graph ? (
          <div className="flex-1 flex items-center justify-center">
            <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
          </div>
        ) : hasData ? (
          <>
            {/* Graph panel */}
            <div className="flex-1 min-w-0 border border-border/50 rounded-lg overflow-hidden">
              <ArchitectureGraphPanel
                nodes={graph!.nodes}
                edges={graph!.edges}
                onNodeClick={setSelectedComponent}
              />
            </div>

            {/* Detail panel */}
            {selectedComponent && details && (
              <div className="w-72 flex-shrink-0 border border-border/50 rounded-lg overflow-hidden bg-card">
                <ComponentDetailPanel details={details} loading={detailsLoading} />
              </div>
            )}
          </>
        ) : (
          <EmptyState workflowName={workflowName} onRebuild={() => rebuild()} />
        )}
      </div>
    </div>
  );
}

function StatCard({
  icon,
  label,
  value,
}: {
  icon: React.ReactNode;
  label: string;
  value: string | number;
}) {
  return (
    <div className="flex items-center gap-2 px-3 py-2 bg-card border border-border/50 rounded-lg">
      {icon}
      <div>
        <div className="text-[10px] text-muted-foreground">{label}</div>
        <div className="text-sm font-medium">{value}</div>
      </div>
    </div>
  );
}

function EmptyState({ workflowName, onRebuild }: { workflowName: string; onRebuild: () => void }) {
  return (
    <div className="flex-1 flex flex-col items-center justify-center text-center space-y-3">
      <GitBranch className="w-12 h-12 text-muted-foreground/30" />
      <div>
        <h3 className="text-sm font-medium text-muted-foreground">No Architecture Data</h3>
        <p className="text-xs text-muted-foreground/60 mt-1 max-w-sm">
          {workflowName
            ? "Run a reflection workflow first, or click Rebuild to generate the architecture model from existing reflection data."
            : "Select a workflow to view its architecture model."}
        </p>
      </div>
      {workflowName && (
        <button
          onClick={onRebuild}
          className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-blue-600/20 hover:bg-blue-600/30 text-blue-400 border border-blue-600/30 rounded-md transition-colors"
        >
          <Hammer className="w-3.5 h-3.5" />
          Rebuild Architecture Model
        </button>
      )}
    </div>
  );
}
