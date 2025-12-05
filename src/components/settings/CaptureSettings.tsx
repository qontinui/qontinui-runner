import { useState, useEffect, useRef } from "react";
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
} from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import type { ScreenshotCaptureSettings, Monitor as MonitorType, LogFunction } from "./types";

interface CaptureSettingsProps {
  onLog: LogFunction;
}

export function CaptureSettings({ onLog }: CaptureSettingsProps) {
  const [captureSettings, setCaptureSettings] = useState<ScreenshotCaptureSettings>({
    enabled: false,
    manualClicksEnabled: false,
    outputFolder: "",
    baseImageName: "screenshot",
    screens: { type: "primary" },
    captureTimings: [0],
  });
  const [monitors, setMonitors] = useState<MonitorType[]>([]);
  const [captureSaving, setCaptureSaving] = useState(false);
  const [captureSaveSuccess, setCaptureSaveSuccess] = useState(false);
  const [captureError, setCaptureError] = useState<string | null>(null);
  const [manualCaptureRunning, setManualCaptureRunning] = useState(false);

  // Capture to Web state
  const [showCaptureToWebMenu, setShowCaptureToWebMenu] = useState(false);
  const [isCapturingToWeb, setIsCapturingToWeb] = useState(false);
  const [captureToWebSuccess, setCaptureToWebSuccess] = useState(false);
  const [captureToWebError, setCaptureToWebError] = useState<string | null>(null);
  const [userProjects, setUserProjects] = useState<Array<{ id: string; name: string }>>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string>("");
  const [selectedCaptureMonitor, setSelectedCaptureMonitor] = useState<number>(0);
  const captureToWebMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    loadMonitors();
    loadUserProjects();
  }, []);

  // Close capture menu when clicking outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (
        captureToWebMenuRef.current &&
        !captureToWebMenuRef.current.contains(event.target as Node)
      ) {
        setShowCaptureToWebMenu(false);
      }
    };

    if (showCaptureToWebMenu) {
      document.addEventListener("mousedown", handleClickOutside);
    }

    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, [showCaptureToWebMenu]);

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
    let unlisten: any;

    listen("executor-event", (event: any) => {
      const data = event.payload?.data;
      if (data?.message === "capture_status_update") {
        setManualCaptureRunning(data.manual_capture_running || false);
      }
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const loadMonitors = async () => {
    try {
      const result: any = await invoke("get_monitors");
      if (result && result.success && result.data && result.data.monitors) {
        setMonitors(result.data.monitors);
        // Auto-select primary monitor for capture-to-web
        const primary = result.data.monitors.find((m: MonitorType) => m.is_primary);
        if (primary) {
          setSelectedCaptureMonitor(primary.index);
        }
      }
    } catch (err) {
      console.error("Failed to load monitors:", err);
    }
  };

  const loadUserProjects = async () => {
    try {
      const result: any = await invoke("get_user_projects");
      if (result && result.success && result.data && result.data.projects) {
        setUserProjects(result.data.projects);
        // Auto-select first project
        if (result.data.projects.length > 0 && !selectedProjectId) {
          setSelectedProjectId(result.data.projects[0].id);
        }
      }
    } catch (err) {
      console.error("Failed to load user projects:", err);
    }
  };

  const handleCaptureAndUploadToWeb = async () => {
    if (!selectedProjectId) {
      setCaptureToWebError("Please select a project first");
      return;
    }

    setIsCapturingToWeb(true);
    setCaptureToWebError(null);
    setCaptureToWebSuccess(false);

    try {
      const result: any = await invoke("capture_and_upload_screenshot", {
        config: {
          project_id: selectedProjectId,
          monitor: selectedCaptureMonitor,
        },
      });

      if (result && result.success) {
        setCaptureToWebSuccess(true);
        onLog("success", `Screenshot uploaded to project successfully`);
        setTimeout(() => setCaptureToWebSuccess(false), 3000);
        setShowCaptureToWebMenu(false);
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

      const result: any = await invoke("update_capture_settings", {
        settings: captureSettings,
      });

      if (result && result.success) {
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
      const result: any = await invoke("update_capture_settings", {
        settings: { ...captureSettings, enabled: true, manualClicksEnabled: true },
      });

      if (result && result.success) {
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
      const result: any = await invoke("update_capture_settings", {
        settings: { ...captureSettings, manualClicksEnabled: false },
      });

      if (result && result.success) {
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
    <div className="space-y-6">
      <SectionHeader
        title="Capture"
        description="Tools for collecting screenshots to build automation configurations. Capture screen images during manual interactions or automation runs."
        icon={<Camera className="w-6 h-6" />}
      />

      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Camera className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Screenshot Capture Tool</h4>
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
            <span className="text-green-400 text-sm">Screenshot capture settings saved!</span>
          </div>
        )}

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

        <div className="space-y-3">
          <div>
            <div className="font-medium mb-1">Manual Click Capture</div>
            <div className="text-sm text-muted-foreground">
              Capture screenshots when you physically click on the screen (for collecting initial
              training data before automation exists)
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

        <div className="space-y-2">
          <div className="font-medium mb-1 flex items-center gap-2">
            <Monitor className="w-4 h-4" />
            Screen Selection
          </div>
          <div className="text-sm text-muted-foreground mb-3">Choose which screens to capture</div>
          <div className="flex flex-col gap-2">
            {monitors.length > 0 ? (
              <>
                {monitors.map((monitor) => {
                  const position = monitor.x < 0 ? "left" : monitor.x > 0 ? "right" : "center";
                  const isSelected =
                    captureSettings.screens.type === "specific" &&
                    captureSettings.screens.indices?.includes(monitor.index);
                  return (
                    <label key={monitor.index} className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="radio"
                        checked={isSelected}
                        onChange={() => handleScreenSelectionChange("specific", monitor.index)}
                        className="w-4 h-4 accent-primary"
                      />
                      <span>
                        Screen #{monitor.index + 1}{" "}
                        {monitor.is_primary && <span className="text-primary">(primary)</span>}
                        <span className="text-xs text-muted-foreground ml-2">
                          {position}, {monitor.width}x{monitor.height}
                        </span>
                      </span>
                    </label>
                  );
                })}
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="radio"
                    checked={captureSettings.screens.type === "all"}
                    onChange={() => handleScreenSelectionChange("all")}
                    className="w-4 h-4 accent-primary"
                  />
                  <span>
                    All screens{" "}
                    <span className="text-xs text-muted-foreground">
                      ({monitors.length} detected)
                    </span>
                  </span>
                </label>
              </>
            ) : (
              <div className="text-sm text-muted-foreground">Loading monitors...</div>
            )}
          </div>
        </div>

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

        <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Screenshots are captured using the
            same tool used for pattern matching during automation. They will be numbered
            automatically based on existing files in the output folder.
          </div>
        </div>

        <div className="flex justify-end">
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

      {/* Capture to Web Section */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Cloud className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Capture to Web Project</h4>
        </div>

        <div className="text-sm text-muted-foreground">
          Take a screenshot and upload it directly to your qontinui-web project. Screenshots are
          captured at physical resolution.
        </div>

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
          {/* Project Selection */}
          <div className="space-y-2">
            <div className="font-medium mb-1">Target Project</div>
            <div className="text-sm text-muted-foreground mb-3">
              Select which qontinui-web project to upload the screenshot to
            </div>
            {userProjects.length > 0 ? (
              <select
                value={selectedProjectId}
                onChange={(e) => setSelectedProjectId(e.target.value)}
                className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
              >
                {userProjects.map((project) => (
                  <option key={project.id} value={project.id}>
                    {project.name}
                  </option>
                ))}
              </select>
            ) : (
              <div className="text-sm text-muted-foreground italic">
                No projects available. Create a project in qontinui-web first.
              </div>
            )}
          </div>

          {/* Monitor Selection */}
          <div className="space-y-2">
            <div className="font-medium mb-1 flex items-center gap-2">
              <Monitor className="w-4 h-4" />
              Capture Screen
            </div>
            <div className="text-sm text-muted-foreground mb-3">
              Select which monitor to capture
            </div>
            <div className="flex flex-wrap gap-2">
              {monitors.length > 0 ? (
                monitors.map((monitor) => (
                  <button
                    key={monitor.index}
                    onClick={() => setSelectedCaptureMonitor(monitor.index)}
                    className={`px-3 py-2 rounded-md border transition-colors flex items-center gap-2 ${
                      selectedCaptureMonitor === monitor.index
                        ? "bg-primary/20 border-primary text-primary"
                        : "bg-input border-border/50 hover:border-primary/50"
                    }`}
                  >
                    <Monitor className="w-4 h-4" />
                    <span>
                      #{monitor.index + 1}
                      {monitor.is_primary && " (primary)"}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {monitor.width}x{monitor.height}
                    </span>
                  </button>
                ))
              ) : (
                <div className="text-sm text-muted-foreground">Loading monitors...</div>
              )}
            </div>
          </div>

          {/* Capture Button */}
          <div className="flex items-center gap-3" ref={captureToWebMenuRef}>
            <button
              onClick={handleCaptureAndUploadToWeb}
              disabled={isCapturingToWeb || userProjects.length === 0}
              className="px-6 py-2 bg-green-600 hover:bg-green-700 disabled:bg-gray-400 disabled:cursor-not-allowed text-white rounded-md font-medium transition-colors flex items-center gap-2"
            >
              {isCapturingToWeb ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Capturing & Uploading...
                </>
              ) : (
                <>
                  <Upload className="w-4 h-4" />
                  Capture & Upload to Web
                </>
              )}
            </button>
          </div>
        </div>

        <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> Screenshots are captured at physical
            resolution (e.g., 3840x2160 on 4K monitors), matching the resolution used by qontinui
            automation.
          </div>
        </div>
      </div>
    </div>
  );
}
