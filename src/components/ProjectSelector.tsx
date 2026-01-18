/**
 * ProjectSelector Component
 *
 * Displays a list of saved RAG projects from ~/.qontinui/rag/
 * and allows selecting and loading them into the executor.
 */

import { useState, useEffect, useCallback } from "react";
import { FolderOpen, RefreshCw, Loader2, Check, AlertCircle } from "lucide-react";
import { getStatusColors } from "@/design-system";

interface RAGProject {
  project_id: string;
  project_name: string;
  version: string;
  exported_at: string;
  screenshot_count: number;
  element_count: number;
  has_embeddings: boolean;
  embedding_status?: string;
}

interface ProjectSelectorProps {
  onProjectLoad?: (projectId: string, projectName: string) => void;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

const RUNNER_API_URL = "http://localhost:9876";

export function ProjectSelector({ onProjectLoad, onLog }: ProjectSelectorProps) {
  const [projects, setProjects] = useState<RAGProject[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingProjectId, setLoadingProjectId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);

  // Fetch projects from the runner
  const fetchProjects = useCallback(async () => {
    setLoading(true);
    setError(null);

    try {
      const response = await fetch(`${RUNNER_API_URL}/rag/list`);
      if (!response.ok) {
        throw new Error(`Failed to fetch projects: ${response.statusText}`);
      }

      const data = await response.json();
      if (data.success && data.data) {
        setProjects(data.data);
        onLog?.("info", `Found ${data.data.length} saved RAG projects`);
      } else {
        throw new Error(data.error || "Unknown error");
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : "Failed to fetch projects";
      setError(message);
      onLog?.("error", message);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  // Load projects on mount
  useEffect(() => {
    fetchProjects();
  }, [fetchProjects]);

  // Load a project into the executor
  const handleLoadProject = useCallback(
    async (project: RAGProject) => {
      setLoadingProjectId(project.project_id);
      setError(null);

      try {
        const response = await fetch(`${RUNNER_API_URL}/rag/${project.project_id}/load`, {
          method: "POST",
        });

        if (!response.ok) {
          throw new Error(`Failed to load project: ${response.statusText}`);
        }

        const data = await response.json();
        if (data.success && data.data?.success) {
          setSelectedProjectId(project.project_id);
          onProjectLoad?.(project.project_id, project.project_name);
          onLog?.("info", `Loaded RAG project: ${project.project_name}`);
        } else {
          throw new Error(data.data?.message || data.error || "Unknown error");
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : "Failed to load project";
        setError(message);
        onLog?.("error", message);
      } finally {
        setLoadingProjectId(null);
      }
    },
    [onProjectLoad, onLog],
  );

  // Format date for display
  const formatDate = (dateString: string) => {
    try {
      const date = new Date(dateString);
      return date.toLocaleDateString(undefined, {
        month: "short",
        day: "numeric",
        year: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      });
    } catch {
      return dateString;
    }
  };

  return (
    <div className="space-y-3">
      {/* Header with refresh button */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium text-muted-foreground">Saved RAG Projects</h3>
        <button
          onClick={fetchProjects}
          disabled={loading}
          className="p-1.5 rounded hover:bg-accent transition-colors disabled:opacity-50"
          title="Refresh projects"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Error message */}
      {error && (
        <div
          className={`flex items-center gap-2 p-2 text-sm ${getStatusColors("error").text} ${getStatusColors("error").bg} rounded border ${getStatusColors("error").border}`}
        >
          <AlertCircle className="w-4 h-4 flex-shrink-0" />
          <span className="truncate">{error}</span>
        </div>
      )}

      {/* Loading state */}
      {loading && projects.length === 0 && (
        <div className="flex items-center justify-center py-4 text-muted-foreground">
          <Loader2 className="w-5 h-5 animate-spin mr-2" />
          <span>Loading projects...</span>
        </div>
      )}

      {/* No projects message */}
      {!loading && projects.length === 0 && !error && (
        <div className="text-center py-4 text-muted-foreground text-sm">
          <FolderOpen className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>No saved RAG projects found</p>
          <p className="text-xs mt-1">Export a project from qontinui-web to create one</p>
        </div>
      )}

      {/* Project list */}
      {projects.length > 0 && (
        <div className="space-y-2 max-h-48 overflow-y-auto pr-1">
          {projects.map((project) => {
            const isSelected = selectedProjectId === project.project_id;
            const isLoading = loadingProjectId === project.project_id;

            return (
              <button
                key={project.project_id}
                onClick={() => handleLoadProject(project)}
                disabled={isLoading}
                className={`
                  w-full text-left p-3 rounded-lg border transition-colors
                  ${
                    isSelected
                      ? "border-primary bg-primary/10"
                      : "border-border/50 hover:border-border hover:bg-accent/30"
                  }
                  ${isLoading ? "opacity-70" : ""}
                `}
              >
                <div className="flex items-start justify-between">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm truncate">{project.project_name}</span>
                      {isSelected && <Check className="w-4 h-4 text-primary flex-shrink-0" />}
                      {isLoading && (
                        <Loader2 className="w-4 h-4 animate-spin text-primary flex-shrink-0" />
                      )}
                    </div>
                    <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
                      <span>{project.element_count} elements</span>
                      <span>{project.screenshot_count} screenshots</span>
                      {project.has_embeddings && (
                        <span className={getStatusColors("success").text}>RAG Ready</span>
                      )}
                    </div>
                  </div>
                </div>
                <div className="text-xs text-muted-foreground/70 mt-1">
                  {formatDate(project.exported_at)}
                </div>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
