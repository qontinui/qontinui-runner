/**
 * DemoVideoPanel
 *
 * UI for selecting pages, configuring recording, previewing AI-generated scripts,
 * and launching demo video recording.
 */

import { useState, useEffect, useCallback, useRef } from "react";
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
  CheckSquare,
  MapPin,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useUIComponent } from "@qontinui/ui-bridge";
import type { ProductTour } from "@/types/product-tour";
import { PRODUCT_TOURS_STORAGE_KEY } from "@/types/product-tour";
import { instanceStorage } from "@/lib/instance-storage";
import { TourPlayer } from "@/components/product-tour/TourPlayer";
import type {
  DemoScript,
  DemoRecordingConfig,
  DemoGenerationState,
  DemoExecutionResult,
} from "@/lib/demo-video/types";
import { DEFAULT_RECORDING_CONFIG, INITIAL_GENERATION_STATE } from "@/lib/demo-video/types";
import { fetchRegisteredElements, planDemoScript } from "@/lib/demo-video/script-planner";
import { executeScript, abortExecution } from "@/lib/demo-video/script-executor";
import { generateNarration } from "@/lib/demo-video/narration-generator";
import type { NarrationOutput } from "@/lib/demo-video/narration-generator";
import type { DiscoveredSpec } from "@/lib/spec-prompt-builder";
import { loadDiscoveredSpecs } from "@/lib/ui-bridge/use-discovered-specs";
import { ScriptPreview } from "./ScriptPreview";

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
    <div className="grid grid-cols-4 gap-3 text-sm">
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

      <label className="space-y-1">
        <span className="text-muted-foreground">Codec</span>
        <select
          value={config.codec}
          onChange={(e) =>
            onChange({ ...config, codec: e.target.value as DemoRecordingConfig["codec"] })
          }
          className="w-full rounded border border-border bg-background px-2 py-1.5 text-sm"
        >
          <option value="h264">H.264</option>
          <option value="h265">H.265</option>
          <option value="vp9">VP9</option>
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

  const [specs, setSpecs] = useState<DiscoveredSpec[]>([]);
  const [activeTour, setActiveTour] = useState<ProductTour | null>(null);
  const [selectedSpecIds, setSelectedSpecIds] = useState<Set<string>>(new Set());
  const [config, setConfig] = useState<DemoRecordingConfig>(DEFAULT_RECORDING_CONFIG);
  const [state, setState] = useState<DemoGenerationState>(INITIAL_GENERATION_STATE);
  const [_narration, setNarration] = useState<NarrationOutput | null>(null);

  // Multi-page batch state
  const [allScripts, setAllScripts] = useState<DemoScript[]>([]);
  const [allResults, setAllResults] = useState<
    { script: DemoScript; result: DemoExecutionResult; narration: NarrationOutput }[]
  >([]);
  const [batchIndex, setBatchIndex] = useState(0);
  const scriptsRef = useRef<DemoScript[]>([]);

  // Load specs on mount via the runner Spec API (Section 13).
  useEffect(() => {
    loadDiscoveredSpecs()
      .then((found) => {
        setSpecs(found);
      })
      .catch(() => {
        setSpecs([]);
      });
  }, []);

  const toggleSpec = useCallback((specId: string) => {
    setSelectedSpecIds((prev) => {
      const next = new Set(prev);
      if (next.has(specId)) next.delete(specId);
      else next.add(specId);
      return next;
    });
  }, []);

  const toggleAll = useCallback(() => {
    setSelectedSpecIds((prev) =>
      prev.size === specs.length ? new Set() : new Set(specs.map((s) => s.specId)),
    );
  }, [specs]);

  const selectedSpecs = specs.filter((s) => selectedSpecIds.has(s.specId));

  // -------------------------------------------------------------------------
  // Plan Scripts (all selected pages)
  // -------------------------------------------------------------------------
  const handlePlanScript = useCallback(async () => {
    if (selectedSpecs.length === 0) return;

    setState((s) => ({ ...s, phase: "planning", error: null, script: null, result: null }));
    setNarration(null);
    setAllScripts([]);
    setAllResults([]);
    setBatchIndex(0);

    try {
      const elements = await fetchRegisteredElements();
      const scripts: DemoScript[] = [];

      for (const entry of selectedSpecs) {
        const script = await planDemoScript(entry.config, elements);
        scripts.push(script);
      }

      scriptsRef.current = scripts;
      setAllScripts(scripts);
      // Show the first script in the preview
      setState((s) => ({ ...s, phase: "previewing", script: scripts[0] }));
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setState((s) => ({ ...s, phase: "error", error: msg }));
    }
  }, [selectedSpecs]);

  // -------------------------------------------------------------------------
  // Record (batch — all planned scripts sequentially)
  // -------------------------------------------------------------------------
  const handleRecord = useCallback(async () => {
    const scripts = scriptsRef.current;
    if (scripts.length === 0) return;

    setState((s) => ({ ...s, phase: "recording", currentStepIndex: 0 }));
    setAllResults([]);

    const results: typeof allResults = [];

    try {
      for (let si = 0; si < scripts.length; si++) {
        const script = scripts[si];
        setBatchIndex(si);
        setState((s) => ({ ...s, script, currentStepIndex: 0 }));

        const result = await executeScript(script, config, {
          onStepStart: (index) => {
            setState((s) => ({ ...s, currentStepIndex: index }));
          },
          onStepComplete: (index) => {
            setState((s) => ({ ...s, currentStepIndex: index }));
          },
          onError: (error, index) => {
            console.warn(`Demo ${script.title} step ${index} error:`, error);
          },
        });

        const narrationOutput = generateNarration(script, result);
        results.push({ script, result, narration: narrationOutput });
      }

      setAllResults(results);
      // Show first result
      setState((s) => ({
        ...s,
        phase: "done",
        result: results[0]?.result ?? null,
      }));
      setNarration(results[0]?.narration ?? null);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setState((s) => ({ ...s, phase: "error", error: msg }));
    }
  }, [config]);

  // -------------------------------------------------------------------------
  // Stop Recording
  // -------------------------------------------------------------------------
  const handleStop = useCallback(() => {
    abortExecution();
  }, []);

  // -------------------------------------------------------------------------
  // Open in Explorer
  // -------------------------------------------------------------------------
  const handleOpenFolder = useCallback(async (videoPath: string) => {
    const dir = videoPath.replace(/[/\\][^/\\]+$/, "");
    try {
      await invoke("open_path", { path: dir });
    } catch {
      await invoke("plugin:shell|open", { path: dir }).catch(() => {});
    }
  }, []);

  // -------------------------------------------------------------------------
  // Reset
  // -------------------------------------------------------------------------
  const handleReset = useCallback(() => {
    setState(INITIAL_GENERATION_STATE);
    setNarration(null);
    setAllScripts([]);
    setAllResults([]);
    setBatchIndex(0);
    scriptsRef.current = [];
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
        {/* Page Selector — multi-select */}
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <label className="text-sm font-medium">Select Pages</label>
            <button
              onClick={toggleAll}
              disabled={state.phase === "recording" || specs.length === 0}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              {selectedSpecIds.size === specs.length ? "Deselect all" : "Select all"}
            </button>
          </div>
          {specs.length === 0 ? (
            <p className="text-sm text-muted-foreground">No specs found</p>
          ) : (
            <div className="max-h-48 overflow-y-auto border border-border rounded-md divide-y divide-border">
              {specs.map((s) => {
                const selected = selectedSpecIds.has(s.specId);
                return (
                  <button
                    key={s.specId}
                    onClick={() => toggleSpec(s.specId)}
                    disabled={state.phase === "recording"}
                    className={`w-full flex items-center gap-2 px-3 py-2 text-left text-sm hover:bg-accent/50 ${
                      selected ? "bg-accent/30" : ""
                    }`}
                  >
                    {selected ? (
                      <CheckSquare className="h-4 w-4 text-primary shrink-0" />
                    ) : (
                      <Square className="h-4 w-4 text-muted-foreground shrink-0" />
                    )}
                    <span className="truncate">{s.config.metadata?.component || s.specId}</span>
                    {s.config.description && (
                      <span className="text-xs text-muted-foreground truncate ml-1">
                        — {s.config.description.slice(0, 50)}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          )}
          {selectedSpecIds.size > 0 && (
            <p className="text-xs text-muted-foreground">
              {selectedSpecIds.size} page{selectedSpecIds.size !== 1 ? "s" : ""} selected
            </p>
          )}
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
              disabled={selectedSpecs.length === 0}
              className="flex items-center gap-2 px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
            >
              <Sparkles className="h-4 w-4" />
              Generate Script{selectedSpecs.length > 1 ? "s" : ""}
            </button>
          )}

          {state.phase === "previewing" && (
            <button
              onClick={handleRecord}
              className="flex items-center gap-2 px-4 py-2 rounded-md bg-green-600 text-white text-sm font-medium hover:bg-green-700"
            >
              <Play className="h-4 w-4" />
              Record {allScripts.length} Video{allScripts.length !== 1 ? "s" : ""}
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
            Planning demo script{selectedSpecs.length > 1 ? "s" : ""} with AI...
          </div>
        )}

        {state.phase === "recording" && (
          <div className="flex items-center gap-2 text-sm text-green-400">
            <div className="h-2 w-2 rounded-full bg-red-500 animate-pulse" />
            Recording
            {allScripts.length > 1 ? ` (video ${batchIndex + 1}/${allScripts.length})` : ""} — Step{" "}
            {state.currentStepIndex + 1} of {state.script?.steps.length ?? 0}
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

        {/* Script Previews */}
        {allScripts.length > 0 && (state.phase === "previewing" || state.phase === "recording") && (
          <div className="space-y-4">
            {allScripts.map((script, i) => (
              <div key={script.targetPage} className="border border-border rounded-md p-4">
                {allScripts.length > 1 && (
                  <div className="text-xs text-muted-foreground mb-2">
                    Video {i + 1} of {allScripts.length}
                  </div>
                )}
                <ScriptPreview
                  script={script}
                  activeStepIndex={
                    state.phase === "recording" && batchIndex === i ? state.currentStepIndex : -1
                  }
                />
              </div>
            ))}
          </div>
        )}

        {/* Output Section — batch results */}
        {state.phase === "done" && allResults.length > 0 && (
          <div className="space-y-4">
            <div className="flex items-center gap-2 text-sm text-green-400">
              <CheckCircle className="h-4 w-4" />
              {allResults.length} video{allResults.length !== 1 ? "s" : ""} recorded
            </div>

            {allResults.map((entry, _i) => (
              <div
                key={entry.result.videoPath}
                className="space-y-3 border border-border rounded-md p-4"
              >
                <h4 className="text-sm font-medium">{entry.script.title}</h4>

                <div className="space-y-1 text-sm">
                  <div className="flex items-center gap-2">
                    <span className="text-muted-foreground">Video:</span>
                    <code className="text-xs bg-muted px-2 py-0.5 rounded truncate max-w-md">
                      {entry.result.videoPath}
                    </code>
                    <button
                      onClick={() => handleOpenFolder(entry.result.videoPath)}
                      className="text-xs text-primary hover:underline flex items-center gap-1 shrink-0"
                    >
                      <FolderOpen className="h-3 w-3" />
                      Open
                    </button>
                  </div>
                  <div>
                    <span className="text-muted-foreground">Duration:</span>{" "}
                    {Math.round(entry.result.totalDurationMs / 1000)}s
                  </div>
                </div>

                <details className="group">
                  <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground">
                    SRT Subtitles
                  </summary>
                  <pre className="mt-2 text-xs bg-muted p-3 rounded-md overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                    {entry.narration.srt}
                  </pre>
                </details>

                <details className="group">
                  <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground">
                    Markdown Script
                  </summary>
                  <pre className="mt-2 text-xs bg-muted p-3 rounded-md overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                    {entry.narration.markdown}
                  </pre>
                </details>

                {/* Cross-link to matching product tour */}
                {(() => {
                  const savedTours = instanceStorage.getJSON<ProductTour[]>(
                    PRODUCT_TOURS_STORAGE_KEY,
                    [],
                  );
                  const matchingTour = savedTours.find((t) =>
                    t.pages.some(
                      (p) =>
                        entry.script.targetPage.includes(p) || p.includes(entry.script.targetPage),
                    ),
                  );
                  if (!matchingTour) return null;
                  return (
                    <button
                      onClick={() => setActiveTour(matchingTour)}
                      className="flex items-center gap-1.5 text-xs text-primary hover:underline mt-1"
                    >
                      <MapPin className="h-3 w-3" />
                      Start interactive tour for this page
                    </button>
                  );
                })()}
              </div>
            ))}
          </div>
        )}
      </div>

      {activeTour && (
        <TourPlayer
          tour={activeTour}
          onClose={() => setActiveTour(null)}
          onComplete={() => setActiveTour(null)}
        />
      )}
    </div>
  );
}
