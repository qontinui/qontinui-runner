/**
 * ExplorationConfigDialog.tsx
 *
 * Dialog for configuring and running automatic UI Bridge exploration.
 * Allows users to customize exploration parameters and view results.
 */

import { useState, useCallback } from "react";
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
} from "lucide-react";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import type { FingerprintDiscoveryResult } from "./StateDiscoveryPanel";

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
  isExploring: boolean;
  lastResult: ExplorationResult | null;
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

export function ExplorationConfigDialog({
  isOpen,
  onClose,
  onRunExploration,
  isExploring,
  lastResult,
}: ExplorationConfigDialogProps) {
  const [config, setConfig] = useState<ExplorationConfig>(DEFAULT_CONFIG);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [blockedKeywordsInput, setBlockedKeywordsInput] = useState(
    DEFAULT_CONFIG.blockedKeywords.join(", ")
  );
  const [safeKeywordsInput, setSafeKeywordsInput] = useState(
    DEFAULT_CONFIG.safeKeywords.join(", ")
  );
  const [showErrors, setShowErrors] = useState(false);
  const [showStates, setShowStates] = useState(false);
  const [copied, setCopied] = useState(false);

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
    const config = buildStateConfig();
    if (!config) return;

    const blob = new Blob([JSON.stringify(config, null, 2)], { type: "application/json" });
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
    const config = buildStateConfig();
    if (!config) return;

    try {
      await navigator.clipboard.writeText(JSON.stringify(config, null, 2));
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
            {/* Description */}
            {isExploring ? (
              <div className="p-4 bg-primary/10 rounded-lg border border-primary/30">
                <div className="flex items-center gap-3">
                  <Loader2 className="w-6 h-6 animate-spin text-primary" />
                  <div>
                    <h3 className="font-medium text-primary">Exploration in Progress</h3>
                    <p className="text-sm text-muted-foreground mt-1">
                      The explorer is systematically clicking interactive elements and
                      capturing fingerprints. This may take a while depending on the
                      page complexity and your configuration.
                    </p>
                  </div>
                </div>
                <div className="mt-3 flex flex-wrap gap-2 text-xs">
                  <span className="px-2 py-1 bg-background rounded">
                    Max depth: {config.maxDepth}
                  </span>
                  <span className="px-2 py-1 bg-background rounded">
                    Max elements: {config.maxTotalElements}
                  </span>
                  <span className="px-2 py-1 bg-background rounded">
                    Delay: {config.actionDelayMs}ms
                  </span>
                </div>
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                Automatically explore interactive elements on the page, capturing
                fingerprints and discovering application states through co-occurrence
                analysis.
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
            {lastResult && (
              <div className="p-3 bg-muted/30 rounded-lg border border-border space-y-2">
                <div className="flex items-center justify-between">
                  <h3 className="text-sm font-medium flex items-center gap-2">
                    {lastResult.errors.length === 0 ? (
                      <CheckCircle className="w-4 h-4 text-green-500" />
                    ) : (
                      <AlertCircle className="w-4 h-4 text-amber-500" />
                    )}
                    Last Exploration Result
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
                {lastResult.stateDiscoveryResult && lastResult.stateDiscoveryResult.states.length > 0 && (
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
                              <Badge variant={state.isGlobal ? "info" : "default"} className="text-[10px]">
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
