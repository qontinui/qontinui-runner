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
} from "lucide-react";
import { MonitorSelector } from "../MonitorSelector";
import type {
  PageAnalysis,
  CommandResponse,
  CollectedAnalysis,
  CollectedAnalysisSet,
  ApiRequestAnalysis,
  AnalysisData,
} from "./types";
import type { SavedApiRequest } from "../../types";

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
}

export function PageAnalyzer({ onAnalysisComplete, onError }: PageAnalyzerProps) {
  // Collected analyses
  const [analyses, setAnalyses] = useState<CollectedAnalysis[]>([]);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  // Add analysis state
  const [showAddMenu, setShowAddMenu] = useState(false);
  const [addMode, setAddMode] = useState<"playwright" | "vision" | "api_request" | null>(null);
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

  // Load Playwright scripts when mode changes
  useEffect(() => {
    if (addMode === "playwright" && playwrightScripts.length === 0) {
      loadPlaywrightScripts();
    }
  }, [addMode, playwrightScripts.length]);

  // Load API requests when mode changes
  useEffect(() => {
    if (addMode === "api_request" && savedRequests.length === 0) {
      loadSavedRequests();
    }
  }, [addMode, savedRequests.length]);

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

  // Run API Request analysis
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

      const apiAnalysis: ApiRequestAnalysis = {
        id: generateId(),
        request_name: selectedRequest.name,
        method: selectedRequest.method,
        url: selectedRequest.url,
        response: data,
        status_code: response.status,
        duration_ms: Date.now() - startTime,
        executed_at: new Date().toISOString(),
        // Include source config for auto-populating api_request_config in tests
        source_request_config: {
          method: selectedRequest.method,
          url: selectedRequest.url,
          headers:
            Object.keys(selectedRequest.headers).length > 0 ? selectedRequest.headers : undefined,
          body: selectedRequest.body,
          timeout_ms: selectedRequest.timeout_ms,
          follow_redirects: selectedRequest.follow_redirects,
        },
      };

      const newAnalysis: CollectedAnalysis = {
        type: "api_request",
        id: apiAnalysis.id,
        name: selectedRequest.name,
        data: apiAnalysis,
      };
      setAnalyses((prev) => [...prev, newAnalysis]);
      setAddMode(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setIsAnalyzing(false);
    }
  }, [selectedRequestId, savedRequests]);

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
    }
  };

  // Get element/item count for an analysis
  const getItemCount = (analysis: CollectedAnalysis): string => {
    if (analysis.type === "playwright" || analysis.type === "vision") {
      return `${analysis.data.elements.length} elements`;
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
                >
                  <Monitor className="w-4 h-4 text-green-400" />
                  Vision Capture
                </button>
                <button
                  onClick={() => {
                    setAddMode("api_request");
                    setShowAddMenu(false);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-neutral-300 hover:bg-neutral-700 rounded"
                >
                  <Network className="w-4 h-4 text-purple-400" />
                  API Request
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
              {addMode === "api_request" && "Add API Request Analysis"}
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

          {/* API Request Options */}
          {addMode === "api_request" && (
            <div className="space-y-3">
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
              else if (addMode === "api_request") runApiRequestAnalysis();
            }}
            disabled={
              isAnalyzing ||
              (addMode === "playwright" && !selectedScriptId) ||
              (addMode === "api_request" && !selectedRequestId)
            }
            className="mt-3 w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            {isAnalyzing ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Analyzing...
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
                  {analysis.type === "playwright" && "Playwright"}
                  {analysis.type === "vision" && "Vision"}
                  {analysis.type === "api_request" && "API"}
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
