/**
 * CaptureTab.tsx
 *
 * Dedicated tab for screenshot capture operations.
 * Contains tools for capturing screenshots to local folders or web projects.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Check,
  X,
  Camera,
  FolderOpen,
  Monitor,
  Plus,
  Trash2,
  Upload,
  Loader2,
  Cloud,
  HardDrive,
} from "lucide-react";
import type { Project } from "../types/auth";

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

interface ScreenshotCaptureSettings {
  enabled: boolean;
  manualClicksEnabled: boolean;
  outputFolder: string;
  baseImageName: string;
  screens: { type: "all" | "primary" | "specific"; indices?: number[] };
  captureTimings: number[];
}

interface CaptureTabProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  projects: Project[];
  selectedProjectId: string | null;
  selectedProjectName: string | null;
}

export function CaptureTab({
  onLog,
  projects,
  selectedProjectId,
  selectedProjectName,
}: CaptureTabProps) {
  // Local capture settings
  const [captureSettings, setCaptureSettings] = useState<ScreenshotCaptureSettings>({
    enabled: false,
    manualClicksEnabled: false,
    outputFolder: "",
    baseImageName: "screenshot",
    screens: { type: "primary" },
    captureTimings: [0],
  });
  const [monitors, setMonitors] = useState<MonitorInfo[]>([]);
  const [captureSaving, setCaptureSaving] = useState(false);
  const [captureSaveSuccess, setCaptureSaveSuccess] = useState(false);
  const [captureError, setCaptureError] = useState<string | null>(null);
  const [manualCaptureRunning, setManualCaptureRunning] = useState(false);

  // Capture to Web state
  const [isCapturingToWeb, setIsCapturingToWeb] = useState(false);
  const [captureToWebSuccess, setCaptureToWebSuccess] = useState(false);
  const [captureToWebError, setCaptureToWebError] = useState<string | null>(null);
  const [selectedCaptureMonitor, setSelectedCaptureMonitor] = useState<number>(0);

  useEffect(() => {
    loadMonitors();
  }, []);

  // Auto-select primary screen when monitors load
  useEffect(() => {
    if (monitors.length > 0 && captureSettings.screens.type === "primary") {
      const primary = monitors.find((m) => m.is_primary);
      if (primary) {
        setCaptureSettings((prev) => ({
          ...prev,
          screens: { type: "specific", indices: [primary.index] },
        }));
      }
    }
  }, [monitors]);

  // Listen for manual capture status updates from Python
  useEffect(() => {
    let unlisten: (() => void) | undefined;

    listen(
      "executor-event",
      (event: { payload?: { data?: { message?: string; manual_capture_running?: boolean } } }) => {
        const data = event.payload?.data;
        if (data?.message === "capture_status_update") {
          setManualCaptureRunning(data.manual_capture_running || false);
        }
      },
    ).then((fn) => {
      unlisten = fn;
    });

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const loadMonitors = async () => {
    try {
      const result = await invoke<{ success: boolean; data?: { monitors: MonitorInfo[] } }>(
        "get_monitors",
      );
      if (result?.success && result.data?.monitors) {
        // Sort monitors by x position (left to right)
        const sortedMonitors = [...result.data.monitors].sort((a, b) => a.x - b.x);
        setMonitors(sortedMonitors);
        // Auto-select primary monitor for capture-to-web
        const primary = sortedMonitors.find((m) => m.is_primary);
        if (primary) {
          setSelectedCaptureMonitor(primary.index);
        }
      }
    } catch (err) {
      console.error("Failed to load monitors:", err);
    }
  };

  const handleCaptureAndUploadToWeb = async () => {
    if (!selectedProjectId) {
      setCaptureToWebError("Please select a project in Settings first");
      return;
    }

    setIsCapturingToWeb(true);
    setCaptureToWebError(null);
    setCaptureToWebSuccess(false);

    try {
      const result = await invoke<{ success: boolean; error?: string }>(
        "capture_and_upload_screenshot",
        {
          config: {
            project_id: selectedProjectId,
            monitor: selectedCaptureMonitor,
          },
        },
      );

      if (result?.success) {
        setCaptureToWebSuccess(true);
        onLog("success", `Screenshot uploaded to project successfully`);
        setTimeout(() => setCaptureToWebSuccess(false), 3000);
      } else {
        const errorMsg = result?.error || "Unknown error";
        setCaptureToWebError(errorMsg);
        onLog("error", `Failed to capture and upload: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to capture and upload:", err);
      setCaptureToWebError(`Failed: ${err}`);
      onLog("error", `Failed to capture and upload: ${err}`);
    } finally {
      setIsCapturingToWeb(false);
    }
  };

  const handleToggleCapture = () => {
    setCaptureSettings((prev) => ({
      ...prev,
      enabled: !prev.enabled,
    }));
  };

  const handleFolderSelect = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Select Output Folder",
      });

      if (selected && typeof selected === "string") {
        setCaptureSettings((prev) => ({
          ...prev,
          outputFolder: selected,
        }));
      }
    } catch (err) {
      console.error("Failed to select folder:", err);
      onLog("error", `Failed to select folder: ${err}`);
    }
  };

  const handleScreenSelectionChange = (type: "all" | "primary" | "specific", index?: number) => {
    if (type === "specific" && index !== undefined) {
      setCaptureSettings((prev) => ({
        ...prev,
        screens: { type: "specific", indices: [index] },
      }));
    } else {
      setCaptureSettings((prev) => ({
        ...prev,
        screens: type === "specific" ? { type, indices: [] } : { type },
      }));
    }
  };

  const handleAddTiming = () => {
    setCaptureSettings((prev) => ({
      ...prev,
      captureTimings: [...prev.captureTimings, 0],
    }));
  };

  const handleRemoveTiming = (index: number) => {
    setCaptureSettings((prev) => ({
      ...prev,
      captureTimings: prev.captureTimings.filter((_, i) => i !== index),
    }));
  };

  const handleTimingChange = (index: number, value: number) => {
    setCaptureSettings((prev) => ({
      ...prev,
      captureTimings: prev.captureTimings.map((t, i) => (i === index ? Math.max(0, value) : t)),
    }));
  };

  const saveCaptureSettings = async () => {
    try {
      setCaptureSaving(true);
      setCaptureError(null);
      setCaptureSaveSuccess(false);

      const result = await invoke<{
        success: boolean;
        message?: string;
        manual_capture_running?: boolean;
      }>("update_capture_settings", { settings: captureSettings });

      if (result?.success) {
        setCaptureSaveSuccess(true);
        setManualCaptureRunning(result.manual_capture_running || false);
        onLog("success", "Screenshot capture settings saved successfully");
        setTimeout(() => setCaptureSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setCaptureError(errorMsg);
        onLog("error", `Failed to save capture settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save capture settings:", err);
      setCaptureError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save capture settings: ${err}`);
    } finally {
      setCaptureSaving(false);
    }
  };

  const handleStartManualCapture = async () => {
    try {
      const result = await invoke<{
        success: boolean;
        message?: string;
        manual_capture_running?: boolean;
      }>("update_capture_settings", {
        settings: { ...captureSettings, enabled: true, manualClicksEnabled: true },
      });

      if (result?.success) {
        setManualCaptureRunning(result.manual_capture_running || false);
        setCaptureSettings((prev) => ({ ...prev, enabled: true, manualClicksEnabled: true }));
        onLog("success", "Manual capture started");
      } else {
        const errorMsg = result?.message || "Unknown error";
        onLog("error", `Failed to start manual capture: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to start manual capture:", err);
      onLog("error", `Failed to start manual capture: ${err}`);
    }
  };

  const handleStopManualCapture = async () => {
    try {
      const result = await invoke<{
        success: boolean;
        message?: string;
        manual_capture_running?: boolean;
      }>("update_capture_settings", {
        settings: { ...captureSettings, manualClicksEnabled: false },
      });

      if (result?.success) {
        setManualCaptureRunning(result.manual_capture_running || false);
        setCaptureSettings((prev) => ({ ...prev, manualClicksEnabled: false }));
        onLog("info", "Manual capture stopped");
      } else {
        const errorMsg = result?.message || "Unknown error";
        onLog("error", `Failed to stop manual capture: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to stop manual capture:", err);
      onLog("error", `Failed to stop manual capture: ${err}`);
    }
  };

  return (
    <div className="space-y-6 p-6 overflow-y-auto max-h-[calc(100vh-200px)]">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Camera className="w-6 h-6 text-primary" />
        <div>
          <h2 className="text-xl font-semibold">Screenshot Capture</h2>
          <p className="text-sm text-muted-foreground">
            Capture screenshots for building automation configurations
          </p>
        </div>
      </div>

      {/* Capture to Web Section */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Cloud className="w-5 h-5 text-primary" />
          <h3 className="font-semibold text-lg">Capture to Web Project</h3>
        </div>

        <p className="text-sm text-muted-foreground">
          Take a screenshot and upload it directly to your qontinui-web project. Screenshots are
          captured at physical resolution.
        </p>

        {captureToWebError && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
            <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
            <span className="text-red-400 text-sm">{captureToWebError}</span>
          </div>
        )}

        {captureToWebSuccess && (
          <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
            <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
            <span className="text-green-400 text-sm">Screenshot uploaded successfully!</span>
          </div>
        )}

        <div className="space-y-4">
          {/* Project Info */}
          <div className="space-y-2">
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
              <div className="text-sm text-orange-400 italic">
                No project selected. Go to Settings → Connection to select a project.
              </div>
            )}
          </div>

          {/* Monitor Selection */}
          <div className="space-y-2">
            <div className="font-medium flex items-center gap-2">
              <Monitor className="w-4 h-4" />
              Capture Screen
            </div>
            <div className="flex flex-wrap gap-3">
              {monitors.length > 0 ? (
                monitors.map((monitor, spatialIndex) => {
                  // Display spatial position (1-indexed, left to right) instead of Windows enumeration index
                  const displayNumber = spatialIndex + 1;
                  return (
                    <button
                      key={monitor.index}
                      onClick={() => setSelectedCaptureMonitor(monitor.index)}
                      className={`flex flex-col items-center gap-1 p-3 rounded-lg border transition-colors min-w-[100px] ${
                        selectedCaptureMonitor === monitor.index
                          ? "bg-primary/20 border-primary text-primary"
                          : "bg-input border-border/50 hover:border-primary/50"
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <Monitor className="w-5 h-5" />
                        <span className="font-medium">
                          #{displayNumber}
                          {monitor.is_primary && (
                            <span className="text-primary ml-1">(primary)</span>
                          )}
                        </span>
                      </div>
                      <span className="text-xs text-muted-foreground">
                        {monitor.width}x{monitor.height}
                      </span>
                    </button>
                  );
                })
              ) : (
                <div className="text-sm text-muted-foreground">Loading monitors...</div>
              )}
            </div>
          </div>

          {/* Capture Button */}
          <button
            onClick={handleCaptureAndUploadToWeb}
            disabled={isCapturingToWeb || !selectedProjectId}
            className="px-6 py-3 bg-green-600 hover:bg-green-700 disabled:bg-gray-400 disabled:cursor-not-allowed text-white rounded-md font-medium transition-colors flex items-center gap-2"
          >
            {isCapturingToWeb ? (
              <>
                <Loader2 className="w-5 h-5 animate-spin" />
                Capturing & Uploading...
              </>
            ) : (
              <>
                <Upload className="w-5 h-5" />
                Capture & Upload to Web
              </>
            )}
          </button>
        </div>
      </div>

      {/* Capture to Local Section */}
      <div className="space-y-4 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <HardDrive className="w-5 h-5 text-primary" />
          <h3 className="font-semibold text-lg">Capture to Local Folder</h3>
        </div>

        {captureError && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
            <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
            <span className="text-red-400 text-sm">{captureError}</span>
          </div>
        )}

        {captureSaveSuccess && (
          <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
            <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
            <span className="text-green-400 text-sm">Capture settings saved!</span>
          </div>
        )}

        {/* Enable Toggle */}
        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Enable Screenshot Capture</div>
              <div className="text-sm text-muted-foreground">
                Automatically capture screenshots on clicks for the configuration builder
              </div>
            </div>
            <button
              onClick={handleToggleCapture}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                captureSettings.enabled ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  captureSettings.enabled ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        {/* Manual Click Capture */}
        <div className="space-y-3">
          <div>
            <div className="font-medium mb-1">Manual Click Capture</div>
            <div className="text-sm text-muted-foreground">
              Capture screenshots when you physically click on the screen
            </div>
          </div>

          <div className="flex items-center gap-3">
            {!manualCaptureRunning ? (
              <button
                onClick={handleStartManualCapture}
                disabled={!captureSettings.enabled || !captureSettings.outputFolder}
                className="px-4 py-2 bg-green-600 hover:bg-green-700 disabled:bg-gray-400 disabled:cursor-not-allowed text-white rounded transition-colors"
                title={
                  !captureSettings.enabled
                    ? "Enable capture tool first"
                    : !captureSettings.outputFolder
                      ? "Set output folder first"
                      : "Start capturing screenshots on mouse clicks"
                }
              >
                Start Manual Capture
              </button>
            ) : (
              <button
                onClick={handleStopManualCapture}
                className="px-4 py-2 bg-red-600 hover:bg-red-700 text-white rounded transition-colors"
              >
                Stop Manual Capture
              </button>
            )}

            {manualCaptureRunning && (
              <span className="flex items-center gap-2 text-green-600 font-medium">
                <span className="inline-block w-2 h-2 bg-green-600 rounded-full animate-pulse"></span>
                Listening for clicks...
              </span>
            )}
          </div>
        </div>

        {/* Output Folder */}
        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Output Folder</div>
            <div className="text-sm text-muted-foreground mb-3">
              Screenshots will be saved to this folder
            </div>
            <div className="flex items-center gap-2">
              <input
                type="text"
                value={captureSettings.outputFolder}
                onChange={(e) =>
                  setCaptureSettings((prev) => ({ ...prev, outputFolder: e.target.value }))
                }
                placeholder="/path/to/screenshots"
                className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md"
              />
              <button
                onClick={handleFolderSelect}
                className="px-3 py-2 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors flex items-center gap-2"
              >
                <FolderOpen className="w-4 h-4" />
                Browse
              </button>
            </div>
          </label>
        </div>

        {/* Base Image Name */}
        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Base Image Name</div>
            <div className="text-sm text-muted-foreground mb-3">
              Base name for screenshot files (will be numbered automatically)
            </div>
            <input
              type="text"
              value={captureSettings.baseImageName}
              onChange={(e) =>
                setCaptureSettings((prev) => ({ ...prev, baseImageName: e.target.value }))
              }
              placeholder="screenshot"
              className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
            />
          </label>
        </div>

        {/* Screen Selection */}
        <div className="space-y-2">
          <div className="font-medium mb-1 flex items-center gap-2">
            <Monitor className="w-4 h-4" />
            Screen Selection
          </div>
          <div className="text-sm text-muted-foreground mb-3">Choose which screens to capture</div>
          <div className="flex flex-wrap gap-3">
            {monitors.length > 0 ? (
              <>
                {monitors.map((monitor) => {
                  const isSelected =
                    captureSettings.screens.type === "specific" &&
                    captureSettings.screens.indices?.includes(monitor.index);
                  return (
                    <label
                      key={monitor.index}
                      className={`flex flex-col items-center gap-1 cursor-pointer p-3 rounded-lg border transition-colors min-w-[100px] ${
                        isSelected
                          ? "bg-primary/20 border-primary"
                          : "bg-input border-border/50 hover:border-primary/50"
                      }`}
                    >
                      <input
                        type="radio"
                        checked={isSelected}
                        onChange={() => handleScreenSelectionChange("specific", monitor.index)}
                        className="sr-only"
                      />
                      <div className="flex items-center gap-2">
                        <Monitor className="w-5 h-5" />
                        <span className="font-medium">
                          #{monitor.index}
                          {monitor.is_primary && (
                            <span className="text-primary ml-1">(primary)</span>
                          )}
                        </span>
                      </div>
                      <span className="text-xs text-muted-foreground">
                        {monitor.width}x{monitor.height}
                      </span>
                    </label>
                  );
                })}
                <label
                  className={`flex flex-col items-center justify-center gap-1 cursor-pointer p-3 rounded-lg border transition-colors min-w-[100px] ${
                    captureSettings.screens.type === "all"
                      ? "bg-primary/20 border-primary"
                      : "bg-input border-border/50 hover:border-primary/50"
                  }`}
                >
                  <input
                    type="radio"
                    checked={captureSettings.screens.type === "all"}
                    onChange={() => handleScreenSelectionChange("all")}
                    className="sr-only"
                  />
                  <div className="flex items-center gap-2">
                    <Monitor className="w-5 h-5" />
                    <span className="font-medium">All</span>
                  </div>
                  <span className="text-xs text-muted-foreground">{monitors.length} screens</span>
                </label>
              </>
            ) : (
              <div className="text-sm text-muted-foreground">Loading monitors...</div>
            )}
          </div>
        </div>

        {/* Capture Timings */}
        <div className="space-y-2">
          <div className="font-medium mb-1 flex items-center justify-between">
            <span>Capture Timings (milliseconds)</span>
            <button
              onClick={handleAddTiming}
              className="px-2 py-1 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors flex items-center gap-1 text-sm"
            >
              <Plus className="w-3 h-3" />
              Add Timing
            </button>
          </div>
          <div className="text-sm text-muted-foreground mb-3">
            Delay after click before taking screenshot (0 = immediate)
          </div>
          <div className="space-y-2">
            {captureSettings.captureTimings.map((timing, index) => (
              <div key={index} className="flex items-center gap-2">
                <input
                  type="number"
                  min="0"
                  step="100"
                  value={timing}
                  onChange={(e) => handleTimingChange(index, parseInt(e.target.value) || 0)}
                  className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md"
                />
                <span className="text-sm text-muted-foreground">ms</span>
                {captureSettings.captureTimings.length > 1 && (
                  <button
                    onClick={() => handleRemoveTiming(index)}
                    className="px-2 py-2 bg-red-500/10 hover:bg-red-500/20 text-red-400 rounded-md transition-colors"
                  >
                    <Trash2 className="w-4 h-4" />
                  </button>
                )}
              </div>
            ))}
          </div>
        </div>

        {/* Save Button */}
        <div className="flex justify-end pt-4 border-t border-border/50">
          <button
            onClick={saveCaptureSettings}
            disabled={captureSaving}
            className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
          >
            {captureSaving ? (
              <>
                <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                Saving...
              </>
            ) : captureSaveSuccess ? (
              <>
                <Check className="w-4 h-4" />
                Saved!
              </>
            ) : (
              <>
                <Camera className="w-4 h-4" />
                Save Capture Settings
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
