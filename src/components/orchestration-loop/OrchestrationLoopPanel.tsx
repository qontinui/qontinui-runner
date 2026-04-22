import { useState, useEffect, useCallback, useRef, Fragment } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cn } from "../../lib/utils";
import {
  Play,
  Square,
  Loader2,
  Zap,
  Save,
  Star,
  Trash2,
  FolderOpen,
  Layers,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { MultiLoopPanel } from "./MultiLoopPanel";

// --- Types matching the Rust backend ---

interface OrchestrationLoopConfig {
  target_runner_port: number | null;
  target_runner_id: string | null;
  supervisor_port: number;
  workflow_id: string;
  /** `null` indicates no iteration cap (unlimited). */
  max_iterations: number | null;
  exit_strategy: { type: string };
  between_iterations: { type: string; rebuild?: boolean };
  retry_on_failure: boolean;
  wait_for_fixer: boolean;
  use_worktree: boolean;
  pipeline: PipelineConfig | null;
}

interface PipelineConfig {
  build: { description: string; context: string | null; context_ids: string[] | null } | null;
  implement_fixes: {
    model: string | null;
    timeout_secs: number | null;
    additional_context: string | null;
  } | null;
  diagnose: {
    assertions: Record<string, unknown>[];
    capture_snapshot: boolean;
    snapshot_max_chars: number;
    model_override: string | null;
  } | null;
}

interface RunnerInstance {
  id: string;
  name: string;
  port: number;
  running: boolean;
  pid: number | null;
  api_ready: boolean;
}

interface OrchestrationLoopStatus {
  running: boolean;
  phase: string;
  current_iteration: number;
  /** `null` = unlimited. Renders as "∞" and disables the progress bar. */
  max_iterations: number | null;
  workflow_id: string;
  target_runner_port: number;
  target_runner_id: string | null;
  is_pipeline: boolean;
  started_at: string | null;
  error: string | null;
  iteration_results: IterationResult[];
}

interface IterationResult {
  iteration: number;
  started_at: string;
  completed_at: string;
  task_run_id: string;
  reflection_task_run_id: string | null;
  fix_count: number | null;
  exit_check: { should_exit: boolean; reason: string };
  generated_workflow_id?: string | null;
  fixes_implemented?: boolean | null;
  rebuild_triggered?: boolean | null;
  diagnostic_result?: {
    passed: boolean;
    page_health: Record<string, unknown>;
    assertion_results: Record<string, unknown>[];
    root_cause: string | null;
    diagnosis: string | null;
    prompt_rewrite_suggestion: string | null;
  } | null;
}

interface SavedOlConfig {
  id: string;
  name: string;
  description: string | null;
  is_favorite: boolean;
  config_json: OrchestrationLoopFormState;
  created_at: string;
  updated_at: string;
}

// Serializable form state (what we save/load)
interface OrchestrationLoopFormState {
  mode: "simple" | "pipeline";
  workflowId: string;
  targetPort: string;
  targetRunnerId: string;
  supervisorPort: string;
  /** `null` means no iteration cap. */
  maxIter: number | null;
  exitStrategy: string;
  between: string;
  buildDesc: string;
  buildContext: string;
  enableFixes: boolean;
  fixContext: string;
  retryOnFailure: boolean;
  waitForFixer: boolean;
  useWorktree: boolean;
  enableDiagnose: boolean;
  diagnoseAssertions: string;
  diagnoseCaptureSnapshot: boolean;
  diagnoseSnapshotMaxChars: number;
  diagnoseModelOverride: string;
}

// --- Phase colors ---

const PHASE_COLORS: Record<string, string> = {
  idle: "text-muted-foreground",
  running_workflow: "text-blue-400",
  building_workflow: "text-yellow-400",
  diagnosing: "text-teal-400",
  evaluating_exit: "text-yellow-400",
  reflecting: "text-purple-400",
  implementing_fixes: "text-orange-400",
  between_iterations: "text-purple-400",
  waiting_for_fixer: "text-cyan-400",
  waiting_for_runner: "text-yellow-400",
  complete: "text-green-400",
  stopped: "text-muted-foreground",
  error: "text-red-400",
};

const PHASE_BG: Record<string, string> = {
  complete: "bg-green-500/10",
  error: "bg-red-500/10",
  running_workflow: "bg-blue-500/10",
  building_workflow: "bg-yellow-500/10",
  diagnosing: "bg-teal-500/10",
  reflecting: "bg-purple-500/10",
  implementing_fixes: "bg-orange-500/10",
  waiting_for_fixer: "bg-cyan-500/10",
};

function PhaseBadge({ phase }: { phase: string }) {
  const color = PHASE_COLORS[phase] || "text-muted-foreground";
  const bg = PHASE_BG[phase] || "bg-muted";
  return (
    <span
      className={cn(
        "inline-flex px-2 py-0.5 rounded-full text-[0.7rem] font-semibold font-mono",
        color,
        bg,
      )}
    >
      {phase.replace(/_/g, " ")}
    </span>
  );
}

// --- Shared input styles ---

const inputCls =
  "px-2 py-1 text-xs bg-muted border border-border rounded text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary";
const labelCls = "text-[0.78rem] text-muted-foreground";
const textareaCls = cn(inputCls, "w-full resize-y font-sans");

// --- Main component ---

export function OrchestrationLoopPanel() {
  const [panelMode, setPanelMode] = useState<"single" | "multi">("single");
  const [status, setStatus] = useState<OrchestrationLoopStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Saved configs library
  const [savedConfigs, setSavedConfigs] = useState<SavedOlConfig[]>([]);
  const [showLibrary, setShowLibrary] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [showSaveInput, setShowSaveInput] = useState(false);

  // Runner instances for the target dropdown
  const [runnerInstances, setRunnerInstances] = useState<RunnerInstance[]>([]);
  const [targetRunner, setTargetRunner] = useState("self"); // "self" or instance id

  // Form state
  const [mode, setMode] = useState<"simple" | "pipeline">("pipeline");
  const [workflowId, setWorkflowId] = useState("");
  const [targetPort, setTargetPort] = useState("");
  const [targetRunnerId, setTargetRunnerId] = useState("");
  const [supervisorPort, setSupervisorPort] = useState("9875");
  // null = unlimited; loop exits on success/stop, not iteration count.
  const [maxIter, setMaxIter] = useState<number | null>(null);
  const [exitStrategy, setExitStrategy] = useState("reflection");
  const [between, setBetween] = useState("restart_on_signal");
  const [buildDesc, setBuildDesc] = useState("");
  const [buildContext, setBuildContext] = useState("");
  const [enableFixes, setEnableFixes] = useState(true);
  const [fixContext, setFixContext] = useState("");
  const [retryOnFailure, setRetryOnFailure] = useState(false);
  const [waitForFixer, setWaitForFixer] = useState(true);
  const [useWorktree, setUseWorktree] = useState(false);
  const [enableDiagnose, setEnableDiagnose] = useState(false);
  const [diagnoseAssertions, setDiagnoseAssertions] = useState("[]");
  const [diagnoseCaptureSnapshot, setDiagnoseCaptureSnapshot] = useState(true);
  const [diagnoseSnapshotMaxChars, setDiagnoseSnapshotMaxChars] = useState(8000);
  const [diagnoseModelOverride, setDiagnoseModelOverride] = useState("");
  const [expandedIters, setExpandedIters] = useState<Set<number>>(new Set());

  const toggleIterExpand = (iter: number) => {
    setExpandedIters((prev) => {
      const next = new Set(prev);
      if (next.has(iter)) next.delete(iter);
      else next.add(iter);
      return next;
    });
  };

  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // --- Form state helpers ---

  const getFormState = (): OrchestrationLoopFormState => ({
    mode,
    workflowId,
    targetPort,
    targetRunnerId,
    supervisorPort,
    maxIter,
    exitStrategy,
    between,
    buildDesc,
    buildContext,
    enableFixes,
    fixContext,
    retryOnFailure,
    waitForFixer,
    useWorktree,
    enableDiagnose,
    diagnoseAssertions,
    diagnoseCaptureSnapshot,
    diagnoseSnapshotMaxChars,
    diagnoseModelOverride,
  });

  const loadFormState = (s: OrchestrationLoopFormState) => {
    setMode(s.mode || "pipeline");
    setWorkflowId(s.workflowId || "");
    setTargetPort(s.targetPort || "");
    setTargetRunnerId(s.targetRunnerId || "");
    setSupervisorPort(s.supervisorPort || "9875");
    setMaxIter(s.maxIter ?? null);
    setExitStrategy(s.exitStrategy || "reflection");
    setBetween(s.between || "restart_on_signal");
    setBuildDesc(s.buildDesc || "");
    setBuildContext(s.buildContext || "");
    setEnableFixes(s.enableFixes ?? true);
    setFixContext(s.fixContext || "");
    setRetryOnFailure(s.retryOnFailure ?? false);
    setWaitForFixer(s.waitForFixer ?? true);
    setUseWorktree(s.useWorktree ?? false);
    setEnableDiagnose(s.enableDiagnose ?? false);
    setDiagnoseAssertions(s.diagnoseAssertions || "[]");
    setDiagnoseCaptureSnapshot(s.diagnoseCaptureSnapshot ?? true);
    setDiagnoseSnapshotMaxChars(s.diagnoseSnapshotMaxChars ?? 8000);
    setDiagnoseModelOverride(s.diagnoseModelOverride || "");
    // Resolve targetRunner from saved port/id
    if (s.targetPort) {
      const match = runnerInstances.find((r) => String(r.port) === s.targetPort);
      setTargetRunner(match ? match.id : "self");
    } else {
      setTargetRunner("self");
    }
  };

  // When targetRunner changes, update the underlying port/id fields
  const handleTargetRunnerChange = (value: string) => {
    setTargetRunner(value);
    if (value === "self") {
      setTargetPort("");
      setTargetRunnerId("");
    } else {
      const inst = runnerInstances.find((r) => r.id === value);
      if (inst) {
        setTargetPort(String(inst.port));
        setTargetRunnerId(inst.id);
      }
    }
  };

  // --- Data fetching ---

  const fetchStatus = useCallback(async () => {
    try {
      const s = await invoke<OrchestrationLoopStatus>("get_orchestration_loop_status");
      setStatus(s);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadSavedConfigs = useCallback(async () => {
    try {
      const configs = await invoke<SavedOlConfig[]>("ol_list_configs");
      setSavedConfigs(configs);
    } catch {
      /* db may not be ready yet */
    }
  }, []);

  const loadRunnerInstances = useCallback(async () => {
    try {
      const instances = await invoke<RunnerInstance[]>("get_runner_instances");
      setRunnerInstances(instances);
    } catch {
      /* instance manager may not be available */
    }
  }, []);

  useEffect(() => {
    fetchStatus();
    loadSavedConfigs();
    loadRunnerInstances();
    pollRef.current = setInterval(fetchStatus, 3000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [fetchStatus, loadSavedConfigs, loadRunnerInstances]);

  // --- Save/Load/Delete ---

  const handleSave = async () => {
    if (!saveName.trim()) return;
    try {
      await invoke("ol_save_config", {
        request: { name: saveName.trim(), description: null, config_json: getFormState() },
      });
      setSaveName("");
      setShowSaveInput(false);
      loadSavedConfigs();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleLoad = (config: SavedOlConfig) => {
    loadFormState(config.config_json);
    setShowLibrary(false);
  };

  const handleToggleFavorite = async (id: string, current: boolean) => {
    try {
      await invoke("ol_toggle_favorite", { id, isFavorite: !current });
      loadSavedConfigs();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDeleteConfig = async (id: string) => {
    try {
      await invoke("ol_delete_config", { id });
      loadSavedConfigs();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleOverwrite = async (config: SavedOlConfig) => {
    try {
      await invoke("ol_update_config", {
        id: config.id,
        request: { name: null, description: null, is_favorite: null, config_json: getFormState() },
      });
      loadSavedConfigs();
    } catch (e) {
      setError(String(e));
    }
  };

  // --- Start/Stop/Signal ---

  const buildBetween = (): { type: string; rebuild?: boolean } => {
    if (between === "restart_on_signal") return { type: "restart_on_signal", rebuild: true };
    if (between === "restart_on_signal_no_rebuild")
      return { type: "restart_on_signal", rebuild: false };
    if (between === "restart_runner") return { type: "restart_runner", rebuild: true };
    if (between === "restart_runner_no_rebuild") return { type: "restart_runner", rebuild: false };
    if (between === "wait_healthy") return { type: "wait_healthy" };
    return { type: "none" };
  };

  const handleStart = async () => {
    setError(null);
    let config: OrchestrationLoopConfig;
    if (mode === "pipeline") {
      if (!buildDesc && !workflowId) {
        setError("Pipeline needs a build description or workflow ID");
        return;
      }
      if (enableDiagnose) {
        try {
          const parsed = JSON.parse(diagnoseAssertions);
          if (!Array.isArray(parsed)) {
            setError("Diagnostic assertions must be a JSON array");
            return;
          }
        } catch {
          setError("Diagnostic assertions contain invalid JSON");
          return;
        }
      }
      config = {
        target_runner_port: targetPort ? parseInt(targetPort) : null,
        target_runner_id: targetRunnerId || null,
        supervisor_port: parseInt(supervisorPort) || 9875,
        workflow_id: workflowId,
        max_iterations: maxIter,
        exit_strategy: { type: enableDiagnose ? exitStrategy : "reflection" },
        between_iterations: buildBetween(),
        retry_on_failure: retryOnFailure,
        wait_for_fixer: waitForFixer,
        use_worktree: useWorktree,
        pipeline: {
          build: buildDesc
            ? { description: buildDesc, context: buildContext || null, context_ids: null }
            : null,
          implement_fixes: enableFixes
            ? { model: null, timeout_secs: 600, additional_context: fixContext || null }
            : null,
          diagnose: enableDiagnose
            ? {
                assertions: (() => {
                  try {
                    return JSON.parse(diagnoseAssertions);
                  } catch {
                    return [];
                  }
                })(),
                capture_snapshot: diagnoseCaptureSnapshot,
                snapshot_max_chars: diagnoseSnapshotMaxChars,
                model_override: diagnoseModelOverride || null,
              }
            : null,
        },
      };
    } else {
      if (!workflowId) {
        setError("Enter a workflow ID");
        return;
      }
      config = {
        target_runner_port: targetPort ? parseInt(targetPort) : null,
        target_runner_id: targetRunnerId || null,
        supervisor_port: parseInt(supervisorPort) || 9875,
        workflow_id: workflowId,
        max_iterations: maxIter,
        exit_strategy: { type: exitStrategy },
        between_iterations: buildBetween(),
        retry_on_failure: retryOnFailure,
        wait_for_fixer: waitForFixer,
        use_worktree: useWorktree,
        pipeline: null,
      };
    }
    try {
      await invoke("start_orchestration_loop", { config });
    } catch (e) {
      setError(String(e));
    }
  };

  const handleStop = async () => {
    try {
      await invoke("stop_orchestration_loop");
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSignal = async () => {
    try {
      await invoke("signal_orchestration_restart");
    } catch (e) {
      setError(String(e));
    }
  };

  const running = status?.running ?? false;
  const phase = status?.phase || "idle";
  // `cfgMax` is the effective cap (server-reported preferred, falls back to
  // the form field). `null` means the loop is uncapped and the progress bar
  // is rendered as 0% (indeterminate).
  const cfgMax = status?.max_iterations ?? maxIter;
  const progressPct =
    running && cfgMax != null && cfgMax > 0
      ? Math.min(100, Math.round(((status?.current_iteration ?? 0) / cfgMax) * 100))
      : phase === "complete"
        ? 100
        : 0;
  const results = status?.iteration_results ?? [];
  const favorites = savedConfigs.filter((c) => c.is_favorite);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        <Loader2 className="w-4 h-4 animate-spin mr-2" />
        Loading...
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full overflow-hidden" data-tutorial-id="ol-panel">
      {/* Header bar */}
      <div
        className="px-4 py-2.5 border-b border-border flex items-center justify-between"
        data-tutorial-id="ol-header"
      >
        <div className="flex items-center gap-3">
          <h2 className="text-sm font-semibold">Orchestration Loop</h2>
          <button
            onClick={() => setPanelMode(panelMode === "single" ? "multi" : "single")}
            className={cn(
              "px-2 py-0.5 text-[0.7rem] font-medium rounded flex items-center gap-1",
              panelMode === "multi"
                ? "bg-primary/15 text-primary"
                : "bg-muted text-muted-foreground hover:text-foreground",
            )}
            title={panelMode === "single" ? "Switch to Multi-Runner mode" : "Switch to Single mode"}
          >
            <Layers className="w-3 h-3" />
            Multi
          </button>
          {running && (
            <>
              <PhaseBadge phase={phase} />
              <span className="text-xs font-mono text-yellow-400">
                {status?.current_iteration ?? 0}/{cfgMax}
              </span>
            </>
          )}
          {!running && phase !== "idle" && <PhaseBadge phase={phase} />}
        </div>
        <div className="flex items-center gap-2">
          {!running && (
            <>
              <button
                onClick={() => setShowLibrary(!showLibrary)}
                className={cn(
                  "px-2.5 py-1 text-xs font-medium rounded",
                  showLibrary
                    ? "bg-primary/15 text-primary"
                    : "bg-muted text-muted-foreground hover:text-foreground",
                )}
              >
                <FolderOpen className="w-3 h-3 inline mr-1" />
                Library
              </button>
              <button
                onClick={() => setShowSaveInput(!showSaveInput)}
                className="px-2.5 py-1 text-xs font-medium rounded bg-muted text-muted-foreground hover:text-foreground"
              >
                <Save className="w-3 h-3 inline mr-1" />
                Save
              </button>
            </>
          )}
          {running && between.startsWith("restart_on_signal") && (
            <button
              onClick={handleSignal}
              className="px-2.5 py-1 text-xs font-medium rounded bg-yellow-500/10 text-yellow-400 hover:bg-yellow-500/20"
            >
              <Zap className="w-3 h-3 inline mr-1" />
              Signal
            </button>
          )}
          {running ? (
            <button
              onClick={handleStop}
              className="px-2.5 py-1 text-xs font-medium rounded bg-red-500/10 text-red-400 hover:bg-red-500/20"
            >
              <Square className="w-3 h-3 inline mr-1" />
              Stop
            </button>
          ) : (
            <button
              onClick={handleStart}
              className="px-2.5 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25"
            >
              <Play className="w-3 h-3 inline mr-1" />
              Run Loop
            </button>
          )}
        </div>
      </div>

      <div className="flex-1 overflow-auto scrollbar-dark p-3 space-y-3">
        {panelMode === "multi" ? (
          <MultiLoopPanel />
        ) : (
          <>
            <p className="text-[0.78rem] text-muted-foreground -mt-1 mb-2">
              Iterative develop-rebuild-verify loop for testing changes to the runner itself.
            </p>

            {/* Save input */}
            {showSaveInput && !running && (
              <div className="flex items-center gap-2">
                <input
                  type="text"
                  value={saveName}
                  onChange={(e) => setSaveName(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && handleSave()}
                  placeholder="Config name..."
                  className={cn(inputCls, "flex-1")}
                  autoFocus
                />
                <button
                  onClick={handleSave}
                  disabled={!saveName.trim()}
                  className="px-2.5 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25 disabled:opacity-40"
                >
                  Save
                </button>
                <button
                  onClick={() => {
                    setShowSaveInput(false);
                    setSaveName("");
                  }}
                  className="px-2.5 py-1 text-xs font-medium rounded bg-muted text-muted-foreground"
                >
                  Cancel
                </button>
              </div>
            )}

            {/* Library panel */}
            {showLibrary && !running && (
              <div className="border border-border rounded">
                <div className="px-3 py-1.5 border-b border-border flex items-center justify-between">
                  <span className="text-xs font-semibold">Saved Configurations</span>
                  <span className="text-[0.7rem] text-muted-foreground">
                    {savedConfigs.length} configs
                  </span>
                </div>
                {savedConfigs.length === 0 ? (
                  <div className="px-3 py-3 text-xs text-muted-foreground text-center">
                    No saved configurations yet
                  </div>
                ) : (
                  <div className="divide-y divide-border/50">
                    {savedConfigs.map((cfg) => (
                      <div
                        key={cfg.id}
                        className="flex items-center gap-2 px-3 py-1.5 hover:bg-muted/30 group"
                      >
                        <button
                          onClick={() => handleToggleFavorite(cfg.id, cfg.is_favorite)}
                          className="shrink-0"
                          title={cfg.is_favorite ? "Remove from favorites" : "Add to favorites"}
                        >
                          <Star
                            className={cn(
                              "w-3.5 h-3.5",
                              cfg.is_favorite
                                ? "fill-yellow-400 text-yellow-400"
                                : "text-muted-foreground/40 hover:text-yellow-400",
                            )}
                          />
                        </button>
                        <button
                          onClick={() => handleLoad(cfg)}
                          className="flex-1 text-left text-xs truncate hover:text-primary"
                          title={`Load "${cfg.name}"`}
                        >
                          <span className="font-medium">{cfg.name}</span>
                          <span className="ml-2 text-muted-foreground text-[0.7rem]">
                            {cfg.config_json?.mode || "?"} &middot;{" "}
                            {new Date(cfg.updated_at).toLocaleDateString()}
                          </span>
                        </button>
                        <button
                          onClick={() => handleOverwrite(cfg)}
                          className="shrink-0 opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-primary"
                          title="Overwrite with current settings"
                        >
                          <Save className="w-3 h-3" />
                        </button>
                        <button
                          onClick={() => handleDeleteConfig(cfg.id)}
                          className="shrink-0 opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-red-400"
                          title="Delete"
                        >
                          <Trash2 className="w-3 h-3" />
                        </button>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {/* Favorite quick-load (shown when library is closed and there are favorites) */}
            {!showLibrary && !running && favorites.length > 0 && (
              <div className="flex items-center gap-1.5 flex-wrap">
                <Star className="w-3 h-3 text-yellow-400 shrink-0" />
                {favorites.map((cfg) => (
                  <button
                    key={cfg.id}
                    onClick={() => handleLoad(cfg)}
                    className="px-2 py-0.5 text-[0.7rem] rounded bg-muted hover:bg-primary/10 hover:text-primary text-muted-foreground transition-colors"
                  >
                    {cfg.name}
                  </button>
                ))}
              </div>
            )}

            {/* Error */}
            {(error || status?.error) && (
              <div className="border-l-2 border-red-500 bg-red-500/10 px-3 py-1.5 text-xs text-red-400 rounded-r">
                {error || status?.error}
              </div>
            )}

            {/* Progress bar */}
            {(running || phase === "complete") && (
              <div
                className={cn(
                  "border-l-2 rounded-r px-3 py-2 flex items-center gap-3",
                  phase === "error" ? "border-red-500" : "border-primary",
                )}
              >
                <PhaseBadge phase={phase} />
                <div className="flex-1 bg-muted rounded h-1.5">
                  <div
                    className={cn(
                      "h-1.5 rounded transition-all duration-300",
                      phase === "complete" ? "bg-green-500" : "bg-primary",
                    )}
                    style={{ width: `${progressPct}%` }}
                  />
                </div>
                <span className="text-xs font-mono text-muted-foreground">
                  {status?.current_iteration ?? 0}/{cfgMax}
                </span>
              </div>
            )}

            {/* Config form */}
            {!running && (
              <div
                className="border border-border rounded p-3 space-y-3"
                data-tutorial-id="ol-config-form"
              >
                <div className="flex items-center gap-4 flex-wrap">
                  <div className="flex items-center gap-2">
                    <label className="flex items-center gap-1 text-xs cursor-pointer">
                      <input
                        type="radio"
                        name="olMode"
                        checked={mode === "simple"}
                        onChange={() => setMode("simple")}
                        className="accent-primary"
                      />{" "}
                      Simple
                    </label>
                    <label className="flex items-center gap-1 text-xs cursor-pointer">
                      <input
                        type="radio"
                        name="olMode"
                        checked={mode === "pipeline"}
                        onChange={() => setMode("pipeline")}
                        className="accent-primary"
                      />{" "}
                      Pipeline
                    </label>
                  </div>
                  <div className="flex items-center gap-1.5">
                    <span className={labelCls}>Between</span>
                    <select
                      value={between}
                      onChange={(e) => setBetween(e.target.value)}
                      className={cn(inputCls, "w-auto [&>option]:text-black [&>option]:bg-white")}
                      style={{ colorScheme: "dark" }}
                    >
                      <option value="restart_on_signal">Signal (rebuild)</option>
                      <option value="restart_on_signal_no_rebuild">Signal (no rebuild)</option>
                      <option value="restart_runner">Always (rebuild)</option>
                      <option value="restart_runner_no_rebuild">Always (no rebuild)</option>
                      <option value="wait_healthy">Wait Healthy</option>
                      <option value="none">None</option>
                    </select>
                  </div>
                  <div className="flex items-center gap-1.5">
                    <span className={labelCls}>Max</span>
                    <input
                      type="number"
                      value={maxIter ?? ""}
                      onChange={(e) => {
                        const raw = e.target.value.trim();
                        if (raw === "") {
                          setMaxIter(null);
                          return;
                        }
                        const parsed = parseInt(raw, 10);
                        setMaxIter(Number.isFinite(parsed) && parsed >= 1 ? parsed : null);
                      }}
                      placeholder="∞"
                      title="Blank = unlimited iterations"
                      min={1}
                      max={50}
                      className={cn(inputCls, "w-14 text-center")}
                    />
                  </div>
                  <div className="flex items-center gap-1.5">
                    <span className={labelCls}>Target</span>
                    <select
                      value={targetRunner}
                      onChange={(e) => handleTargetRunnerChange(e.target.value)}
                      className={cn(inputCls, "w-auto [&>option]:text-black [&>option]:bg-white")}
                      style={{ colorScheme: "dark" }}
                    >
                      <option value="self">This runner (self)</option>
                      {runnerInstances.map((inst) => (
                        <option key={inst.id} value={inst.id}>
                          {inst.name} (:{inst.port}){inst.running ? "" : " [stopped]"}
                        </option>
                      ))}
                    </select>
                  </div>
                </div>

                <div className="flex items-center gap-4 flex-wrap">
                  <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                    <input
                      type="checkbox"
                      checked={retryOnFailure}
                      onChange={(e) => setRetryOnFailure(e.target.checked)}
                      className="accent-primary"
                    />
                    <span className="text-muted-foreground">Retry on failure</span>
                    <span className="text-muted-foreground/60 text-[0.7rem]">
                      — Re-run after fixer completes
                    </span>
                  </label>
                  {retryOnFailure && (
                    <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                      <input
                        type="checkbox"
                        checked={waitForFixer}
                        onChange={(e) => setWaitForFixer(e.target.checked)}
                        className="accent-primary"
                      />
                      <span className="text-muted-foreground">Wait for fixer</span>
                      <span className="text-muted-foreground/60 text-[0.7rem]">
                        — Pause until fixer workflow finishes
                      </span>
                    </label>
                  )}
                  <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                    <input
                      type="checkbox"
                      checked={useWorktree}
                      onChange={(e) => setUseWorktree(e.target.checked)}
                      className="accent-primary"
                    />
                    <span className="text-muted-foreground">Run in isolated worktree</span>
                    <span className="text-muted-foreground/60 text-[0.7rem]">
                      — Create a new git branch and worktree for this run. Changes stay isolated
                      until merged.
                    </span>
                  </label>
                </div>

                {mode === "simple" ? (
                  <div className="flex items-center gap-4 flex-wrap">
                    <div className="flex items-center gap-1.5 flex-1 min-w-[200px]">
                      <span className={labelCls}>Workflow</span>
                      <input
                        type="text"
                        value={workflowId}
                        onChange={(e) => setWorkflowId(e.target.value)}
                        placeholder="workflow-id"
                        className={cn(inputCls, "flex-1")}
                      />
                    </div>
                    <div className="flex items-center gap-1.5">
                      <span className={labelCls}>Exit</span>
                      <select
                        value={exitStrategy}
                        onChange={(e) => setExitStrategy(e.target.value)}
                        className={cn(inputCls, "w-auto [&>option]:text-black [&>option]:bg-white")}
                        style={{ colorScheme: "dark" }}
                      >
                        <option value="reflection">Reflection (0 fixes)</option>
                        <option value="workflow_verification">Verification</option>
                        <option value="fixed_iterations">Fixed Iterations</option>
                        <option value="diagnostic_evaluation">Diagnostic Evaluation</option>
                      </select>
                    </div>
                  </div>
                ) : (
                  <>
                    <div
                      className={cn("grid gap-2", enableFixes ? "grid-cols-3" : "grid-cols-2")}
                      data-tutorial-id="ol-pipeline-textareas"
                    >
                      <div>
                        <div className={cn(labelCls, "mb-1")}>Build Description</div>
                        <textarea
                          value={buildDesc}
                          onChange={(e) => setBuildDesc(e.target.value)}
                          placeholder="Describe the workflow to generate..."
                          rows={8}
                          className={textareaCls}
                        />
                      </div>
                      <div>
                        <div className={cn(labelCls, "mb-1")}>Build Context (optional)</div>
                        <textarea
                          value={buildContext}
                          onChange={(e) => setBuildContext(e.target.value)}
                          placeholder="Additional context for the builder..."
                          rows={8}
                          className={textareaCls}
                        />
                      </div>
                      {enableFixes && (
                        <div>
                          <div className={cn(labelCls, "mb-1")}>Fix Context (optional)</div>
                          <textarea
                            value={fixContext}
                            onChange={(e) => setFixContext(e.target.value)}
                            placeholder="Extra instructions for the fix agent..."
                            rows={8}
                            className={textareaCls}
                          />
                        </div>
                      )}
                    </div>
                    <div className="flex items-center gap-4 flex-wrap">
                      <div className="flex items-center gap-1.5 flex-1 min-w-[200px]">
                        <span className={cn(labelCls, "whitespace-nowrap")}>
                          Workflow (fallback)
                        </span>
                        <input
                          type="text"
                          value={workflowId}
                          onChange={(e) => setWorkflowId(e.target.value)}
                          placeholder="None (use build)"
                          className={cn(inputCls, "flex-1")}
                        />
                      </div>
                      <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                        <input
                          type="checkbox"
                          checked={enableFixes}
                          onChange={(e) => setEnableFixes(e.target.checked)}
                          className="accent-primary"
                        />
                        <span className="text-muted-foreground">Enable Fix Implementation</span>
                        <span className="text-muted-foreground/60 text-[0.7rem]">
                          — Spawns Claude Code after reflection
                        </span>
                      </label>
                      {enableDiagnose && (
                        <div
                          className="flex items-center gap-1.5"
                          data-tutorial-id="ol-diag-exit-strategy"
                        >
                          <span className={labelCls}>Exit</span>
                          <select
                            value={exitStrategy}
                            onChange={(e) => setExitStrategy(e.target.value)}
                            className={cn(
                              inputCls,
                              "w-auto [&>option]:text-black [&>option]:bg-white",
                            )}
                            style={{ colorScheme: "dark" }}
                          >
                            <option value="reflection">Reflection (0 fixes)</option>
                            <option value="diagnostic_evaluation">Diagnostic Evaluation</option>
                            <option value="fixed_iterations">Fixed Iterations</option>
                          </select>
                        </div>
                      )}
                      <label
                        data-tutorial-id="ol-enable-diagnose"
                        className="flex items-center gap-1.5 text-xs cursor-pointer"
                      >
                        <input
                          type="checkbox"
                          checked={enableDiagnose}
                          onChange={(e) => setEnableDiagnose(e.target.checked)}
                          className="accent-primary"
                        />
                        <span className="text-muted-foreground">Enable Diagnostic Phase</span>
                        <span className="text-muted-foreground/60 text-[0.7rem]">
                          — UI Bridge assertions after execution
                        </span>
                      </label>
                    </div>
                    {enableDiagnose && (
                      <div
                        data-tutorial-id="ol-diag-config-panel"
                        className="border border-teal-500/20 rounded p-3 space-y-2 bg-teal-500/5"
                      >
                        <div className="text-[0.7rem] font-semibold text-teal-400 mb-1">
                          Diagnostic Configuration
                        </div>
                        <div>
                          <div className={cn(labelCls, "mb-1")}>Assertions (JSON array)</div>
                          <textarea
                            value={diagnoseAssertions}
                            onChange={(e) => setDiagnoseAssertions(e.target.value)}
                            placeholder={
                              '[\n  { "type": "visible", "role": "tab", "name": "Transitions" },\n  { "type": "count", "role": "row", "min": 1 }\n]'
                            }
                            rows={6}
                            className={cn(textareaCls, "font-mono text-[0.7rem]")}
                          />
                        </div>
                        <div className="flex items-center gap-4 flex-wrap">
                          <label className="flex items-center gap-1.5 text-xs cursor-pointer">
                            <input
                              type="checkbox"
                              checked={diagnoseCaptureSnapshot}
                              onChange={(e) => setDiagnoseCaptureSnapshot(e.target.checked)}
                              className="accent-primary"
                            />
                            <span className="text-muted-foreground">Capture DOM snapshot</span>
                          </label>
                          <div className="flex items-center gap-1.5">
                            <span className={labelCls}>Max chars</span>
                            <input
                              type="number"
                              value={diagnoseSnapshotMaxChars}
                              onChange={(e) => setDiagnoseSnapshotMaxChars(Number(e.target.value))}
                              min={1000}
                              max={50000}
                              step={1000}
                              className={cn(inputCls, "w-20 text-center")}
                            />
                          </div>
                          <div
                            data-tutorial-id="ol-diag-model-override"
                            className="flex items-center gap-1.5"
                          >
                            <span className={labelCls}>Model override</span>
                            <input
                              type="text"
                              value={diagnoseModelOverride}
                              onChange={(e) => setDiagnoseModelOverride(e.target.value)}
                              placeholder="(default)"
                              className={cn(inputCls, "w-36")}
                            />
                          </div>
                        </div>
                      </div>
                    )}
                  </>
                )}
              </div>
            )}

            {/* Iteration History */}
            <div data-tutorial-id="ol-iteration-history" className="border border-border rounded">
              <div className="px-3 py-1.5 border-b border-border flex items-center justify-between">
                <span className="text-xs font-semibold">Iteration History</span>
                <span className="text-[0.7rem] text-muted-foreground">
                  {results.length} iterations
                </span>
              </div>
              <div className="overflow-x-auto">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="border-b border-border text-muted-foreground">
                      <th className="px-3 py-1.5 text-left font-medium">#</th>
                      <th className="px-3 py-1.5 text-left font-medium">Duration</th>
                      <th className="px-3 py-1.5 text-left font-medium">Exit</th>
                      <th className="px-3 py-1.5 text-left font-medium">Reason</th>
                      <th className="px-3 py-1.5 text-left font-medium">Diag</th>
                      <th className="px-3 py-1.5 text-left font-medium">Pipeline</th>
                    </tr>
                  </thead>
                  <tbody>
                    {results.map((iter) => {
                      const duration =
                        iter.started_at && iter.completed_at
                          ? (
                              (new Date(iter.completed_at).getTime() -
                                new Date(iter.started_at).getTime()) /
                              1000
                            ).toFixed(1) + "s"
                          : "--";
                      const meta: string[] = [];
                      if (iter.fix_count != null) meta.push(`fixes=${iter.fix_count}`);
                      if (iter.fixes_implemented != null)
                        meta.push(`applied=${iter.fixes_implemented ? "yes" : "no"}`);
                      if (iter.rebuild_triggered) meta.push("rebuild");
                      if (iter.generated_workflow_id) meta.push("built");
                      const diag = iter.diagnostic_result;
                      const isExpanded = expandedIters.has(iter.iteration);
                      return (
                        <Fragment key={iter.iteration}>
                          <tr
                            className={cn(
                              "border-b border-border/50 hover:bg-muted/30",
                              diag && "cursor-pointer",
                            )}
                            onClick={() => diag && toggleIterExpand(iter.iteration)}
                          >
                            <td className="px-3 py-1.5 text-primary font-semibold">
                              <span className="flex items-center gap-1">
                                {diag &&
                                  (isExpanded ? (
                                    <ChevronDown className="w-3 h-3 text-muted-foreground" />
                                  ) : (
                                    <ChevronRight className="w-3 h-3 text-muted-foreground" />
                                  ))}
                                #{iter.iteration}
                              </span>
                            </td>
                            <td className="px-3 py-1.5 font-mono">{duration}</td>
                            <td className="px-3 py-1.5">
                              <span
                                className={
                                  iter.exit_check?.should_exit
                                    ? "text-green-400"
                                    : "text-yellow-400"
                                }
                              >
                                {iter.exit_check?.should_exit ? "EXIT" : "CONTINUE"}
                              </span>
                            </td>
                            <td
                              className="px-3 py-1.5 max-w-[300px] truncate"
                              title={iter.exit_check?.reason}
                            >
                              {iter.exit_check?.reason || "--"}
                            </td>
                            <td className="px-3 py-1.5">
                              {diag ? (
                                <span
                                  className={cn(
                                    "inline-flex px-1.5 py-0.5 rounded text-[0.65rem] font-semibold",
                                    diag.passed
                                      ? "bg-green-500/10 text-green-400"
                                      : "bg-red-500/10 text-red-400",
                                  )}
                                >
                                  {diag.passed ? "PASS" : "FAIL"}
                                  {diag.root_cause && ` · ${diag.root_cause.replace(/_/g, " ")}`}
                                </span>
                              ) : (
                                <span className="text-muted-foreground/40">--</span>
                              )}
                            </td>
                            <td className="px-3 py-1.5 text-muted-foreground text-[0.7rem]">
                              {meta.length > 0 ? meta.join(", ") : "--"}
                            </td>
                          </tr>
                          {diag && isExpanded && (
                            <tr
                              key={`${iter.iteration}-diag`}
                              className="border-b border-border/50"
                            >
                              <td colSpan={6} className="px-3 py-2">
                                <div className="ml-6 space-y-1.5 text-xs">
                                  {diag.diagnosis && (
                                    <div>
                                      <span className="text-teal-400 font-semibold">
                                        Diagnosis:{" "}
                                      </span>
                                      <span className="text-foreground/80">{diag.diagnosis}</span>
                                    </div>
                                  )}
                                  {diag.prompt_rewrite_suggestion && (
                                    <div className="border-l-2 border-teal-500/30 pl-2 bg-teal-500/5 rounded-r py-1">
                                      <span className="text-teal-400/70 font-semibold text-[0.7rem]">
                                        Suggested prompt rewrite:{" "}
                                      </span>
                                      <span className="text-foreground/70 text-[0.7rem]">
                                        {diag.prompt_rewrite_suggestion}
                                      </span>
                                    </div>
                                  )}
                                  {diag.assertion_results.length > 0 && (
                                    <div>
                                      <span className="text-muted-foreground font-semibold text-[0.7rem]">
                                        Assertions ({diag.assertion_results.length}):
                                      </span>
                                      <div className="mt-0.5 flex flex-wrap gap-1">
                                        {diag.assertion_results.map((a, i) => {
                                          const passed = (a as Record<string, unknown>).passed;
                                          return (
                                            <span
                                              key={i}
                                              className={cn(
                                                "px-1.5 py-0.5 rounded text-[0.65rem] font-mono",
                                                passed
                                                  ? "bg-green-500/10 text-green-400"
                                                  : "bg-red-500/10 text-red-400",
                                              )}
                                              title={JSON.stringify(a, null, 2)}
                                            >
                                              {((a as Record<string, unknown>).type as
                                                | string
                                                | undefined) ?? `#${i + 1}`}
                                              {passed ? " ✓" : " ✗"}
                                            </span>
                                          );
                                        })}
                                      </div>
                                    </div>
                                  )}
                                </div>
                              </td>
                            </tr>
                          )}
                        </Fragment>
                      );
                    })}
                    {results.length === 0 && (
                      <tr>
                        <td colSpan={6} className="px-3 py-3 text-center text-muted-foreground">
                          No iterations yet
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
