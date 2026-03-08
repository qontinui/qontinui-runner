import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, Search, Loader2, Package, CheckSquare, Square } from "lucide-react";

interface Project {
  path: string;
  name: string;
  type: string;
  manifest: string;
}

interface ProjectStepProps {
  selectedProjects: Project[];
  onProjectsChange: (projects: Project[]) => void;
  onWorkspacePathChange: (path: string) => void;
  onNext: () => void;
  onBack: () => void;
}

const TYPE_LABELS: Record<string, string> = {
  node: "Node.js",
  python: "Python",
  rust: "Rust",
  go: "Go",
  "java-maven": "Java (Maven)",
  "java-gradle": "Java (Gradle)",
  ruby: "Ruby",
  flutter: "Flutter",
  php: "PHP",
  dotnet: ".NET",
};

const TYPE_COLORS: Record<string, string> = {
  node: "text-green-400",
  python: "text-blue-400",
  rust: "text-orange-400",
  go: "text-cyan-400",
  "java-maven": "text-red-400",
  "java-gradle": "text-red-400",
  ruby: "text-rose-400",
  flutter: "text-sky-400",
  php: "text-violet-400",
  dotnet: "text-purple-400",
};

export function ProjectStep({
  selectedProjects,
  onProjectsChange,
  onWorkspacePathChange,
  onNext,
  onBack,
}: ProjectStepProps) {
  const [scanPath, setScanPath] = useState("");
  const [discoveredProjects, setDiscoveredProjects] = useState<Project[]>([]);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pickFolder = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (selected) {
        const path = selected as string;
        setScanPath(path);
        onWorkspacePathChange(path);
        setDiscoveredProjects([]);
        setScanned(false);
        setError(null);
      }
    } catch (err) {
      setError(`Failed to open folder picker: ${err}`);
    }
  }, [onWorkspacePathChange]);

  const scanWorkspace = useCallback(async () => {
    if (!scanPath) return;
    setScanning(true);
    setError(null);
    try {
      const result = await invoke<Project[]>("scan_workspace_for_setup", {
        path: scanPath,
        maxDepth: 3,
      });
      setDiscoveredProjects(result);
      // Auto-select all discovered projects
      onProjectsChange(result);
      setScanned(true);
    } catch (err) {
      setError(`Scan failed: ${err}`);
    } finally {
      setScanning(false);
    }
  }, [scanPath, onProjectsChange]);

  const toggleProject = useCallback(
    (project: Project) => {
      const isSelected = selectedProjects.some((p) => p.path === project.path);
      if (isSelected) {
        onProjectsChange(selectedProjects.filter((p) => p.path !== project.path));
      } else {
        onProjectsChange([...selectedProjects, project]);
      }
    },
    [selectedProjects, onProjectsChange],
  );

  const selectAll = useCallback(() => {
    onProjectsChange([...discoveredProjects]);
  }, [discoveredProjects, onProjectsChange]);

  const deselectAll = useCallback(() => {
    onProjectsChange([]);
  }, [onProjectsChange]);

  return (
    <div className="tutorial-fade-in flex flex-col gap-6 py-4 px-4">
      <div className="text-center">
        <h2 className="text-h2 mb-2">Discover Projects</h2>
        <p className="text-body text-muted-foreground">
          Select a folder to scan for software projects
        </p>
      </div>

      {/* Folder picker */}
      <div className="flex gap-2 items-center max-w-xl mx-auto w-full">
        <button className="btn-secondary flex items-center gap-2 shrink-0" onClick={pickFolder}>
          <FolderOpen className="w-4 h-4" />
          Browse
        </button>
        <div className="flex-1 panel px-3 py-2 text-sm truncate min-w-0">
          {scanPath || <span className="text-muted-foreground">No folder selected</span>}
        </div>
        <button
          className="btn-primary flex items-center gap-2 shrink-0"
          onClick={scanWorkspace}
          disabled={!scanPath || scanning}
        >
          {scanning ? <Loader2 className="w-4 h-4 animate-spin" /> : <Search className="w-4 h-4" />}
          Scan
        </button>
      </div>

      {error && (
        <div className="panel border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300 max-w-xl mx-auto w-full">
          {error}
        </div>
      )}

      {/* Discovered projects */}
      {scanned && (
        <div className="max-w-xl mx-auto w-full">
          {discoveredProjects.length === 0 ? (
            <div className="empty-state py-8">
              <Package className="w-10 h-10 text-muted-foreground mb-2 mx-auto" />
              <p className="text-muted-foreground">No projects found in this directory</p>
            </div>
          ) : (
            <>
              <div className="flex items-center justify-between mb-3">
                <span className="text-label text-muted-foreground">
                  {discoveredProjects.length} project{discoveredProjects.length !== 1 ? "s" : ""}{" "}
                  found
                </span>
                <div className="flex gap-2">
                  <button className="text-xs text-primary hover:underline" onClick={selectAll}>
                    Select All
                  </button>
                  <button
                    className="text-xs text-muted-foreground hover:underline"
                    onClick={deselectAll}
                  >
                    Deselect All
                  </button>
                </div>
              </div>
              <div className="flex flex-col gap-1.5 max-h-72 overflow-y-auto pr-1">
                {discoveredProjects.map((project) => {
                  const isSelected = selectedProjects.some((p) => p.path === project.path);
                  return (
                    <button
                      key={project.path}
                      className={`panel-interactive flex items-center gap-3 p-3 text-left transition-colors ${
                        isSelected ? "border-primary/50" : ""
                      }`}
                      onClick={() => toggleProject(project)}
                    >
                      {isSelected ? (
                        <CheckSquare className="w-4 h-4 text-primary shrink-0" />
                      ) : (
                        <Square className="w-4 h-4 text-zinc-500 shrink-0" />
                      )}
                      <div className="flex-1 min-w-0">
                        <div className="font-medium text-sm">{project.name}</div>
                        <div className="text-xs text-muted-foreground truncate">{project.path}</div>
                      </div>
                      <span
                        className={`badge text-xs ${TYPE_COLORS[project.type] || "text-zinc-400"}`}
                      >
                        {TYPE_LABELS[project.type] || project.type}
                      </span>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </div>
      )}

      {/* Navigation */}
      <div className="flex justify-between max-w-xl mx-auto w-full pt-4">
        <button className="btn-secondary" onClick={onBack}>
          Back
        </button>
        <button className="btn-primary" onClick={onNext} disabled={!scanned}>
          {selectedProjects.length > 0
            ? `Continue with ${selectedProjects.length} project${selectedProjects.length !== 1 ? "s" : ""}`
            : "Skip"}
        </button>
      </div>
    </div>
  );
}
