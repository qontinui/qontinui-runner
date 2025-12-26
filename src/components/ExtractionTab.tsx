/**
 * ExtractionTab.tsx
 *
 * Dedicated tab for web extraction operations.
 * Allows users to extract UI elements and states from web pages for automation.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Globe,
  Play,
  Square,
  Plus,
  Trash2,
  Settings2,
  Loader2,
  Check,
  X,
  Cloud,
  Monitor,
  ChevronUp,
  ChevronDown,
  AlertCircle,
  Zap,
} from "lucide-react";
import { MonitorSelector } from "./MonitorSelector";
import type {
  WebExtractionConfig,
  ExtractionStatus,
  ExtractionCompletedPayload,
} from "../types/extraction";
import type { Project } from "../types/auth";
import { useExecution } from "../contexts";
import { eventRouter } from "../managers";
import { useExtractionState } from "../hooks";

interface MonitorInfo {
  index: number;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
  is_primary: boolean;
  name?: string;
}

interface ExtractionTabProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  projects: Project[];
  selectedProjectId: string | null;
  selectedProjectName: string | null;
}

export function ExtractionTab({
  onLog,
  projects,
  selectedProjectId,
  selectedProjectName,
}: ExtractionTabProps) {
  // Get Python executor status from context
  const { pythonStatus } = useExecution();

  // Persisted extraction state (survives tab switches and app restarts)
  const extractionState = useExtractionState();
  const {
    urls,
    addUrl,
    removeUrl,
    selectedMonitor,
    setSelectedMonitor,
    captureHoverStates,
    setCaptureHoverStates,
    captureFocusStates,
    setCaptureFocusStates,
    captureScrollStates,
    setCaptureScrollStates,
    maxDepth,
    setMaxDepth,
    maxPages,
    setMaxPages,
    showAdvanced,
    setShowAdvanced,
  } = extractionState;

  // URL input (not persisted - just for the input field)
  const [newUrl, setNewUrl] = useState("");

  // Monitor list (from system, not persisted - used for resolution calculation)
  const [availableMonitors, setAvailableMonitors] = useState<MonitorInfo[]>([]);

  // Transient UI state (not persisted)
  const [isExtracting, setIsExtracting] = useState(false);
  const [extractionProgress, setExtractionProgress] = useState<{
    pages_extracted: number;
    total_pages: number;
    current_url: string;
  } | null>(null);
  const [extractionError, setExtractionError] = useState<string | null>(null);
  const [extractionSuccess, setExtractionSuccess] = useState(false);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);

  // Load available monitors on mount
  useEffect(() => {
    const loadMonitors = async () => {
      try {
        const response = await invoke<{
          success: boolean;
          message?: string;
          data?: {
            count: number;
            monitors: MonitorInfo[];
          };
        }>("get_monitors");

        if (response.success && response.data?.monitors) {
          // Sort monitors by x position (left to right) for consistent spatial ordering
          const sortedMonitors = [...response.data.monitors].sort((a, b) => a.x - b.x);
          setAvailableMonitors(sortedMonitors);
          // Default to primary monitor or first one (find in sorted array)
          const primaryMonitor = sortedMonitors.find((m) => m.is_primary);
          setSelectedMonitor(
            primaryMonitor ? primaryMonitor.index : (sortedMonitors[0]?.index ?? 0),
          );
          onLog("debug", `Found ${sortedMonitors.length} monitor(s)`);
        } else {
          onLog("error", "No monitors found in response");
        }
      } catch (err) {
        console.error("Failed to load monitors:", err);
        onLog("error", `Failed to load monitors: ${err}`);
      }
    };
    loadMonitors();
  }, [onLog]);

  // Listen for extraction events via EventRouter
  useEffect(() => {
    // Handler for extraction_started event
    const handleExtractionStarted = (payload: unknown) => {
      console.log("[EXTRACTION_TAB] Extraction started:", payload);
      onLog("info", "Extraction process started");
      setIsExtracting(true);
      setExtractionError(null);
    };

    // Handler for extraction_progress event
    const handleExtractionProgress = (payload: unknown) => {
      console.log("[EXTRACTION_TAB] Extraction progress:", payload);
      const data =
        payload && typeof payload === "object" && "data" in payload ? payload.data : null;
      if (data && typeof data === "object" && data !== null) {
        const typedData = data as {
          pages_extracted?: number;
          total_pages?: number;
          current_url?: string;
        };
        setExtractionProgress({
          pages_extracted: typedData.pages_extracted || 0,
          total_pages: typedData.total_pages || urls.length,
          current_url: typedData.current_url || "",
        });
      }
    };

    // Handler for extraction_completed event
    const handleExtractionCompleted = (payload: unknown) => {
      console.log("[EXTRACTION_TAB] Extraction completed:", payload);
      setIsExtracting(false);
      setExtractionSuccess(true);
      setExtractionError(null);
      onLog("success", "Web extraction completed successfully");

      // Extract data from payload (handle both direct and nested formats)
      let data: ExtractionCompletedPayload | null = null;
      if (payload && typeof payload === "object") {
        if ("data" in payload && payload.data && typeof payload.data === "object") {
          data = payload.data as ExtractionCompletedPayload;
        } else if ("session_id" in payload || "state_structure" in payload) {
          data = payload as ExtractionCompletedPayload;
        }
      }

      const sessionId = data?.session_id || currentSessionId;
      const stateStructure = data?.state_structure;

      // Automatically upload state structure to qontinui-web if available
      if (sessionId && stateStructure) {
        // Use Promise for async operation without making handler async
        void (async () => {
          try {
            onLog("info", "Uploading state structure to cloud...");
            await invoke("upload_state_structure", {
              request: {
                extraction_id: sessionId,
                state_structure: stateStructure,
              },
            });
            onLog("success", "State structure uploaded successfully");
          } catch (uploadErr) {
            console.error("Failed to upload state structure:", uploadErr);
            onLog(
              "warning",
              `Could not upload state structure: ${uploadErr}. The extraction data is still saved locally.`,
            );
          }
        })();
      } else if (stateStructure && !sessionId) {
        onLog(
          "warning",
          "State structure generated but no cloud session - data saved locally only",
        );
      }

      // Clear session ID after completion
      setCurrentSessionId(null);
      setTimeout(() => setExtractionSuccess(false), 3000);
    };

    // Handler for extraction_error event
    const handleExtractionError = (payload: unknown) => {
      console.log("[EXTRACTION_TAB] Extraction error:", payload);
      let errorMsg = "Extraction failed";
      if (payload && typeof payload === "object") {
        if ("data" in payload && payload.data && typeof payload.data === "object") {
          const data = payload.data as { message?: string };
          errorMsg = data.message || errorMsg;
        } else if ("message" in payload && typeof payload.message === "string") {
          errorMsg = payload.message;
        }
      }
      setIsExtracting(false);
      setExtractionError(errorMsg);
      onLog("error", `Extraction failed: ${errorMsg}`);
    };

    // Subscribe to all extraction-related events
    const unsubStarted = eventRouter.subscribe("extraction_started", handleExtractionStarted);
    const unsubProgress = eventRouter.subscribe("extraction_progress", handleExtractionProgress);
    const unsubCompleted = eventRouter.subscribe("extraction_completed", handleExtractionCompleted);
    const unsubError = eventRouter.subscribe("extraction_error", handleExtractionError);

    // Also listen for the old-style executor-event for backward compatibility
    let unlistenLegacy: (() => void) | undefined;
    listen(
      "executor-event",
      (event: {
        payload?: {
          data?: {
            message?: string;
            extraction_status?: ExtractionStatus;
          };
        };
      }) => {
        const data = event.payload?.data;
        if (data?.message === "extraction_progress_update" && data.extraction_status) {
          const status = data.extraction_status;
          setExtractionProgress(status.progress);

          if (status.status === "completed") {
            setIsExtracting(false);
            setExtractionSuccess(true);
            setExtractionError(null);
            onLog("success", "Web extraction completed successfully");
            setTimeout(() => setExtractionSuccess(false), 3000);
          } else if (status.status === "failed") {
            setIsExtracting(false);
            setExtractionError(status.error || "Extraction failed");
            onLog("error", `Extraction failed: ${status.error || "Unknown error"}`);
          }
        }
      },
    ).then((fn) => {
      unlistenLegacy = fn;
    });

    return () => {
      unsubStarted();
      unsubProgress();
      unsubCompleted();
      unsubError();
      if (unlistenLegacy) unlistenLegacy();
    };
  }, [onLog, urls.length, currentSessionId]);

  const validateUrl = (url: string): boolean => {
    try {
      new URL(url);
      return true;
    } catch {
      return false;
    }
  };

  const handleAddUrl = () => {
    const trimmedUrl = newUrl.trim();
    if (!trimmedUrl) {
      onLog("warning", "Please enter a URL");
      return;
    }

    if (!validateUrl(trimmedUrl)) {
      onLog("error", "Invalid URL format. Must start with http:// or https://");
      return;
    }

    if (urls.includes(trimmedUrl)) {
      onLog("warning", "URL already added");
      return;
    }

    addUrl(trimmedUrl);
    setNewUrl("");
    onLog("info", `Added URL: ${trimmedUrl}`);
  };

  const handleRemoveUrl = (index: number) => {
    const removedUrl = urls[index];
    removeUrl(index);
    onLog("info", `Removed URL: ${removedUrl}`);
  };

  // Get physical resolution from selected monitor
  const getMonitorResolution = (): [number, number] => {
    // Find monitor by its system index (not array position)
    const monitor = availableMonitors.find((m) => m.index === selectedMonitor);
    if (monitor) {
      return [monitor.width, monitor.height];
    }
    // Fallback to common resolution
    return [1920, 1080];
  };

  const handleStartExtraction = async () => {
    if (urls.length === 0) {
      setExtractionError("Please add at least one URL");
      onLog("error", "Cannot start extraction: no URLs added");
      return;
    }

    if (!selectedProjectId) {
      setExtractionError("Please select a project in Settings first");
      onLog("error", "Cannot start extraction: no project selected");
      return;
    }

    // Check if Python executor is running
    if (pythonStatus !== "running") {
      onLog("info", "Python executor not running, attempting to start...");
      try {
        const startResult = await invoke<{ success: boolean; message?: string }>(
          "start_python_executor",
        );
        if (!startResult.success) {
          // Check if it's actually already running
          if (startResult.message?.toLowerCase().includes("already running")) {
            onLog("info", "Python executor is already running");
          } else {
            setExtractionError(
              "Failed to start Python executor. Please try loading a configuration first.",
            );
            onLog(
              "error",
              `Python executor failed to start: ${startResult.message || "Unknown error"}`,
            );
            return;
          }
        } else {
          onLog("success", "Python executor started");
          // Give it a moment to initialize
          await new Promise((resolve) => setTimeout(resolve, 1000));
        }
      } catch (err) {
        setExtractionError(
          "Failed to start Python executor. Please try loading a configuration first.",
        );
        onLog("error", `Failed to start Python executor: ${err}`);
        return;
      }
    }

    setIsExtracting(true);
    setExtractionError(null);
    setExtractionSuccess(false);
    setExtractionProgress({ pages_extracted: 0, total_pages: urls.length, current_url: urls[0] });

    try {
      const resolution = getMonitorResolution();
      const config: WebExtractionConfig = {
        urls,
        viewport: resolution, // Single resolution matching target monitor
        capture_hover_states: captureHoverStates,
        capture_focus_states: captureFocusStates,
        capture_scroll_states: captureScrollStates,
        max_depth: maxDepth,
        max_pages: maxPages,
      };

      // Step 1: Create extraction session in qontinui-web backend
      onLog("info", "Creating extraction session in cloud...");
      let sessionId: string | null = null;

      try {
        const sessionResponse = await invoke<{
          id: string;
          project_id: string;
          status: string;
        }>("create_extraction_session", {
          request: {
            project_id: selectedProjectId,
            source_urls: urls,
            config,
          },
        });

        sessionId = sessionResponse.id;
        setCurrentSessionId(sessionId); // Store for use in completion handler
        onLog("success", `Cloud session created: ${sessionId}`);
      } catch (sessionErr) {
        // Log warning but continue - extraction can still work locally
        console.warn("Failed to create cloud session:", sessionErr);
        onLog(
          "warning",
          `Could not create cloud session: ${sessionErr}. Continuing with local extraction.`,
        );
      }

      // Step 2: Start the actual extraction via Python bridge
      const result = await invoke<{ success: boolean; error?: string }>("start_web_extraction", {
        config: {
          ...config,
          project_id: selectedProjectId,
          session_id: sessionId, // Pass session ID so Python can update it
        },
      });

      if (result?.success) {
        onLog("info", "Web extraction started");
      } else {
        const errorMsg = result?.error || "Unknown error";
        setExtractionError(errorMsg);
        setIsExtracting(false);
        onLog("error", `Failed to start extraction: ${errorMsg}`);

        // Update session status to failed if we have one
        if (sessionId) {
          try {
            await invoke("update_extraction_session", {
              request: {
                extraction_id: sessionId,
                status: "failed",
                error_message: errorMsg,
              },
            });
          } catch (updateErr) {
            console.error("Failed to update session status:", updateErr);
          }
        }
      }
    } catch (err) {
      console.error("Failed to start extraction:", err);
      setExtractionError(`Failed: ${err}`);
      setIsExtracting(false);
      onLog("error", `Failed to start extraction: ${err}`);
    }
  };

  const handleStopExtraction = async () => {
    try {
      const result = await invoke<{ success: boolean; error?: string }>("stop_web_extraction");

      if (result?.success) {
        setIsExtracting(false);
        setExtractionProgress(null);
        onLog("info", "Web extraction stopped");
      } else {
        const errorMsg = result?.error || "Unknown error";
        onLog("error", `Failed to stop extraction: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to stop extraction:", err);
      onLog("error", `Failed to stop extraction: ${err}`);
    }
  };

  return (
    <div className="space-y-6 p-6 overflow-y-auto max-h-[calc(100vh-200px)]">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Globe className="w-6 h-6 text-primary" />
          <div>
            <h2 className="text-xl font-semibold">Web Extraction</h2>
            <p className="text-sm text-muted-foreground">
              Extract UI elements and states from web pages for automation
            </p>
          </div>
        </div>

        {/* Python Status Indicator */}
        <div className="flex items-center gap-2">
          <Zap
            className={`w-4 h-4 ${pythonStatus === "running" ? "text-green-400" : "text-yellow-400"}`}
          />
          <span
            className={`text-sm ${pythonStatus === "running" ? "text-green-400" : "text-yellow-400"}`}
          >
            {pythonStatus === "running" ? "Python Ready" : "Python Starting..."}
          </span>
        </div>
      </div>

      {/* URL Input Section */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Globe className="w-5 h-5 text-primary" />
          <h3 className="font-semibold text-lg">URLs to Extract</h3>
        </div>

        {/* URL Input */}
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={newUrl}
            onChange={(e) => setNewUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                handleAddUrl();
              }
            }}
            placeholder="https://example.com"
            className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md"
          />
          <button
            onClick={handleAddUrl}
            className="px-4 py-2 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors flex items-center gap-2"
          >
            <Plus className="w-4 h-4" />
            Add URL
          </button>
        </div>

        {/* URL List */}
        {urls.length > 0 ? (
          <div className="space-y-2">
            <div className="text-sm font-medium text-muted-foreground">
              {urls.length} URL{urls.length !== 1 ? "s" : ""} added
            </div>
            {urls.map((url, index) => (
              <div
                key={index}
                className="flex items-center justify-between p-3 bg-input border border-border/50 rounded-md"
              >
                <span className="text-sm truncate flex-1">{url}</span>
                <button
                  onClick={() => handleRemoveUrl(index)}
                  className="ml-2 p-1 bg-red-500/10 hover:bg-red-500/20 text-red-400 rounded transition-colors"
                >
                  <Trash2 className="w-4 h-4" />
                </button>
              </div>
            ))}
          </div>
        ) : (
          <div className="text-sm text-muted-foreground italic">
            No URLs added yet. Add at least one URL to start extraction.
          </div>
        )}
      </div>

      {/* Target Monitor Selection */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Monitor className="w-5 h-5 text-primary" />
          <div>
            <h3 className="font-semibold text-lg">Target Monitor</h3>
            <p className="text-sm text-muted-foreground">
              Screenshots will be captured at this monitor's resolution for pattern matching
            </p>
          </div>
        </div>

        {/* Monitor Card Selector */}
        <MonitorSelector
          selectedMonitors={[selectedMonitor]}
          onSelectionChange={(indices) => {
            if (indices.length > 0) {
              setSelectedMonitor(indices[0]);
            }
          }}
          multiSelect={false}
        />
      </div>

      {/* Target Project Display */}
      <div className="space-y-2 bg-card rounded-lg border border-border/50 p-6">
        <div className="font-medium">Target Project</div>
        {selectedProjectId ? (
          <div className="flex items-center gap-2 px-3 py-2 bg-input border border-border/50 rounded-md">
            <Cloud className="w-4 h-4 text-primary" />
            <span className="font-medium">
              {selectedProjectName ||
                projects.find((p) => p.id === selectedProjectId)?.name ||
                "Loading..."}
            </span>
          </div>
        ) : (
          <div className="p-3 bg-orange-500/10 border border-orange-500/30 rounded-lg flex items-start gap-2">
            <AlertCircle className="w-5 h-5 text-orange-400 shrink-0 mt-0.5" />
            <span className="text-orange-400 text-sm">
              No project selected. Go to Settings → Connection to select a project.
            </span>
          </div>
        )}
      </div>

      {/* Advanced Configuration */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <button
          onClick={() => setShowAdvanced(!showAdvanced)}
          className="flex items-center justify-between w-full"
        >
          <div className="flex items-center gap-3">
            <Settings2 className="w-5 h-5 text-primary" />
            <h3 className="font-semibold text-lg">Advanced Options</h3>
          </div>
          {showAdvanced ? (
            <ChevronUp className="w-5 h-5 text-muted-foreground" />
          ) : (
            <ChevronDown className="w-5 h-5 text-muted-foreground" />
          )}
        </button>

        {showAdvanced && (
          <div className="space-y-4 pt-4 border-t border-border/50">
            {/* Max Depth */}
            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">Maximum Navigation Depth</div>
                <div className="text-sm text-muted-foreground mb-3">
                  How many levels deep to follow links (1-10)
                </div>
                <input
                  type="number"
                  min="1"
                  max="10"
                  value={maxDepth}
                  onChange={(e) =>
                    setMaxDepth(Math.min(10, Math.max(1, parseInt(e.target.value) || 1)))
                  }
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            {/* Max Pages */}
            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">Maximum Pages to Extract</div>
                <div className="text-sm text-muted-foreground mb-3">
                  Stop after extracting this many pages (1-500)
                </div>
                <input
                  type="number"
                  min="1"
                  max="500"
                  value={maxPages}
                  onChange={(e) =>
                    setMaxPages(Math.min(500, Math.max(1, parseInt(e.target.value) || 1)))
                  }
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            {/* State Capture Options */}
            <div className="space-y-3">
              <div className="font-medium">State Capture Options</div>
              <div className="text-sm text-muted-foreground mb-3">
                Choose which UI states to capture during extraction
              </div>

              {/* Hover States */}
              <label className="flex items-center justify-between cursor-pointer">
                <div className="space-y-1">
                  <div className="font-medium">Capture Hover States</div>
                  <div className="text-sm text-muted-foreground">
                    Record how elements look when hovered over
                  </div>
                </div>
                <button
                  onClick={() => setCaptureHoverStates(!captureHoverStates)}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    captureHoverStates ? "bg-primary" : "bg-muted"
                  }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      captureHoverStates ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
              </label>

              {/* Focus States */}
              <label className="flex items-center justify-between cursor-pointer">
                <div className="space-y-1">
                  <div className="font-medium">Capture Focus States</div>
                  <div className="text-sm text-muted-foreground">
                    Record how form inputs look when focused
                  </div>
                </div>
                <button
                  onClick={() => setCaptureFocusStates(!captureFocusStates)}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    captureFocusStates ? "bg-primary" : "bg-muted"
                  }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      captureFocusStates ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
              </label>

              {/* Scroll States */}
              <label className="flex items-center justify-between cursor-pointer">
                <div className="space-y-1">
                  <div className="font-medium">Capture Scroll States</div>
                  <div className="text-sm text-muted-foreground">
                    Record UI elements revealed by scrolling
                  </div>
                </div>
                <button
                  onClick={() => setCaptureScrollStates(!captureScrollStates)}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    captureScrollStates ? "bg-primary" : "bg-muted"
                  }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      captureScrollStates ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
              </label>
            </div>
          </div>
        )}
      </div>

      {/* Error/Success Messages */}
      {extractionError && (
        <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
          <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <span className="text-red-400 text-sm">{extractionError}</span>
        </div>
      )}

      {extractionSuccess && (
        <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
          <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
          <span className="text-green-400 text-sm">Web extraction completed successfully!</span>
        </div>
      )}

      {/* Progress Section */}
      {isExtracting && extractionProgress && (
        <div className="space-y-3 bg-card rounded-lg border border-border/50 p-6">
          <div className="flex items-center gap-2">
            <Loader2 className="w-5 h-5 text-primary animate-spin" />
            <span className="font-semibold">Extracting...</span>
          </div>

          <div className="space-y-2">
            <div className="text-sm text-muted-foreground">Current URL:</div>
            <div className="text-sm font-mono bg-input p-2 rounded border border-border/50 truncate">
              {extractionProgress.current_url}
            </div>
          </div>

          <div className="space-y-2">
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Progress:</span>
              <span className="font-medium">
                {extractionProgress.pages_extracted} / {extractionProgress.total_pages} pages
              </span>
            </div>
            <div className="w-full bg-muted rounded-full h-2">
              <div
                className="bg-primary h-2 rounded-full transition-all duration-300"
                style={{
                  width: `${(extractionProgress.pages_extracted / extractionProgress.total_pages) * 100}%`,
                }}
              />
            </div>
          </div>
        </div>
      )}

      {/* Action Buttons */}
      <div className="flex items-center gap-3">
        {!isExtracting ? (
          <button
            onClick={handleStartExtraction}
            disabled={urls.length === 0 || !selectedProjectId}
            className="px-6 py-3 bg-green-600 hover:bg-green-700 disabled:bg-gray-400 disabled:cursor-not-allowed text-white rounded-md font-medium transition-colors flex items-center gap-2"
          >
            <Play className="w-5 h-5" />
            Start Extraction
          </button>
        ) : (
          <button
            onClick={handleStopExtraction}
            className="px-6 py-3 bg-red-600 hover:bg-red-700 text-white rounded-md font-medium transition-colors flex items-center gap-2"
          >
            <Square className="w-5 h-5" />
            Stop Extraction
          </button>
        )}
      </div>
    </div>
  );
}
