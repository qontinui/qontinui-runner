/**
 * DemoVideoPanel
 *
 * UI for selecting pages, configuring recording, previewing AI-generated scripts,
 * and launching demo video recording.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Video,
  Play,
  Square,
  FolderOpen,
  Loader2,
  AlertCircle,
  CheckCircle,
  Sparkles,
  RefreshCw,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useUIComponent } from "ui-bridge";
import type { SpecConfig } from "@/lib/spec-prompt-builder";
import type {
  DemoScript,
  DemoRecordingConfig,
  DemoGenerationState,
  DemoExecutionResult,
} from "@/lib/demo-video/types";
import {
  DEFAULT_RECORDING_CONFIG,
  INITIAL_GENERATION_STATE,
} from "@/lib/demo-video/types";
import {
  fetchRegisteredElements,
  planDemoScript,
} from "@/lib/demo-video/script-planner";
import {
  executeScript,
  abortExecution,
} from "@/lib/demo-video/script-executor";
import { generateNarration } from "@/lib/demo-video/narration-generator";
import type { NarrationOutput } from "@/lib/demo-video/narration-generator";
import { ScriptPreview } from "./ScriptPreview";

// =============================================================================
// Spec Discovery
// =============================================================================

interface DiscoveredSpecEntry {
  specId: string;
  config: SpecConfig;
}

async function discoverSpecs(): Promise<DiscoveredSpecEntry[]> {
  // Load specs from the specs directory via dynamic import of the spec index
  // Fall back to fetching from the API
  try {
    const modules = import.meta.glob("../../specs/*.spec.uibridge.json", {
      eager: true,
    }) as Record<string, { default: SpecConfig }>;

    return Object.entries(modules).map(([path, mod]) => {
      const fileName = path.split("/").pop()?.replace(".spec.uibridge.json", "") ?? "unknown";
      return {
        specId: fileName,
        config: mod.default,
      };
    });
  } catch {
    return [];
  }
}

// =============================================================================
// Recording Config Panel
// =============================================================================

function RecordingConfigPanel({
  config,
  onChange,
}: {
  config: DemoRecordingConfig;
  onChange: (config: DemoRecordingConfig) => void;
}) {
  return (
    <div className="grid grid-cols-3 gap-3 text-sm">
      <label className="space-y-1">
        <span className="text-muted-foreground">Resolution</span>
        <select
          value={config.resolution}
          onChange={(e) => onChange({ ...config, resolution: e.target.value })}
          className="w-full rounded border border-border bg-background px-2 py-1.5 text-sm"
        >
          <option value="1280x720">720p</option>
          <option value="1920x1080">1080p</option>
          <option value="2560x1440">1440p</option>
        </select>
      </label>

      <label className="space-y-1">
        <span className="text-muted-foreground">Framerate</span>
        <select
          value={config.framerate}
          onChange={(e) => onChange({ ...config, framerate: Number(e.target.value) })}
          className="w-full rounded border border-border bg-background px-2 py-1.5 text-sm"
        >
          <option value={15}>15 fps</option>
          <option value={24}>24 fps</option>
          <option value={30}>30 fps</option>
        </select>
      </label>

      <label className="space-y-1">
        <span className="text-muted-foreground">Quality</span>
        <select
          value={config.quality}
          onChange={(e) =>
            onChange({ ...config, quality: e.target.value as DemoRecordingConfig["quality"] })
          }
          className="w-full rounded border border-border bg-background px-2 py-1.5 text-sm"
        >
          <option value="low">Low</option>
          <option value="medium">Medium</option>
          <option value="high">High</option>
        </select>
      </label>
    </div>
  );
}

// =============================================================================
// DemoVideoPanel
// =============================================================================

export function DemoVideoPanel() {
  useUIComponent({
    id: "demo-video-panel",
    name: "Demo Video Generator",
    description: "Generate demo videos from UI Bridge page specs",
    actions: [],
  });

  const [specs, setSpecs] = useState<DiscoveredSpecEntry[]>([]);
  const [selectedSpecId, setSelectedSpecId] = useState<string>("");
  const [config, setConfig] = useState<DemoRecordingConfig>(DEFAULT_RECORDING_CONFIG);
  const [state, setState] = useState<DemoGenerationState>(INITIAL_GENERATION_STATE);
  const [narration, setNarration] = useState<NarrationOutput | null>(null);

  // Load specs on mount
  useEffect(() => {
    discoverSpecs().then((found) => {
      setSpecs(found);
      if (found.length > 0) {
        setSelectedSpecId((prev) => prev || found[0].specId);
      }
    });
  }, []);

  const selectedSpec = specs.find((s) => s.specId === selectedSpecId)?.config;

  // -------------------------------------------------------------------------
  // Plan Script
  // -------------------------------------------------------------------------
  const handlePlanScript = useCallback(async () => {
    if (!selectedSpec) return;

    setState((s) => ({ ...s, phase: "planning", error: null, script: null, result: null }));
    setNarration(null);

    try {
      const elements = await fetchRegisteredElements();
      const script = await planDemoScript(selectedSpec, elements);
      setState((s) => ({ ...s, phase: "previewing", script }));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setState((s) => ({ ...s, phase: "error", error: msg }));
    }
  }, [selectedSpec]);

  // -------------------------------------------------------------------------
  // Record
  // -------------------------------------------------------------------------
  const handleRecord = useCallback(async () => {
    if (!state.script) return;

    setState((s) => ({ ...s, phase: "recording", currentStepIndex: 0 }));

    try {
      const result = await executeScript(state.script, config, {
        onStepStart: (index) => {
          setState((s) => ({ ...s, currentStepIndex: index }));
        },
        onStepComplete: (index) => {
          setState((s) => ({ ...s, currentStepIndex: index }));
        },
        onError: (error, index) => {
          console.warn(`Demo step ${index} error:`, error);
        },
      });

      setState((s) => ({
        ...s,
        phase: "post-processing",
        result,
      }));

      // Generate narration
      const narrationOutput = generateNarration(state.script, result);
      setNarration(narrationOutput);

      setState((s) => ({ ...s, phase: "done" }));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setState((s) => ({ ...s, phase: "error", error: msg }));
    }
  }, [state.script, config]);

  // -------------------------------------------------------------------------
  // Stop Recording
  // -------------------------------------------------------------------------
  const handleStop = useCallback(() => {
    abortExecution();
  }, []);

  // -------------------------------------------------------------------------
  // Open in Explorer
  // -------------------------------------------------------------------------
  const handleOpenFolder = useCallback(async () => {
    if (!state.result?.videoPath) return;
    const dir = state.result.videoPath.replace(/[/\\][^/\\]+$/, "");
    try {
      await invoke("open_path", { path: dir });
    } catch {
      // Fallback: try shell open
      await invoke("plugin:shell|open", { path: dir }).catch(() => {});
    }
  }, [state.result?.videoPath]);

  // -------------------------------------------------------------------------
  // Reset
  // -------------------------------------------------------------------------
  const handleReset = useCallback(() => {
    setState(INITIAL_GENERATION_STATE);
    setNarration(null);
  }, []);

  // =========================================================================
  // Render
  // =========================================================================
  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center gap-3 p-4 border-b border-border shrink-0">
        <Video className="h-5 w-5 text-primary" />
        <h2 className="text-lg font-semibold">Demo Video Generator</h2>
        {state.phase !== "idle" && (
          <button
            onClick={handleReset}
            className="ml-auto text-xs text-muted-foreground hover:text-foreground flex items-center gap-1"
          >
            <RefreshCw className="h-3 w-3" />
            Reset
          </button>
        )}
      </div>

      <div className="flex-1 overflow-y-auto p-4 space-y-6">
        {/* Page Selector */}
        <div className="space-y-2">
          <label className="text-sm font-medium">Select Page</label>
          <select
            value={selectedSpecId}
            onChange={(e) => setSelectedSpecId(e.target.value)}
            disabled={state.phase === "recording"}
            className="w-full rounded border border-border bg-background px-3 py-2 text-sm"
          >
            {specs.length === 0 && <option value="">No specs found</option>}
            {specs.map((s) => (
              <option key={s.specId} value={s.specId}>
                {s.config.metadata?.component || s.specId}
                {s.config.description ? ` — ${s.config.description.slice(0, 60)}` : ""}
              </option>
            ))}
          </select>
        </div>

        {/* Recording Config */}
        <div className="space-y-2">
          <label className="text-sm font-medium">Recording Settings</label>
          <RecordingConfigPanel config={config} onChange={setConfig} />
        </div>

        {/* Action Buttons */}
        <div className="flex items-center gap-3">
          {(state.phase === "idle" || state.phase === "error") && (
            <button
              onClick={handlePlanScript}
              disabled={!selectedSpec}
              className="flex items-center gap-2 px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
            >
              <Sparkles className="h-4 w-4" />
              Generate Script
            </button>
          )}

          {state.phase === "previewing" && (
            <button
              onClick={handleRecord}
              className="flex items-center gap-2 px-4 py-2 rounded-md bg-green-600 text-white text-sm font-medium hover:bg-green-700"
            >
              <Play className="h-4 w-4" />
              Start Recording
            </button>
          )}

          {state.phase === "previewing" && (
            <button
              onClick={handlePlanScript}
              className="flex items-center gap-2 px-3 py-2 rounded-md border border-border text-sm hover:bg-accent"
            >
              <RefreshCw className="h-3.5 w-3.5" />
              Regenerate
            </button>
          )}

          {state.phase === "recording" && (
            <button
              onClick={handleStop}
              className="flex items-center gap-2 px-4 py-2 rounded-md bg-red-600 text-white text-sm font-medium hover:bg-red-700"
            >
              <Square className="h-4 w-4" />
              Stop Recording
            </button>
          )}
        </div>

        {/* Status */}
        {state.phase === "planning" && (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Planning demo script with AI...
          </div>
        )}

        {state.phase === "recording" && (
          <div className="flex items-center gap-2 text-sm text-green-400">
            <div className="h-2 w-2 rounded-full bg-red-500 animate-pulse" />
            Recording — Step {state.currentStepIndex + 1} of{" "}
            {state.script?.steps.length ?? 0}
          </div>
        )}

        {state.phase === "post-processing" && (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Generating narration...
          </div>
        )}

        {state.phase === "error" && state.error && (
          <div className="flex items-start gap-2 text-sm text-red-400 bg-red-500/10 border border-red-500/20 rounded-md p-3">
            <AlertCircle className="h-4 w-4 shrink-0 mt-0.5" />
            <span>{state.error}</span>
          </div>
        )}

        {/* Script Preview */}
        {state.script && (state.phase === "previewing" || state.phase === "recording") && (
          <div className="border border-border rounded-md p-4">
            <ScriptPreview
              script={state.script}
              activeStepIndex={state.phase === "recording" ? state.currentStepIndex : -1}
            />
          </div>
        )}

        {/* Output Section */}
        {state.phase === "done" && state.result && (
          <div className="space-y-4 border border-border rounded-md p-4">
            <div className="flex items-center gap-2 text-sm text-green-400">
              <CheckCircle className="h-4 w-4" />
              Recording complete
            </div>

            <div className="space-y-2 text-sm">
              <div className="flex items-center gap-2">
                <span className="text-muted-foreground">Video:</span>
                <code className="text-xs bg-muted px-2 py-0.5 rounded">
                  {state.result.videoPath}
                </code>
                <button
                  onClick={handleOpenFolder}
                  className="text-xs text-primary hover:underline flex items-center gap-1"
                >
                  <FolderOpen className="h-3 w-3" />
                  Open folder
                </button>
              </div>

              <div>
                <span className="text-muted-foreground">Duration:</span>{" "}
                {Math.round(state.result.totalDurationMs / 1000)}s
              </div>
            </div>

            {/* Narration Output */}
            {narration && (
              <div className="space-y-3">
                <h4 className="text-sm font-medium">Narration Script</h4>

                <details className="group">
                  <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground">
                    SRT Subtitles
                  </summary>
                  <pre className="mt-2 text-xs bg-muted p-3 rounded-md overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                    {narration.srt}
                  </pre>
                </details>

                <details className="group" open>
                  <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground">
                    Markdown Script
                  </summary>
                  <pre className="mt-2 text-xs bg-muted p-3 rounded-md overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                    {narration.markdown}
                  </pre>
                </details>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
