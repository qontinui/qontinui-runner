/**
 * LiveTestPanel — Interactive state machine testing controls.
 *
 * Provides controls to manually execute transitions, navigate to states,
 * and view an activity log. Requires the Python executor to be running.
 */

import { useState, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Play,
  RefreshCw,
  Trash2,
  AlertCircle,
  Loader2,
  Zap,
  ArrowRight,
  ChevronRight,
} from "lucide-react";
import type { StateMachineState, StateMachineTransition } from "@qontinui/shared-types";

// =============================================================================
// Types
// =============================================================================

interface LiveTestPanelProps {
  states: StateMachineState[];
  transitions: StateMachineTransition[];
}

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

interface LogEntry {
  id: number;
  timestamp: string;
  type: "transition" | "navigate" | "info" | "error";
  message: string;
}

// =============================================================================
// Helpers
// =============================================================================

let logCounter = 0;

const LOG_COLORS: Record<LogEntry["type"], string> = {
  transition: "text-blue-400",
  navigate: "text-cyan-400",
  info: "text-text-secondary",
  error: "text-red-400",
};

// =============================================================================
// Component
// =============================================================================

export function LiveTestPanel({ states, transitions }: LiveTestPanelProps) {
  const [selectedTransitionId, setSelectedTransitionId] = useState<string | null>(null);
  const [selectedStateId, setSelectedStateId] = useState<string | null>(null);
  const [executing, setExecuting] = useState(false);
  const [navigating, setNavigating] = useState(false);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [transitionFilter, setTransitionFilter] = useState("");

  const addLog = useCallback((type: LogEntry["type"], message: string) => {
    const entry: LogEntry = {
      id: ++logCounter,
      timestamp: new Date().toLocaleTimeString(),
      type,
      message,
    };
    setLog((prev) => [entry, ...prev].slice(0, 200));
  }, []);

  const stateName = useCallback(
    (stateId: string) => states.find((s) => s.state_id === stateId)?.name ?? stateId,
    [states],
  );

  // Filtered transitions for the list
  const filteredTransitions = useMemo(() => {
    if (!transitionFilter.trim()) return transitions;
    const q = transitionFilter.toLowerCase();
    return transitions.filter(
      (t) =>
        t.name.toLowerCase().includes(q) ||
        t.from_states.some((id) => stateName(id).toLowerCase().includes(q)) ||
        t.activate_states.some((id) => stateName(id).toLowerCase().includes(q)),
    );
  }, [transitions, transitionFilter, stateName]);

  const selectedTrans = useMemo(
    () => transitions.find((t) => t.transition_id === selectedTransitionId) ?? null,
    [transitions, selectedTransitionId],
  );

  // ---------- Handlers ----------

  const handleExecuteTransition = useCallback(async () => {
    if (!selectedTransitionId || !selectedTrans) return;

    setExecuting(true);
    setError(null);
    addLog(
      "transition",
      `Executing: ${selectedTrans.name} (${selectedTrans.from_states.map(stateName).join(", ")} \u2192 ${selectedTrans.activate_states.map(stateName).join(", ")})`,
    );

    try {
      const response = await invoke<CommandResponse>("execute_transition", {
        transitionId: selectedTransitionId,
      });
      if (response.success) {
        addLog("info", `\u2713 ${response.message || "Transition executed"}`);
      } else {
        addLog("error", `\u2717 ${response.message || "Transition failed"}`);
      }
    } catch (err) {
      const msg = String(err);
      setError(msg);
      addLog("error", `\u2717 ${msg}`);
    } finally {
      setExecuting(false);
    }
  }, [selectedTransitionId, selectedTrans, addLog, stateName]);

  const handleNavigateToState = useCallback(async () => {
    if (!selectedStateId) return;
    const state = states.find((s) => s.state_id === selectedStateId);
    if (!state) return;

    setNavigating(true);
    setError(null);
    addLog("navigate", `Navigating to: ${state.name}`);

    try {
      const response = await invoke<CommandResponse>("navigate_to_state", {
        stateId: selectedStateId,
      });
      if (response.success) {
        addLog("info", `\u2713 ${response.message || "Navigation sent"}`);
      } else {
        addLog("error", `\u2717 ${response.message || "Navigation failed"}`);
      }
    } catch (err) {
      const msg = String(err);
      setError(msg);
      addLog("error", `\u2717 ${msg}`);
    } finally {
      setNavigating(false);
    }
  }, [selectedStateId, states, addLog]);

  const handleRefreshActionLog = useCallback(async () => {
    try {
      const response = await invoke<CommandResponse>("get_action_log_view");
      if (response.success && response.data) {
        addLog("info", `Action log refreshed: ${JSON.stringify(response.data).slice(0, 200)}`);
      }
    } catch (err) {
      addLog("error", `Failed to get action log: ${err}`);
    }
  }, [addLog]);

  const handleClearLog = useCallback(() => {
    setLog([]);
    logCounter = 0;
  }, []);

  // ---------- Quick-execute a transition by clicking in the list ----------

  const handleQuickExecute = useCallback(
    async (transitionId: string) => {
      const trans = transitions.find((t) => t.transition_id === transitionId);
      if (!trans) return;

      setExecuting(true);
      setError(null);
      addLog(
        "transition",
        `Executing: ${trans.name} (${trans.from_states.map(stateName).join(", ")} \u2192 ${trans.activate_states.map(stateName).join(", ")})`,
      );

      try {
        const response = await invoke<CommandResponse>("execute_transition", {
          transitionId,
        });
        if (response.success) {
          addLog("info", `\u2713 ${response.message || "Transition executed"}`);
        } else {
          addLog("error", `\u2717 ${response.message || "Transition failed"}`);
        }
      } catch (err) {
        const msg = String(err);
        setError(msg);
        addLog("error", `\u2717 ${msg}`);
      } finally {
        setExecuting(false);
      }
    },
    [transitions, addLog, stateName],
  );

  // ---------- Render ----------

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="shrink-0 p-4 pb-2 flex items-center justify-between">
        <h3 className="text-sm font-semibold text-text-primary">Live Test</h3>
        <span className="flex items-center gap-1.5 text-[10px] text-text-muted">
          <Zap className="w-3 h-3" />
          Requires running executor
        </span>
      </div>

      {/* Error banner */}
      {error && (
        <div className="shrink-0 mx-4 mb-2 flex items-start gap-2 p-2.5 text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded">
          <AlertCircle className="w-3.5 h-3.5 shrink-0 mt-0.5" />
          <div className="flex-1 min-w-0">
            <span className="break-words">{error}</span>
            <button onClick={() => setError(null)} className="ml-2 underline">
              dismiss
            </button>
          </div>
        </div>
      )}

      {/* Main content — scrollable */}
      <div className="flex-1 overflow-auto px-4 pb-4 space-y-4">
        {/* Execute transition section */}
        <div className="space-y-2">
          <label className="text-xs font-medium text-text-secondary">Execute Transition</label>

          {/* Transition search/filter */}
          <input
            type="text"
            value={transitionFilter}
            onChange={(e) => setTransitionFilter(e.target.value)}
            placeholder="Filter transitions..."
            className="w-full px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary placeholder:text-text-muted"
          />

          {/* Transition list */}
          <div className="max-h-[200px] overflow-auto border border-border-secondary rounded bg-bg-tertiary divide-y divide-border-secondary">
            {filteredTransitions.length === 0 ? (
              <p className="text-xs text-text-muted italic text-center py-4">
                {transitionFilter ? "No matching transitions" : "No transitions"}
              </p>
            ) : (
              filteredTransitions.map((t) => {
                const isSelected = t.transition_id === selectedTransitionId;
                return (
                  <div
                    key={t.transition_id}
                    className={`flex items-center gap-2 px-2.5 py-2 cursor-pointer transition-colors ${
                      isSelected ? "bg-brand-primary/10" : "hover:bg-bg-secondary"
                    }`}
                    onClick={() => setSelectedTransitionId(isSelected ? null : t.transition_id)}
                  >
                    <div className="flex-1 min-w-0">
                      <div className="text-xs font-medium text-text-primary truncate">{t.name}</div>
                      <div className="text-[10px] text-text-muted flex items-center gap-1">
                        <span className="text-blue-400 truncate">
                          {t.from_states.map(stateName).join(", ") || "any"}
                        </span>
                        <ChevronRight className="w-2.5 h-2.5 shrink-0" />
                        <span className="text-green-400 truncate">
                          {t.activate_states.map(stateName).join(", ")}
                        </span>
                      </div>
                    </div>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        handleQuickExecute(t.transition_id);
                      }}
                      disabled={executing}
                      className="shrink-0 p-1 text-text-muted hover:text-brand-primary hover:bg-brand-primary/10 rounded disabled:opacity-50"
                      title="Execute this transition"
                    >
                      <Play className="w-3.5 h-3.5" />
                    </button>
                  </div>
                );
              })
            )}
          </div>

          {/* Execute selected button */}
          {selectedTrans && (
            <div className="flex items-center gap-2">
              <button
                onClick={handleExecuteTransition}
                disabled={executing}
                className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-white bg-brand-primary hover:bg-brand-primary/90 rounded disabled:opacity-50"
              >
                {executing ? (
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                ) : (
                  <Play className="w-3.5 h-3.5" />
                )}
                Execute: {selectedTrans.name}
              </button>
              <span className="text-[10px] text-text-muted">
                {selectedTrans.actions.length} action
                {selectedTrans.actions.length !== 1 ? "s" : ""} · cost:{" "}
                {selectedTrans.path_cost.toFixed(1)}
              </span>
            </div>
          )}
        </div>

        {/* Navigate to state */}
        <div className="space-y-2">
          <label className="text-xs font-medium text-text-secondary">Navigate to State</label>
          <div className="flex gap-2">
            <select
              value={selectedStateId ?? ""}
              onChange={(e) => setSelectedStateId(e.target.value || null)}
              className="flex-1 px-2 py-1.5 text-sm bg-bg-tertiary border border-border-secondary rounded text-text-primary"
            >
              <option value="">Select a state...</option>
              {states.map((s) => (
                <option key={s.state_id} value={s.state_id}>
                  {s.name} ({s.element_ids.length} elements)
                </option>
              ))}
            </select>
            <button
              onClick={handleNavigateToState}
              disabled={!selectedStateId || navigating}
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-text-primary border border-border-secondary hover:border-border-primary rounded disabled:opacity-50"
            >
              {navigating ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <ArrowRight className="w-3.5 h-3.5" />
              )}
              Navigate
            </button>
          </div>
        </div>

        {/* Activity log */}
        <div className="space-y-1.5">
          <div className="flex items-center justify-between">
            <label className="text-xs font-medium text-text-secondary">Activity Log</label>
            <div className="flex items-center gap-1">
              <button
                onClick={handleRefreshActionLog}
                className="p-1 text-text-muted hover:text-text-primary hover:bg-bg-tertiary rounded"
                title="Refresh action log"
              >
                <RefreshCw className="w-3 h-3" />
              </button>
              <button
                onClick={handleClearLog}
                className="p-1 text-text-muted hover:text-text-primary hover:bg-bg-tertiary rounded"
                title="Clear log"
              >
                <Trash2 className="w-3 h-3" />
              </button>
            </div>
          </div>

          <div className="max-h-[300px] overflow-auto border border-border-secondary rounded bg-bg-tertiary">
            {log.length === 0 ? (
              <p className="text-xs text-text-muted italic text-center py-6">
                No activity yet. Execute a transition or navigate to a state.
              </p>
            ) : (
              <div className="divide-y divide-border-secondary">
                {log.map((entry) => (
                  <div key={entry.id} className="px-2.5 py-1.5 text-[11px] flex items-start gap-2">
                    <span className="text-text-muted shrink-0 font-mono">{entry.timestamp}</span>
                    <span className={LOG_COLORS[entry.type]}>{entry.message}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
