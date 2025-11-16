import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Settings as SettingsIcon, Check, X, Camera, FolderOpen, Monitor, Plus, Trash2, Wifi, Cloud, AlertCircle, HardDrive, Trash } from "lucide-react";

interface DebugSettings {
  enable_image_debug: boolean;
  top_matches_count: number;
}

interface AppSettings {
  auto_load_last_config: boolean;
}

interface WebSocketSettings {
  // Connection
  enabled: boolean;
  url: string;
  token: string;
  projectId: string;
  connected: boolean;

  // Cloud permission status (read-only, from API)
  cloudPermissionEnabled: boolean;
  sessionsLimit: number | null;  // null = unlimited
  sessionsUsed: number;
  sessionsResetAt: string | null;

  // User controls
  sendToCloud: boolean;          // Master toggle
  sendLogs: boolean;             // Default: true if sendToCloud
  sendScreenshots: boolean;      // Default: true if sendToCloud
  sendVideos: boolean;           // Default: false
}

type ScreenSelectionType =
  | { type: 'all' }
  | { type: 'primary' }
  | { type: 'specific'; indices: number[] };

interface ScreenshotCaptureSettings {
  enabled: boolean;
  manualClicksEnabled: boolean;
  outputFolder: string;
  baseImageName: string;
  screens: ScreenSelectionType;
  captureTimings: number[]; // delays in ms
}

interface Monitor {
  index: number;
  x: number;
  y: number;
  width: number;
  height: number;
  is_primary: boolean;
}

interface SettingsProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  onDebugModeChange: (enabled: boolean) => void;
}

export function Settings({ onLog, onDebugModeChange }: SettingsProps) {
  const [settings, setSettings] = useState<DebugSettings>({
    enable_image_debug: false,
    top_matches_count: 5,
  });
  const [appSettings, setAppSettings] = useState<AppSettings>({
    auto_load_last_config: false,
  });
  const [wsSettings, setWsSettings] = useState<WebSocketSettings>({
    // Connection
    enabled: false,
    url: "ws://localhost:8001",
    token: "",
    projectId: "",
    connected: false,

    // Cloud permission status
    cloudPermissionEnabled: false,
    sessionsLimit: null,
    sessionsUsed: 0,
    sessionsResetAt: null,

    // User controls
    sendToCloud: false,
    sendLogs: true,
    sendScreenshots: true,
    sendVideos: false,
  });
  const [captureSettings, setCaptureSettings] = useState<ScreenshotCaptureSettings>({
    enabled: false,
    manualClicksEnabled: false,
    outputFolder: '',
    baseImageName: 'screenshot',
    screens: { type: 'primary' },
    captureTimings: [0],
  });
  const [monitors, setMonitors] = useState<Monitor[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [captureSaving, setCaptureSaving] = useState(false);
  const [captureSaveSuccess, setCaptureSaveSuccess] = useState(false);
  const [manualCaptureRunning, setManualCaptureRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);

  // Storage state
  const [storageUsage, setStorageUsage] = useState({
    screenshots: 0,
    videos: 0,
    screenshotCount: 0,
    videoCount: 0,
  });
  const [storagePaths, setStoragePaths] = useState({
    screenshot_path: "",
    video_path: "",
    max_screenshot_mb: 500,
    max_video_mb: 500,
    auto_cleanup: true,
  });
  const [storageLoading, setStorageLoading] = useState(false);

  // Load current settings on mount
  useEffect(() => {
    loadSettings();
    loadAppSettings();
    loadMonitors();
    loadStorageInfo();
  }, []);

  // Auto-select primary screen as specific when monitors load
  useEffect(() => {
    if (monitors.length > 0 && captureSettings.screens.type === 'primary') {
      const primary = monitors.find(m => m.is_primary);
      if (primary) {
        setCaptureSettings(prev => ({
          ...prev,
          screens: { type: 'specific', indices: [primary.index] },
        }));
      }
    }
  }, [monitors]);

  // Check cloud permission when token changes
  useEffect(() => {
    const checkCloudPermission = async () => {
      if (wsSettings.token) {
        try {
          const response = await fetch('http://localhost:8001/api/v1/users/me/automation-streaming', {
            headers: {
              'Authorization': `Bearer ${wsSettings.token}`
            }
          });
          if (response.ok) {
            const data = await response.json();
            setWsSettings(prev => ({
              ...prev,
              cloudPermissionEnabled: data.enabled,
              sessionsLimit: data.sessions_limit,
              sessionsUsed: data.sessions_used,
              sessionsResetAt: data.sessions_reset_at,
            }));
          } else {
            // Token invalid or permission not available
            setWsSettings(prev => ({
              ...prev,
              cloudPermissionEnabled: false,
            }));
          }
        } catch (error) {
          console.error('Failed to check cloud permission:', error);
          setWsSettings(prev => ({
            ...prev,
            cloudPermissionEnabled: false,
          }));
        }
      } else {
        // No token, reset permission status
        setWsSettings(prev => ({
          ...prev,
          cloudPermissionEnabled: false,
        }));
      }
    };

    checkCloudPermission();
  }, [wsSettings.token]);

  // Listen for manual capture status updates from Python
  useEffect(() => {
    let unlisten: any;

    listen('executor-event', (event: any) => {
      const data = event.payload?.data;
      if (data?.message === 'capture_status_update') {
        console.log('Received capture status update:', data.manual_capture_running);
        setManualCaptureRunning(data.manual_capture_running || false);
      }
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);
      console.log("Loading debug settings...");

      const result: any = await invoke("get_debug_settings");
      console.log("get_debug_settings result:", result);

      if (result && result.success && result.data) {
        const loadedSettings: DebugSettings = {
          enable_image_debug: result.data.enable_image_debug || false,
          top_matches_count: result.data.top_matches_count || 5,
        };
        setSettings(loadedSettings);
        console.log("Settings loaded:", loadedSettings);
        onLog("debug", "Debug settings loaded");
      } else {
        console.warn("Failed to load settings, using defaults:", result);
        onLog("warning", "Using default debug settings");
      }
    } catch (err) {
      console.error("Failed to load debug settings:", err);
      setError(`Failed to load settings: ${err}`);
      onLog("error", `Failed to load settings: ${err}`);
    } finally {
      setLoading(false);
    }
  };

  const loadAppSettings = async () => {
    try {
      console.log("Loading app settings...");
      const result: any = await invoke("get_auto_load_last_config");
      console.log("get_auto_load_last_config result:", result);

      if (result && result.success && result.data) {
        setAppSettings({
          auto_load_last_config: result.data.enabled || false,
        });
        console.log("App settings loaded:", result.data.enabled);
      }
    } catch (err) {
      console.error("Failed to load app settings:", err);
    }
  };

  const loadMonitors = async () => {
    try {
      console.log("Loading monitors...");
      const result: any = await invoke("get_monitors");
      console.log("get_monitors result:", result);

      if (result && result.success && result.data && result.data.monitors) {
        setMonitors(result.data.monitors);
        console.log("Monitors loaded:", result.data.monitors);
      }
    } catch (err) {
      console.error("Failed to load monitors:", err);
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);
      console.log("Saving debug settings:", settings);

      const result: any = await invoke("set_debug_settings", {
        enableImageDebug: settings.enable_image_debug,
        topMatchesCount: settings.top_matches_count,
      });
      console.log("set_debug_settings result:", result);

      if (result && result.success) {
        setSaveSuccess(true);
        console.log("Settings saved successfully");
        onLog("success", "Debug settings saved successfully");
        // Update the parent component's debug mode state
        onDebugModeChange(settings.enable_image_debug);
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const errorMsg = result?.message || "Unknown error";
        setError(errorMsg);
        onLog("error", `Failed to save settings: ${errorMsg}`);
      }
    } catch (err) {
      console.error("Failed to save debug settings:", err);
      setError(`Failed to save settings: ${err}`);
      onLog("error", `Failed to save settings: ${err}`);
    } finally {
      setSaving(false);
    }
  };

  const applyWebSocketSettings = async () => {
    if (!wsSettings.enabled) {
      onLog("info", "WebSocket streaming is disabled");
      return;
    }

    if (!wsSettings.url || !wsSettings.token) {
      onLog("error", "WebSocket URL and token are required");
      setError("WebSocket URL and token are required");
      return;
    }

    // Check cloud permission first
    if (!wsSettings.cloudPermissionEnabled) {
      onLog("error", "Cloud sync not available. Enable it in your qontinui.com profile.");
      setError("Cloud sync not available. Enable it in your qontinui.com profile.");
      return;
    }

    if (!wsSettings.sendToCloud) {
      onLog("info", "Cloud sync disabled by user preference");
      return;
    }

    try {
      console.log("Applying WebSocket settings...");

      // Parse project ID as integer (or null if empty)
      const projectId = wsSettings.projectId ? parseInt(wsSettings.projectId, 10) : null;

      // Configure WebSocket
      const configResult: any = await invoke("configure_websocket", {
        config: {
          enabled: wsSettings.enabled,
          url: wsSettings.url,
          token: wsSettings.token,
          project_id: projectId,
        },
      });

      if (configResult && configResult.success) {
        onLog("success", "WebSocket configured successfully");

        // Connect to WebSocket
        const connectResult: any = await invoke("connect_websocket");
        if (connectResult && connectResult.success) {
          setWsSettings(prev => ({ ...prev, connected: true }));
          onLog("success", "WebSocket connected successfully");
        } else {
          onLog("error", `Failed to connect WebSocket: ${connectResult?.message || "Unknown error"}`);
        }
      } else {
        onLog("error", `Failed to configure WebSocket: ${configResult?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to apply WebSocket settings:", err);
      onLog("error", `Failed to apply WebSocket settings: ${err}`);
      setError(`Failed to apply WebSocket settings: ${err}`);
    }
  };

  const disconnectWebSocket = async () => {
    try {
      const result: any = await invoke("disconnect_websocket");
      if (result && result.success) {
        setWsSettings(prev => ({ ...prev, connected: false }));
        onLog("success", "WebSocket disconnected");
      } else {
        onLog("error", `Failed to disconnect WebSocket: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to disconnect WebSocket:", err);
      onLog("error", `Failed to disconnect WebSocket: ${err}`);
    }
  };

  const handleToggleDebug = () => {
    setSettings((prev) => ({
      ...prev,
      enable_image_debug: !prev.enable_image_debug,
    }));
  };

  const handleTopMatchesChange = (value: number) => {
    // Clamp value between 1 and 10
    const clampedValue = Math.max(1, Math.min(10, value));
    setSettings((prev) => ({
      ...prev,
      top_matches_count: clampedValue,
    }));
  };

  const handleToggleAutoLoad = async () => {
    const newValue = !appSettings.auto_load_last_config;
    setAppSettings((prev) => ({
      ...prev,
      auto_load_last_config: newValue,
    }));

    // Save immediately
    try {
      const result: any = await invoke("save_auto_load_last_config", { enabled: newValue });
      if (result && result.success) {
        onLog("success", `Auto-load last config ${newValue ? "enabled" : "disabled"}`);
      } else {
        onLog("error", "Failed to save auto-load setting");
        // Revert on failure
        setAppSettings((prev) => ({
          ...prev,
          auto_load_last_config: !newValue,
        }));
      }
    } catch (err) {
      console.error("Failed to save auto-load setting:", err);
      onLog("error", `Failed to save auto-load setting: ${err}`);
      // Revert on failure
      setAppSettings((prev) => ({
        ...prev,
        auto_load_last_config: !newValue,
      }));
    }
  };

  const handleToggleSendToCloud = (checked: boolean) => {
    setWsSettings((prev) => ({
      ...prev,
      sendToCloud: checked,
      // Auto-enable logs and screenshots when turning on cloud sync
      sendLogs: checked ? true : prev.sendLogs,
      sendScreenshots: checked ? true : prev.sendScreenshots,
    }));
  };

  const handleToggleCapture = () => {
    setCaptureSettings((prev) => ({
      ...prev,
      enabled: !prev.enabled,
    }));
  };

  const handleFolderSelect = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        directory: true,
        multiple: false,
        title: 'Select Output Folder',
      });

      if (selected && typeof selected === 'string') {
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

  const handleScreenSelectionChange = (type: 'all' | 'primary' | 'specific', index?: number) => {
    if (type === 'specific' && index !== undefined) {
      setCaptureSettings((prev) => ({
        ...prev,
        screens: { type: 'specific', indices: [index] },
      }));
    } else {
      setCaptureSettings((prev) => ({
        ...prev,
        screens: type === 'specific' ? { type, indices: [] } : { type },
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
      captureTimings: prev.captureTimings.map((t, i) => i === index ? Math.max(0, value) : t),
    }));
  };

  const saveCaptureSettings = async () => {
    try {
      setCaptureSaving(true);
      setCaptureError(null);
      setCaptureSaveSuccess(false);
      console.log("Saving capture settings:", captureSettings);

      const result: any = await invoke("update_capture_settings", {
        settings: captureSettings,
      });
      console.log("update_capture_settings result:", result);

      if (result && result.success) {
        setCaptureSaveSuccess(true);
        setManualCaptureRunning(result.manual_capture_running || false);
        console.log("Capture settings saved successfully");
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
      console.log("Starting manual capture...");
      const result: any = await invoke("update_capture_settings", {
        settings: { ...captureSettings, enabled: true, manualClicksEnabled: true },
      });
      console.log("Start manual capture result:", result);

      if (result && result.success) {
        setManualCaptureRunning(result.manual_capture_running || false);
        setCaptureSettings(prev => ({ ...prev, enabled: true, manualClicksEnabled: true }));
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
      console.log("Stopping manual capture...");
      const result: any = await invoke("update_capture_settings", {
        settings: { ...captureSettings, manualClicksEnabled: false },
      });
      console.log("Stop manual capture result:", result);

      if (result && result.success) {
        setManualCaptureRunning(result.manual_capture_running || false);
        setCaptureSettings(prev => ({ ...prev, manualClicksEnabled: false }));
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

  // Storage management functions
  const loadStorageInfo = async () => {
    try {
      setStorageLoading(true);

      // Load storage usage
      const usageResult: any = await invoke("get_local_storage_usage");
      if (usageResult && usageResult.success && usageResult.data) {
        setStorageUsage({
          screenshots: usageResult.data.screenshots_mb || 0,
          videos: usageResult.data.videos_mb || 0,
          screenshotCount: usageResult.data.screenshot_count || 0,
          videoCount: usageResult.data.video_count || 0,
        });
      }

      // Load storage paths
      const pathsResult: any = await invoke("get_storage_paths");
      if (pathsResult && pathsResult.success && pathsResult.data) {
        setStoragePaths({
          screenshot_path: pathsResult.data.screenshot_path || "",
          video_path: pathsResult.data.video_path || "",
          max_screenshot_mb: pathsResult.data.max_screenshot_mb || 500,
          max_video_mb: pathsResult.data.max_video_mb || 500,
          auto_cleanup: pathsResult.data.auto_cleanup ?? true,
        });
      }
    } catch (err) {
      console.error("Failed to load storage info:", err);
    } finally {
      setStorageLoading(false);
    }
  };

  const handleDeleteOldSessions = async (type: 'screenshots' | 'videos', days: number) => {
    try {
      const result: any = await invoke("delete_old_sessions", {
        storageType: type,
        olderThanDays: days,
      });

      if (result && result.success) {
        onLog("success", `Deleted ${type} older than ${days} days`);
        await loadStorageInfo(); // Refresh storage info
      } else {
        onLog("error", `Failed to delete old sessions: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to delete old sessions:", err);
      onLog("error", `Failed to delete old sessions: ${err}`);
    }
  };

  const handleClearAllStorage = async () => {
    if (!confirm("Are you sure you want to delete all screenshots and videos? This cannot be undone.")) {
      return;
    }

    try {
      const result: any = await invoke("clear_all_storage");

      if (result && result.success) {
        onLog("success", "All storage cleared successfully");
        await loadStorageInfo(); // Refresh storage info
      } else {
        onLog("error", `Failed to clear storage: ${result?.message || "Unknown error"}`);
      }
    } catch (err) {
      console.error("Failed to clear storage:", err);
      onLog("error", `Failed to clear storage: ${err}`);
    }
  };

  const handleOpenStorageFolder = async (type: 'screenshots' | 'videos') => {
    try {
      const path = type === 'screenshots' ? storagePaths.screenshot_path : storagePaths.video_path;
      await invoke("open_folder", { path });
    } catch (err) {
      console.error("Failed to open folder:", err);
      onLog("error", `Failed to open folder: ${err}`);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading settings...</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <SettingsIcon className="w-6 h-6 text-primary" />
        <h3 className="text-xl font-semibold">Settings</h3>
      </div>

      {/* Error message */}
      {error && (
        <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
          <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
          <span className="text-red-400 text-sm">{error}</span>
        </div>
      )}

      {/* Success message */}
      {saveSuccess && (
        <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
          <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
          <span className="text-green-400 text-sm">Settings saved successfully!</span>
        </div>
      )}

      {/* Application Settings */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Application</h4>

        {/* Auto-load Last Config Toggle */}
        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Auto-load Last Configuration on Startup</div>
              <div className="text-sm text-muted-foreground">
                Automatically load the last used configuration file and workflow when the application starts
              </div>
            </div>
            <button
              onClick={handleToggleAutoLoad}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                appSettings.auto_load_last_config ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  appSettings.auto_load_last_config ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>
      </div>

      {/* Cloud Sync Settings */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Cloud className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Cloud Sync</h4>
          {wsSettings.connected && (
            <span className="flex items-center gap-2 text-green-600 text-sm">
              <span className="inline-block w-2 h-2 bg-green-600 rounded-full animate-pulse"></span>
              Connected
            </span>
          )}
        </div>

        {/* Enable WebSocket Toggle */}
        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Enable Automation Streaming</div>
              <div className="text-sm text-muted-foreground">
                Connect to qontinui.com for real-time monitoring and cloud storage
              </div>
            </div>
            <button
              onClick={() => setWsSettings(prev => ({ ...prev, enabled: !prev.enabled }))}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                wsSettings.enabled ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  wsSettings.enabled ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        {/* Configuration fields */}
        {wsSettings.enabled && (
          <>
            {/* WebSocket URL */}
            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">WebSocket URL</div>
                <div className="text-sm text-muted-foreground mb-3">
                  qontinui-web backend WebSocket endpoint
                </div>
                <input
                  type="text"
                  value={wsSettings.url}
                  onChange={(e) => setWsSettings(prev => ({ ...prev, url: e.target.value }))}
                  placeholder="ws://localhost:8001"
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            {/* JWT Token */}
            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">JWT Token</div>
                <div className="text-sm text-muted-foreground mb-3">
                  Authentication token from qontinui.com
                </div>
                <input
                  type="password"
                  value={wsSettings.token}
                  onChange={(e) => setWsSettings(prev => ({ ...prev, token: e.target.value }))}
                  placeholder="eyJhbGciOiJIUzI1NiIsInR5cCI6..."
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md font-mono text-sm"
                />
              </label>
            </div>

            {/* Project ID */}
            <div className="space-y-2">
              <label className="block">
                <div className="font-medium mb-1">Project ID</div>
                <div className="text-sm text-muted-foreground mb-3">
                  Project identifier (integer)
                </div>
                <input
                  type="text"
                  value={wsSettings.projectId}
                  onChange={(e) => setWsSettings(prev => ({ ...prev, projectId: e.target.value }))}
                  placeholder="1"
                  className="w-full px-3 py-2 bg-input border border-border/50 rounded-md"
                />
              </label>
            </div>

            {/* Cloud Permission Status */}
            {wsSettings.cloudPermissionEnabled && wsSettings.token && (
              <div className="p-3 bg-primary/10 border border-primary/30 rounded-lg">
                <div className="text-sm">
                  <strong>Cloud Storage Available</strong>
                  <p className="text-muted-foreground mt-1">
                    {wsSettings.sessionsLimit
                      ? `${wsSettings.sessionsUsed} of ${wsSettings.sessionsLimit} sessions used this month`
                      : 'Unlimited sessions available'
                    }
                  </p>
                </div>
              </div>
            )}

            {!wsSettings.cloudPermissionEnabled && wsSettings.token && (
              <div className="p-3 bg-orange-500/10 border border-orange-500/30 rounded-lg flex items-start gap-2">
                <AlertCircle className="h-4 w-4 text-orange-400 shrink-0 mt-0.5" />
                <div className="text-sm text-orange-300">
                  Cloud sync is not enabled for your account. Enable it in your profile at qontinui.com.
                </div>
              </div>
            )}

            {/* Master Toggle - Send to Cloud */}
            <div className="space-y-2">
              <label className="flex items-center justify-between cursor-pointer">
                <div className="space-y-1">
                  <div className="font-medium">Send to Cloud</div>
                  <div className="text-sm text-muted-foreground">
                    Upload automation data to qontinui.com for analysis and collaboration
                  </div>
                </div>
                <button
                  onClick={() => handleToggleSendToCloud(!wsSettings.sendToCloud)}
                  disabled={!wsSettings.cloudPermissionEnabled}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    wsSettings.sendToCloud ? "bg-primary" : "bg-muted"
                  } ${!wsSettings.cloudPermissionEnabled ? "opacity-50 cursor-not-allowed" : ""}`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      wsSettings.sendToCloud ? "translate-x-6" : "translate-x-1"
                    }`}
                  />
                </button>
              </label>
            </div>

            {/* What to Send - only shown if sendToCloud enabled */}
            {wsSettings.sendToCloud && (
              <div className="space-y-3 pl-6 border-l-2 border-primary/30">
                <div className="text-sm font-medium">What to send:</div>

                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={wsSettings.sendLogs}
                    onChange={(e) => setWsSettings(prev => ({ ...prev, sendLogs: e.target.checked }))}
                    className="w-4 h-4 rounded border-gray-300 text-primary focus:ring-primary"
                  />
                  <span className="text-sm">Logs & Events (required for analysis)</span>
                </label>

                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={wsSettings.sendScreenshots}
                    onChange={(e) => setWsSettings(prev => ({ ...prev, sendScreenshots: e.target.checked }))}
                    className="w-4 h-4 rounded border-gray-300 text-primary focus:ring-primary"
                  />
                  <span className="text-sm">Screenshots (required for State Discovery)</span>
                </label>

                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={wsSettings.sendVideos}
                    onChange={(e) => setWsSettings(prev => ({ ...prev, sendVideos: e.target.checked }))}
                    className="w-4 h-4 rounded border-gray-300 text-primary focus:ring-primary"
                  />
                  <span className="text-sm">Videos (optional, large files)</span>
                </label>
              </div>
            )}

            {/* Info box */}
            <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
              <div className="text-sm text-muted-foreground">
                <strong className="text-foreground">Note:</strong> Cloud sync requires both account permission
                and your consent. You can control exactly what data is uploaded.
              </div>
            </div>

            {/* Apply/Disconnect buttons */}
            <div className="flex gap-3">
              {!wsSettings.connected ? (
                <button
                  onClick={applyWebSocketSettings}
                  className="px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
                >
                  Connect
                </button>
              ) : (
                <button
                  onClick={disconnectWebSocket}
                  className="px-4 py-2 bg-destructive text-destructive-foreground rounded-md hover:bg-destructive/90 transition-colors"
                >
                  Disconnect
                </button>
              )}
            </div>
          </>
        )}
      </div>

      {/* Debug Settings Form */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <h4 className="font-semibold text-lg mb-4">Debug</h4>

        {/* Image Debug Mode Toggle */}
        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer">
            <div className="space-y-1">
              <div className="font-medium">Enable Image Match Debug Mode</div>
              <div className="text-sm text-muted-foreground">
                Collect and display detailed match information in the Images tab, including top match candidates, confidence scores, and failure diagnostics
              </div>
            </div>
            <button
              onClick={handleToggleDebug}
              className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                settings.enable_image_debug ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                  settings.enable_image_debug ? "translate-x-6" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        {/* Top Matches Count */}
        <div className="space-y-2">
          <label className="block">
            <div className="font-medium mb-1">Top Matches to Display</div>
            <div className="text-sm text-muted-foreground mb-3">
              Number of top match candidates to show in debug information (1-10)
            </div>
            <div className="flex items-center gap-4">
              <input
                type="range"
                min="1"
                max="10"
                value={settings.top_matches_count}
                onChange={(e) => handleTopMatchesChange(parseInt(e.target.value))}
                className="flex-1 h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary"
              />
              <input
                type="number"
                min="1"
                max="10"
                value={settings.top_matches_count}
                onChange={(e) => handleTopMatchesChange(parseInt(e.target.value))}
                className="w-16 px-3 py-2 bg-input border border-border/50 rounded-md text-center"
              />
            </div>
          </label>
        </div>

        {/* Info box */}
        <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Debug settings take effect on the
            next image recognition attempt. These settings control the level of detail shown in the
            Image Recognition Debug tab.
          </div>
        </div>

        {/* Save Debug Settings Button */}
        <div className="flex justify-end">
          <button
            onClick={saveSettings}
            disabled={saving}
            className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
          >
            {saving ? (
              <>
                <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                Saving...
              </>
            ) : saveSuccess ? (
              <>
                <Check className="w-4 h-4" />
                Saved!
              </>
            ) : (
              <>
                <SettingsIcon className="w-4 h-4" />
                Save Debug Settings
              </>
            )}
          </button>
        </div>
      </div>

      {/* Screenshot Capture Settings */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <Camera className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Screenshot Capture Tool</h4>
        </div>

        {/* Capture Error message */}
        {captureError && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg flex items-start gap-2">
            <X className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
            <span className="text-red-400 text-sm">{captureError}</span>
          </div>
        )}

        {/* Capture Success message */}
        {captureSaveSuccess && (
          <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
            <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
            <span className="text-green-400 text-sm">Screenshot capture settings saved!</span>
          </div>
        )}

        {/* Enable Capture Toggle */}
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

        {/* Manual Click Capture Controls */}
        <div className="space-y-3">
          <div>
            <div className="font-medium mb-1">Manual Click Capture</div>
            <div className="text-sm text-muted-foreground">
              Capture screenshots when you physically click on the screen (for collecting initial training data before automation exists)
            </div>
          </div>

          <div className="flex items-center gap-3">
            {!manualCaptureRunning ? (
              <button
                onClick={handleStartManualCapture}
                disabled={!captureSettings.enabled || !captureSettings.outputFolder}
                className="px-4 py-2 bg-green-600 hover:bg-green-700 disabled:bg-gray-400 disabled:cursor-not-allowed text-white rounded transition-colors"
                title={!captureSettings.enabled ? "Enable capture tool first" : !captureSettings.outputFolder ? "Set output folder first" : "Start capturing screenshots on mouse clicks"}
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
                onChange={(e) => setCaptureSettings(prev => ({ ...prev, outputFolder: e.target.value }))}
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
              onChange={(e) => setCaptureSettings(prev => ({ ...prev, baseImageName: e.target.value }))}
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
          <div className="text-sm text-muted-foreground mb-3">
            Choose which screens to capture
          </div>
          <div className="flex flex-col gap-2">
            {monitors.length > 0 ? (
              <>
                {monitors.map((monitor) => {
                  const position = monitor.x < 0 ? 'left' : monitor.x > 0 ? 'right' : 'center';
                  const isSelected = captureSettings.screens.type === 'specific' &&
                                   captureSettings.screens.indices?.includes(monitor.index);
                  return (
                    <label key={monitor.index} className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="radio"
                        checked={isSelected}
                        onChange={() => handleScreenSelectionChange('specific', monitor.index)}
                        className="w-4 h-4 accent-primary"
                      />
                      <span>
                        Screen #{monitor.index + 1} {monitor.is_primary && <span className="text-primary">(primary)</span>}
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
                    checked={captureSettings.screens.type === 'all'}
                    onChange={() => handleScreenSelectionChange('all')}
                    className="w-4 h-4 accent-primary"
                  />
                  <span>All screens <span className="text-xs text-muted-foreground">({monitors.length} detected)</span></span>
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

        {/* Info box */}
        <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Screenshots are captured using the same
            tool used for pattern matching during automation. They will be numbered automatically based
            on existing files in the output folder.
          </div>
        </div>

        {/* Save Capture Settings Button */}
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

      {/* Local Storage Management */}
      <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
        <div className="flex items-center gap-3">
          <HardDrive className="w-5 h-5 text-primary" />
          <h4 className="font-semibold text-lg">Local Storage</h4>
        </div>

        {/* Storage Usage Display */}
        <div className="space-y-4">
          <div>
            <div className="font-medium mb-2">Storage Usage</div>

            {/* Screenshots */}
            <div className="space-y-2 mb-4">
              <div className="flex items-center justify-between text-sm">
                <span className="text-muted-foreground">Screenshots</span>
                <span className="font-medium">
                  {storageUsage.screenshots.toFixed(2)} MB / {storagePaths.max_screenshot_mb} MB
                  ({storageUsage.screenshotCount} files)
                </span>
              </div>
              <div className="w-full bg-muted rounded-full h-2">
                <div
                  className="bg-primary h-2 rounded-full transition-all"
                  style={{
                    width: `${Math.min(
                      100,
                      (storageUsage.screenshots / storagePaths.max_screenshot_mb) * 100
                    )}%`,
                  }}
                ></div>
              </div>
            </div>

            {/* Videos */}
            <div className="space-y-2">
              <div className="flex items-center justify-between text-sm">
                <span className="text-muted-foreground">Videos</span>
                <span className="font-medium">
                  {storageUsage.videos.toFixed(2)} MB / {storagePaths.max_video_mb} MB
                  ({storageUsage.videoCount} files)
                </span>
              </div>
              <div className="w-full bg-muted rounded-full h-2">
                <div
                  className="bg-primary h-2 rounded-full transition-all"
                  style={{
                    width: `${Math.min(
                      100,
                      (storageUsage.videos / storagePaths.max_video_mb) * 100
                    )}%`,
                  }}
                ></div>
              </div>
            </div>
          </div>

          {/* Storage Paths */}
          <div className="space-y-2">
            <div className="font-medium">Storage Locations</div>
            <div className="space-y-2 text-sm">
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Screenshots:</span>
                <div className="flex items-center gap-2">
                  <code className="px-2 py-1 bg-muted rounded text-xs max-w-md truncate">
                    {storagePaths.screenshot_path || "~/qontinui/screenshots/"}
                  </code>
                  <button
                    onClick={() => handleOpenStorageFolder('screenshots')}
                    className="px-2 py-1 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors"
                    title="Open folder"
                  >
                    <FolderOpen className="w-3 h-3" />
                  </button>
                </div>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">Videos:</span>
                <div className="flex items-center gap-2">
                  <code className="px-2 py-1 bg-muted rounded text-xs max-w-md truncate">
                    {storagePaths.video_path || "~/qontinui/videos/"}
                  </code>
                  <button
                    onClick={() => handleOpenStorageFolder('videos')}
                    className="px-2 py-1 bg-primary/10 hover:bg-primary/20 text-primary rounded-md transition-colors"
                    title="Open folder"
                  >
                    <FolderOpen className="w-3 h-3" />
                  </button>
                </div>
              </div>
            </div>
          </div>

          {/* Cleanup Actions */}
          <div className="space-y-2">
            <div className="font-medium">Storage Cleanup</div>
            <div className="flex flex-wrap gap-2">
              <button
                onClick={() => handleDeleteOldSessions('screenshots', 30)}
                className="px-3 py-2 bg-amber-500/10 hover:bg-amber-500/20 text-amber-600 rounded-md transition-colors flex items-center gap-2 text-sm"
              >
                <Trash2 className="w-4 h-4" />
                Delete Screenshots (30+ days)
              </button>
              <button
                onClick={() => handleDeleteOldSessions('videos', 30)}
                className="px-3 py-2 bg-amber-500/10 hover:bg-amber-500/20 text-amber-600 rounded-md transition-colors flex items-center gap-2 text-sm"
              >
                <Trash2 className="w-4 h-4" />
                Delete Videos (30+ days)
              </button>
              <button
                onClick={handleClearAllStorage}
                className="px-3 py-2 bg-red-500/10 hover:bg-red-500/20 text-red-600 rounded-md transition-colors flex items-center gap-2 text-sm"
              >
                <Trash className="w-4 h-4" />
                Clear All Storage
              </button>
            </div>
          </div>

          {/* Info box */}
          <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
            <div className="text-sm text-muted-foreground">
              <strong className="text-foreground">Storage Info:</strong> Screenshots and videos are
              organized by session. Auto-cleanup removes oldest sessions when storage limits are reached.
              Files are stored locally on your machine at the paths shown above.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

