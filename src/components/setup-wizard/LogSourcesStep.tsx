import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FileText, Loader2, CheckSquare, Square, AlertTriangle, FolderOpen } from "lucide-react";

interface LogSource {
  id: string;
  name: string;
  description: string;
  category: string;
  type: string;
  path: string;
  pattern: string | null;
  format: string;
  parser: string;
  enabled: boolean;
  keywords: string[];
  error_patterns: string[];
  warning_patterns: string[];
  tail_lines: number;
  color: string | null;
  timestamp_pattern: string | null;
  timezone: string;
  ignore_patterns: string[];
  poll_interval_ms: number;
}

interface SuggestResult {
  framework: {
    framework: string;
    key: string;
    category: string;
  };
  suggested_sources: LogSource[];
  needs_logging_setup: boolean;
}

interface Project {
  path: string;
  name: string;
  type: string;
  manifest: string;
}

interface WorkspaceSourcesResult {
  sources: LogSource[];
  dev_logs_dir: string | null;
}

interface LogSourcesStepProps {
  workspacePath: string;
  selectedProjects: Project[];
  selectedSources: LogSource[];
  onSourcesChange: (sources: LogSource[]) => void;
  onNext: () => void;
  onBack: () => void;
}

const CATEGORY_COLORS: Record<string, string> = {
  frontend: "badge-info",
  backend: "badge-success",
  api: "badge-purple",
  mobile: "badge-warning",
  database: "badge-danger",
  build: "badge-muted",
  testing: "badge-info",
  runner: "badge-success",
  general: "badge-muted",
};

export function LogSourcesStep({
  workspacePath,
  selectedProjects,
  selectedSources,
  onSourcesChange,
  onNext,
  onBack,
}: LogSourcesStepProps) {
  const [loading, setLoading] = useState(false);
  const [allSuggestions, setAllSuggestions] = useState<
    { project: Project; result: SuggestResult }[]
  >([]);
  const [workspaceSources, setWorkspaceSources] = useState<LogSource[]>([]);
  const [error, setError] = useState<string | null>(null);

  // Fetch workspace-level dev-log sources AND per-project suggestions
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);

    const fetchAll = async () => {
      // 1. Workspace-level dev-logs (always scan if we have a workspace path)
      let wsSources: LogSource[] = [];
      if (workspacePath) {
        try {
          const wsResult = await invoke<WorkspaceSourcesResult>(
            "suggest_workspace_sources_for_setup",
            { workspacePath },
          );
          wsSources = wsResult.sources || [];
        } catch (err) {
          console.error("Failed to scan workspace dev-logs:", err);
        }
      }

      // 2. Per-project suggestions
      const projectResults = await Promise.all(
        selectedProjects.map(async (project) => {
          try {
            const result = await invoke<SuggestResult>("suggest_log_sources_for_setup", {
              projectPath: project.path,
            });
            return { project, result };
          } catch (err) {
            console.error(`Failed to get suggestions for ${project.name}:`, err);
            return null;
          }
        }),
      );

      if (cancelled) return;

      const valid = projectResults.filter(
        (r): r is { project: Project; result: SuggestResult } => r !== null,
      );

      setWorkspaceSources(wsSources);
      setAllSuggestions(valid);

      // Auto-select: workspace sources + project sources
      const projectSources = valid.flatMap((r) => r.result.suggested_sources);
      onSourcesChange([...wsSources, ...projectSources]);
      setLoading(false);
    };

    fetchAll();

    return () => {
      cancelled = true;
    };
  }, [workspacePath, selectedProjects]);

  const toggleSource = useCallback(
    (source: LogSource) => {
      const isSelected = selectedSources.some((s) => s.id === source.id);
      if (isSelected) {
        onSourcesChange(selectedSources.filter((s) => s.id !== source.id));
      } else {
        onSourcesChange([...selectedSources, source]);
      }
    },
    [selectedSources, onSourcesChange],
  );

  const truncatePath = (p: string) => {
    if (p.length <= 60) return p;
    return "..." + p.slice(-57);
  };

  if (selectedProjects.length === 0) {
    return (
      <div className="tutorial-fade-in flex flex-col items-center py-8 px-4">
        <div className="empty-state py-8">
          <FileText className="w-10 h-10 text-muted-foreground mb-2 mx-auto" />
          <p className="text-muted-foreground mb-4">
            No projects selected. You can skip this step or go back to select projects.
          </p>
        </div>
        <div className="flex justify-between w-full max-w-xl pt-4">
          <button className="btn-secondary" onClick={onBack}>
            Back
          </button>
          <button className="btn-primary" onClick={onNext}>
            Skip
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="tutorial-fade-in flex flex-col gap-6 py-4 px-4">
      <div className="text-center">
        <h2 className="text-h2 mb-2">Configure Log Sources</h2>
        <p className="text-body text-muted-foreground">Select which log sources to monitor</p>
      </div>

      {loading ? (
        <div className="flex flex-col items-center py-12 gap-3">
          <Loader2 className="w-8 h-8 text-primary animate-spin" />
          <p className="text-muted-foreground">Analyzing projects...</p>
        </div>
      ) : error ? (
        <div className="panel border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300 max-w-xl mx-auto w-full">
          {error}
        </div>
      ) : (
        <div className="max-w-xl mx-auto w-full flex flex-col gap-6">
          {/* Workspace-level dev-logs */}
          {workspaceSources.length > 0 && (
            <div>
              <div className="flex items-center gap-2 mb-2">
                <FileText className="w-4 h-4 text-emerald-400" />
                <span className="text-label font-medium">Dev Logs</span>
                <span className="badge badge-success text-xs">.dev-logs</span>
              </div>
              <div className="flex flex-col gap-1.5 ml-2">
                {workspaceSources.map((source) => {
                  const isSelected = selectedSources.some((s) => s.id === source.id);
                  return (
                    <button
                      key={source.id}
                      className={`panel-interactive flex items-center gap-3 p-2.5 text-left transition-colors ${
                        isSelected ? "border-primary/50" : ""
                      }`}
                      onClick={() => toggleSource(source)}
                      data-ui-id={`setup-source-${source.id}`}
                    >
                      {isSelected ? (
                        <CheckSquare className="w-4 h-4 text-primary shrink-0" />
                      ) : (
                        <Square className="w-4 h-4 text-zinc-500 shrink-0" />
                      )}
                      <div className="flex-1 min-w-0">
                        <div className="text-sm font-medium">{source.name}</div>
                        <div className="text-xs text-muted-foreground truncate">
                          {truncatePath(source.path)}
                        </div>
                      </div>
                      <div className="flex gap-1.5 shrink-0">
                        <span
                          className={`badge text-xs ${CATEGORY_COLORS[source.category] || "badge-muted"}`}
                        >
                          {source.category}
                        </span>
                        <span className="badge badge-muted text-xs">{source.format}</span>
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
          )}

          {/* Per-project log sources */}
          {allSuggestions.map(({ project, result }) => (
            <div key={project.path}>
              <div className="flex items-center gap-2 mb-2">
                <FolderOpen className="w-4 h-4 text-muted-foreground" />
                <span className="text-label font-medium">{project.name}</span>
                {result.framework.framework !== "unknown" && (
                  <span className="badge badge-info text-xs">{result.framework.framework}</span>
                )}
              </div>

              {result.needs_logging_setup && (
                <div className="panel border-amber-500/30 bg-amber-500/10 p-3 text-sm text-amber-300 mb-3 flex items-start gap-2">
                  <AlertTriangle className="w-4 h-4 shrink-0 mt-0.5" />
                  <span>
                    This framework doesn't log to files by default. You may need to configure
                    file-based logging separately.
                  </span>
                </div>
              )}

              {result.suggested_sources.length === 0 ? (
                <p className="text-sm text-muted-foreground ml-6">
                  No log sources found for this project
                </p>
              ) : (
                <div className="flex flex-col gap-1.5 ml-2">
                  {result.suggested_sources.map((source) => {
                    const isSelected = selectedSources.some((s) => s.id === source.id);
                    return (
                      <button
                        key={source.id}
                        className={`panel-interactive flex items-center gap-3 p-2.5 text-left transition-colors ${
                          isSelected ? "border-primary/50" : ""
                        }`}
                        onClick={() => toggleSource(source)}
                        data-ui-id={`setup-source-${source.id}`}
                      >
                        {isSelected ? (
                          <CheckSquare className="w-4 h-4 text-primary shrink-0" />
                        ) : (
                          <Square className="w-4 h-4 text-zinc-500 shrink-0" />
                        )}
                        <div className="flex-1 min-w-0">
                          <div className="text-sm font-medium">{source.name}</div>
                          <div className="text-xs text-muted-foreground truncate">
                            {truncatePath(source.path)}
                          </div>
                        </div>
                        <div className="flex gap-1.5 shrink-0">
                          <span
                            className={`badge text-xs ${CATEGORY_COLORS[source.category] || "badge-muted"}`}
                          >
                            {source.category}
                          </span>
                          <span className="badge badge-muted text-xs">{source.format}</span>
                        </div>
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Navigation */}
      <div className="flex justify-between max-w-xl mx-auto w-full pt-4">
        <button className="btn-secondary" onClick={onBack}>
          Back
        </button>
        <button
          className="btn-primary"
          onClick={onNext}
          disabled={loading}
          data-ui-id="setup-wizard-sources-next"
        >
          {selectedSources.length > 0
            ? `Continue with ${selectedSources.length} source${selectedSources.length !== 1 ? "s" : ""}`
            : "Skip"}
        </button>
      </div>
    </div>
  );
}
