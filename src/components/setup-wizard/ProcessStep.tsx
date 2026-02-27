import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Server, Loader2, Play, CheckSquare, Square } from "lucide-react";

interface Project {
  path: string;
  name: string;
  type?: string;
  framework?: string;
}

interface ProcessConfig {
  id: string;
  name: string;
  command: string;
  args: string[];
  cwd: string;
  health_port?: number;
  parser: string;
  category: string;
  auto_start: boolean;
  enabled: boolean;
  buffer_size: number;
  env?: Record<string, string>;
}

interface ProcessStepProps {
  selectedProjects: Project[];
  selectedConfigs: ProcessConfig[];
  onConfigsChange: (configs: ProcessConfig[]) => void;
  onNext: () => void;
  onBack: () => void;
}

const CATEGORY_COLORS: Record<string, string> = {
  frontend: "badge-info",
  backend: "badge-success",
  general: "badge-muted",
};

export function ProcessStep({
  selectedProjects,
  selectedConfigs,
  onConfigsChange,
  onNext,
  onBack,
}: ProcessStepProps) {
  const [suggestions, setSuggestions] = useState<ProcessConfig[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadSuggestions() {
      if (selectedProjects.length === 0) return;
      setLoading(true);
      setError(null);
      try {
        const result = await invoke<ProcessConfig[]>("suggest_process_configs_for_setup", {
          projects: selectedProjects,
        });
        if (cancelled) return;
        setSuggestions(result);
        // Auto-select all suggestions if none are currently selected
        if (selectedConfigs.length === 0 && result.length > 0) {
          onConfigsChange(result);
        }
      } catch (err) {
        if (cancelled) return;
        setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    loadSuggestions();

    return () => {
      cancelled = true;
    };
  }, [selectedProjects, selectedConfigs.length, onConfigsChange]);

  const toggleConfig = useCallback(
    (config: ProcessConfig) => {
      const isSelected = selectedConfigs.some((c) => c.id === config.id);
      if (isSelected) {
        onConfigsChange(selectedConfigs.filter((c) => c.id !== config.id));
      } else {
        onConfigsChange([...selectedConfigs, config]);
      }
    },
    [selectedConfigs, onConfigsChange],
  );

  const truncatePath = (p: string) => {
    if (p.length <= 60) return p;
    return "..." + p.slice(-57);
  };

  if (selectedProjects.length === 0) {
    return (
      <div className="tutorial-fade-in flex flex-col items-center py-8 px-4">
        <div className="empty-state py-8">
          <Server className="w-10 h-10 text-muted-foreground mb-2 mx-auto" />
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
        <h2 className="text-h2 mb-2">Managed Processes</h2>
        <p className="text-body text-muted-foreground">
          Select dev processes to manage. The runner can start, stop, restart, and capture output
          from these processes.
        </p>
      </div>

      {loading ? (
        <div className="flex flex-col items-center py-12 gap-3">
          <Loader2 className="w-8 h-8 text-primary animate-spin" />
          <p className="text-muted-foreground">Detecting processes...</p>
        </div>
      ) : error ? (
        <div className="panel border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300 max-w-xl mx-auto w-full">
          {error}
        </div>
      ) : suggestions.length === 0 ? (
        <div className="max-w-xl mx-auto w-full">
          <div className="empty-state py-8">
            <Server className="w-10 h-10 text-muted-foreground mb-2 mx-auto" />
            <p className="text-muted-foreground">
              No process suggestions found for the selected projects.
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              You can add processes manually later from Settings.
            </p>
          </div>
        </div>
      ) : (
        <div className="max-w-xl mx-auto w-full flex flex-col gap-1.5">
          {suggestions.map((config) => {
            const isSelected = selectedConfigs.some((c) => c.id === config.id);
            return (
              <button
                key={config.id}
                className={`panel-interactive flex items-center gap-3 p-2.5 text-left transition-colors ${
                  isSelected ? "border-primary/50" : ""
                }`}
                onClick={() => toggleConfig(config)}
                data-ui-id={`setup-process-${config.id}`}
              >
                {isSelected ? (
                  <CheckSquare className="w-4 h-4 text-primary shrink-0" />
                ) : (
                  <Square className="w-4 h-4 text-zinc-500 shrink-0" />
                )}
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <Play className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="text-sm font-medium">{config.name}</span>
                  </div>
                  <div className="mt-0.5 text-xs text-muted-foreground font-mono truncate">
                    {config.command} {config.args.join(" ")}
                  </div>
                  <div className="mt-0.5 text-xs text-muted-foreground flex items-center gap-3">
                    <span>{truncatePath(config.cwd)}</span>
                    {config.health_port && <span>Port: {config.health_port}</span>}
                  </div>
                </div>
                <div className="flex gap-1.5 shrink-0">
                  <span
                    className={`badge text-xs ${CATEGORY_COLORS[config.category] || "badge-muted"}`}
                  >
                    {config.category}
                  </span>
                </div>
              </button>
            );
          })}
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
          data-ui-id="setup-wizard-processes-next"
        >
          {selectedConfigs.length > 0
            ? `Continue with ${selectedConfigs.length} process${selectedConfigs.length !== 1 ? "es" : ""}`
            : "Skip"}
        </button>
      </div>
    </div>
  );
}
