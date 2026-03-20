/**
 * ExplorationPanel.tsx
 *
 * Main container for State Explorer.
 * Allows configuration and execution of exploration tasks.
 */

import { useState, useCallback, useMemo } from "react";
import {
  Play,
  Eye,
  Loader2,
  Settings2,
  FileSearch,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  Monitor,
  Camera,
  StopCircle,
} from "lucide-react";
import { useExecution } from "../../contexts/ExecutionContext";
import { useStateExplorer } from "../../hooks/useStateExplorer";
import type { ExplorationConfig, ExplorationPlan } from "../../types/state-explorer";
import { getDefaultExplorationConfig } from "../../types/state-explorer";

interface ExplorationPanelProps {
  /** Callback when exploration completes to show report */
  onViewReport?: (runId: string) => void;
}

export function ExplorationPanel({ onViewReport }: ExplorationPanelProps) {
  const { config: loadedConfig, selectedMonitors } = useExecution();
  const {
    strategies,
    strategiesLoading,
    previewPlan,
    previewLoading,
    currentPlan,
    startExploration,
    explorationRunning,
    error,
    clearError,
  } = useStateExplorer();

  // Form state (configPath is derived from loaded config)
  const [_configPath, _setConfigPath] = useState<string>("");
  const [strategy, setStrategy] = useState<string>("smoke_test");
  const [maxStates, setMaxStates] = useState<number>(0);
  const [captureScreenshots, setCaptureScreenshots] = useState(true);
  const [stopOnFirstFailure, setStopOnFirstFailure] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [stateDelayMs, setStateDelayMs] = useState(500);
  const [maxDurationSeconds, setMaxDurationSeconds] = useState(0);

  // Use loaded config path if available
  const effectiveConfigPath = useMemo(() => {
    if (_configPath) return _configPath;
    // Get config path from loaded config if available
    // This is a simplified approach - in reality you'd need to track the path
    return loadedConfig?.name ? `<current loaded config: ${loadedConfig.name}>` : "";
  }, [_configPath, loadedConfig]);

  // Build config object
  const buildConfig = useCallback((): ExplorationConfig => {
    const base = getDefaultExplorationConfig(effectiveConfigPath);
    return {
      ...base,
      strategy: strategy as ExplorationConfig["strategy"],
      max_states: maxStates,
      max_duration_seconds: maxDurationSeconds,
      capture_screenshots: captureScreenshots,
      stop_on_first_failure: stopOnFirstFailure,
      state_delay_ms: stateDelayMs,
      monitor_index: selectedMonitors.length > 0 ? selectedMonitors[0] : undefined,
    };
  }, [
    effectiveConfigPath,
    strategy,
    maxStates,
    maxDurationSeconds,
    captureScreenshots,
    stopOnFirstFailure,
    stateDelayMs,
    selectedMonitors,
  ]);

  // Handle preview
  const handlePreview = useCallback(async () => {
    clearError();
    const config = buildConfig();
    await previewPlan(config);
  }, [buildConfig, previewPlan, clearError]);

  // Handle start exploration
  const handleStart = useCallback(async () => {
    clearError();
    const config = buildConfig();
    const result = await startExploration(config);
    if (result && onViewReport) {
      onViewReport(result.run_id);
    }
  }, [buildConfig, startExploration, onViewReport, clearError]);

  // Check if we can run exploration
  const canRunExploration = useMemo(() => {
    return loadedConfig !== null && !explorationRunning;
  }, [loadedConfig, explorationRunning]);

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex-shrink-0 p-4 border-b border-border">
        <div className="flex items-center gap-3">
          <FileSearch className="w-5 h-5 text-primary" />
          <h2 className="text-lg font-semibold">State Explorer</h2>
        </div>
        <p className="text-sm text-muted-foreground mt-1">
          Automatically explore and verify application states
        </p>
      </div>

      {/* Error Display */}
      {error && (
        <div className="mx-4 mt-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center gap-2">
          <AlertCircle className="w-4 h-4 flex-shrink-0" />
          <span className="flex-1">{error}</span>
          <button onClick={clearError} className="text-destructive/70 hover:text-destructive">
            Dismiss
          </button>
        </div>
      )}

      {/* Configuration Form */}
      <div className="flex-1 overflow-auto p-4 space-y-4">
        {/* Current Config Display */}
        <div className="p-3 bg-card rounded-lg border border-border">
          <div className="flex items-center gap-2 text-sm">
            <Settings2 className="w-4 h-4 text-muted-foreground" />
            <span className="text-muted-foreground">Current Config:</span>
            <span className="font-medium">{loadedConfig?.name || "No config loaded"}</span>
          </div>
          {!loadedConfig && (
            <p className="text-xs text-amber-500 mt-1">
              Load a configuration in the Run tab to enable exploration
            </p>
          )}
        </div>

        {/* Strategy Selection */}
        <div className="space-y-2">
          <label className="text-sm font-medium">Exploration Strategy</label>
          <select
            value={strategy}
            onChange={(e) => setStrategy(e.target.value)}
            disabled={strategiesLoading || explorationRunning}
            className="w-full px-3 py-2 bg-input border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
          >
            {strategies.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name} - {s.description}
              </option>
            ))}
          </select>
          {strategies.find((s) => s.id === strategy)?.recommended_for && (
            <p className="text-xs text-muted-foreground">
              Recommended for: {strategies.find((s) => s.id === strategy)?.recommended_for}
            </p>
          )}
        </div>

        {/* Basic Options */}
        <div className="grid grid-cols-2 gap-4">
          {/* Max States */}
          <div className="space-y-2">
            <label className="text-sm font-medium">Max States</label>
            <input
              type="number"
              value={maxStates}
              onChange={(e) => setMaxStates(parseInt(e.target.value) || 0)}
              min={0}
              disabled={explorationRunning}
              className="w-full px-3 py-2 bg-input border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
              placeholder="0 = unlimited"
            />
            <p className="text-xs text-muted-foreground">0 = visit all states</p>
          </div>

          {/* Monitor Selection */}
          <div className="space-y-2">
            <label className="text-sm font-medium flex items-center gap-2">
              <Monitor className="w-4 h-4" />
              Monitor
            </label>
            <div className="px-3 py-2 bg-muted/50 border border-border rounded-lg text-sm">
              {selectedMonitors.length > 0
                ? `Monitor ${selectedMonitors[0]}`
                : "Default (from Run tab)"}
            </div>
          </div>
        </div>

        {/* Checkboxes */}
        <div className="space-y-2">
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={captureScreenshots}
              onChange={(e) => setCaptureScreenshots(e.target.checked)}
              disabled={explorationRunning}
              className="w-4 h-4 rounded border-border"
            />
            <Camera className="w-4 h-4 text-muted-foreground" />
            <span className="text-sm">Capture screenshots at each state</span>
          </label>

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={stopOnFirstFailure}
              onChange={(e) => setStopOnFirstFailure(e.target.checked)}
              disabled={explorationRunning}
              className="w-4 h-4 rounded border-border"
            />
            <StopCircle className="w-4 h-4 text-muted-foreground" />
            <span className="text-sm">Stop on first failure</span>
          </label>
        </div>

        {/* Advanced Options */}
        <div className="border border-border rounded-lg overflow-hidden">
          <button
            onClick={() => setShowAdvanced(!showAdvanced)}
            className="w-full px-4 py-2 bg-muted/30 text-sm font-medium flex items-center justify-between hover:bg-muted/50 transition-colors"
          >
            <span>Advanced Options</span>
            {showAdvanced ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
          </button>

          {showAdvanced && (
            <div className="p-4 space-y-4 border-t border-border">
              {/* State Delay */}
              <div className="space-y-2">
                <label className="text-sm font-medium">State Delay (ms)</label>
                <input
                  type="number"
                  value={stateDelayMs}
                  onChange={(e) => setStateDelayMs(parseInt(e.target.value) || 500)}
                  min={100}
                  max={5000}
                  step={100}
                  disabled={explorationRunning}
                  className="w-full px-3 py-2 bg-input border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
                />
                <p className="text-xs text-muted-foreground">
                  Delay between visiting states (default: 500ms)
                </p>
              </div>

              {/* Max Duration */}
              <div className="space-y-2">
                <label className="text-sm font-medium">Max Duration (seconds)</label>
                <input
                  type="number"
                  value={maxDurationSeconds}
                  onChange={(e) => setMaxDurationSeconds(parseInt(e.target.value) || 0)}
                  min={0}
                  disabled={explorationRunning}
                  className="w-full px-3 py-2 bg-input border border-border rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
                  placeholder="0 = unlimited"
                />
                <p className="text-xs text-muted-foreground">0 = no time limit</p>
              </div>
            </div>
          )}
        </div>

        {/* Preview Plan */}
        {currentPlan && <PlanPreview plan={currentPlan} />}
      </div>

      {/* Action Buttons */}
      <div className="flex-shrink-0 p-4 border-t border-border flex gap-3">
        <button
          onClick={handlePreview}
          disabled={!canRunExploration || previewLoading}
          className="flex items-center gap-2 px-4 py-2 bg-muted hover:bg-muted/80 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg text-sm font-medium transition-colors"
        >
          {previewLoading ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <Eye className="w-4 h-4" />
          )}
          Preview Plan
        </button>

        <button
          onClick={handleStart}
          disabled={!canRunExploration}
          className="flex-1 flex items-center justify-center gap-2 px-4 py-2 bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg text-sm font-medium transition-colors"
        >
          {explorationRunning ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              Running Exploration...
            </>
          ) : (
            <>
              <Play className="w-4 h-4" />
              Start Exploration
            </>
          )}
        </button>
      </div>
    </div>
  );
}

/**
 * Plan Preview Component
 */
function PlanPreview({ plan }: { plan: ExplorationPlan }) {
  const [showStates, setShowStates] = useState(false);
  const [showTransitions, setShowTransitions] = useState(false);

  return (
    <div className="p-4 bg-card rounded-lg border border-border space-y-3">
      <h3 className="font-medium flex items-center gap-2">
        <Eye className="w-4 h-4" />
        Exploration Plan Preview
      </h3>

      {/* Summary Stats */}
      <div className="grid grid-cols-2 gap-3 text-sm">
        <div className="flex justify-between">
          <span className="text-muted-foreground">Strategy:</span>
          <span className="font-medium">{plan.strategy}</span>
        </div>
        <div className="flex justify-between">
          <span className="text-muted-foreground">Estimated Cost:</span>
          <span className="font-medium">{plan.estimated_cost}</span>
        </div>
        <div className="flex justify-between">
          <span className="text-muted-foreground">States to Visit:</span>
          <span className="font-medium">
            {plan.states_to_visit} / {plan.total_states_in_config}
          </span>
        </div>
        <div className="flex justify-between">
          <span className="text-muted-foreground">Transitions to Verify:</span>
          <span className="font-medium">
            {plan.transitions_to_verify} / {plan.total_transitions_in_config}
          </span>
        </div>
      </div>

      {/* States List */}
      {plan.states.length > 0 && (
        <div className="border-t border-border pt-3">
          <button
            onClick={() => setShowStates(!showStates)}
            className="flex items-center gap-2 text-sm font-medium hover:text-primary transition-colors"
          >
            {showStates ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
            States ({plan.states.length})
          </button>
          {showStates && (
            <ul className="mt-2 space-y-1 pl-6 text-sm text-muted-foreground">
              {plan.states.map((state, idx) => (
                <li key={`state-${idx}-${state.slice(0, 20)}`} className="flex items-center gap-2">
                  <span className="w-5 text-xs text-muted-foreground/50">{idx + 1}.</span>
                  {state}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* Transitions List */}
      {plan.transitions.length > 0 && (
        <div className="border-t border-border pt-3">
          <button
            onClick={() => setShowTransitions(!showTransitions)}
            className="flex items-center gap-2 text-sm font-medium hover:text-primary transition-colors"
          >
            {showTransitions ? (
              <ChevronDown className="w-4 h-4" />
            ) : (
              <ChevronRight className="w-4 h-4" />
            )}
            Transitions ({plan.transitions.length})
          </button>
          {showTransitions && (
            <ul className="mt-2 space-y-1 pl-6 text-sm text-muted-foreground">
              {plan.transitions.map((transition, idx) => (
                <li
                  key={`transition-${idx}-${transition.slice(0, 20)}`}
                  className="flex items-center gap-2"
                >
                  <span className="w-5 text-xs text-muted-foreground/50">{idx + 1}.</span>
                  {transition}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
