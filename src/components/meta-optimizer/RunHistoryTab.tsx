/**
 * RunHistoryTab — Optimizer run history with trigger button.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

const logger = createLogger("MetaOptimizerRunHistory");

interface MetaOptimizerRun {
  id: string;
  optimizer_type: string;
  trigger_type: string;
  runs_analyzed: number;
  recommendations_produced: number;
  task_run_id: string | null;
  status: string;
  created_at: string;
  completed_at: string | null;
}

const TYPE_LABELS: Record<string, string> = {
  pipeline_prompt: "Pipeline Prompt",
  architecture: "Architecture",
  generation_template: "Generation Template",
};

const STATUS_COLORS: Record<string, string> = {
  running: "text-blue-400 bg-blue-900/30",
  complete: "text-green-400 bg-green-900/30",
  failed: "text-red-400 bg-red-900/30",
};

export function RunHistoryTab() {
  const [runs, setRuns] = useState<MetaOptimizerRun[]>([]);
  const [loading, setLoading] = useState(true);
  const [triggering, setTriggering] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<MetaOptimizerRun[]>("get_meta_optimizer_runs");
      setRuns(result);
    } catch (e) {
      console.error("Failed to load optimizer runs:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleTrigger = async (optimizerType: string) => {
    try {
      setTriggering(optimizerType);
      const taskRunId = await invoke<string>("trigger_meta_optimizer", { optimizerType });
      logger.info("Triggered optimizer, task run:", taskRunId);
      // Reload after a short delay to show the new run
      setTimeout(load, 1000);
    } catch (e) {
      console.error("Failed to trigger optimizer:", e);
    } finally {
      setTriggering(null);
    }
  };

  return (
    <div className="p-4 space-y-4">
      {/* Manual trigger buttons */}
      <div className="flex items-center gap-3">
        <span className="text-sm text-zinc-400">Manual Trigger:</span>
        {["pipeline_prompt", "architecture", "generation_template"].map((type) => (
          <button
            key={type}
            onClick={() => handleTrigger(type)}
            disabled={triggering !== null}
            className="px-3 py-1.5 text-xs bg-amber-800/50 text-amber-200 rounded hover:bg-amber-700/50 disabled:opacity-50"
          >
            {triggering === type ? "Launching..." : TYPE_LABELS[type] || type}
          </button>
        ))}
        <button
          onClick={load}
          className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1 ml-auto"
        >
          Refresh
        </button>
      </div>

      {/* Run history table */}
      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : runs.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No optimizer runs yet. Trigger one manually or wait for the threshold-based trigger.
        </div>
      ) : (
        <table className="w-full text-sm">
          <thead>
            <tr className="text-zinc-500 text-xs border-b border-zinc-800">
              <th className="text-left py-2 px-3">Type</th>
              <th className="text-left py-2 px-3">Trigger</th>
              <th className="text-left py-2 px-3">Status</th>
              <th className="text-right py-2 px-3">Runs Analyzed</th>
              <th className="text-right py-2 px-3">Recommendations</th>
              <th className="text-left py-2 px-3">Started</th>
              <th className="text-left py-2 px-3">Completed</th>
            </tr>
          </thead>
          <tbody>
            {runs.map((run) => (
              <tr key={run.id} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
                <td className="py-2 px-3 text-zinc-200">
                  {TYPE_LABELS[run.optimizer_type] || run.optimizer_type}
                </td>
                <td className="py-2 px-3 text-zinc-400">{run.trigger_type}</td>
                <td className="py-2 px-3">
                  <span
                    className={`text-xs px-2 py-0.5 rounded ${STATUS_COLORS[run.status] || ""}`}
                  >
                    {run.status}
                  </span>
                </td>
                <td className="py-2 px-3 text-right text-zinc-300">{run.runs_analyzed}</td>
                <td className="py-2 px-3 text-right text-zinc-300">
                  {run.recommendations_produced}
                </td>
                <td className="py-2 px-3 text-zinc-500 text-xs">
                  {new Date(run.created_at).toLocaleString()}
                </td>
                <td className="py-2 px-3 text-zinc-500 text-xs">
                  {run.completed_at ? new Date(run.completed_at).toLocaleString() : "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
