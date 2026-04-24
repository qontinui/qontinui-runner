/**
 * SpecPartitionWizard
 *
 * Modal-like wizard flow that discovers all specs, lets the user pick a
 * partition strategy, previews the partition, generates workflows, and
 * launches a multi-loop.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cn } from "../../lib/utils";
import { Loader2, Play, ChevronRight, ChevronLeft } from "lucide-react";
import { getAllSpecs } from "../../lib/spec-registry";
import { getApiBase, tracedFetch } from "../../lib/runner-api";
import {
  buildMultiRunnerSpecWorkflow,
  type RunnerTarget,
  type MultiRunnerSpecWorkflowResult,
} from "../../lib/workflow-builder";
import { type PartitionStrategy } from "../../lib/workflow-builder";
import type { DiscoveredSpec } from "../../lib/spec-prompt-builder";

interface RunnerInstance {
  id: string;
  name: string;
  port: number;
  running: boolean;
  pid: number | null;
  api_ready: boolean;
}

interface SpecPartitionWizardProps {
  onClose: () => void;
  onLaunched: () => void;
}

const inputCls =
  "px-2 py-1 text-xs bg-muted border border-border rounded text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary";
const labelCls = "text-[0.78rem] text-muted-foreground";

type WizardStep = "select-runners" | "configure" | "preview" | "launching";

export function SpecPartitionWizard({ onClose, onLaunched }: SpecPartitionWizardProps) {
  const [step, setStep] = useState<WizardStep>("select-runners");
  const [error, setError] = useState<string | null>(null);

  // Data
  const [specs, setSpecs] = useState<DiscoveredSpec[]>([]);
  const [runnerInstances, setRunnerInstances] = useState<RunnerInstance[]>([]);
  const [selectedRunners, setSelectedRunners] = useState<Set<string>>(new Set());

  // Config
  const [strategy, setStrategy] = useState<PartitionStrategy>("balanced");
  const [maxIter, setMaxIter] = useState(5);
  const [specMaxIter, setSpecMaxIter] = useState(3);
  const [between, setBetween] = useState("restart_on_signal");
  const [stopAllOnError, setStopAllOnError] = useState(false);

  // Preview
  const [result, setResult] = useState<MultiRunnerSpecWorkflowResult | null>(null);

  // Load data on mount. Both setState calls are deferred into a microtask
  // so the effect body itself doesn't synchronously trigger state updates
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (cancelled) return;
      setSpecs(getAllSpecs());
      invoke<RunnerInstance[]>("get_runner_instances")
        .then((instances) => {
          if (!cancelled) setRunnerInstances(instances);
        })
        .catch(() => {});
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const runningRunners = runnerInstances.filter((r) => r.running);

  const toggleRunner = (id: string) => {
    setSelectedRunners((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const buildBetween = useCallback(() => {
    if (between === "restart_on_signal")
      return { type: "restart_on_signal" as const, rebuild: true };
    if (between === "restart_runner") return { type: "restart_runner" as const, rebuild: true };
    if (between === "wait_healthy") return { type: "wait_healthy" as const };
    return { type: "none" as const };
  }, [between]);

  // Generate preview when moving to preview step
  const generatePreview = useCallback(() => {
    const runners: RunnerTarget[] = runnerInstances
      .filter((r) => selectedRunners.has(r.id))
      .map((r) => ({ runnerId: r.id, port: r.port, name: r.name }));

    // Build the full result
    const buildResult = buildMultiRunnerSpecWorkflow({
      specs,
      runners,
      partitionStrategy: strategy,
      loopSettings: {
        maxIterations: maxIter,
        betweenIterations: buildBetween(),
      },
      specMaxIterations: specMaxIter,
      stopAllOnError,
    });
    setResult(buildResult);
  }, [
    specs,
    runnerInstances,
    selectedRunners,
    strategy,
    maxIter,
    specMaxIter,
    stopAllOnError,
    buildBetween,
  ]);

  // Launch the multi-loop
  const handleLaunch = async () => {
    if (!result) return;
    setStep("launching");
    setError(null);

    try {
      // Save each generated workflow via HTTP API
      for (const { workflow } of result.workflows) {
        const resp = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(workflow),
        });
        if (!resp.ok) {
          throw new Error(`Failed to save workflow: ${resp.statusText}`);
        }
      }

      // Start the multi-loop
      await invoke("start_multi_orchestration_loop", { config: result.multiLoopConfig });
      onLaunched();
    } catch (e) {
      setError(String(e));
      setStep("preview");
    }
  };

  return (
    <div className="border border-border rounded p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold">Spec Partition Wizard</h4>
        <button onClick={onClose} className="text-xs text-muted-foreground hover:text-foreground">
          Cancel
        </button>
      </div>

      {/* Step indicator */}
      <div className="flex items-center gap-1 text-[0.65rem] text-muted-foreground">
        {(["select-runners", "configure", "preview"] as WizardStep[]).map((s, i) => (
          <span key={s} className="flex items-center gap-1">
            {i > 0 && <ChevronRight className="w-3 h-3" />}
            <span
              className={cn(
                "px-1.5 py-0.5 rounded",
                step === s ? "bg-primary/15 text-primary font-semibold" : "",
              )}
            >
              {s === "select-runners" ? "Runners" : s === "configure" ? "Configure" : "Preview"}
            </span>
          </span>
        ))}
      </div>

      {error && (
        <div className="border-l-2 border-red-500 bg-red-500/10 px-3 py-1.5 text-xs text-red-400 rounded-r">
          {error}
        </div>
      )}

      {/* Step 1: Select runners */}
      {step === "select-runners" && (
        <div className="space-y-2">
          <p className="text-[0.75rem] text-muted-foreground">
            {specs.length} specs discovered. Select target runners to distribute them across.
          </p>
          {runningRunners.length === 0 ? (
            <p className="text-[0.75rem] text-muted-foreground italic">
              No running runner instances found. Start additional runners first.
            </p>
          ) : (
            <div className="space-y-1">
              {runningRunners.map((r) => (
                <label
                  key={r.id}
                  className="flex items-center gap-2 px-2 py-1 rounded hover:bg-muted/50 cursor-pointer text-xs"
                >
                  <input
                    type="checkbox"
                    checked={selectedRunners.has(r.id)}
                    onChange={() => toggleRunner(r.id)}
                    className="accent-primary"
                  />
                  <span className="font-medium">{r.name}</span>
                  <span className="text-muted-foreground">:{r.port}</span>
                  {r.api_ready && <span className="text-green-400 text-[0.6rem]">ready</span>}
                </label>
              ))}
            </div>
          )}
          <div className="flex justify-end">
            <button
              onClick={() => setStep("configure")}
              disabled={selectedRunners.size === 0}
              className="px-3 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25 disabled:opacity-40 flex items-center gap-1"
            >
              Next <ChevronRight className="w-3 h-3" />
            </button>
          </div>
        </div>
      )}

      {/* Step 2: Configure */}
      {step === "configure" && (
        <div className="space-y-3">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className={labelCls}>Partition Strategy</label>
              <select
                aria-label="Partition Strategy"
                value={strategy}
                onChange={(e) => setStrategy(e.target.value as PartitionStrategy)}
                className={cn(inputCls, "w-full mt-0.5")}
              >
                <option value="balanced">Balanced (by assertion count)</option>
                <option value="round-robin">Round Robin</option>
                <option value="by-app">By Application</option>
                <option value="by-tag">By Tag</option>
              </select>
            </div>
            <div>
              <label className={labelCls}>Loop Max Iterations</label>
              <input
                type="number"
                aria-label="Loop Max Iterations"
                min={1}
                max={50}
                value={maxIter}
                onChange={(e) => setMaxIter(parseInt(e.target.value) || 5)}
                className={cn(inputCls, "w-full mt-0.5")}
              />
            </div>
            <div>
              <label className={labelCls}>Spec Workflow Iterations</label>
              <input
                type="number"
                aria-label="Spec Workflow Iterations"
                min={1}
                max={10}
                value={specMaxIter}
                onChange={(e) => setSpecMaxIter(parseInt(e.target.value) || 3)}
                className={cn(inputCls, "w-full mt-0.5")}
              />
            </div>
            <div>
              <label className={labelCls}>Between Iterations</label>
              <select
                aria-label="Between Iterations"
                value={between}
                onChange={(e) => setBetween(e.target.value)}
                className={cn(inputCls, "w-full mt-0.5")}
              >
                <option value="restart_on_signal">Restart on signal (rebuild)</option>
                <option value="restart_runner">Always restart (rebuild)</option>
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
          <div className="flex justify-between">
            <button
              onClick={() => setStep("select-runners")}
              className="px-3 py-1 text-xs font-medium rounded bg-muted text-muted-foreground hover:text-foreground flex items-center gap-1"
            >
              <ChevronLeft className="w-3 h-3" /> Back
            </button>
            <button
              onClick={() => {
                generatePreview();
                setStep("preview");
              }}
              className="px-3 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25 flex items-center gap-1"
            >
              Preview <ChevronRight className="w-3 h-3" />
            </button>
          </div>
        </div>
      )}

      {/* Step 3: Preview */}
      {step === "preview" && result && (
        <div className="space-y-3">
          <p className="text-[0.75rem] text-muted-foreground">
            {result.partitions.length} partitions across {selectedRunners.size} runners.{" "}
            {specs.length} specs, {result.partitions.reduce((s, p) => s + p.assertionCount, 0)}{" "}
            total assertions.
          </p>
          <div className="border border-border rounded overflow-hidden">
            <div className="grid grid-cols-[1fr_auto_auto_auto] gap-x-3 px-3 py-1 border-b border-border text-[0.65rem] font-semibold text-muted-foreground uppercase tracking-wider">
              <span>Partition</span>
              <span>Runner</span>
              <span>Specs</span>
              <span>Assertions</span>
            </div>
            {result.partitions.map((p) => (
              <div
                key={p.loopId}
                className="grid grid-cols-[1fr_auto_auto_auto] gap-x-3 px-3 py-1.5 border-b border-border/50 text-xs"
              >
                <span className="font-medium truncate">{p.label}</span>
                <span className="text-muted-foreground">{p.runner.name}</span>
                <span className="font-mono text-right">{p.specCount}</span>
                <span className="font-mono text-right">{p.assertionCount}</span>
              </div>
            ))}
          </div>
          <div className="flex justify-between">
            <button
              onClick={() => setStep("configure")}
              className="px-3 py-1 text-xs font-medium rounded bg-muted text-muted-foreground hover:text-foreground flex items-center gap-1"
            >
              <ChevronLeft className="w-3 h-3" /> Back
            </button>
            <button
              onClick={handleLaunch}
              className="px-3 py-1 text-xs font-medium rounded bg-primary/15 text-primary hover:bg-primary/25 flex items-center gap-1"
            >
              <Play className="w-3 h-3" /> Launch Multi-Loop
            </button>
          </div>
        </div>
      )}

      {/* Launching spinner */}
      {step === "launching" && (
        <div className="flex items-center justify-center py-4 text-muted-foreground text-xs">
          <Loader2 className="w-4 h-4 animate-spin mr-2" />
          Saving workflows and starting loops...
        </div>
      )}
    </div>
  );
}
