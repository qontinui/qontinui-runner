/**
 * ExplorationConfigDialog.tsx
 *
 * Dialog for configuring and running automatic UI Bridge exploration.
 * Allows users to customize exploration parameters and view results.
 * Includes real-time progress updates during exploration.
 */

import { useState, useCallback, useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as Dialog from "@radix-ui/react-dialog";
import {
  X,
  Compass,
  Play,
  Loader2,
  Settings2,
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronRight,
  Download,
  Copy,
  Check,
  Square,
  StopCircle,
  Clock,
  Layers,
  MousePointer2,
  Network,
} from "lucide-react";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import type { FingerprintDiscoveryResult } from "./StateDiscoveryPanel";
import type { ExplorationProgress, ExplorationPhase } from "../../types";

export interface ExplorationConfig {
  maxDepth: number;
  maxElementsPerPage: number;
  maxTotalElements: number;
  actionDelayMs: number;
  blockedKeywords: string[];
  safeKeywords: string[];
  captureScreenshots: boolean;
}

export interface ExplorationResult {
  explorationId: string;
  elementsDiscovered: number;
  elementsExplored: number;
  errors: string[];
  stateDiscoveryResult?: FingerprintDiscoveryResult;
}

interface ExplorationConfigDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onRunExploration: (config: ExplorationConfig) => Promise<ExplorationResult | null>;
  onStopExploration?: () => Promise<void>;
  isExploring: boolean;
  isStopping?: boolean;
  lastResult: ExplorationResult | null;
  /** Whether the last result was from a stopped exploration (partial results) */
  wasStopped?: boolean;
}

const DEFAULT_CONFIG: ExplorationConfig = {
  maxDepth: 2,
  maxElementsPerPage: 15,
  maxTotalElements: 50,
  actionDelayMs: 500,
  blockedKeywords: ["delete", "remove", "logout", "signout", "cancel", "close"],
  safeKeywords: ["view", "show", "details", "expand", "open"],
  captureScreenshots: false,
};

/** Phase display configuration */
const PHASE_CONFIG: Record<
  ExplorationPhase,
  { label: string; icon: React.ComponentType<{ className?: string }>; color: string }
> = {
  connecting: { label: "Connecting", icon: Network, color: "text-blue-500" },
  discovering: { label: "Discovering", icon: Layers, color: "text-purple-500" },
  exploring: { label: "Exploring", icon: MousePointer2, color: "text-primary" },
  analyzing: { label: "Analyzing", icon: Compass, color: "text-amber-500" },
  complete: { label: "Complete", icon: CheckCircle, color: "text-green-500" },
  stopped: { label: "Stopped", icon: StopCircle, color: "text-amber-500" },
};

/** Format milliseconds to human readable time */
function formatTime(ms: number): string {
  if (ms < 0) return "--";
  if (ms < 1000) return "< 1s";
  const seconds = Math.floor(ms / 1000);
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes > 0) {
    return `${minutes}m ${remainingSeconds}s`;
  }
  return `${seconds}s`;
}

export function ExplorationConfigDialog({
  isOpen,
  onClose,
  onRunExploration,
  onStopExploration,
  isExploring,
  isStopping = false,
  lastResult,
  wasStopped = false,
}: ExplorationConfigDialogProps) {
  const [config, setConfig] = useState<ExplorationConfig>(DEFAULT_CONFIG);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [blockedKeywordsInput, setBlockedKeywordsInput] = useState(
    DEFAULT_CONFIG.blockedKeywords.join(", "),
  );
  const [safeKeywordsInput, setSafeKeywordsInput] = useState(
    DEFAULT_CONFIG.safeKeywords.join(", "),
  );
  const [showErrors, setShowErrors] = useState(false);
  const [showStates, setShowStates] = useState(false);
  const [copied, setCopied] = useState(false);

  // Real-time progress state
  const [progress, setProgress] = useState<ExplorationProgress | null>(null);

  // Listen for exploration progress events
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    const setupListener = async () => {
      unlisten = await listen<ExplorationProgress>("exploration-progress", (event) => {
        setProgress(event.payload);
      });
    };

    if (isExploring) {
      setupListener();
    }

    return () => {
      if (unlisten) {
        unlisten();
      }
      // Reset progress when not exploring
      if (!isExploring) {
        setProgress(null);
      }
    };
  }, [isExploring]);

  // Reset progress when exploration stops
  useEffect(() => {
    if (!isExploring) {
      // Keep progress for a moment after exploration ends to show final state
      const timer = setTimeout(() => {
        setProgress(null);
      }, 2000);
      return () => clearTimeout(timer);
    }
  }, [isExploring]);

  // Build state machine config from exploration results
  const buildStateConfig = useCallback(() => {
    if (!lastResult?.stateDiscoveryResult) return null;

    const states = lastResult.stateDiscoveryResult.states.map((state) => ({
      id: state.stateId,
      name: state.name,
      fingerprints: state.fingerprintHashes,
      elementIds: state.elementIds,
      properties: {
        isGlobal: state.isGlobal,
        isModal: state.isModal,
        positionZone: state.positionZone,
        landmarkContext: state.landmarkContext,
        confidence: state.confidence,
      },
    }));

    const transitions = lastResult.stateDiscoveryResult.transitions.map((t) => ({
      from: t.fromStateId,
      to: t.toStateId,
      action: t.actionType,
      count: t.count,
    }));

    return {
      version: "1.0",
      generatedAt: new Date().toISOString(),
      explorationId: lastResult.explorationId,
      statistics: {
        elementsDiscovered: lastResult.elementsDiscovered,
        elementsExplored: lastResult.elementsExplored,
        statesDiscovered: states.length,
        transitionsDiscovered: transitions.length,
        ...lastResult.stateDiscoveryResult.statistics,
      },
      states,
      transitions,
    };
  }, [lastResult]);

  // Export states to JSON file
  const handleExportStates = useCallback(async () => {
    const stateConfig = buildStateConfig();
    if (!stateConfig) return;

    const blob = new Blob([JSON.stringify(stateConfig, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `discovered-states-${lastResult?.explorationId?.slice(0, 8) || "export"}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }, [buildStateConfig, lastResult?.explorationId]);

  // Copy states to clipboard
  const handleCopyStates = useCallback(async () => {
    const stateConfig = buildStateConfig();
    if (!stateConfig) return;

    try {
      await navigator.clipboard.writeText(JSON.stringify(stateConfig, null, 2));
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  }, [buildStateConfig]);

  const handleRunExploration = useCallback(async () => {
    // Parse keywords from input
    const blockedKeywords = blockedKeywordsInput
      .split(",")
      .map((k) => k.trim())
      .filter((k) => k.length > 0);
    const safeKeywords = safeKeywordsInput
      .split(",")
      .map((k) => k.trim())
      .filter((k) => k.length > 0);

    await onRunExploration({
      ...config,
      blockedKeywords,
      safeKeywords,
    });
  }, [config, blockedKeywordsInput, safeKeywordsInput, onRunExploration]);

  // Calculate progress percentage
  const progressPercent =
    progress && config.maxTotalElements > 0
      ? Math.min(Math.round((progress.elementsExplored / config.maxTotalElements) * 100), 100)
      : 0;

  // Get phase configuration
  const phaseConfig = progress?.phase ? PHASE_CONFIG[progress.phase] : null;
  const PhaseIcon = phaseConfig?.icon || Loader2;

  return (
    <Dialog.Root open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/60 z-50" />
        <Dialog.Content className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 z-50 w-[500px] max-h-[85vh] overflow-hidden bg-background rounded-lg shadow-xl border border-border flex flex-col">
          {/* Header */}
          <div className="flex items-center justify-between p-4 border-b border-border">
            <div className="flex items-center gap-2">
              <Compass className="w-5 h-5 text-primary" />
              <Dialog.Title className="text-lg font-semibold">
                Auto-Explore Configuration
              </Dialog.Title>
            </div>
            <Dialog.Close asChild>
              <button className="p-1 hover:bg-muted rounded">
                <X className="w-5 h-5" />
              </button>
            </Dialog.Close>
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto p-4 space-y-4">
            {/* Progress Display (when exploring) */}
            {isExploring && (
              <div
                className={`p-4 rounded-lg border ${isStopping ? "bg-amber-500/10 border-amber-500/30" : "bg-primary/10 border-primary/30"}`}
              >
                {/* Phase Header with Stop Button */}
                <div className="flex items-center justify-between mb-3">
                  <div className="flex items-center gap-3">
                    {isStopping ? (
                      <StopCircle className="w-6 h-6 text-amber-500" />
                    ) : (
                      <PhaseIcon
                        className={`w-6 h-6 ${phaseConfig?.color || "text-primary"} ${progress?.phase === "exploring" ? "animate-pulse" : ""}`}
                      />
                    )}
                    <div>
                      <h3
                        className={`font-medium ${isStopping ? "text-amber-500" : phaseConfig?.color || "text-primary"}`}
                      >
                        {isStopping ? "Stopping..." : phaseConfig?.label || "Starting..."}
                        {progress?.phase === "exploring" && progress.currentDepth > 0 && (
                          <span className="ml-2 text-sm font-normal text-muted-foreground">
                            (Depth {progress.currentDepth}/{progress.maxDepth})
                          </span>
                        )}
                      </h3>
                      <p className="text-sm text-muted-foreground mt-0.5">
                        {progress?.message || "Initializing exploration..."}
                      </p>
                    </div>
                  </div>
                  {onStopExploration && !isStopping && (
                    <Button
                      variant="danger"
                      size="sm"
                      onClick={onStopExploration}
                      className="flex-shrink-0 ml-4"
                    >
                      <Square className="w-3.5 h-3.5 mr-1.5 fill-current" />
                      Stop
                    </Button>
                  )}
                </div>

                {/* Progress Bar */}
                <div className="space-y-2">
                  <div className="flex items-center justify-between text-xs text-muted-foreground">
                    <span>Progress</span>
                    <span>{progressPercent}%</span>
                  </div>
                  <div className="h-2 bg-background rounded-full overflow-hidden">
                    <div
                      className={`h-full transition-all duration-300 rounded-full ${isStopping ? "bg-amber-500" : "bg-primary"}`}
                      style={{ width: `${progressPercent}%` }}
                    />
                  </div>
                </div>

                {/* Current Element */}
                {progress?.currentElement && (
                  <div className="mt-3 p-2 bg-background rounded text-sm">
                    <span className="text-muted-foreground">Current: </span>
                    <span className="font-medium">
                      {progress.currentElement.action === "click" ? "Clicking" : "Focusing"}{" "}
                    </span>
                    <span className="text-primary truncate">
                      "{progress.currentElement.label.slice(0, 40)}
                      {progress.currentElement.label.length > 40 ? "..." : ""}"
                    </span>
                  </div>
                )}

                {/* Live Stats */}
                <div className="mt-3 grid grid-cols-4 gap-2 text-center">
                  <div className="p-2 bg-background rounded">
                    <div className="text-lg font-semibold text-foreground">
                      {progress?.elementsDiscovered ?? 0}
                    </div>
                    <div className="text-[10px] text-muted-foreground uppercase tracking-wider">
                      Discovered
                    </div>
                  </div>
                  <div className="p-2 bg-background rounded">
                    <div className="text-lg font-semibold text-foreground">
                      {progress?.elementsExplored ?? 0}
                    </div>
                    <div className="text-[10px] text-muted-foreground uppercase tracking-wider">
                      Explored
                    </div>
                  </div>
                  <div className="p-2 bg-background rounded">
                    <div className="text-lg font-semibold text-primary">
                      {progress?.statesFound ?? 0}
                    </div>
                    <div className="text-[10px] text-muted-foreground uppercase tracking-wider">
                      States
                    </div>
                  </div>
                  <div className="p-2 bg-background rounded">
                    <div
                      className={`text-lg font-semibold ${(progress?.errorsCount ?? 0) > 0 ? "text-destructive" : "text-foreground"}`}
                    >
                      {progress?.errorsCount ?? 0}
                    </div>
                    <div className="text-[10px] text-muted-foreground uppercase tracking-wider">
                      Errors
                    </div>
                  </div>
                </div>

                {/* Time Display */}
                <div className="mt-3 flex items-center justify-between text-xs text-muted-foreground">
                  <div className="flex items-center gap-1">
                    <Clock className="w-3 h-3" />
                    <span>Elapsed: {formatTime(progress?.elapsedTimeMs ?? 0)}</span>
                  </div>
                  {(progress?.estimatedRemainingMs ?? -1) >= 0 && (
                    <div className="flex items-center gap-1">
                      <span>Remaining: ~{formatTime(progress?.estimatedRemainingMs ?? 0)}</span>
                    </div>
                  )}
                </div>
              </div>
            )}

            {/* Description (when not exploring) */}
            {!isExploring && (
              <p className="text-sm text-muted-foreground">
                Automatically explore interactive elements on the page, capturing fingerprints and
                discovering application states through co-occurrence analysis.
              </p>
            )}

            {/* Basic Configuration */}
            <div className="space-y-3">
              <h3 className="text-sm font-medium">Exploration Limits</h3>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="text-xs text-muted-foreground">Max Depth</label>
                  <input
                    type="number"
                    value={config.maxDepth}
                    onChange={(e) =>
                      setConfig({ ...config, maxDepth: parseInt(e.target.value) || 1 })
                    }
                    min={1}
                    max={5}
                    disabled={isExploring}
                    className="w-full px-2 py-1.5 bg-input border border-border rounded text-sm"
                  />
                </div>
                <div>
                  <label className="text-xs text-muted-foreground">Max Elements/Page</label>
                  <input
                    type="number"
                    value={config.maxElementsPerPage}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        maxElementsPerPage: parseInt(e.target.value) || 10,
                      })
                    }
                    min={1}
                    max={50}
                    disabled={isExploring}
                    className="w-full px-2 py-1.5 bg-input border border-border rounded text-sm"
                  />
                </div>
                <div>
                  <label className="text-xs text-muted-foreground">Max Total Elements</label>
                  <input
                    type="number"
                    value={config.maxTotalElements}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        maxTotalElements: parseInt(e.target.value) || 50,
                      })
                    }
                    min={10}
                    max={200}
                    disabled={isExploring}
                    className="w-full px-2 py-1.5 bg-input border border-border rounded text-sm"
                  />
                </div>
                <div>
                  <label className="text-xs text-muted-foreground">Action Delay (ms)</label>
                  <input
                    type="number"
                    value={config.actionDelayMs}
                    onChange={(e) =>
                      setConfig({
                        ...config,
                        actionDelayMs: parseInt(e.target.value) || 500,
                      })
                    }
                    min={100}
                    max={2000}
                    step={100}
                    disabled={isExploring}
                    className="w-full px-2 py-1.5 bg-input border border-border rounded text-sm"
                  />
                </div>
              </div>
            </div>

            {/* Advanced Configuration */}
            <div className="border border-border rounded-lg overflow-hidden">
              <button
                onClick={() => setShowAdvanced(!showAdvanced)}
                className="w-full px-3 py-2 bg-muted/30 text-sm font-medium flex items-center justify-between hover:bg-muted/50 transition-colors"
              >
                <div className="flex items-center gap-2">
                  <Settings2 className="w-4 h-4" />
                  Advanced Options
                </div>
                {showAdvanced ? (
                  <ChevronDown className="w-4 h-4" />
                ) : (
                  <ChevronRight className="w-4 h-4" />
                )}
              </button>

              {showAdvanced && (
                <div className="p-3 space-y-3 border-t border-border">
                  <div>
                    <label className="text-xs text-muted-foreground">
                      Blocked Keywords (comma-separated)
                    </label>
                    <input
                      type="text"
                      value={blockedKeywordsInput}
                      onChange={(e) => setBlockedKeywordsInput(e.target.value)}
                      disabled={isExploring}
                      placeholder="delete, remove, logout..."
                      className="w-full px-2 py-1.5 bg-input border border-border rounded text-sm"
                    />
                    <p className="text-xs text-muted-foreground mt-1">
                      Elements containing these keywords will not be clicked
                    </p>
                  </div>

                  <div>
                    <label className="text-xs text-muted-foreground">
                      Safe Keywords (comma-separated)
                    </label>
                    <input
                      type="text"
                      value={safeKeywordsInput}
                      onChange={(e) => setSafeKeywordsInput(e.target.value)}
                      disabled={isExploring}
                      placeholder="view, show, details..."
                      className="w-full px-2 py-1.5 bg-input border border-border rounded text-sm"
                    />
                    <p className="text-xs text-muted-foreground mt-1">
                      Elements with these keywords are always considered safe
                    </p>
                  </div>

                  <label className="flex items-center gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={config.captureScreenshots}
                      onChange={(e) =>
                        setConfig({ ...config, captureScreenshots: e.target.checked })
                      }
                      disabled={isExploring}
                      className="w-4 h-4 rounded border-border"
                    />
                    <span className="text-sm">Capture screenshots at each step</span>
                  </label>
                </div>
              )}
            </div>

            {/* Last Result */}
            {lastResult && !isExploring && (
              <div className="p-3 bg-muted/30 rounded-lg border border-border space-y-2">
                <div className="flex items-center justify-between">
                  <h3 className="text-sm font-medium flex items-center gap-2">
                    {wasStopped ? (
                      <StopCircle className="w-4 h-4 text-amber-500" />
                    ) : lastResult.errors.length === 0 ? (
                      <CheckCircle className="w-4 h-4 text-green-500" />
                    ) : (
                      <AlertCircle className="w-4 h-4 text-amber-500" />
                    )}
                    {wasStopped ? "Partial Results (Stopped)" : "Last Exploration Result"}
                  </h3>
                  <span className="text-xs text-muted-foreground font-mono">
                    {lastResult.explorationId.slice(0, 8)}
                  </span>
                </div>

                <div className="grid grid-cols-3 gap-2 text-sm">
                  <div>
                    <span className="text-muted-foreground">Discovered:</span>
                    <span className="ml-1 font-medium">{lastResult.elementsDiscovered}</span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">Explored:</span>
                    <span className="ml-1 font-medium">{lastResult.elementsExplored}</span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">States:</span>
                    <span className="ml-1 font-medium text-primary">
                      {lastResult.stateDiscoveryResult?.states.length ?? 0}
                    </span>
                  </div>
                </div>

                {/* Export Buttons */}
                {lastResult.stateDiscoveryResult &&
                  lastResult.stateDiscoveryResult.states.length > 0 && (
                    <div className="flex items-center gap-2 pt-2 border-t border-border/50">
                      <span className="text-xs text-muted-foreground">Export:</span>
                      <button
                        onClick={handleCopyStates}
                        className="flex items-center gap-1 px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
                        title="Copy state config to clipboard"
                      >
                        {copied ? (
                          <>
                            <Check className="w-3 h-3 text-green-500" />
                            Copied!
                          </>
                        ) : (
                          <>
                            <Copy className="w-3 h-3" />
                            Copy JSON
                          </>
                        )}
                      </button>
                      <button
                        onClick={handleExportStates}
                        className="flex items-center gap-1 px-2 py-1 text-xs bg-muted hover:bg-muted/80 rounded transition-colors"
                        title="Download state config as JSON file"
                      >
                        <Download className="w-3 h-3" />
                        Download
                      </button>
                    </div>
                  )}

                {/* Discovered States */}
                {lastResult.stateDiscoveryResult &&
                  lastResult.stateDiscoveryResult.states.length > 0 && (
                    <div>
                      <button
                        onClick={() => setShowStates(!showStates)}
                        className="flex items-center gap-1 text-xs text-primary hover:underline"
                      >
                        {showStates ? (
                          <ChevronDown className="w-3 h-3" />
                        ) : (
                          <ChevronRight className="w-3 h-3" />
                        )}
                        View {lastResult.stateDiscoveryResult.states.length} discovered states
                      </button>
                      {showStates && (
                        <div className="mt-2 space-y-1 max-h-32 overflow-y-auto">
                          {lastResult.stateDiscoveryResult.states.map((state) => (
                            <div
                              key={state.stateId}
                              className="flex items-center gap-2 text-xs p-1.5 bg-background rounded"
                            >
                              <Badge
                                variant={state.isGlobal ? "info" : "default"}
                                className="text-[10px]"
                              >
                                {state.isGlobal ? "Global" : state.isModal ? "Modal" : "Local"}
                              </Badge>
                              <span className="font-medium truncate">{state.name}</span>
                              <span className="text-muted-foreground ml-auto">
                                {state.fingerprintHashes.length} elements
                              </span>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  )}

                {/* Errors */}
                {lastResult.errors.length > 0 && (
                  <div>
                    <button
                      onClick={() => setShowErrors(!showErrors)}
                      className="flex items-center gap-1 text-xs text-destructive hover:underline"
                    >
                      {showErrors ? (
                        <ChevronDown className="w-3 h-3" />
                      ) : (
                        <ChevronRight className="w-3 h-3" />
                      )}
                      {lastResult.errors.length} errors occurred
                    </button>
                    {showErrors && (
                      <div className="mt-2 space-y-1 max-h-24 overflow-y-auto">
                        {lastResult.errors.map((error, i) => (
                          <div
                            key={i}
                            className="text-xs text-destructive bg-destructive/10 p-1.5 rounded"
                          >
                            {error}
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>

          {/* Footer */}
          <div className="flex items-center justify-end gap-2 p-4 border-t border-border">
            <Button variant="ghost" size="sm" onClick={onClose} disabled={isExploring}>
              Cancel
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={handleRunExploration}
              disabled={isExploring}
            >
              {isExploring ? (
                <>
                  <Loader2 className="w-4 h-4 mr-1.5 animate-spin" />
                  Exploring...
                </>
              ) : (
                <>
                  <Play className="w-4 h-4 mr-1.5" />
                  Start Exploration
                </>
              )}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
