import { useState, useMemo } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  Loader2,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  CheckCircle,
  Smartphone,
  Terminal,
  Camera,
} from "lucide-react";
import { useRunSelection } from "../../contexts/RunSelectionContext";
import {
  useMobileStates,
  useMobileLogs,
  useMobileErrors,
  useMobileDevices,
  useCaptureFeedback,
} from "../../hooks/useMobileData";
import { getAccentColors } from "@/design-system";

export function MobileStateSection() {
  const { selectedRunId } = useRunSelection();
  const { data: states, isLoading, error } = useMobileStates(selectedRunId, 50);
  const { data: devices } = useMobileDevices();
  const captureFeedback = useCaptureFeedback();
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());

  const toggleExpanded = (id: number) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const handleCapture = async () => {
    if (!selectedRunId || !devices?.length) return;
    try {
      await captureFeedback.mutateAsync({
        taskRunId: selectedRunId,
        deviceId: devices[0].device_id,
      });
    } catch (err) {
      console.error("Failed to capture mobile feedback:", err);
    }
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Smartphone className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile state</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading mobile state: {String(error)}</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Mobile App State</h3>
        <div className="flex items-center gap-2">
          {devices && devices.length > 0 && (
            <span className="text-xs text-muted-foreground">
              {devices.length} device(s) connected
            </span>
          )}
          <button
            onClick={handleCapture}
            disabled={captureFeedback.isPending || !devices?.length}
            className="px-3 py-1.5 text-sm rounded-md bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {captureFeedback.isPending ? "Capturing..." : "Capture Now"}
          </button>
        </div>
      </div>

      {!states || states.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <Smartphone className="w-8 h-8 mx-auto mb-3 opacity-50" />
          <p className="text-sm">No mobile state captures for this task run</p>
          <p className="text-xs mt-1">Click &quot;Capture Now&quot; to capture device state</p>
        </div>
      ) : (
        <div className="space-y-2">
          {states.map((state) => (
            <div key={state.id} className="border rounded-lg bg-card overflow-hidden">
              <button
                onClick={() => toggleExpanded(state.id)}
                className="w-full flex items-center gap-3 p-3 hover:bg-muted/50 transition-colors"
              >
                {expandedIds.has(state.id) ? (
                  <ChevronDown className="w-4 h-4 shrink-0" />
                ) : (
                  <ChevronRight className="w-4 h-4 shrink-0" />
                )}
                <div className="flex-1 flex items-center gap-3">
                  <span
                    className={`px-2 py-0.5 text-xs rounded-full ${
                      state.device_type === "emulator"
                        ? getAccentColors("blue").bg + " " + getAccentColors("blue").text
                        : getAccentColors("green").bg + " " + getAccentColors("green").text
                    }`}
                  >
                    {state.device_type || "unknown"}
                  </span>
                  <span className="text-sm font-medium">{state.device_id || "Unknown Device"}</span>
                  {state.has_errors && <AlertCircle className="w-4 h-4 text-destructive" />}
                </div>
                <span className="text-xs text-muted-foreground">
                  {new Date(state.timestamp).toLocaleTimeString()}
                </span>
              </button>

              {expandedIds.has(state.id) && (
                <div className="px-3 pb-3 pt-1 border-t bg-muted/20">
                  <div className="grid grid-cols-2 gap-4 text-sm">
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">App State</div>
                      <div>{state.app_state || "-"}</div>
                    </div>
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">Metro</div>
                      <div className="flex items-center gap-2">
                        <span
                          className={`w-2 h-2 rounded-full ${
                            state.metro_connected ? "bg-green-500" : "bg-muted"
                          }`}
                        />
                        {state.metro_connected ? "Connected" : "Disconnected"}
                      </div>
                    </div>
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">Bundle Status</div>
                      <div>{state.bundle_status || "-"}</div>
                    </div>
                    <div>
                      <div className="text-muted-foreground text-xs mb-1">Last Reload</div>
                      <div>{state.last_reload_type || "-"}</div>
                    </div>
                    {state.screenshot_path && (
                      <div className="col-span-2">
                        <div className="text-muted-foreground text-xs mb-1">Screenshot</div>
                        <div className="text-xs font-mono truncate">{state.screenshot_path}</div>
                      </div>
                    )}
                    {state.error_summary && (
                      <div className="col-span-2">
                        <div className="text-muted-foreground text-xs mb-1">Error Summary</div>
                        <div className="text-destructive text-sm">{state.error_summary}</div>
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function MobileScreenshotsSection() {
  const { selectedRunId } = useRunSelection();
  const { data: states, isLoading, error } = useMobileStates(selectedRunId, 100);

  const statesWithScreenshots = useMemo(
    () => states?.filter((s) => s.screenshot_path) || [],
    [states],
  );

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Camera className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile screenshots</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading screenshots: {String(error)}</p>
      </div>
    );
  }

  if (statesWithScreenshots.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Camera className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">No mobile screenshots for this task run</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <h3 className="text-lg font-semibold">Mobile Screenshots</h3>
      <div className="grid grid-cols-2 gap-4">
        {statesWithScreenshots.map((state) => (
          <div key={state.id} className="border rounded-lg bg-card overflow-hidden">
            <div className="aspect-[9/16] bg-muted relative">
              {state.screenshot_path && (
                <img
                  src={convertFileSrc(state.screenshot_path)}
                  alt={`Screenshot at ${state.timestamp}`}
                  className="w-full h-full object-contain"
                  onError={(e) => {
                    (e.target as HTMLImageElement).style.display = "none";
                  }}
                />
              )}
            </div>
            <div className="p-2 border-t">
              <div className="text-xs text-muted-foreground">
                {new Date(state.timestamp).toLocaleString()}
              </div>
              <div className="text-xs truncate font-mono mt-1">
                {state.device_id || "Unknown Device"}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export function MobileLogsSection() {
  const { selectedRunId } = useRunSelection();
  const [logSource, setLogSource] = useState<string | undefined>(undefined);
  const { data: logs, isLoading, error } = useMobileLogs(selectedRunId, logSource, false, 500);

  const getLogLevelStyle = (level?: string) => {
    const l = level?.toLowerCase() || "";
    if (l === "e" || l === "error" || l === "fatal" || l === "f") {
      return "text-destructive";
    }
    if (l === "w" || l === "warn" || l === "warning") {
      return "text-yellow-600 dark:text-yellow-500";
    }
    if (l === "d" || l === "debug") {
      return "text-muted-foreground";
    }
    return "text-foreground";
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <Terminal className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile logs</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading logs: {String(error)}</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Mobile Logs</h3>
        <select
          value={logSource || "all"}
          onChange={(e) => setLogSource(e.target.value === "all" ? undefined : e.target.value)}
          className="text-sm border rounded-md px-2 py-1 bg-background"
        >
          <option value="all">All Sources</option>
          <option value="logcat">Logcat</option>
          <option value="metro">Metro</option>
          <option value="build">Build</option>
        </select>
      </div>

      {!logs || logs.length === 0 ? (
        <div className="text-center py-8 text-muted-foreground">
          <Terminal className="w-8 h-8 mx-auto mb-3 opacity-50" />
          <p className="text-sm">No mobile logs for this task run</p>
        </div>
      ) : (
        <div className="space-y-1 font-mono text-xs">
          {logs.map((log) => (
            <div
              key={log.id}
              className={`flex gap-2 p-1 hover:bg-muted/50 rounded ${getLogLevelStyle(log.log_level)}`}
            >
              <span className="text-muted-foreground shrink-0 w-20">
                {new Date(log.timestamp).toLocaleTimeString()}
              </span>
              <span className="shrink-0 w-6 text-center font-bold">
                {log.log_level?.charAt(0).toUpperCase() || "-"}
              </span>
              <span className="shrink-0 w-24 truncate text-muted-foreground">
                {log.log_tag || log.log_source}
              </span>
              <span className="flex-1 break-all">{log.message}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function MobileErrorsSection() {
  const { selectedRunId } = useRunSelection();
  const { data: errors, isLoading, error } = useMobileErrors(selectedRunId, 100);
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set());

  const toggleExpanded = (id: number) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  if (!selectedRunId) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
        <AlertCircle className="w-8 h-8 mb-3 opacity-50" />
        <p className="text-sm">Select a task run to view mobile errors</p>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-destructive">
        <AlertCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">Error loading mobile errors: {String(error)}</p>
      </div>
    );
  }

  if (!errors || errors.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-8 text-green-600 dark:text-green-500">
        <CheckCircle className="w-8 h-8 mb-3" />
        <p className="text-sm">No errors detected in mobile logs</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold text-destructive">Mobile Errors ({errors.length})</h3>
      </div>

      <div className="space-y-2">
        {errors.map((err) => (
          <div
            key={err.id}
            className="border border-destructive/30 rounded-lg bg-destructive/5 overflow-hidden"
          >
            <button
              onClick={() => toggleExpanded(err.id)}
              className="w-full flex items-center gap-3 p-3 hover:bg-destructive/10 transition-colors text-left"
            >
              {expandedIds.has(err.id) ? (
                <ChevronDown className="w-4 h-4 shrink-0" />
              ) : (
                <ChevronRight className="w-4 h-4 shrink-0" />
              )}
              <AlertCircle className="w-4 h-4 shrink-0 text-destructive" />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium truncate text-destructive">
                  {err.error_type || err.log_tag || "Error"}
                </div>
                <div className="text-xs text-muted-foreground truncate">{err.message}</div>
              </div>
              <span className="text-xs text-muted-foreground shrink-0">
                {new Date(err.timestamp).toLocaleTimeString()}
              </span>
            </button>

            {expandedIds.has(err.id) && (
              <div className="px-3 pb-3 pt-1 border-t border-destructive/20 space-y-3">
                <div>
                  <div className="text-xs text-muted-foreground mb-1">Message</div>
                  <div className="text-sm">{err.message}</div>
                </div>

                {err.stack_trace && (
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Stack Trace</div>
                    <pre className="text-xs bg-muted p-2 rounded overflow-x-auto">
                      {err.stack_trace}
                    </pre>
                  </div>
                )}

                {err.file_path && (
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Location</div>
                    <div className="text-sm font-mono">
                      {err.file_path}
                      {err.line_number && `:${err.line_number}`}
                      {err.column_number && `:${err.column_number}`}
                    </div>
                  </div>
                )}

                {err.raw_line && (
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">Raw Log</div>
                    <pre className="text-xs bg-muted p-2 rounded overflow-x-auto font-mono">
                      {err.raw_line}
                    </pre>
                  </div>
                )}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
