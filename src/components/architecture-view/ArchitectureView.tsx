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
  X,
  FileCode,
  Cpu,
  ArrowRight,
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
import { DeveloperArchitecturePanel } from "./DeveloperArchitecturePanel";
import { ErrorBoundary } from "@/components/ui/ErrorBoundary";

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
  const [viewMode, setViewMode] = useState<"reflection" | "sdk" | "developer">("reflection");

  useEffect(() => {
    if (sdkArch.specs.length > 0 && workflows.length === 0) {
      setViewMode("developer");
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
          <button
            onClick={() => setViewMode("developer")}
            className={`px-2.5 py-1 text-xs rounded transition-colors ${
              viewMode === "developer"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            Developer
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
      ) : viewMode === "sdk" ? (
        <ErrorBoundary
          componentName="SdkArchitecturePanel"
          fallback={
            <DeveloperPanelFallback panelName="SDK Architecture" onRetry={sdkArch.refresh} />
          }
        >
          <SdkArchitecturePanel
            specs={sdkArch.specs}
            loading={sdkArch.loading}
            error={sdkArch.error}
            onRefresh={sdkArch.refresh}
          />
        </ErrorBoundary>
      ) : (
        <ErrorBoundary
          componentName="DeveloperArchitecturePanel"
          fallback={
            <DeveloperPanelFallback panelName="Developer Architecture" onRetry={sdkArch.refresh} />
          }
        >
          <DeveloperArchitecturePanel
            specs={sdkArch.specs}
            loading={sdkArch.loading}
            error={sdkArch.error}
            onRefresh={sdkArch.refresh}
          />
        </ErrorBoundary>
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

function DeveloperPanelFallback({
  panelName,
  onRetry,
}: {
  panelName: string;
  onRetry: () => void;
}) {
  return (
    <div className="flex-1 flex flex-col items-center justify-center text-center space-y-3 p-6">
      <AlertTriangle className="w-10 h-10 text-red-400/50" />
      <div>
        <h3 className="text-sm font-medium text-red-400">{panelName} failed to render</h3>
        <p className="text-xs text-muted-foreground/60 mt-1 max-w-sm">
          An error occurred while rendering this panel. This may be caused by malformed data in the
          architecture spec.
        </p>
      </div>
      <button
        onClick={onRetry}
        className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 text-foreground rounded-md transition-colors"
      >
        <RefreshCw className="w-3.5 h-3.5" />
        Retry
      </button>
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
  const [selectedNode, setSelectedNode] = useState<
    import("@/types/architecture").SdkArchitectureNode | null
  >(null);
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
              <option key={`${s.projectName}-${i}`} value={i}>
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
                  onClick={() => setSelectedNode(selectedNode?.id === node.id ? null : node)}
                  onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); setSelectedNode(selectedNode?.id === node.id ? null : node); }}}
                  role="button"
                  tabIndex={0}
                  className={`flex items-center gap-2 px-2 py-1.5 rounded text-sm cursor-pointer transition-colors ${
                    selectedNode?.id === node.id
                      ? "bg-blue-500/15 border border-blue-500/30"
                      : "bg-muted/50 hover:bg-muted"
                  }`}
                >
                  <span
                    className={`w-2 h-2 rounded-full shrink-0 ${
                      node.status === "completed"
                        ? "bg-green-400"
                        : node.status === "in-progress"
                          ? "bg-blue-400"
                          : node.status === "blocked"
                            ? "bg-red-400"
                            : "bg-gray-400"
                    }`}
                  />
                  <span className="flex-1 truncate">{node.label}</span>
                  {node.priority && (
                    <span
                      className={`text-[10px] px-1.5 py-0.5 rounded shrink-0 ${
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
                    <div
                      key={node.id}
                      onClick={() => setSelectedNode(selectedNode?.id === node.id ? null : node)}
                      onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); setSelectedNode(selectedNode?.id === node.id ? null : node); }}}
                      role="button"
                      tabIndex={0}
                      className={`px-2 py-1.5 rounded text-sm cursor-pointer transition-colors ${
                        selectedNode?.id === node.id
                          ? "bg-blue-500/15 border border-blue-500/30"
                          : "bg-muted/50 hover:bg-muted"
                      }`}
                    >
                      {node.label}
                      {node.category && (
                        <span className="text-[10px] text-muted-foreground ml-1.5">
                          {node.category}
                        </span>
                      )}
                    </div>
                  ))}
              </div>
            </>
          )}
        </div>

        {/* Detail panel */}
        {selectedNode && (
          <div className="w-72 flex-shrink-0 border border-border/50 rounded-lg overflow-auto bg-card">
            <div className="flex items-center justify-between px-3 py-2 border-b border-border/50">
              <div className="flex items-center gap-1.5 min-w-0">
                <span
                  className={`text-[10px] px-1.5 py-0.5 rounded font-medium shrink-0 ${
                    selectedNode.type === "feature"
                      ? "bg-blue-500/20 text-blue-400"
                      : "bg-purple-500/20 text-purple-400"
                  }`}
                >
                  {selectedNode.type}
                </span>
                <span className="text-sm font-medium truncate">{selectedNode.label}</span>
              </div>
              <button
                onClick={() => setSelectedNode(null)}
                className="p-0.5 rounded hover:bg-muted transition-colors shrink-0"
              >
                <X className="w-3.5 h-3.5 text-muted-foreground" />
              </button>
            </div>
            <div className="p-3 space-y-3">
              {selectedNode.description && (
                <p className="text-xs text-muted-foreground leading-relaxed">
                  {selectedNode.description}
                </p>
              )}

              {selectedNode.status && (
                <div className="flex items-center gap-2 text-xs">
                  <span className="text-muted-foreground/60">Status</span>
                  <span
                    className={`px-1.5 py-0.5 rounded ${
                      selectedNode.status === "active"
                        ? "bg-green-500/15 text-green-400"
                        : selectedNode.status === "deprecated"
                          ? "bg-red-500/15 text-red-400"
                          : "bg-gray-500/15 text-gray-400"
                    }`}
                  >
                    {selectedNode.status}
                  </span>
                  {selectedNode.priority && (
                    <>
                      <span className="text-muted-foreground/60">Priority</span>
                      <span className="text-foreground">{selectedNode.priority}</span>
                    </>
                  )}
                </div>
              )}

              {selectedNode.entryPoints && selectedNode.entryPoints.length > 0 && (
                <div>
                  <div className="flex items-center gap-1 text-[10px] text-muted-foreground/60 uppercase tracking-wider mb-1">
                    <FileCode className="w-3 h-3" />
                    Entry Points
                  </div>
                  <div className="space-y-0.5">
                    {selectedNode.entryPoints.map((ep, i) => (
                      <div key={`${ep}-${i}`} className="text-xs font-mono text-blue-400 truncate" title={ep}>
                        {ep}
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {selectedNode.techUsed && selectedNode.techUsed.length > 0 && (
                <div>
                  <div className="flex items-center gap-1 text-[10px] text-muted-foreground/60 uppercase tracking-wider mb-1">
                    <Cpu className="w-3 h-3" />
                    Tech Used
                  </div>
                  <div className="flex flex-wrap gap-1">
                    {selectedNode.techUsed.map((t, i) => (
                      <span
                        key={`${t}-${i}`}
                        className="text-[10px] px-1.5 py-0.5 rounded bg-muted text-muted-foreground"
                      >
                        {t}
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {selectedNode.usedBy && selectedNode.usedBy.length > 0 && (
                <div>
                  <div className="flex items-center gap-1 text-[10px] text-muted-foreground/60 uppercase tracking-wider mb-1">
                    <ArrowRight className="w-3 h-3" />
                    Used By
                  </div>
                  <div className="flex flex-wrap gap-1">
                    {selectedNode.usedBy.map((f, i) => (
                      <span
                        key={`${f}-${i}`}
                        className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500/10 text-blue-400"
                      >
                        {f}
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {/* Related dependencies */}
              {project.edges.filter(
                (e) => e.source === selectedNode.id || e.target === selectedNode.id,
              ).length > 0 && (
                <div>
                  <div className="text-[10px] text-muted-foreground/60 uppercase tracking-wider mb-1">
                    Dependencies
                  </div>
                  <div className="space-y-0.5">
                    {project.edges
                      .filter((e) => e.source === selectedNode.id || e.target === selectedNode.id)
                      .map((edge, i) => {
                        const isSource = edge.source === selectedNode.id;
                        const other = isSource
                          ? edge.target.replace("feature:", "")
                          : edge.source.replace("feature:", "");
                        return (
                          <div key={`${edge.source}-${edge.target}-${i}`} className="text-xs text-muted-foreground">
                            {isSource ? (
                              <>
                                <span className="text-muted-foreground/60">{edge.type}</span>{" "}
                                <span className="text-foreground">{other}</span>
                              </>
                            ) : (
                              <>
                                <span className="text-foreground">{other}</span>{" "}
                                <span className="text-muted-foreground/60">{edge.type} this</span>
                              </>
                            )}
                            {edge.label && (
                              <span className="text-muted-foreground/40 ml-1">— {edge.label}</span>
                            )}
                          </div>
                        );
                      })}
                  </div>
                </div>
              )}
            </div>
          </div>
        )}

        {/* Tech stack & directories */}
        <div className="w-64 flex-shrink-0 border border-border/50 rounded-lg overflow-auto p-3">
          <h3 className="text-xs font-medium text-muted-foreground mb-2 uppercase tracking-wider">
            Tech Stack
          </h3>
          <div className="space-y-1.5">
            {project.techStack.map((tech, i) => (
              <div key={`${tech.name}-${i}`} className="px-2 py-1.5 bg-muted/50 rounded">
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
                  <div key={`${dir.path}-${i}`} className="px-2 py-1 text-xs">
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
                  <div key={`${edge.source}-${edge.target}-${i}`} className="px-2 py-1 text-xs text-muted-foreground">
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
