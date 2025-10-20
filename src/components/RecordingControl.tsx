import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Circle, FolderOpen, Settings2, Info, Clock, ExternalLink, Upload } from "lucide-react";

interface RecordingControlProps {
  pythonStatus: "stopped" | "running";
  configLoaded: boolean;
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

interface RecordingStatus {
  isRecording: boolean;
  snapshotDirectory?: string;
  statistics?: {
    actions_recorded: number;
    screenshots_captured: number;
    patterns_tracked: number;
  };
}

interface RecordingSettings {
  baseDir: string;
  autoImport: boolean;
}

interface RecordingHistoryEntry {
  runId: string;
  timestamp: number;
  directory: string;
  actionCount: number;
  screenshotCount: number;
  duration: number;
  status: "success" | "failed";
}

function RecordingControl({ pythonStatus, configLoaded, onLog }: RecordingControlProps) {
  const [recordingStatus, setRecordingStatus] = useState<RecordingStatus>({
    isRecording: false,
  });
  const [settings, setSettings] = useState<RecordingSettings>(() => {
    const saved = localStorage.getItem("recordingSettings");
    return saved ? JSON.parse(saved) : { baseDir: "/tmp/qontinui-snapshots", autoImport: false };
  });
  const [showSettings, setShowSettings] = useState(false);
  const [pollingInterval, setPollingInterval] = useState<number | null>(null);
  const [recordingStartTime, setRecordingStartTime] = useState<number | null>(null);
  const [elapsedTime, setElapsedTime] = useState<number>(0);
  const [history, setHistory] = useState<RecordingHistoryEntry[]>(() => {
    const saved = localStorage.getItem("recordingHistory");
    return saved ? JSON.parse(saved) : [];
  });
  const [isImporting, setIsImporting] = useState(false);

  // Save settings to localStorage whenever they change
  useEffect(() => {
    localStorage.setItem("recordingSettings", JSON.stringify(settings));
  }, [settings]);

  // Save history to localStorage whenever it changes
  useEffect(() => {
    localStorage.setItem("recordingHistory", JSON.stringify(history));
  }, [history]);

  // Update elapsed time every second when recording
  useEffect(() => {
    let timer: number | null = null;
    if (recordingStatus.isRecording && recordingStartTime) {
      timer = window.setInterval(() => {
        setElapsedTime(Math.floor((Date.now() - recordingStartTime) / 1000));
      }, 1000);
    } else {
      setElapsedTime(0);
    }

    return () => {
      if (timer) {
        window.clearInterval(timer);
      }
    };
  }, [recordingStatus.isRecording, recordingStartTime]);

  // Poll recording status when recording is active
  useEffect(() => {
    if (recordingStatus.isRecording && !pollingInterval) {
      const interval = window.setInterval(async () => {
        try {
          const result: any = await invoke("get_recording_status");
          if (result.success && result.data) {
            setRecordingStatus({
              isRecording: result.data.is_recording || false,
              snapshotDirectory: result.data.snapshot_directory,
              statistics: result.data.statistics,
            });
          }
        } catch (error) {
          console.error("Failed to poll recording status:", error);
        }
      }, 2000); // Poll every 2 seconds

      setPollingInterval(interval);
    } else if (!recordingStatus.isRecording && pollingInterval) {
      window.clearInterval(pollingInterval);
      setPollingInterval(null);
    }

    return () => {
      if (pollingInterval) {
        window.clearInterval(pollingInterval);
      }
    };
  }, [recordingStatus.isRecording, pollingInterval]);

  // Listen for recording events
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setupListeners = async () => {
      const { listen } = await import("@tauri-apps/api/event");

      const unlistenFn = await listen("executor-event", (event: any) => {
        const data = event.payload;

        if (data.event === "recording_started") {
          setRecordingStatus({
            isRecording: true,
            snapshotDirectory: data.data.snapshot_directory,
          });
          setRecordingStartTime(Date.now());
          onLog("success", `Recording started: ${data.data.snapshot_directory}`);
        }

        if (data.event === "recording_stopped") {
          const stopTime = Date.now();
          const duration = recordingStartTime
            ? Math.floor((stopTime - recordingStartTime) / 1000)
            : 0;

          setRecordingStatus({
            isRecording: false,
            snapshotDirectory: data.data.snapshot_directory,
          });

          // Add to history
          const newEntry: RecordingHistoryEntry = {
            runId: data.data.run_id || `recording-${stopTime}`,
            timestamp: stopTime,
            directory: data.data.snapshot_directory,
            actionCount: recordingStatus.statistics?.actions_recorded || 0,
            screenshotCount: recordingStatus.statistics?.screenshots_captured || 0,
            duration,
            status: "success",
          };

          setHistory((prev) => [newEntry, ...prev.slice(0, 4)]);
          setRecordingStartTime(null);

          onLog(
            "success",
            `Recording stopped: ${data.data.snapshot_directory} (${duration}s, ${newEntry.actionCount} actions, ${newEntry.screenshotCount} screenshots)`,
          );

          // Auto-import if enabled
          if (settings.autoImport) {
            handleImportSnapshot(data.data.snapshot_directory);
          }
        }
      });

      unlisten = unlistenFn;
    };

    setupListeners();

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, [onLog, recordingStartTime, recordingStatus.statistics, settings.autoImport]);

  const handleStartRecording = async () => {
    try {
      onLog("info", `Starting recording with base directory: ${settings.baseDir}`);
      const result: any = await invoke("start_recording", { baseDir: settings.baseDir });

      if (result.success) {
        setRecordingStatus({
          isRecording: true,
          snapshotDirectory: result.snapshot_directory,
        });
        setRecordingStartTime(Date.now());
        onLog("success", `Recording started: ${result.snapshot_directory}`);
      } else {
        onLog("error", `Failed to start recording: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog("error", `Failed to start recording: ${error}`);
    }
  };

  const handleStopRecording = async () => {
    try {
      onLog("info", "Stopping recording...");
      const result: any = await invoke("stop_recording");

      if (result.success) {
        const stopTime = Date.now();
        const duration = recordingStartTime
          ? Math.floor((stopTime - recordingStartTime) / 1000)
          : 0;

        setRecordingStatus({
          isRecording: false,
          snapshotDirectory: result.snapshot_directory,
        });

        onLog(
          "success",
          `Recording stopped: ${result.snapshot_directory} (Duration: ${formatDuration(duration)})`,
        );
      } else {
        onLog("error", `Failed to stop recording: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog("error", `Failed to stop recording: ${error}`);
    }
  };

  const handleBrowseDirectory = async () => {
    try {
      const result = await open({
        directory: true,
        multiple: false,
        defaultPath: settings.baseDir,
      });

      if (result && typeof result === "string") {
        setSettings((prev) => ({ ...prev, baseDir: result }));
        onLog("info", `Base directory updated: ${result}`);
      }
    } catch (error) {
      onLog("error", `Failed to open directory picker: ${error}`);
    }
  };

  const handleOpenFolder = async (directory: string) => {
    try {
      onLog("info", `Opening folder: ${directory}`);
      const result: any = await invoke("open_folder", { path: directory });

      if (result.success) {
        onLog("success", `Opened folder: ${directory}`);
      } else {
        onLog("error", `Failed to open folder: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog("error", `Failed to open folder: ${error}`);
    }
  };

  const handleImportSnapshot = async (directory: string) => {
    setIsImporting(true);
    try {
      onLog("info", `Importing snapshot from: ${directory}`);

      const response = await fetch("http://localhost:8000/api/snapshots/import", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ directory }),
      });

      if (response.ok) {
        const data = await response.json();
        onLog("success", `Snapshot imported successfully: ${data.run_id || directory}`);
      } else {
        const error = await response.text();
        onLog("error", `Failed to import snapshot: ${error}`);
      }
    } catch (error) {
      onLog("error", `Failed to import snapshot: ${error}`);
    } finally {
      setIsImporting(false);
    }
  };

  const formatDuration = (seconds: number): string => {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return mins > 0 ? `${mins}m ${secs}s` : `${secs}s`;
  };

  const formatTimestamp = (timestamp: number): string => {
    const date = new Date(timestamp);
    return date.toLocaleString();
  };

  const canRecord = pythonStatus === "running" && configLoaded;

  return (
    <div className="space-y-4">
      {/* Main Recording Control */}
      <div className="bg-card rounded-lg border border-border/50 p-4">
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Circle
              className={`w-4 h-4 ${
                recordingStatus.isRecording
                  ? "fill-red-500 text-red-500 animate-pulse"
                  : "text-muted-foreground"
              }`}
            />
            <h3 className="text-lg font-semibold">Snapshot Recording</h3>
          </div>

          <button
            onClick={() => setShowSettings(!showSettings)}
            className="p-1 hover:bg-accent/10 rounded-md transition-colors"
            title="Recording Settings"
          >
            <Settings2 className="w-4 h-4 text-muted-foreground" />
          </button>
        </div>

        {showSettings && (
          <div className="mb-3 p-3 bg-background/50 rounded-lg border border-border/30 space-y-3">
            <div>
              <label className="block text-sm font-medium mb-2 text-muted-foreground">
                Base Directory
              </label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={settings.baseDir}
                  onChange={(e) => setSettings((prev) => ({ ...prev, baseDir: e.target.value }))}
                  disabled={recordingStatus.isRecording}
                  className="flex-1 px-3 py-2 bg-input border border-border/50 rounded-md text-sm disabled:opacity-50"
                  placeholder="/tmp/qontinui-snapshots"
                />
                <button
                  onClick={handleBrowseDirectory}
                  disabled={recordingStatus.isRecording}
                  className="px-3 py-2 bg-input border border-border/50 rounded-md hover:bg-accent/10 transition-colors disabled:opacity-50"
                  title="Browse for directory"
                >
                  <FolderOpen className="w-4 h-4" />
                </button>
              </div>
              <p className="text-xs text-muted-foreground mt-2">
                Snapshots will be saved to a timestamped subdirectory
              </p>
            </div>

            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="auto-import"
                checked={settings.autoImport}
                onChange={(e) => setSettings((prev) => ({ ...prev, autoImport: e.target.checked }))}
                className="w-4 h-4 rounded border-border/50 bg-input"
              />
              <label htmlFor="auto-import" className="text-sm text-muted-foreground cursor-pointer">
                Auto-import snapshot to database after recording
              </label>
            </div>
          </div>
        )}

        <div className="flex gap-2 mb-3">
          {!recordingStatus.isRecording ? (
            <button
              onClick={handleStartRecording}
              disabled={!canRecord}
              className="flex-1 bg-red-500 hover:bg-red-600 text-white px-4 py-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-2"
              title={
                !canRecord
                  ? "Python executor must be running and config must be loaded"
                  : "Start recording snapshots"
              }
            >
              <Circle className="w-4 h-4" />
              Start Recording
            </button>
          ) : (
            <button
              onClick={handleStopRecording}
              className="flex-1 bg-gray-600 hover:bg-gray-700 text-white px-4 py-2 rounded-md font-medium transition-colors flex items-center justify-center gap-2"
            >
              <Circle className="w-4 h-4 fill-red-500 animate-pulse" />
              Stop Recording
            </button>
          )}
        </div>

        {recordingStatus.isRecording && (
          <div className="space-y-2">
            <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg">
              <div className="flex items-start gap-2">
                <Circle className="w-4 h-4 fill-red-500 text-red-500 animate-pulse mt-0.5" />
                <div className="flex-1">
                  <div className="text-sm font-semibold text-red-400 mb-1">Recording Active</div>
                  {recordingStatus.snapshotDirectory && (
                    <div className="text-xs font-mono text-muted-foreground break-all mb-2">
                      {recordingStatus.snapshotDirectory}
                    </div>
                  )}
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <Clock className="w-3 h-3" />
                    <span>Elapsed: {formatDuration(elapsedTime)}</span>
                  </div>
                </div>
              </div>
            </div>

            {recordingStatus.statistics && (
              <div className="grid grid-cols-3 gap-2 text-sm">
                <div className="p-2 bg-background/50 rounded-lg border border-border/30">
                  <div className="text-muted-foreground text-xs">Actions</div>
                  <div className="text-lg font-semibold text-accent">
                    {recordingStatus.statistics.actions_recorded}
                  </div>
                </div>
                <div className="p-2 bg-background/50 rounded-lg border border-border/30">
                  <div className="text-muted-foreground text-xs">Screenshots</div>
                  <div className="text-lg font-semibold text-primary">
                    {recordingStatus.statistics.screenshots_captured}
                  </div>
                </div>
                <div className="p-2 bg-background/50 rounded-lg border border-border/30">
                  <div className="text-muted-foreground text-xs">Patterns</div>
                  <div className="text-lg font-semibold text-secondary">
                    {recordingStatus.statistics.patterns_tracked}
                  </div>
                </div>
              </div>
            )}
          </div>
        )}

        {!recordingStatus.isRecording && recordingStatus.snapshotDirectory && (
          <div className="p-3 bg-background/50 rounded-lg border border-border/30">
            <div className="flex items-start gap-2">
              <Info className="w-4 h-4 text-muted-foreground mt-0.5" />
              <div className="flex-1">
                <div className="text-sm font-medium mb-1">Last Recording</div>
                <div className="text-xs font-mono text-muted-foreground break-all mb-2">
                  {recordingStatus.snapshotDirectory}
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={() => handleOpenFolder(recordingStatus.snapshotDirectory!)}
                    className="px-2 py-1 text-xs bg-input border border-border/50 rounded hover:bg-accent/10 transition-colors flex items-center gap-1"
                  >
                    <ExternalLink className="w-3 h-3" />
                    Open Folder
                  </button>
                  {!settings.autoImport && (
                    <button
                      onClick={() => handleImportSnapshot(recordingStatus.snapshotDirectory!)}
                      disabled={isImporting}
                      className="px-2 py-1 text-xs bg-input border border-border/50 rounded hover:bg-accent/10 transition-colors flex items-center gap-1 disabled:opacity-50"
                    >
                      <Upload className="w-3 h-3" />
                      {isImporting ? "Importing..." : "Import"}
                    </button>
                  )}
                </div>
              </div>
            </div>
          </div>
        )}

        {!canRecord && (
          <div className="mt-2 p-2 bg-yellow-500/10 border border-yellow-500/30 rounded-lg">
            <div className="flex items-center gap-2 text-xs text-yellow-400">
              <Info className="w-3 h-3" />
              <span>Python executor and configuration required for recording</span>
            </div>
          </div>
        )}
      </div>

      {/* Recording History */}
      {history.length > 0 && (
        <div className="bg-card rounded-lg border border-border/50 p-4">
          <h4 className="text-sm font-semibold mb-3 text-muted-foreground">Recent Recordings</h4>
          <div className="space-y-2">
            {history.map((entry, index) => (
              <div
                key={`${entry.timestamp}-${index}`}
                className="p-2 bg-background/50 rounded-lg border border-border/30 text-xs"
              >
                <div className="flex items-start justify-between gap-2 mb-1">
                  <div className="flex-1 min-w-0">
                    <div className="font-mono text-muted-foreground truncate">{entry.runId}</div>
                    <div className="text-[10px] text-muted-foreground/70">
                      {formatTimestamp(entry.timestamp)}
                    </div>
                  </div>
                  <div
                    className={`px-2 py-0.5 rounded text-[10px] font-medium ${
                      entry.status === "success"
                        ? "bg-green-500/20 text-green-400"
                        : "bg-red-500/20 text-red-400"
                    }`}
                  >
                    {entry.status}
                  </div>
                </div>
                <div className="flex items-center gap-3 text-[10px] text-muted-foreground mb-2">
                  <span>{entry.actionCount} actions</span>
                  <span>•</span>
                  <span>{entry.screenshotCount} screenshots</span>
                  <span>•</span>
                  <span>{formatDuration(entry.duration)}</span>
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={() => handleOpenFolder(entry.directory)}
                    className="px-2 py-1 text-[10px] bg-input border border-border/50 rounded hover:bg-accent/10 transition-colors flex items-center gap-1"
                  >
                    <ExternalLink className="w-2.5 h-2.5" />
                    Open
                  </button>
                  <button
                    onClick={() => handleImportSnapshot(entry.directory)}
                    disabled={isImporting}
                    className="px-2 py-1 text-[10px] bg-input border border-border/50 rounded hover:bg-accent/10 transition-colors flex items-center gap-1 disabled:opacity-50"
                  >
                    <Upload className="w-2.5 h-2.5" />
                    Import
                  </button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

export default RecordingControl;
