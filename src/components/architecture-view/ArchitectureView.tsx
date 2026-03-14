import { useState, useEffect, useCallback } from "react";
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
import {
  useArchitectureGraph,
  useComponentDetails,
  useSdkArchitecture,
} from "@/hooks/useArchitecture";
import { ArchitectureGraphPanel } from "./ArchitectureGraphPanel";
import { ComponentDetailPanel } from "./ComponentDetailPanel";
import { TrendsPanel } from "./TrendsPanel";
import { SdkAppConnector } from "@/components/ui-bridge/SdkAppConnector";

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
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only fetch workflow list on mount
  }, []);

  const { graph, loading, error, refresh, rebuild } = useArchitectureGraph(workflowName);
  const { details, loading: detailsLoading } = useComponentDetails(workflowName, selectedComponent);
  const sdkArch = useSdkArchitecture();
  const [viewMode, setViewMode] = useState<"reflection" | "sdk">("reflection");

  useEffect(() => {
    if (sdkArch.specs.length > 0 && workflows.length === 0) {
      setViewMode("sdk");
    }
  }, [sdkArch.specs.length, workflows.length]);

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
        <div className="flex items-center bg-muted rounded-md p-0.5">
          <button
            onClick={() => setViewMode("reflection")}
            className={`px-2.5 py-1 text-xs rounded transition-colors ${
              viewMode === "reflection"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            Reflection
          </button>
          <button
            onClick={() => setViewMode("sdk")}
            className={`px-2.5 py-1 text-xs rounded transition-colors ${
              viewMode === "sdk"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            SDK Projects{sdkArch.specs.length > 0 ? ` (${sdkArch.specs.length})` : ""}
          </button>
        </div>
        {viewMode === "reflection" ? (
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
        ) : (
          <button
            onClick={sdkArch.refresh}
            disabled={sdkArch.loading}
            className="p-1.5 rounded hover:bg-muted transition-colors"
            title="Refresh SDK specs"
          >
            <RefreshCw className={`w-4 h-4 ${sdkArch.loading ? "animate-spin" : ""}`} />
          </button>
        )}
      </div>

      {/* Error */}
      {viewMode === "reflection" && error && (
        <div className="px-3 py-2 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-400">
          {error}
        </div>
      )}

      {viewMode === "reflection" ? (
        <>
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
          {hasData && (
            <TrendsPanel workflowName={workflowName} selectedComponent={selectedComponent} />
          )}

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
        </>
      ) : (
        <SdkArchitecturePanel
          specs={sdkArch.specs}
          loading={sdkArch.loading}
          error={sdkArch.error}
          onRefresh={sdkArch.refresh}
        />
      )}
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

function SdkArchitecturePanel({
  specs,
  loading,
  error,
  onRefresh,
}: {
  specs: import("@/types/architecture").SdkArchitectureGraph[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
}) {
  const [selectedProject, setSelectedProject] = useState(0);
  const [discovering, setDiscovering] = useState(false);

  // When a user connects to an app, trigger discover-and-cache for its specs
  const handleAppConnected = useCallback(
    async (app: import("@/hooks/useAppDiscovery").DiscoveredApp) => {
      setDiscovering(true);
      try {
        await tracedFetch(`${getApiBase()}/ui-bridge/sdk/discover-and-cache`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ url: app.url, app_name: app.appName }),
        });
        // Refresh the architecture specs list after caching
        await onRefresh();
      } catch {
        // Silently fail — the specs list will just remain empty
      } finally {
        setDiscovering(false);
      }
    },
    [onRefresh],
  );

  if (loading && specs.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (specs.length === 0) {
    return (
      <div className="flex-1 flex flex-col gap-4">
        {/* App connector */}
        <SdkAppConnector onConnected={handleAppConnected} compact />

        {/* Status */}
        {discovering && (
          <div className="flex items-center gap-2 px-3 py-2 bg-blue-500/10 border border-blue-500/20 rounded text-sm text-blue-400">
            <Loader2 className="w-4 h-4 animate-spin" />
            Discovering specs from connected app...
          </div>
        )}

        {error && !discovering && (
          <div className="px-3 py-2 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-400">
            {error}
          </div>
        )}

        {/* Empty state (below connector) */}
        {!discovering && (
          <div className="flex-1 flex flex-col items-center justify-center text-center space-y-3">
            <Boxes className="w-12 h-12 text-muted-foreground/30" />
            <div>
              <h3 className="text-sm font-medium text-muted-foreground">No SDK Projects Found</h3>
              <p className="text-xs text-muted-foreground/60 mt-1 max-w-sm">
                Scan for SDK apps above, connect to one, and its architecture specs will appear here
                automatically.
              </p>
            </div>
          </div>
        )}
      </div>
    );
  }

  const clampedIndex = Math.min(selectedProject, specs.length - 1);
  const project = specs[clampedIndex];

  return (
    <div className="flex-1 min-h-0 flex flex-col gap-3">
      {/* Project selector */}
      {specs.length > 1 && (
        <div className="flex-shrink-0">
          <select
            value={clampedIndex}
            onChange={(e) => setSelectedProject(Number(e.target.value))}
            className="px-3 py-1.5 text-sm bg-muted border border-border rounded-md"
          >
            {specs.map((s, i) => (
              <option key={i} value={i}>
                {s.projectName} ({s.appUrl})
              </option>
            ))}
          </select>
        </div>
      )}

      {/* Project info */}
      <div className="flex gap-3 flex-shrink-0 flex-wrap">
        <StatCard
          icon={<Boxes className="w-4 h-4 text-blue-400" />}
          label="Features"
          value={project.nodes.filter((n) => n.type === "feature").length}
        />
        <StatCard
          icon={<Link2 className="w-4 h-4 text-purple-400" />}
          label="Dependencies"
          value={project.edges.length}
        />
        <StatCard
          icon={<Activity className="w-4 h-4 text-green-400" />}
          label="Tech Stack"
          value={project.techStack.length}
        />
      </div>

      {/* Content panels */}
      <div className="flex-1 min-h-0 flex gap-3 overflow-hidden">
        {/* Features */}
        <div className="flex-1 border border-border/50 rounded-lg overflow-auto p-3">
          <h3 className="text-xs font-medium text-muted-foreground mb-2 uppercase tracking-wider">
            Features
          </h3>
          <div className="space-y-1.5">
            {project.nodes
              .filter((n) => n.type === "feature")
              .map((node) => (
                <div
                  key={node.id}
                  className="flex items-center gap-2 px-2 py-1.5 bg-muted/50 rounded text-sm"
                >
                  <span
                    className={`w-2 h-2 rounded-full ${
                      node.status === "completed"
                        ? "bg-green-400"
                        : node.status === "in-progress"
                          ? "bg-blue-400"
                          : node.status === "blocked"
                            ? "bg-red-400"
                            : "bg-gray-400"
                    }`}
                  />
                  <span className="flex-1">{node.label}</span>
                  {node.priority && (
                    <span
                      className={`text-[10px] px-1.5 py-0.5 rounded ${
                        node.priority === "critical"
                          ? "bg-red-500/20 text-red-400"
                          : node.priority === "high"
                            ? "bg-orange-500/20 text-orange-400"
                            : node.priority === "medium"
                              ? "bg-yellow-500/20 text-yellow-400"
                              : "bg-gray-500/20 text-gray-400"
                      }`}
                    >
                      {node.priority}
                    </span>
                  )}
                </div>
              ))}
            {project.nodes.filter((n) => n.type === "feature").length === 0 && (
              <p className="text-xs text-muted-foreground/60">No features defined</p>
            )}
          </div>

          {/* Patterns */}
          {project.nodes.filter((n) => n.type === "pattern").length > 0 && (
            <>
              <h3 className="text-xs font-medium text-muted-foreground mb-2 mt-4 uppercase tracking-wider">
                Patterns
              </h3>
              <div className="space-y-1.5">
                {project.nodes
                  .filter((n) => n.type === "pattern")
                  .map((node) => (
                    <div key={node.id} className="px-2 py-1.5 bg-muted/50 rounded text-sm">
                      {node.label}
                    </div>
                  ))}
              </div>
            </>
          )}
        </div>

        {/* Tech stack & directories */}
        <div className="w-64 flex-shrink-0 border border-border/50 rounded-lg overflow-auto p-3">
          <h3 className="text-xs font-medium text-muted-foreground mb-2 uppercase tracking-wider">
            Tech Stack
          </h3>
          <div className="space-y-1.5">
            {project.techStack.map((tech, i) => (
              <div key={i} className="px-2 py-1.5 bg-muted/50 rounded">
                <div className="text-sm font-medium">
                  {tech.name}
                  {tech.version ? ` v${tech.version}` : ""}
                </div>
                <div className="text-[10px] text-muted-foreground">{tech.purpose}</div>
              </div>
            ))}
          </div>

          {project.directories.length > 0 && (
            <>
              <h3 className="text-xs font-medium text-muted-foreground mb-2 mt-4 uppercase tracking-wider">
                Directories
              </h3>
              <div className="space-y-1">
                {project.directories.map((dir, i) => (
                  <div key={i} className="px-2 py-1 text-xs">
                    <span className="font-mono text-blue-400">{dir.path}</span>
                    <span className="text-muted-foreground ml-1">&mdash; {dir.purpose}</span>
                  </div>
                ))}
              </div>
            </>
          )}

          {/* Dependency edges */}
          {project.edges.length > 0 && (
            <>
              <h3 className="text-xs font-medium text-muted-foreground mb-2 mt-4 uppercase tracking-wider">
                Dependencies
              </h3>
              <div className="space-y-1">
                {project.edges.map((edge, i) => (
                  <div key={i} className="px-2 py-1 text-xs text-muted-foreground">
                    <span className="text-foreground">{edge.source.replace("feature:", "")}</span>
                    {" \u2192 "}
                    <span className="text-foreground">{edge.target.replace("feature:", "")}</span>
                    <span className="ml-1 text-[10px]">({edge.type})</span>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
