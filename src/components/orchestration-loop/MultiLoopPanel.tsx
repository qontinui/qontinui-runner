import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cn } from "../../lib/utils";
import { Play, Square, Loader2, Zap, AlertTriangle, FileSearch } from "lucide-react";
import { SpecPartitionWizard } from "./SpecPartitionWizard";
import { MultiLoopResults } from "./MultiLoopResults";

// --- Types matching the Rust multi-loop backend ---

interface MultiLoopConfig {
  loops: MultiLoopEntry[];
  stop_all_on_error: boolean;
}

interface MultiLoopEntry {
  loop_id: string;
  label: string | null;
  config: {
    target_runner_port: number | null;
    target_runner_id: string | null;
    supervisor_port: number;
    workflow_id: string;
    /** `null` means unlimited iterations. */
    max_iterations: number | null;
    exit_strategy: { type: string };
    between_iterations: { type: string; rebuild?: boolean };
    retry_on_failure: boolean;
    wait_for_fixer: boolean;
    pipeline: null;
  };
}

interface MultiLoopStatus {
  loops: LoopInstanceStatus[];
  all_complete: boolean;
  any_error: boolean;
  stop_all_on_error: boolean;
}

interface LoopInstanceStatus {
  loop_id: string;
  label: string | null;
  status: {
    running: boolean;
    phase: string;
    current_iteration: number;
    /** `null` = unlimited cap. */
    max_iterations: number | null;
    workflow_id: string;
    target_runner_port: number;
    target_runner_id: string | null;
    is_pipeline: boolean;
    started_at: string | null;
    error: string | null;
    iteration_results: IterationResult[];
  };
}

interface IterationResult {
  iteration: number;
  started_at: string;
  completed_at: string;
  task_run_id: string;
  fix_count: number | null;
  exit_check: { should_exit: boolean; reason: string };
  reflection_task_run_id: string | null;
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

interface RunnerInstance {
  id: string;
  name: string;
  port: number;
  running: boolean;
  pid: number | null;
  api_ready: boolean;
}

interface SavedWorkflow {
  id: string;
  name: string;
}

// --- Phase styling ---

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
  diagnosing: "bg-teal-500/10",
  reflecting: "bg-purple-500/10",
};

function PhaseBadge({ phase }: { phase: string }) {
  const color = PHASE_COLORS[phase] || "text-muted-foreground";
  const bg = PHASE_BG[phase] || "bg-muted";
  return (
    <span
      className={cn(
        "inline-flex px-2 py-0.5 rounded-full text-[0.65rem] font-semibold font-mono",
        color,
        bg,
      )}
    >
      {phase.replace(/_/g, " ")}
    </span>
  );
}

// --- Shared styles ---

const inputCls =
  "px-2 py-1 text-xs bg-muted border border-border rounded text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary";
const labelCls = "text-[0.78rem] text-muted-foreground";

// --- Types for loop assignment ---

interface LoopAssignment {
  runnerId: string;
  runnerName: string;
  port: number;
  workflowId: string;
  label: string;
}

export function MultiLoopPanel() {
  const [multiStatus, setMultiStatus] = useState<MultiLoopStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [runnerInstances, setRunnerInstances] = useState<RunnerInstance[]>([]);
  const [workflows, setWorkflows] = useState<SavedWorkflow[]>([]);

  // Spec partition wizard
  const [showWizard, setShowWizard] = useState(false);

  // Form: assignment of runners to workflows
  const [assignments, setAssignments] = useState<LoopAssignment[]>([]);
  // null = unlimited (loop exits on success/stop, not iteration count).
  const [maxIter, setMaxIter] = useState<number | null>(null);
  const [exitStrategy, setExitStrategy] = useState("reflection");
  const [between, setBetween] = useState("restart_on_signal");
  const [stopAllOnError, setStopAllOnError] = useState(false);
  const [supervisorPort, setSupervisorPort] = useState("9875");

  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // --- Data fetching ---

  const fetchMultiStatus = useCallback(async () => {
    try {
      const s = await invoke<MultiLoopStatus>("get_multi_orchestration_loop_status");
      setMultiStatus(s);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadRunnerInstances = useCallback(async () => {
    try {
      const instances = await invoke<RunnerInstance[]>("get_runner_instances");
      setRunnerInstances(instances);
    } catch {
      /* not available */
    }
  }, []);

  const loadWorkflows = useCallback(async () => {
    try {
      const wfs = await invoke<SavedWorkflow[]>("list_unified_workflows");
      setWorkflows(wfs);
    } catch {
      /* not available */
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    fetchMultiStatus();
    loadRunnerInstances();
    loadWorkflows();
    pollRef.current = setInterval(fetchMultiStatus, 3000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [fetchMultiStatus, loadRunnerInstances, loadWorkflows]);

  // --- Assignment management ---

  const availableRunners = runnerInstances.filter(
    (r) => r.running && !assignments.some((a) => a.runnerId === r.id),
  );

  const addAssignment = (runner: RunnerInstance) => {
    setAssignments((prev) => [
      ...prev,
      {
        runnerId: runner.id,
        runnerName: runner.name,
        port: runner.port,
        workflowId: "",
        label: runner.name,
      },
    ]);
  };

  const removeAssignment = (idx: number) => {
    setAssignments((prev) => prev.filter((_, i) => i !== idx));
  };

  const updateAssignment = (idx: number, field: keyof LoopAssignment, value: string | number) => {
    setAssignments((prev) => prev.map((a, i) => (i === idx ? { ...a, [field]: value } : a)));
  };

  // --- Start / Stop ---

  const buildBetween = (): { type: string; rebuild?: boolean } => {
    if (between === "restart_on_signal") return { type: "restart_on_signal", rebuild: true };
    if (between === "restart_on_signal_no_rebuild")
      return { type: "restart_on_signal", rebuild: false };
    if (between === "restart_runner") return { type: "restart_runner", rebuild: true };
    if (between === "restart_runner_no_rebuild") return { type: "restart_runner", rebuild: false };
    if (between === "wait_healthy") return { type: "wait_healthy" };
    return { type: "none" };
  };

  const handleStartMulti = async () => {
    setError(null);

    const missing = assignments.filter((a) => !a.workflowId);
    if (missing.length > 0) {
      setError(`Missing workflow ID for: ${missing.map((m) => m.runnerName).join(", ")}`);
      return;
    }
    if (assignments.length === 0) {
      setError("Add at least one runner assignment");
      return;
    }

    const config: MultiLoopConfig = {
      stop_all_on_error: stopAllOnError,
      loops: assignments.map((a) => ({
        loop_id: `multi-${a.runnerId}`,
        label: a.label || a.runnerName,
        config: {
          target_runner_port: a.port,
          target_runner_id: a.runnerId,
          supervisor_port: parseInt(supervisorPort) || 9875,
          workflow_id: a.workflowId,
          max_iterations: maxIter,
          exit_strategy: { type: exitStrategy },
          between_iterations: buildBetween(),
          retry_on_failure: false,
          wait_for_fixer: true,
          pipeline: null,
        },
      })),
    };

    try {
      await invoke("start_multi_orchestration_loop", { config });
    } catch (e) {
      setError(String(e));
    }
  };

  const handleStopAll = async () => {
    try {
      await invoke("stop_all_orchestration_loops");
    } catch (e) {
      setError(String(e));
    }
  };

  const handleStopOne = async (loopId: string) => {
    try {
      await invoke("stop_orchestration_loop_by_id", { loopId });
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSignalOne = async (loopId: string) => {
    try {
      await invoke("signal_orchestration_restart_by_id", { loopId });
    } catch (e) {
      setError(String(e));
    }
  };

  // --- Derived state ---

  const activeLoops = multiStatus?.loops.filter((l) => l.status.running) ?? [];
  const anyRunning = activeLoops.length > 0;
  const allLoops = multiStatus?.loops ?? [];
  // Only show status grid if there are non-idle loops
  const visibleLoops = allLoops.filter((l) => l.status.phase !== "idle" || l.status.running);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        <Loader2 className="w-4 h-4 animate-spin mr-2" />
        Loading...
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-semibold">Multi-Runner Loops</h3>
          {anyRunning && (
            <span className="text-[0.7rem] font-mono text-blue-400">
              {activeLoops.length} running
            </span>
          )}
          {multiStatus?.all_complete && visibleLoops.length > 0 && (
            <span className="text-[0.7rem] font-mono text-green-400">all complete</span>
          )}
          {multiStatus?.any_error && (
            <span className="text-[0.7rem] font-mono text-red-400">
              <AlertTriangle className="w-3 h-3 inline mr-0.5" />
              error
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          {anyRunning ? (
            <button
              onClick={handleStopAll}
              className="px-2.5 py-1 text-xs font-medium rounded bg-red-500/10 text-red-400 hover:bg-red-500/20"
            >
              <Square className="w-3 h-3 inline mr-1" />
              Stop All
            </button>
          ) : (
            <>
              <button
                onClick={() => setShowWizard(!showWizard)}
                className={cn(
                  "px-2.5 py-1 text-xs font-medium rounded",
                  showWizard
                    ? "bg-primary/15 text-primary"
                    : "bg-muted text-muted-foreground hover:text-foreground",
                )}
              >
                <FileSearch className="w-3 h-3 inline mr-1" />
                From Specs
              </button>
              <button
                onClick={handleStartMulti}
                disabled={assignments.length === 0}
                className="px-2.5 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25 disabled:opacity-40"
              >
                <Play className="w-3 h-3 inline mr-1" />
                Start All
              </button>
            </>
          )}
        </div>
      </div>

      {/* Error */}
      {error && (
        <div className="border-l-2 border-red-500 bg-red-500/10 px-3 py-1.5 text-xs text-red-400 rounded-r">
          {error}
        </div>
      )}

      {/* Status grid — visible loops */}
      {visibleLoops.length > 0 && (
        <div className="border border-border rounded overflow-hidden">
          <div className="grid grid-cols-[1fr_auto_auto_auto_auto] gap-x-3 px-3 py-1.5 border-b border-border text-[0.7rem] font-semibold text-muted-foreground uppercase tracking-wider">
            <span>Loop</span>
            <span>Phase</span>
            <span>Progress</span>
            <span>Fixes</span>
            <span />
          </div>
          {visibleLoops.map((loop) => {
            const s = loop.status;
            // Progress bar is indeterminate for unlimited (null) runs — no
            // ceiling to divide by.
            const pct =
              s.max_iterations != null && s.max_iterations > 0
                ? Math.round((s.current_iteration / s.max_iterations) * 100)
                : 0;
            const totalFixes = s.iteration_results.reduce((sum, r) => sum + (r.fix_count ?? 0), 0);
            return (
              <div
                key={loop.loop_id}
                className="grid grid-cols-[1fr_auto_auto_auto_auto] gap-x-3 px-3 py-1.5 border-b border-border/50 items-center text-xs"
              >
                <div className="truncate">
                  <span className="font-medium">{loop.label || loop.loop_id}</span>
                  <span className="ml-1.5 text-muted-foreground text-[0.65rem]">
                    :{s.target_runner_port}
                  </span>
                </div>
                <PhaseBadge phase={s.phase} />
                <div className="flex items-center gap-1.5 min-w-[80px]">
                  <div className="flex-1 bg-muted rounded h-1">
                    <div
                      className={cn(
                        "h-1 rounded transition-all",
                        s.phase === "complete" ? "bg-green-500" : "bg-primary",
                      )}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <span className="text-[0.65rem] font-mono text-muted-foreground w-8 text-right">
                    {s.current_iteration}/{s.max_iterations}
                  </span>
                </div>
                <span className="text-[0.7rem] font-mono text-muted-foreground w-6 text-right">
                  {totalFixes > 0 ? totalFixes : "-"}
                </span>
                <div className="flex items-center gap-1">
                  {s.running && (
                    <>
                      <button
                        onClick={() => handleSignalOne(loop.loop_id)}
                        className="p-0.5 rounded hover:bg-yellow-500/20 text-muted-foreground hover:text-yellow-400"
                        title="Signal restart"
                      >
                        <Zap className="w-3 h-3" />
                      </button>
                      <button
                        onClick={() => handleStopOne(loop.loop_id)}
                        className="p-0.5 rounded hover:bg-red-500/20 text-muted-foreground hover:text-red-400"
                        title="Stop this loop"
                      >
                        <Square className="w-3 h-3" />
                      </button>
                    </>
                  )}
                  {s.error && (
                    <span
                      className="text-red-400 text-[0.65rem] truncate max-w-[120px]"
                      title={s.error}
                    >
                      {s.error}
                    </span>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Aggregated results — show when all loops complete */}
      {!anyRunning &&
        multiStatus &&
        (multiStatus.all_complete || multiStatus.any_error) &&
        visibleLoops.length > 0 && (
          <MultiLoopResults
            loops={visibleLoops.map((l) => ({
              loop_id: l.loop_id,
              label: l.label,
              running: l.status.running,
              phase: l.status.phase,
              current_iteration: l.status.current_iteration,
              max_iterations: l.status.max_iterations,
              workflow_id: l.status.workflow_id,
              target_runner_port: l.status.target_runner_port,
              target_runner_id: l.status.target_runner_id ?? null,
              error: l.status.error,
              iteration_results: l.status.iteration_results,
            }))}
          />
        )}

      {/* Spec partition wizard */}
      {showWizard && !anyRunning && (
        <SpecPartitionWizard
          onClose={() => setShowWizard(false)}
          onLaunched={() => {
            setShowWizard(false);
            fetchMultiStatus();
          }}
        />
      )}

      {/* Config form — only when not running */}
      {!anyRunning && !showWizard && (
        <div className="border border-border rounded p-3 space-y-3">
          {/* Runner assignments */}
          <div>
            <label className={labelCls}>Runner → Workflow Assignments</label>
            <div className="mt-1 space-y-1.5">
              {assignments.map((a, idx) => (
                <div key={a.runnerId} className="flex items-center gap-2">
                  <span className="text-xs font-mono min-w-[100px] truncate" title={a.runnerName}>
                    {a.runnerName}
                    <span className="text-muted-foreground">:{a.port}</span>
                  </span>
                  <select
                    value={a.workflowId}
                    onChange={(e) => updateAssignment(idx, "workflowId", e.target.value)}
                    className={cn(inputCls, "flex-1")}
                  >
                    <option value="">Select workflow...</option>
                    {workflows.map((wf) => (
                      <option key={wf.id} value={wf.id}>
                        {wf.name}
                      </option>
                    ))}
                  </select>
                  <input
                    type="text"
                    value={a.label}
                    onChange={(e) => updateAssignment(idx, "label", e.target.value)}
                    className={cn(inputCls, "w-[100px]")}
                    placeholder="Label"
                  />
                  <button
                    onClick={() => removeAssignment(idx)}
                    className="text-muted-foreground hover:text-red-400 text-xs"
                  >
                    ✕
                  </button>
                </div>
              ))}
              {availableRunners.length > 0 && (
                <div className="flex items-center gap-2">
                  <select
                    value=""
                    onChange={(e) => {
                      const runner = runnerInstances.find((r) => r.id === e.target.value);
                      if (runner) addAssignment(runner);
                    }}
                    className={cn(inputCls, "flex-1")}
                  >
                    <option value="">+ Add runner...</option>
                    {availableRunners.map((r) => (
                      <option key={r.id} value={r.id}>
                        {r.name} (:{r.port})
                      </option>
                    ))}
                  </select>
                </div>
              )}
              {assignments.length === 0 && availableRunners.length === 0 && (
                <p className="text-[0.7rem] text-muted-foreground">
                  No running runner instances found. Start additional runners first.
                </p>
              )}
            </div>
          </div>

          {/* Shared settings */}
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className={labelCls}>
                Max Iterations <span className="text-muted-foreground/60">(blank = unlimited)</span>
              </label>
              <input
                type="number"
                min={1}
                max={50}
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
                placeholder="Unlimited"
                className={cn(inputCls, "w-full mt-0.5")}
              />
            </div>
            <div>
              <label className={labelCls}>Supervisor Port</label>
              <input
                type="text"
                value={supervisorPort}
                onChange={(e) => setSupervisorPort(e.target.value)}
                className={cn(inputCls, "w-full mt-0.5")}
              />
            </div>
            <div>
              <label className={labelCls}>Exit Strategy</label>
              <select
                value={exitStrategy}
                onChange={(e) => setExitStrategy(e.target.value)}
                className={cn(inputCls, "w-full mt-0.5")}
              >
                <option value="reflection">Reflection (0 fixes)</option>
                <option value="workflow_verification">Verification</option>
                <option value="fixed_iterations">Fixed Iterations</option>
              </select>
            </div>
            <div>
              <label className={labelCls}>Between Iterations</label>
              <select
                value={between}
                onChange={(e) => setBetween(e.target.value)}
                className={cn(inputCls, "w-full mt-0.5")}
              >
                <option value="restart_on_signal">Restart on signal (rebuild)</option>
                <option value="restart_on_signal_no_rebuild">Restart on signal (no rebuild)</option>
                <option value="restart_runner">Always restart (rebuild)</option>
                <option value="restart_runner_no_rebuild">Always restart (no rebuild)</option>
                <option value="wait_healthy">Wait healthy</option>
                <option value="none">None</option>
              </select>
            </div>
          </div>

          <label className="flex items-center gap-2 text-xs cursor-pointer">
            <input
              type="checkbox"
              checked={stopAllOnError}
              onChange={(e) => setStopAllOnError(e.target.checked)}
              className="accent-primary"
            />
            Stop all loops if any loop errors
          </label>
        </div>
      )}
    </div>
  );
}
