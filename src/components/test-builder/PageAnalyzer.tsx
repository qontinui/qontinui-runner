/**
 * Page Analyzer Component
 *
 * Allows users to collect multiple analyses for AI test generation.
 * Supports three analysis methods:
 * - Playwright: Run a script from the Scripts library to analyze DOM
 * - Vision: Capture monitor screenshot and analyze with OCR/segmentation
 * - API Request: Execute a saved API request from the Library
 *
 * All collected analyses are displayed in a table for review before
 * sending to the AI for test generation.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Loader2,
  Camera,
  Monitor,
  Globe,
  Network,
  Plus,
  Trash2,
  Eye,
  ChevronDown,
  Check,
  X,
  AlertCircle,
  Play,
  RefreshCw,
  Zap,
} from "lucide-react";
import { MonitorSelector } from "../MonitorSelector";
import type {
  PageAnalysis,
  CommandResponse,
  CollectedAnalysis,
  CollectedAnalysisSet,
  AnalysisData,
  StepOutput,
  LiveBrowserAnalysis,
  LiveBrowserElement,
} from "./types";
import { createAnalysisFromStepOutput } from "./types";
import type { SavedApiRequest } from "../../types";
import { stepOutputRegistry } from "../../lib/step-output-handlers";
import { useLiveBrowser, type DiscoveredElement } from "../../hooks/useLiveBrowser";

// Script info from the Scripts library API
interface PlaywrightScript {
  id: string;
  name: string;
  description: string;
  target_url: string;
  script_content: string;
}

const API_BASE = "http://localhost:9876";

// Generate unique ID
function generateId(): string {
  return `analysis_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

interface PageAnalyzerProps {
  onAnalysisComplete: (analysis: AnalysisData) => void;
  onError?: (error: string) => void;
  /** Initial analyses to restore when component mounts (e.g., from parent state) */
  initialAnalyses?: CollectedAnalysis[];
}

export function PageAnalyzer({ onAnalysisComplete, initialAnalyses }: PageAnalyzerProps) {
  // Collected analyses - initialize from prop if provided
  const [analyses, setAnalyses] = useState<CollectedAnalysis[]>(initialAnalyses ?? []);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  // Add analysis state
  const [showAddMenu, setShowAddMenu] = useState(false);
  const [addMode, setAddMode] = useState<"playwright" | "vision" | "step_output" | "live_browser" | null>(null);
  // Step output sub-mode: select from recent outputs or run a new step
  const [stepOutputMode, setStepOutputMode] = useState<"recent" | "run_api">("recent");
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Playwright selection
  const [playwrightScripts, setPlaywrightScripts] = useState<PlaywrightScript[]>([]);
  const [selectedScriptId, setSelectedScriptId] = useState<string>("");
  const [loadingScripts, setLoadingScripts] = useState(false);

  // Vision selection
  const [selectedMonitors, setSelectedMonitors] = useState<number[]>([0]);

  // API Request selection
  const [savedRequests, setSavedRequests] = useState<SavedApiRequest[]>([]);
  const [selectedRequestId, setSelectedRequestId] = useState<string>("");
  const [loadingRequests, setLoadingRequests] = useState(false);

  // Step Output selection
  const [recentStepOutputs, setRecentStepOutputs] = useState<StepOutput[]>([]);
  const [selectedStepOutputIds, setSelectedStepOutputIds] = useState<string[]>([]);
  const [loadingStepOutputs, setLoadingStepOutputs] = useState(false);

  // Live browser (for live capture mode)
  const liveBrowser = useLiveBrowser();

  // Load Playwright scripts when mode changes
  useEffect(() => {
    if (addMode === "playwright" && playwrightScripts.length === 0) {
      loadPlaywrightScripts();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addMode, playwrightScripts.length]);

  // Load step outputs and API requests when step_output mode changes
  useEffect(() => {
    if (addMode === "step_output") {
      if (stepOutputMode === "recent") {
        loadRecentStepOutputs();
      } else if (stepOutputMode === "run_api" && savedRequests.length === 0) {
        loadSavedRequests();
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addMode, stepOutputMode, savedRequests.length]);

  const loadPlaywrightScripts = async () => {
    setLoadingScripts(true);
    try {
      const response = await fetch(`${API_BASE}/playwright/scripts`);
      const result = await response.json();
      if (result.success && result.data) {
        setPlaywrightScripts(result.data);
        if (result.data.length > 0 && !selectedScriptId) {
          setSelectedScriptId(result.data[0].id);
        }
      }
    } catch (err) {
      console.error("Failed to load Playwright scripts:", err);
    } finally {
      setLoadingScripts(false);
    }
  };

  const loadSavedRequests = async () => {
    setLoadingRequests(true);
    try {
      const response = await fetch(`${API_BASE}/saved-api-requests`);
      const result = await response.json();
      if (result.success && result.data) {
        setSavedRequests(result.data);
        if (result.data.length > 0 && !selectedRequestId) {
          setSelectedRequestId(result.data[0].id);
        }
      }
    } catch (err) {
      console.error("Failed to load saved API requests:", err);
    } finally {
      setLoadingRequests(false);
    }
  };

  const loadRecentStepOutputs = async () => {
    setLoadingStepOutputs(true);
    try {
      const response = await fetch(`${API_BASE}/recent-step-outputs`);
      const result = await response.json();
      if (result.success && result.data) {
        setRecentStepOutputs(result.data);
        setSelectedStepOutputIds([]);
      }
    } catch (err) {
      console.error("Failed to load recent step outputs:", err);
      // Continue without step outputs - endpoint may not exist yet
      setRecentStepOutputs([]);
    } finally {
      setLoadingStepOutputs(false);
    }
  };

  // Run Playwright analysis
  const runPlaywrightAnalysis = useCallback(async () => {
    const selectedScript = playwrightScripts.find((s) => s.id === selectedScriptId);
    if (!selectedScript) {
      setError("Please select a Playwright script");
      return;
    }

    const script = selectedScript.script_content || "";
    if (!script.trim()) {
      setError("Selected script has no Playwright code");
      return;
    }

    setIsAnalyzing(true);
    setError(null);

    try {
      const response = await invoke<CommandResponse<{ analysis: PageAnalysis }>>(
        "analyze_page_playwright_script",
        { script },
      );

      const rawData = response.data?.analysis || response.data;
      const analysisData = rawData as PageAnalysis | undefined;

      if (response.success && analysisData?.elements) {
        const newAnalysis: CollectedAnalysis = {
          type: "playwright",
          id: generateId(),
          name: selectedScript.name,
          data: analysisData,
        };
        setAnalyses((prev) => [...prev, newAnalysis]);
        setAddMode(null);
      } else {
        setError(response.message || "Analysis failed - no elements found");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsAnalyzing(false);
    }
  }, [selectedScriptId, playwrightScripts]);

  // Run Vision analysis
  const runVisionAnalysis = useCallback(async () => {
    const monitorIndex = selectedMonitors[0] ?? 0;
    setIsAnalyzing(true);
    setError(null);

    try {
      const response = await invoke<CommandResponse<{ analysis: PageAnalysis }>>(
        "analyze_page_vision",
        { monitorIndex },
      );

      const rawData = response.data?.analysis || response.data;
      const analysisData = rawData as PageAnalysis | undefined;

      if (response.success && analysisData?.elements) {
        const newAnalysis: CollectedAnalysis = {
          type: "vision",
          id: generateId(),
          name: `Monitor ${monitorIndex} Capture`,
          data: analysisData,
        };
        setAnalyses((prev) => [...prev, newAnalysis]);
        setAddMode(null);
      } else {
        setError(response.message || "Analysis failed - no elements found");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setIsAnalyzing(false);
    }
  }, [selectedMonitors]);

  // Run API Request and create step output
  const runApiRequestAnalysis = useCallback(async () => {
    const selectedRequest = savedRequests.find((r) => r.id === selectedRequestId);
    if (!selectedRequest) {
      setError("Please select an API request");
      return;
    }

    setIsAnalyzing(true);
    setError(null);

    const startTime = Date.now();

    try {
      const options: RequestInit = {
        method: selectedRequest.method,
        headers: {
          "Content-Type": selectedRequest.body_content_type || "application/json",
          ...selectedRequest.headers,
        },
      };

      if (selectedRequest.method !== "GET" && selectedRequest.body) {
        options.body = selectedRequest.body;
      }

      const response = await fetch(selectedRequest.url, options);
      const data = await response.json();

      const durationMs = Date.now() - startTime;
      const executedAt = new Date().toISOString();
      const outputId = generateId();

      // Create as StepOutput (api_request type)
      const stepOutput: StepOutput = {
        id: outputId,
        step_type: "api_request",
        step_name: selectedRequest.name,
        executed_at: executedAt,
        duration_ms: durationMs,
        success: response.status < 400,
        method: selectedRequest.method,
        url: selectedRequest.url,
        status_code: response.status,
        response: data,
        source_config: {
          method: selectedRequest.method,
          url: selectedRequest.url,
          headers:
            Object.keys(selectedRequest.headers).length > 0 ? selectedRequest.headers : undefined,
          body: selectedRequest.body,
          timeout_ms: selectedRequest.timeout_ms,
          follow_redirects: selectedRequest.follow_redirects,
        },
      } as StepOutput;

      const newAnalysis = createAnalysisFromStepOutput(stepOutput);
      setAnalyses((prev) => [...prev, newAnalysis]);
      setAddMode(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsAnalyzing(false);
    }
  }, [selectedRequestId, savedRequests]);

  // Add selected step outputs
  const addStepOutputs = useCallback(() => {
    if (selectedStepOutputIds.length === 0) {
      setError("Please select at least one step output");
      return;
    }

    const selectedOutputs = recentStepOutputs.filter((o) => selectedStepOutputIds.includes(o.id));
    const newAnalyses = selectedOutputs.map((output) => createAnalysisFromStepOutput(output));

    setAnalyses((prev) => [...prev, ...newAnalyses]);
    setSelectedStepOutputIds([]);
    setAddMode(null);
  }, [selectedStepOutputIds, recentStepOutputs]);

  // Capture elements from live browser and create analysis
  const captureLiveBrowserAnalysis = useCallback(() => {
    if (liveBrowser.elements.length === 0) {
      setError("No elements discovered. Connect to a browser tab and refresh elements.");
      return;
    }

    const liveBrowserElements: LiveBrowserElement[] = liveBrowser.elements.map((el: DiscoveredElement) => ({
      id: el.id,
      tagName: el.tagName,
      type: el.type,
      text: el.text,
      label: el.label,
      value: el.value,
      checked: el.checked,
      visible: el.visible,
      enabled: el.enabled,
      bounds: { x: el.bounds.x, y: el.bounds.y, width: el.bounds.width, height: el.bounds.height },
    }));

    const analysisData: LiveBrowserAnalysis = {
      elements: liveBrowserElements,
      url: liveBrowser.connectedTarget?.url ?? "",
      title: liveBrowser.connectedTarget?.name ?? "Browser Tab",
      captured_at: new Date().toISOString(),
    };

    const newAnalysis: CollectedAnalysis = {
      type: "live_browser",
      id: generateId(),
      name: liveBrowser.connectedTarget?.name ?? "Live Capture",
      data: analysisData,
    };

    setAnalyses((prev) => [...prev, newAnalysis]);
    setAddMode(null);
  }, [liveBrowser.elements, liveBrowser.connectedTarget]);

  // Toggle step output selection
  const toggleStepOutputSelection = useCallback((id: string) => {
    setSelectedStepOutputIds((prev) =>
      prev.includes(id) ? prev.filter((i) => i !== id) : [...prev, id],
    );
  }, []);

  // Remove an analysis
  const removeAnalysis = useCallback(
    (id: string) => {
      setAnalyses((prev) => prev.filter((a) => a.id !== id));
      if (expandedId === id) setExpandedId(null);
    },
    [expandedId],
  );

  // Notify parent when analyses change
  useEffect(() => {
    if (analyses.length > 0) {
      const analysisSet: CollectedAnalysisSet = {
        analyses,
        collected_at: new Date().toISOString(),
      };
      onAnalysisComplete({ type: "collected", data: analysisSet });
    }
  }, [analyses, onAnalysisComplete]);

  // Get type badge color
  const getTypeBadge = (type: CollectedAnalysis["type"]) => {
    switch (type) {
      case "playwright":
        return "bg-blue-500/20 text-blue-400";
      case "vision":
        return "bg-green-500/20 text-green-400";
      case "api_request":
        return "bg-purple-500/20 text-purple-400";
      case "step_output":
        return "bg-orange-500/20 text-orange-400";
      case "live_browser":
        return "bg-cyan-500/20 text-cyan-400";
    }
  };

  // Get type label for display
  const getTypeLabel = (analysis: CollectedAnalysis): string => {
    if (analysis.type === "step_output") {
      const stepType = analysis.data.step_type;
      // Capitalize and clean up step type
      return stepType
        .split("_")
        .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
        .join(" ");
    }
    switch (analysis.type) {
      case "playwright":
        return "Playwright";
      case "vision":
        return "Vision";
      case "api_request":
        return "API";
      case "live_browser":
        return "Live";
    }
  };

  // Get element/item count for an analysis
  const getItemCount = (analysis: CollectedAnalysis): string => {
    if (analysis.type === "playwright" || analysis.type === "vision") {
      return `${analysis.data.elements.length} elements`;
    }
    if (analysis.type === "live_browser") {
      return `${analysis.data.elements.length} elements`;
    }
    if (analysis.type === "step_output") {
      const handler = stepOutputRegistry.get(analysis.data.step_type);
      if (handler) {
        const fields = handler.getAssertableFields(analysis.data);
        return `${fields.length} fields`;
      }
      return "Step data";
    }
    return "Response data";
  };

  return (
    <div className="p-4 bg-neutral-900 rounded-lg border border-neutral-700">
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-2">
          <Camera className="w-5 h-5 text-blue-400" />
          <h3 className="text-sm font-medium text-neutral-200">Collected Analyses</h3>
          {analyses.length > 0 && (
            <span className="px-1.5 py-0.5 text-xs bg-blue-500/20 text-blue-400 rounded">
              {analyses.length}
            </span>
          )}
        </div>

        {/* Add Analysis Menu */}
        <div className="relative">
          <button
            onClick={() => setShowAddMenu(!showAddMenu)}
            className="flex items-center gap-1 px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700 transition-colors"
            data-ui-id="test-builder-add-analysis-btn"
          >
            <Plus className="w-4 h-4" />
            Add Analysis
            <ChevronDown
              className={`w-3 h-3 transition-transform ${showAddMenu ? "rotate-180" : ""}`}
            />
          </button>

          {showAddMenu && (
            <div className="absolute right-0 top-full mt-1 w-48 bg-neutral-800 border border-neutral-700 rounded-lg shadow-lg z-10">
              <div className="p-1">
                <button
                  onClick={() => {
                    setAddMode("playwright");
                    setShowAddMenu(false);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-neutral-300 hover:bg-neutral-700 rounded"
                  data-ui-id="test-builder-add-playwright-btn"
                >
                  <Globe className="w-4 h-4 text-blue-400" />
                  Playwright Script
                </button>
                <button
                  onClick={() => {
                    setAddMode("vision");
                    setShowAddMenu(false);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-neutral-300 hover:bg-neutral-700 rounded"
                  data-ui-id="test-builder-add-vision-btn"
                >
                  <Monitor className="w-4 h-4 text-green-400" />
                  Vision Capture
                </button>
                <button
                  onClick={() => {
                    setAddMode("step_output");
                    setStepOutputMode("recent");
                    setShowAddMenu(false);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-neutral-300 hover:bg-neutral-700 rounded"
                  data-ui-id="test-builder-add-step-output-btn"
                >
                  <Play className="w-4 h-4 text-orange-400" />
                  Run Step
                </button>
                <button
                  onClick={() => {
                    setAddMode("live_browser");
                    setShowAddMenu(false);
                    liveBrowser.refreshTargets();
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-neutral-300 hover:bg-neutral-700 rounded"
                  data-ui-id="test-builder-add-live-capture-btn"
                >
                  <Zap className="w-4 h-4 text-cyan-400" />
                  Live Capture
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Add Analysis Panel */}
      {addMode && (
        <div className="mb-4 p-4 bg-neutral-800 border border-neutral-700 rounded-lg">
          <div className="flex items-center justify-between mb-3">
            <h4 className="text-sm font-medium text-neutral-200">
              {addMode === "playwright" && "Add Playwright Analysis"}
              {addMode === "vision" && "Add Vision Analysis"}
              {addMode === "step_output" && "Run Step"}
              {addMode === "live_browser" && "Live Browser Capture"}
            </h4>
            <button
              onClick={() => {
                setAddMode(null);
                setError(null);
              }}
              className="p-1 text-neutral-500 hover:text-neutral-300"
            >
              <X className="w-4 h-4" />
            </button>
          </div>

          {/* Playwright Options */}
          {addMode === "playwright" && (
            <div className="space-y-3">
              {loadingScripts ? (
                <div className="flex items-center gap-2 text-sm text-neutral-500">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Loading scripts...
                </div>
              ) : playwrightScripts.length === 0 ? (
                <p className="text-sm text-neutral-400">
                  No Playwright scripts found. Create one in the Script Builder first.
                </p>
              ) : (
                <select
                  value={selectedScriptId}
                  onChange={(e) => setSelectedScriptId(e.target.value)}
                  className="w-full px-3 py-2 bg-neutral-900 border border-neutral-700 rounded text-sm text-neutral-200 focus:outline-none focus:border-blue-500"
                >
                  {playwrightScripts.map((script) => (
                    <option key={script.id} value={script.id}>
                      {script.name}
                    </option>
                  ))}
                </select>
              )}
            </div>
          )}

          {/* Vision Options */}
          {addMode === "vision" && (
            <div className="space-y-3">
              <MonitorSelector
                selectedMonitors={selectedMonitors}
                onSelectionChange={setSelectedMonitors}
                multiSelect={false}
                compact={true}
              />
            </div>
          )}

          {/* Step Output Options */}
          {addMode === "step_output" && (
            <div className="space-y-3">
              {/* Mode selector tabs */}
              <div className="flex gap-1 p-1 bg-neutral-900 rounded-lg">
                <button
                  onClick={() => setStepOutputMode("recent")}
                  className={`flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-xs rounded transition-colors ${
                    stepOutputMode === "recent"
                      ? "bg-orange-500/20 text-orange-400"
                      : "text-neutral-400 hover:text-neutral-200"
                  }`}
                >
                  <RefreshCw className="w-3 h-3" />
                  Recent Outputs
                </button>
                <button
                  onClick={() => setStepOutputMode("run_api")}
                  className={`flex-1 flex items-center justify-center gap-2 px-3 py-1.5 text-xs rounded transition-colors ${
                    stepOutputMode === "run_api"
                      ? "bg-orange-500/20 text-orange-400"
                      : "text-neutral-400 hover:text-neutral-200"
                  }`}
                >
                  <Network className="w-3 h-3" />
                  Run API Request
                </button>
              </div>

              {/* Recent outputs sub-panel */}
              {stepOutputMode === "recent" && (
                <>
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-neutral-400">
                      Select step outputs from recent workflow execution
                    </span>
                    <button
                      onClick={loadRecentStepOutputs}
                      disabled={loadingStepOutputs}
                      className="flex items-center gap-1 px-2 py-1 text-xs text-neutral-400 hover:text-neutral-200 transition-colors"
                    >
                      <RefreshCw
                        className={`w-3 h-3 ${loadingStepOutputs ? "animate-spin" : ""}`}
                      />
                      Refresh
                    </button>
                  </div>

                  {loadingStepOutputs ? (
                    <div className="flex items-center gap-2 text-sm text-neutral-500">
                      <Loader2 className="w-4 h-4 animate-spin" />
                      Loading step outputs...
                    </div>
                  ) : recentStepOutputs.length === 0 ? (
                    <p className="text-sm text-neutral-400">
                      No recent step outputs found. Run a workflow first.
                    </p>
                  ) : (
                    <div className="max-h-60 overflow-y-auto space-y-1">
                      {recentStepOutputs.map((output) => (
                        <label
                          key={output.id}
                          className={`flex items-center gap-3 p-2 rounded cursor-pointer transition-colors ${
                            selectedStepOutputIds.includes(output.id)
                              ? "bg-orange-500/20 border border-orange-500/50"
                              : "bg-neutral-900 border border-neutral-700 hover:border-neutral-600"
                          }`}
                        >
                          <input
                            type="checkbox"
                            checked={selectedStepOutputIds.includes(output.id)}
                            onChange={() => toggleStepOutputSelection(output.id)}
                            className="sr-only"
                          />
                          <div
                            className={`w-4 h-4 rounded border flex items-center justify-center ${
                              selectedStepOutputIds.includes(output.id)
                                ? "bg-orange-500 border-orange-500"
                                : "border-neutral-600"
                            }`}
                          >
                            {selectedStepOutputIds.includes(output.id) && (
                              <Check className="w-3 h-3 text-white" />
                            )}
                          </div>
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2">
                              <span className="text-sm text-neutral-200 truncate">
                                {output.step_name}
                              </span>
                              <span className="text-xs px-1.5 py-0.5 bg-neutral-700 text-neutral-400 rounded">
                                {output.step_type}
                              </span>
                            </div>
                            <div className="text-xs text-neutral-500 flex items-center gap-2">
                              <span>{output.success ? "Success" : "Failed"}</span>
                              {output.duration_ms && <span>{output.duration_ms}ms</span>}
                            </div>
                          </div>
                        </label>
                      ))}
                    </div>
                  )}

                  {selectedStepOutputIds.length > 0 && (
                    <div className="text-xs text-orange-400">
                      {selectedStepOutputIds.length} output(s) selected
                    </div>
                  )}
                </>
              )}

              {/* Run API Request sub-panel */}
              {stepOutputMode === "run_api" && (
                <>
                  {loadingRequests ? (
                    <div className="flex items-center gap-2 text-sm text-neutral-500">
                      <Loader2 className="w-4 h-4 animate-spin" />
                      Loading API requests...
                    </div>
                  ) : savedRequests.length === 0 ? (
                    <p className="text-sm text-neutral-400">
                      No saved API requests found. Create one in the Library tab first.
                    </p>
                  ) : (
                    <select
                      value={selectedRequestId}
                      onChange={(e) => setSelectedRequestId(e.target.value)}
                      className="w-full px-3 py-2 bg-neutral-900 border border-neutral-700 rounded text-sm text-neutral-200 focus:outline-none focus:border-blue-500"
                    >
                      {savedRequests.map((request) => (
                        <option key={request.id} value={request.id}>
                          [{request.method}] {request.name}
                        </option>
                      ))}
                    </select>
                  )}
                </>
              )}
            </div>
          )}

          {/* Live Browser Options */}
          {addMode === "live_browser" && (
            <div className="space-y-3">
              {/* Extension status */}
              <div className={`flex items-center gap-2 p-2 rounded text-xs ${
                liveBrowser.isExtensionConnected
                  ? "bg-green-500/10 border border-green-500/30 text-green-400"
                  : "bg-amber-500/10 border border-amber-500/30 text-amber-400"
              }`}>
                <Zap className="w-3 h-3" />
                {liveBrowser.isExtensionConnected ? "Extension connected" : "Extension not connected"}
              </div>

              {/* Browser tabs */}
              {liveBrowser.isLoadingTargets ? (
                <div className="flex items-center gap-2 text-sm text-neutral-500">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Loading tabs...
                </div>
              ) : liveBrowser.connectionStatus === "connected" && liveBrowser.connectedTarget ? (
                <div className="space-y-2">
                  <div className="flex items-center gap-2 p-2 bg-cyan-500/10 border border-cyan-500/30 rounded">
                    <div className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
                    <span className="text-sm text-cyan-400 truncate flex-1">{liveBrowser.connectedTarget.name}</span>
                    <button
                      onClick={liveBrowser.disconnect}
                      className="text-xs text-neutral-400 hover:text-neutral-200"
                    >
                      Disconnect
                    </button>
                  </div>
                  <div className="text-xs text-neutral-400">
                    {liveBrowser.elements.length} elements discovered
                    {liveBrowser.isLoadingElements && (
                      <Loader2 className="w-3 h-3 animate-spin inline ml-1" />
                    )}
                  </div>
                </div>
              ) : liveBrowser.browserTabs.length === 0 ? (
                <p className="text-sm text-neutral-400">
                  No browser tabs found. Make sure the extension is installed and enabled.
                </p>
              ) : (
                <div className="max-h-48 overflow-y-auto space-y-1">
                  {liveBrowser.browserTabs.map((tab) => (
                    <div
                      key={tab.id}
                      className="flex items-center gap-2 p-2 bg-neutral-900 border border-neutral-700 rounded hover:border-neutral-600"
                    >
                      <Globe className="w-3.5 h-3.5 text-neutral-500 flex-shrink-0" />
                      <div className="flex-1 min-w-0">
                        <p className="text-xs text-neutral-200 truncate">{tab.title}</p>
                        <p className="text-[10px] text-neutral-500 truncate">{tab.url}</p>
                      </div>
                      <button
                        onClick={() => liveBrowser.connectToTab(tab.id)}
                        disabled={liveBrowser.connectionStatus === "connecting"}
                        className="flex items-center gap-1 px-2 py-1 text-xs bg-cyan-600 hover:bg-cyan-500 text-white rounded transition-colors disabled:opacity-50"
                      >
                        {liveBrowser.connectionStatus === "connecting" ? (
                          <Loader2 className="w-3 h-3 animate-spin" />
                        ) : (
                          <Play className="w-3 h-3" />
                        )}
                        Connect
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* Error display */}
          {error && (
            <div className="mt-3 p-2 bg-red-900/30 border border-red-700 rounded flex items-start gap-2">
              <AlertCircle className="w-4 h-4 text-red-400 mt-0.5 flex-shrink-0" />
              <p className="text-xs text-red-300">{error}</p>
            </div>
          )}

          {/* Run button */}
          <button
            onClick={() => {
              if (addMode === "playwright") runPlaywrightAnalysis();
              else if (addMode === "vision") runVisionAnalysis();
              else if (addMode === "live_browser") captureLiveBrowserAnalysis();
              else if (addMode === "step_output") {
                if (stepOutputMode === "recent") addStepOutputs();
                else if (stepOutputMode === "run_api") runApiRequestAnalysis();
              }
            }}
            disabled={
              isAnalyzing ||
              (addMode === "playwright" && !selectedScriptId) ||
              (addMode === "live_browser" && (liveBrowser.connectionStatus !== "connected" || liveBrowser.elements.length === 0)) ||
              (addMode === "step_output" &&
                stepOutputMode === "recent" &&
                selectedStepOutputIds.length === 0) ||
              (addMode === "step_output" && stepOutputMode === "run_api" && !selectedRequestId)
            }
            className="mt-3 w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            data-ui-id="test-builder-run-analysis-btn"
          >
            {isAnalyzing ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                {stepOutputMode === "run_api" ? "Running..." : "Analyzing..."}
              </>
            ) : addMode === "live_browser" ? (
              <>
                <Zap className="w-4 h-4" />
                Capture Elements ({liveBrowser.elements.length})
              </>
            ) : addMode === "step_output" && stepOutputMode === "recent" ? (
              <>
                <Plus className="w-4 h-4" />
                Add Selected Outputs
              </>
            ) : addMode === "step_output" && stepOutputMode === "run_api" ? (
              <>
                <Play className="w-4 h-4" />
                Run & Collect Output
              </>
            ) : (
              <>
                <Camera className="w-4 h-4" />
                Run Analysis
              </>
            )}
          </button>
        </div>
      )}

      {/* Analyses Table */}
      {analyses.length === 0 ? (
        <div className="p-6 bg-neutral-800/50 border border-neutral-700 rounded-lg text-center">
          <Camera className="w-8 h-8 text-neutral-600 mx-auto mb-2" />
          <p className="text-sm text-neutral-400">No analyses collected yet.</p>
          <p className="text-xs text-neutral-500 mt-1">
            Add analyses to provide ground truth data for AI test generation.
          </p>
        </div>
      ) : (
        <div className="border border-neutral-700 rounded-lg overflow-hidden">
          {/* Table Header */}
          <div className="grid grid-cols-[auto_1fr_auto_auto] gap-4 px-4 py-2 bg-neutral-800 text-xs font-medium text-neutral-400 border-b border-neutral-700">
            <div>Type</div>
            <div>Name</div>
            <div>Data</div>
            <div>Actions</div>
          </div>

          {/* Table Rows */}
          {analyses.map((analysis) => (
            <div key={analysis.id}>
              <div className="grid grid-cols-[auto_1fr_auto_auto] gap-4 px-4 py-3 items-center hover:bg-neutral-800/50">
                <span
                  className={`px-2 py-0.5 text-xs font-medium rounded ${getTypeBadge(analysis.type)}`}
                >
                  {getTypeLabel(analysis)}
                </span>
                <span className="text-sm text-neutral-200 truncate">{analysis.name}</span>
                <span className="text-xs text-neutral-400">{getItemCount(analysis)}</span>
                <div className="flex items-center gap-1">
                  <button
                    onClick={() => setExpandedId(expandedId === analysis.id ? null : analysis.id)}
                    className="p-1.5 text-neutral-500 hover:text-neutral-300 transition-colors"
                    title="View details"
                  >
                    <Eye className="w-4 h-4" />
                  </button>
                  <button
                    onClick={() => removeAnalysis(analysis.id)}
                    className="p-1.5 text-neutral-500 hover:text-red-400 transition-colors"
                    title="Remove"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                </div>
              </div>

              {/* Expanded Details */}
              {expandedId === analysis.id && (
                <div className="px-4 py-3 bg-neutral-850 border-t border-neutral-700">
                  {(analysis.type === "playwright" || analysis.type === "vision") && (
                    <div className="space-y-2">
                      <div className="flex items-center gap-4 text-xs text-neutral-400">
                        <span>Elements: {analysis.data.elements.length}</span>
                        {analysis.data.url && <span>URL: {analysis.data.url}</span>}
                        <span>
                          Captured: {new Date(analysis.data.captured_at).toLocaleString()}
                        </span>
                      </div>
                      <div className="max-h-48 overflow-auto">
                        <table className="w-full text-xs">
                          <thead className="bg-neutral-800">
                            <tr>
                              <th className="px-2 py-1 text-left text-neutral-400">#</th>
                              <th className="px-2 py-1 text-left text-neutral-400">Label</th>
                              <th className="px-2 py-1 text-left text-neutral-400">Type</th>
                              <th className="px-2 py-1 text-left text-neutral-400">Text</th>
                            </tr>
                          </thead>
                          <tbody>
                            {analysis.data.elements.slice(0, 20).map((el, idx) => (
                              <tr key={el.id} className="border-t border-neutral-700">
                                <td className="px-2 py-1 text-neutral-500">{idx + 1}</td>
                                <td className="px-2 py-1 text-neutral-200">{el.label}</td>
                                <td className="px-2 py-1 text-neutral-400">{el.element_type}</td>
                                <td className="px-2 py-1 text-neutral-400 truncate max-w-[200px]">
                                  {el.text_content || "-"}
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                        {analysis.data.elements.length > 20 && (
                          <p className="text-xs text-neutral-500 mt-2">
                            ...and {analysis.data.elements.length - 20} more elements
                          </p>
                        )}
                      </div>
                    </div>
                  )}

                  {analysis.type === "api_request" && (
                    <div className="space-y-2">
                      <div className="flex items-center gap-4 text-xs text-neutral-400">
                        <span className="font-mono">{analysis.data.method}</span>
                        <span className="truncate">{analysis.data.url}</span>
                        {analysis.data.status_code && (
                          <span
                            className={
                              analysis.data.status_code < 400 ? "text-green-400" : "text-red-400"
                            }
                          >
                            Status: {analysis.data.status_code}
                          </span>
                        )}
                        {analysis.data.duration_ms && <span>{analysis.data.duration_ms}ms</span>}
                      </div>
                      <div className="max-h-48 overflow-auto p-2 bg-neutral-900 rounded border border-neutral-700">
                        <pre className="text-xs text-neutral-300 font-mono whitespace-pre-wrap">
                          {JSON.stringify(analysis.data.response, null, 2)}
                        </pre>
                      </div>
                    </div>
                  )}

                  {analysis.type === "step_output" && (
                    <div className="space-y-3">
                      <div className="flex items-center gap-4 text-xs text-neutral-400">
                        <span className="font-mono">{analysis.data.step_type}</span>
                        <span className={analysis.data.success ? "text-green-400" : "text-red-400"}>
                          {analysis.data.success ? "Success" : "Failed"}
                        </span>
                        {analysis.data.duration_ms && <span>{analysis.data.duration_ms}ms</span>}
                        <span>
                          Executed: {new Date(analysis.data.executed_at).toLocaleString()}
                        </span>
                      </div>

                      {/* API Request specific info */}
                      {analysis.data.step_type === "api_request" && (
                        <div className="flex items-center gap-4 text-xs text-neutral-400">
                          <span className="font-mono font-semibold text-blue-400">
                            {(analysis.data as { method?: string }).method}
                          </span>
                          <span className="truncate text-neutral-300">
                            {(analysis.data as { url?: string }).url}
                          </span>
                          {(analysis.data as { status_code?: number }).status_code && (
                            <span
                              className={
                                ((analysis.data as { status_code?: number }).status_code ?? 0) < 400
                                  ? "text-green-400"
                                  : "text-red-400"
                              }
                            >
                              Status: {(analysis.data as { status_code?: number }).status_code}
                            </span>
                          )}
                        </div>
                      )}

                      {analysis.data.error && (
                        <div className="p-2 bg-red-900/30 border border-red-700 rounded text-xs text-red-300">
                          Error: {analysis.data.error}
                        </div>
                      )}

                      {/* Response Data (for API requests) */}
                      {analysis.data.step_type === "api_request" &&
                        (analysis.data as { response?: unknown }).response && (
                          <div className="space-y-1">
                            <span className="text-xs font-medium text-neutral-300">
                              Response Data:
                            </span>
                            <div className="max-h-64 overflow-auto p-2 bg-neutral-900 rounded border border-neutral-700">
                              <pre className="text-xs text-neutral-300 font-mono whitespace-pre-wrap">
                                {JSON.stringify(
                                  (analysis.data as { response?: unknown }).response,
                                  null,
                                  2,
                                )}
                              </pre>
                            </div>
                          </div>
                        )}

                      {/* Assertable Fields */}
                      <div className="space-y-1">
                        <span className="text-xs font-medium text-neutral-300">
                          Assertable Fields:
                        </span>
                        <div className="max-h-48 overflow-auto">
                          {(() => {
                            const handler = stepOutputRegistry.get(analysis.data.step_type);
                            if (!handler) return null;
                            const fields = handler.getAssertableFields(analysis.data);
                            return (
                              <table className="w-full text-xs">
                                <thead className="bg-neutral-800">
                                  <tr>
                                    <th className="px-2 py-1 text-left text-neutral-400">Field</th>
                                    <th className="px-2 py-1 text-left text-neutral-400">Type</th>
                                    <th className="px-2 py-1 text-left text-neutral-400">Value</th>
                                  </tr>
                                </thead>
                                <tbody>
                                  {fields.slice(0, 15).map((field, idx) => (
                                    <tr key={idx} className="border-t border-neutral-700">
                                      <td className="px-2 py-1 text-neutral-200">{field.name}</td>
                                      <td className="px-2 py-1 text-neutral-400">{field.type}</td>
                                      <td className="px-2 py-1 text-neutral-400 truncate max-w-[200px]">
                                        {String(field.example_value ?? "-")}
                                      </td>
                                    </tr>
                                  ))}
                                </tbody>
                              </table>
                            );
                          })()}
                        </div>
                      </div>
                    </div>
                  )}

                  {analysis.type === "live_browser" && (
                    <div className="space-y-2">
                      <div className="flex items-center gap-4 text-xs text-neutral-400">
                        <span>Elements: {analysis.data.elements.length}</span>
                        {analysis.data.url && <span>URL: {analysis.data.url}</span>}
                        <span>
                          Captured: {new Date(analysis.data.captured_at).toLocaleString()}
                        </span>
                      </div>
                      <div className="max-h-48 overflow-auto">
                        <table className="w-full text-xs">
                          <thead className="bg-neutral-800">
                            <tr>
                              <th className="px-2 py-1 text-left text-neutral-400">#</th>
                              <th className="px-2 py-1 text-left text-neutral-400">ID</th>
                              <th className="px-2 py-1 text-left text-neutral-400">Type</th>
                              <th className="px-2 py-1 text-left text-neutral-400">Text</th>
                            </tr>
                          </thead>
                          <tbody>
                            {analysis.data.elements.slice(0, 20).map((el, idx) => (
                              <tr key={el.id} className="border-t border-neutral-700">
                                <td className="px-2 py-1 text-neutral-500">{idx + 1}</td>
                                <td className="px-2 py-1 text-neutral-200 font-mono text-[10px]">{el.id}</td>
                                <td className="px-2 py-1 text-neutral-400">{el.type}</td>
                                <td className="px-2 py-1 text-neutral-400 truncate max-w-[200px]">
                                  {el.text || el.label || "-"}
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                        {analysis.data.elements.length > 20 && (
                          <p className="text-xs text-neutral-500 mt-2">
                            ...and {analysis.data.elements.length - 20} more elements
                          </p>
                        )}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Summary */}
      {analyses.length > 0 && (
        <div className="mt-4 p-3 bg-neutral-800/50 border border-neutral-700 rounded-lg">
          <div className="flex items-center gap-2 text-sm">
            <Check className="w-4 h-4 text-green-400" />
            <span className="text-neutral-200">
              {analyses.length} {analyses.length === 1 ? "analysis" : "analyses"} ready for AI test
              generation
            </span>
          </div>
          <p className="text-xs text-neutral-500 mt-1">
            Switch to the AI Generator tab and describe the verification test you want to create.
          </p>
        </div>
      )}
    </div>
  );
}
