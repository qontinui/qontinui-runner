/**
 * CiRunnerSettings.tsx
 *
 * Settings panel for enabling/disabling a GitHub Actions self-hosted CI runner
 * on this machine. Communicates with the supervisor process at localhost:9875
 * which manages the runner lifecycle (install, start, stop, uninstall).
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Play, Square, Info, X, Check, CircleDot } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import type { LogFunction } from "./types";

// --- Types ---

interface CiRunnerStatus {
  installed: boolean;
  status: {
    status: "idle" | "busy" | "offline";
    labels: string[];
    service_names: string[];
  };
}

interface CiRunnerSettingsProps {
  onLog: LogFunction;
}

const SUPERVISOR_BASE = "http://localhost:9875";
const POLL_INTERVAL_MS = 10_000;

// Shown when the runner is unpaired (no coord device-JWT). Enabling/disabling
// the CI runner mints a GitHub Actions registration token via coord, which now
// requires a FleetPrincipal — so we MUST present the device credential and
// never call the supervisor anonymously.
const UNPAIRED_ERROR =
  "Pair this runner before enabling CI — no device credential. " +
  "Sign in / pair under Settings → Account, then try again.";

// Sentinel for the unpaired case so the catch-blocks can render the actionable
// message verbatim (without the "Failed to …" prefix the generic path adds).
class UnpairedError extends Error {}

/**
 * Resolve the runner's coord device-JWT via the Tauri command, or throw an
 * {@link UnpairedError} when the device is unpaired (command returns null).
 * Returns the `Authorization: Bearer <jwt>` value to attach to supervisor calls
 * that perform the FleetPrincipal-gated registration-token mint.
 */
async function deviceBearerHeader(): Promise<string> {
  const token = await invoke<string | null>("get_coord_device_token");
  if (!token) {
    throw new UnpairedError(UNPAIRED_ERROR);
  }
  return `Bearer ${token}`;
}

// Toggle Switch (matches ContainerSettings / SelfHealingSettings pattern)
function ToggleSwitch({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={() => {
        if (!disabled) onChange(!checked);
      }}
      disabled={disabled}
      className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
        checked ? "bg-primary" : "bg-muted"
      } ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
    >
      <span
        className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
          checked ? "translate-x-4" : "translate-x-1"
        }`}
      />
    </button>
  );
}

function StatusBadge({ status }: { status: "idle" | "busy" | "offline" }) {
  const colors: Record<string, string> = {
    idle: "text-green-400",
    busy: "text-yellow-400",
    offline: "text-zinc-400",
  };
  const labels: Record<string, string> = {
    idle: "Idle",
    busy: "Busy",
    offline: "Offline",
  };
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${colors[status]}`}>
      <CircleDot className="w-3 h-3" />
      {labels[status]}
    </span>
  );
}

export function CiRunnerSettings({ onLog }: CiRunnerSettingsProps) {
  const [status, setStatus] = useState<CiRunnerStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [actionLoading, setActionLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionSuccess, setActionSuccess] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchStatus = useCallback(async () => {
    try {
      const resp = await fetch(`${SUPERVISOR_BASE}/ci-runner/status`);
      if (!resp.ok) {
        throw new Error(`HTTP ${resp.status}`);
      }
      const data: CiRunnerStatus = await resp.json();
      setStatus(data);
      setError(null);
    } catch (err) {
      setError(`Failed to reach supervisor: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  // Poll status on mount and every POLL_INTERVAL_MS
  useEffect(() => {
    void fetchStatus();
    timerRef.current = setInterval(() => {
      void fetchStatus();
    }, POLL_INTERVAL_MS);
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [fetchStatus]);

  const handleEnable = useCallback(async () => {
    setActionLoading(true);
    setError(null);
    setActionSuccess(null);
    try {
      const authHeader = await deviceBearerHeader();
      const resp = await fetch(`${SUPERVISOR_BASE}/ci-runner/enable`, {
        method: "POST",
        headers: { "Content-Type": "application/json", Authorization: authHeader },
        body: JSON.stringify({
          labels: ["self-hosted", "qontinui"],
        }),
      });
      if (!resp.ok) {
        const body = await resp.text();
        throw new Error(body || `HTTP ${resp.status}`);
      }
      setActionSuccess("CI runner enabled successfully");
      onLog("success", "CI runner enabled");
      // Refresh status immediately
      await fetchStatus();
      setTimeout(() => setActionSuccess(null), 3000);
    } catch (err) {
      if (err instanceof UnpairedError) {
        setError(err.message);
        onLog("error", `Cannot enable CI runner: ${err.message}`);
        return;
      }
      const msg = err instanceof Error ? err.message : String(err);
      setError(`Failed to enable CI runner: ${msg}`);
      onLog("error", `Failed to enable CI runner: ${msg}`);
    } finally {
      setActionLoading(false);
    }
  }, [fetchStatus, onLog]);

  const handleDisable = useCallback(async () => {
    setActionLoading(true);
    setError(null);
    setActionSuccess(null);
    try {
      const authHeader = await deviceBearerHeader();
      const resp = await fetch(`${SUPERVISOR_BASE}/ci-runner/disable`, {
        method: "POST",
        headers: { Authorization: authHeader },
      });
      if (!resp.ok) {
        const body = await resp.text();
        throw new Error(body || `HTTP ${resp.status}`);
      }
      setActionSuccess("CI runner disabled successfully");
      onLog("success", "CI runner disabled");
      await fetchStatus();
      setTimeout(() => setActionSuccess(null), 3000);
    } catch (err) {
      if (err instanceof UnpairedError) {
        setError(err.message);
        onLog("error", `Cannot disable CI runner: ${err.message}`);
        return;
      }
      const msg = err instanceof Error ? err.message : String(err);
      setError(`Failed to disable CI runner: ${msg}`);
      onLog("error", `Failed to disable CI runner: ${msg}`);
    } finally {
      setActionLoading(false);
    }
  }, [fetchStatus, onLog]);

  const handleStart = useCallback(async () => {
    setActionLoading(true);
    setError(null);
    setActionSuccess(null);
    try {
      const resp = await fetch(`${SUPERVISOR_BASE}/ci-runner/start`, {
        method: "POST",
      });
      if (!resp.ok) {
        const body = await resp.text();
        throw new Error(body || `HTTP ${resp.status}`);
      }
      setActionSuccess("CI runner started");
      onLog("success", "CI runner started");
      await fetchStatus();
      setTimeout(() => setActionSuccess(null), 3000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(`Failed to start CI runner: ${msg}`);
      onLog("error", `Failed to start CI runner: ${msg}`);
    } finally {
      setActionLoading(false);
    }
  }, [fetchStatus, onLog]);

  const handleStop = useCallback(async () => {
    setActionLoading(true);
    setError(null);
    setActionSuccess(null);
    try {
      const resp = await fetch(`${SUPERVISOR_BASE}/ci-runner/stop`, {
        method: "POST",
      });
      if (!resp.ok) {
        const body = await resp.text();
        throw new Error(body || `HTTP ${resp.status}`);
      }
      setActionSuccess("CI runner stopped");
      onLog("success", "CI runner stopped");
      await fetchStatus();
      setTimeout(() => setActionSuccess(null), 3000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(`Failed to stop CI runner: ${msg}`);
      onLog("error", `Failed to stop CI runner: ${msg}`);
    } finally {
      setActionLoading(false);
    }
  }, [fetchStatus, onLog]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-muted-foreground">Loading CI runner status...</div>
      </div>
    );
  }

  const installed = status?.installed ?? false;
  const runnerStatus = status?.status.status ?? "offline";
  const labels = status?.status.labels ?? [];

  return (
    <div className="space-y-6">
      <SectionHeader
        title="CI Runner"
        description="Enable a GitHub Actions self-hosted runner on this machine. The supervisor manages the runner lifecycle — registration, service install, start/stop, and removal."
        icon={<CircleDot className="w-6 h-6" />}
      />

      {error && (
        <div className={`p-3 ${getAccentColors("red").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("red").text} text-xs`}>{error}</span>
        </div>
      )}

      {actionSuccess && (
        <div className={`p-3 ${getAccentColors("green").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("green").text} text-xs`}>{actionSuccess}</span>
        </div>
      )}

      {/* Enable/Disable Toggle */}
      <div className="rounded-lg bg-card/50 overflow-hidden">
        <div className="p-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <CircleDot className="w-5 h-5 text-primary" />
            <div>
              <h4 className="font-medium text-sm">Enable CI Runner</h4>
              <p className="text-xs text-muted-foreground">
                Register and install a GitHub Actions self-hosted runner
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            {installed && <StatusBadge status={runnerStatus} />}
            <ToggleSwitch
              checked={installed}
              onChange={(checked) => {
                if (checked) {
                  void handleEnable();
                } else {
                  void handleDisable();
                }
              }}
              disabled={actionLoading}
            />
          </div>
        </div>
      </div>

      {/* Runner details — only when installed */}
      {installed && (
        <div className="rounded-lg bg-card/50 overflow-hidden">
          <div className="p-4 space-y-4">
            {/* Labels */}
            {labels.length > 0 && (
              <div className="space-y-1.5">
                <label className="text-xs font-medium">Labels</label>
                <div className="flex flex-wrap gap-1.5">
                  {labels.map((label) => (
                    <span
                      key={label}
                      className="inline-flex items-center px-2 py-0.5 rounded text-[0.7rem] font-medium bg-muted text-muted-foreground"
                    >
                      {label}
                    </span>
                  ))}
                </div>
              </div>
            )}

            {/* Start/Stop Controls */}
            <div className="space-y-3 pt-2 border-t border-border/50">
              <div className="flex items-center justify-between">
                <div>
                  <h4 className="text-xs font-medium">Service Control</h4>
                  <p className="text-[10px] text-muted-foreground">
                    Start or stop the runner service without removing it
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleStart}
                    disabled={actionLoading || runnerStatus !== "offline"}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-green-600 hover:bg-green-700 text-white rounded-md text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {actionLoading ? (
                      <div className="w-3.5 h-3.5 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                    ) : (
                      <Play className="w-3.5 h-3.5" />
                    )}
                    Start
                  </button>
                  <button
                    onClick={handleStop}
                    disabled={actionLoading || runnerStatus === "offline"}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-red-600 hover:bg-red-700 text-white rounded-md text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {actionLoading ? (
                      <div className="w-3.5 h-3.5 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                    ) : (
                      <Square className="w-3.5 h-3.5" />
                    )}
                    Stop
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Info banner */}
      <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
        <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
        <p className={`text-xs ${getAccentColors("blue").text}`}>
          The CI runner registers this machine as a GitHub Actions self-hosted runner. Once enabled,
          workflows targeting the <code className="font-mono">self-hosted</code> and{" "}
          <code className="font-mono">qontinui</code> labels will be picked up by this machine. The
          supervisor polls the coordinator for a registration token during setup.
        </p>
      </div>
    </div>
  );
}
