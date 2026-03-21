/**
 * CaptureTab.tsx
 *
 * Dedicated tab for interaction recording - capturing user mouse and keyboard
 * input along with screen video for creating State Machines from user interactions.
 *
 * This complements Web Extraction (for websites) by enabling State Machine creation
 * from non-web or mixed environments.
 */

import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Video,
  Square,
  Play,
  MousePointer2,
  Keyboard,
  Timer,
  FolderOpen,
  Settings2,
  CheckCircle,
  AlertCircle,
  Loader2,
  Info,
} from "lucide-react";
import { getAccentColors, getStatusColors } from "@/design-system";

interface InteractionRecordingStatus {
  is_recording: boolean;
  session_id?: string;
  duration?: number;
  events_count?: number;
  fps?: number;
}

interface CaptureTabProps {
  onLog: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

export function CaptureTab({ onLog }: CaptureTabProps) {
  // Recording state
  const [isRecording, setIsRecording] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [duration, setDuration] = useState(0);
  const [eventsCount, setEventsCount] = useState(0);
  const [isStarting, setIsStarting] = useState(false);
  const [isStopping, setIsStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastRecording, setLastRecording] = useState<{
    sessionId: string;
    duration: number;
    eventsCount: number;
    eventsFile?: string;
  } | null>(null);

  // Settings
  const [fps, setFps] = useState(30);
  const [showSettings, setShowSettings] = useState(false);

  // Timer ref for duration updates
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startTimeRef = useRef<number | null>(null);

  // Listen for recording events from Python executor
  useEffect(() => {
    let unlisten: (() => void) | undefined;

    listen(
      "executor-event",
      (event: {
        payload?: {
          data?: {
            type?: string;
            session_id?: string;
            events_count?: number;
            duration?: number;
            events_file?: string;
          };
        };
      }) => {
        const data = event.payload?.data;
        if (!data) return;

        if (data.type === "interaction_recording_started") {
          setIsRecording(true);
          setSessionId(data.session_id || null);
          setEventsCount(0);
          startTimeRef.current = Date.now();
          onLog("success", `Interaction recording started: ${data.session_id}`);
        } else if (data.type === "interaction_recording_stopped") {
          setIsRecording(false);
          setLastRecording({
            sessionId: data.session_id || "",
            duration: data.duration || duration,
            eventsCount: data.events_count || eventsCount,
            eventsFile: data.events_file,
          });
          onLog(
            "success",
            `Recording stopped: ${data.events_count} events captured in ${(data.duration || 0).toFixed(1)}s`,
          );
        }
      },
    ).then((fn) => {
      unlisten = fn;
    });

    return () => {
      if (unlisten) unlisten();
    };
  }, [duration, eventsCount, onLog]);

  // Update duration while recording
  useEffect(() => {
    if (isRecording && startTimeRef.current) {
      timerRef.current = setInterval(() => {
        const elapsed = (Date.now() - (startTimeRef.current || Date.now())) / 1000;
        setDuration(elapsed);
      }, 100);
    } else {
      if (timerRef.current) {
        clearInterval(timerRef.current);
        timerRef.current = null;
      }
    }

    return () => {
      if (timerRef.current) {
        clearInterval(timerRef.current);
      }
    };
  }, [isRecording]);

  // Poll for status updates during recording
  useEffect(() => {
    if (!isRecording) return;

    const pollStatus = async () => {
      try {
        const result = await invoke<{ success: boolean; data?: InteractionRecordingStatus }>(
          "get_interaction_recording_status",
        );
        if (result?.success && result.data) {
          if (result.data.events_count !== undefined) {
            setEventsCount(result.data.events_count);
          }
        }
      } catch {
        // Ignore polling errors
      }
    };

    const pollInterval = setInterval(pollStatus, 1000);
    return () => clearInterval(pollInterval);
  }, [isRecording]);

  const handleStartRecording = async () => {
    setError(null);
    setIsStarting(true);

    try {
      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: { session_id?: string };
      }>("start_interaction_recording", {
        config: {
          fps,
        },
      });

      if (result?.success) {
        setIsRecording(true);
        setSessionId(result.data?.session_id || null);
        setDuration(0);
        setEventsCount(0);
        startTimeRef.current = Date.now();
        onLog("success", "Interaction recording started");
      } else {
        const errorMsg = result?.message || "Failed to start recording";
        setError(errorMsg);
        onLog("error", errorMsg);
      }
    } catch (err) {
      const errorMsg = `Failed to start recording: ${err}`;
      setError(errorMsg);
      onLog("error", errorMsg);
    } finally {
      setIsStarting(false);
    }
  };

  const handleStopRecording = async () => {
    setError(null);
    setIsStopping(true);

    try {
      const result = await invoke<{
        success: boolean;
        message?: string;
        data?: {
          session_id?: string;
          events_file?: string;
          events_count?: number;
          duration?: number;
        };
      }>("stop_interaction_recording");

      if (result?.success) {
        setIsRecording(false);
        setLastRecording({
          sessionId: result.data?.session_id || sessionId || "",
          duration: result.data?.duration || duration,
          eventsCount: result.data?.events_count || eventsCount,
          eventsFile: result.data?.events_file,
        });
        setSessionId(null);
        onLog(
          "success",
          `Recording stopped: ${result.data?.events_count || eventsCount} events captured`,
        );
      } else {
        const errorMsg = result?.message || "Failed to stop recording";
        setError(errorMsg);
        onLog("error", errorMsg);
      }
    } catch (err) {
      const errorMsg = `Failed to stop recording: ${err}`;
      setError(errorMsg);
      onLog("error", errorMsg);
    } finally {
      setIsStopping(false);
    }
  };

  const formatDuration = (seconds: number): string => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    const ms = Math.floor((seconds % 1) * 10);
    return `${mins.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}.${ms}`;
  };

  const openOutputFolder = async () => {
    if (lastRecording?.eventsFile) {
      try {
        // Extract directory from file path
        const dir = lastRecording.eventsFile.replace(/[/][^/]+$/, "");
        await invoke("open_folder", { path: dir });
      } catch (err) {
        onLog("error", `Failed to open folder: ${err}`);
      }
    }
  };

  return (
    <div className="h-full flex flex-col p-4 gap-4 overflow-hidden">
      {/* Header */}
      <div className="shrink-0 px-4 py-3 rounded-lg bg-muted/30">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div
              className={`w-8 h-8 rounded-full flex items-center justify-center ${
                isRecording ? getStatusColors("error").bg : "bg-muted"
              }`}
            >
              <Video
                className={`w-4 h-4 ${isRecording ? getStatusColors("error").icon : "text-muted-foreground"}`}
              />
            </div>
            <div>
              <h3 className="font-semibold text-foreground text-sm">Interaction Recording</h3>
              <p className="text-xs text-muted-foreground">
                Capture mouse and keyboard input for State Machine creation
              </p>
            </div>
          </div>
          <button
            onClick={() => setShowSettings(!showSettings)}
            className={`p-2 rounded-lg transition-colors ${
              showSettings
                ? "bg-primary/20 text-primary"
                : "hover:bg-muted/50 text-muted-foreground"
            }`}
          >
            <Settings2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Settings Panel (collapsible) */}
      {showSettings && (
        <div className="shrink-0 px-4 py-3 rounded-lg bg-card/50 border border-border/50">
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <span className="text-xs font-medium text-muted-foreground">Video FPS</span>
              <div className="flex items-center gap-2">
                <input
                  type="range"
                  min="10"
                  max="60"
                  step="5"
                  value={fps}
                  onChange={(e) => setFps(parseInt(e.target.value))}
                  className="w-24 h-1.5 bg-muted rounded-lg appearance-none cursor-pointer"
                  disabled={isRecording}
                />
                <span className="text-xs w-8 text-right">{fps}</span>
              </div>
            </div>
            <p className="text-[10px] text-muted-foreground">
              Higher FPS gives smoother video but larger files. 30 FPS is recommended.
            </p>
          </div>
        </div>
      )}

      {/* Main Content */}
      <div className="flex-1 flex flex-col min-h-0 rounded-lg bg-card/50 overflow-hidden">
        {/* Recording Status */}
        <div className="shrink-0 p-4">
          {error && (
            <div
              className={`mb-3 px-3 py-2 ${getStatusColors("error").bg} rounded-lg flex items-start gap-2`}
            >
              <AlertCircle className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
              <span className={`${getStatusColors("error").text} text-xs`}>{error}</span>
            </div>
          )}

          {isRecording ? (
            <div className="space-y-4">
              {/* Recording indicator */}
              <div className="flex items-center justify-center gap-3">
                <span className="relative flex h-3 w-3">
                  <span
                    className={`animate-ping absolute inline-flex h-full w-full rounded-full ${getAccentColors("red").bgSolid} opacity-75`}
                  ></span>
                  <span
                    className={`relative inline-flex rounded-full h-3 w-3 ${getAccentColors("red").bgSolid}`}
                  ></span>
                </span>
                <span className={`text-lg font-mono font-semibold ${getAccentColors("red").text}`}>
                  {formatDuration(duration)}
                </span>
              </div>

              {/* Stats */}
              <div className="flex justify-center gap-6">
                <div className="flex items-center gap-2 text-muted-foreground">
                  <MousePointer2 className="w-4 h-4" />
                  <span className="text-sm">{eventsCount} events</span>
                </div>
                <div className="flex items-center gap-2 text-muted-foreground">
                  <Video className="w-4 h-4" />
                  <span className="text-sm">{fps} FPS</span>
                </div>
              </div>

              {/* Session ID */}
              {sessionId && (
                <div className="text-center text-xs text-muted-foreground">
                  Session: {sessionId}
                </div>
              )}

              {/* Stop Button */}
              <div className="flex justify-center">
                <button
                  onClick={handleStopRecording}
                  disabled={isStopping}
                  className={`flex items-center gap-2 px-6 py-3 ${getAccentColors("red").bgSolid} hover:opacity-90 disabled:opacity-50 text-white rounded-lg font-medium transition-colors`}
                >
                  {isStopping ? (
                    <>
                      <Loader2 className="w-5 h-5 animate-spin" />
                      Stopping...
                    </>
                  ) : (
                    <>
                      <Square className="w-5 h-5" />
                      Stop Recording
                    </>
                  )}
                </button>
              </div>
            </div>
          ) : (
            <div className="space-y-4">
              {/* Start Recording */}
              <div className="flex flex-col items-center gap-4">
                <div className="text-center space-y-2">
                  <div className="flex items-center justify-center gap-4 text-muted-foreground">
                    <div className="flex items-center gap-1.5">
                      <MousePointer2 className="w-4 h-4" />
                      <span className="text-sm">Mouse</span>
                    </div>
                    <div className="flex items-center gap-1.5">
                      <Keyboard className="w-4 h-4" />
                      <span className="text-sm">Keyboard</span>
                    </div>
                    <div className="flex items-center gap-1.5">
                      <Video className="w-4 h-4" />
                      <span className="text-sm">Video</span>
                    </div>
                  </div>
                  <p className="text-xs text-muted-foreground max-w-sm">
                    Record your interactions to create State Machines from complex workflows, drags,
                    and multi-step processes.
                  </p>
                </div>

                <button
                  onClick={handleStartRecording}
                  disabled={isStarting}
                  className={`flex items-center gap-2 px-6 py-3 ${getAccentColors("green").bgSolid} hover:opacity-90 disabled:opacity-50 text-white rounded-lg font-medium transition-colors`}
                >
                  {isStarting ? (
                    <>
                      <Loader2 className="w-5 h-5 animate-spin" />
                      Starting...
                    </>
                  ) : (
                    <>
                      <Play className="w-5 h-5" />
                      Start Recording
                    </>
                  )}
                </button>
              </div>

              {/* Last Recording Info */}
              {lastRecording && (
                <div className="mt-6 p-3 rounded-lg bg-muted/30 space-y-2">
                  <div className="flex items-center gap-2 text-sm font-medium text-foreground">
                    <CheckCircle className={`w-4 h-4 ${getStatusColors("success").icon}`} />
                    Last Recording
                  </div>
                  <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
                    <div className="flex items-center gap-1.5">
                      <Timer className="w-3 h-3" />
                      <span>{lastRecording.duration.toFixed(1)}s</span>
                    </div>
                    <div className="flex items-center gap-1.5">
                      <MousePointer2 className="w-3 h-3" />
                      <span>{lastRecording.eventsCount} events</span>
                    </div>
                  </div>
                  {lastRecording.eventsFile && (
                    <button
                      onClick={openOutputFolder}
                      className="flex items-center gap-1.5 text-xs text-primary hover:text-primary/80 transition-colors"
                    >
                      <FolderOpen className="w-3 h-3" />
                      Open Output Folder
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
        </div>

        {/* Info Section */}
        <div className="shrink-0 mt-auto p-4 border-t border-border/30">
          <div className="flex items-start gap-2 text-muted-foreground">
            <Info className="w-4 h-4 shrink-0 mt-0.5" />
            <div className="text-xs space-y-1">
              <p>
                <strong>Purpose:</strong> Create State Machines from desktop applications and mixed
                environments (not just websites).
              </p>
              <p>
                <strong>Output:</strong> Input events (JSONL) with timestamps for correlation with
                video frames.
              </p>
              <p>
                <strong>Use cases:</strong> Complex drag operations, multi-step workflows, desktop
                app automation.
              </p>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
