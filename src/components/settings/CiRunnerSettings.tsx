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
import { getAccentColors, type AccentColor } from "@/design-system";
import type { LogFunction } from "./types";

// --- Types ---

/**
 * The wire values of `runner_status` in the supervisor's
 * `GET /ci-runner/status` response.
 *
 * `distro_down` and `probe_failed` arrive from the supervisor half of plan
 * `2026-08-21-supervisor-watchdog-observer-effect`. The probe used to collapse
 * "WSL was unreachable" into "the service is inactive" (`.unwrap_or(false)`),
 * so an operator reading "Offline" could not tell whether the runner had
 * crashed or the probe had simply been unable to ask:
 *
 * - `distro_down`  — the WSL distro is not running, so no runner can be online.
 *   The probe deliberately issues no `wsl -e` command in this state; waking the
 *   distro to measure it is the observer effect the plan exists to remove.
 * - `probe_failed` — the probe itself failed. Explicitly NOT "offline": it is a
 *   cause, not a verdict about the runner.
 *
 * Both are REPORTED states — the supervisor was reached and told us this. That
 * is a different fact from the transport-level `"unknown"` below, and the two
 * are deliberately kept distinct (see {@link CiRunnerDisplayState}).
 */
type CiRunnerState = "idle" | "busy" | "offline" | "distro_down" | "probe_failed";

/**
 * NO-DOWNGRADE (L3): what we DISPLAY. `status?.runner_status ?? "offline"`
 * reported a supervisor outage as "Offline" — a definitive verdict about the
 * CI runner derived from a failure to reach the thing that would know. A
 * supervisor we cannot reach means UNKNOWN, and both actions are disabled in
 * that state rather than acting on a guess.
 *
 * `"unknown"` is a statement about OUR hop: we could not reach the supervisor.
 * It must never absorb the supervisor's own `probe_failed`, which says the
 * supervisor WAS reached and reported that it could not ask WSL. Collapsing
 * them would recreate, one layer up, exactly the conflation the server-side
 * change removes — so they carry separate labels, colours and copy.
 */
type CiRunnerDisplayState = CiRunnerState | "unknown";

/**
 * Shape actually returned by the supervisor's `GET /ci-runner/status` — FLAT:
 * `{ runner_status, labels, service_names, installed }`.
 *
 * This component previously declared a NESTED shape (`{ installed, status: {
 * status, labels, service_names } }`) that the supervisor has never sent, and
 * then read `status?.status.status` — optional only on the outer binding. With
 * `status.status` always `undefined`, that expression threw
 * `TypeError: Cannot read properties of undefined (reading 'status')` on the
 * FIRST render after the fetch resolved, escaping to the app-level
 * ErrorBoundary and taking the whole window down.
 *
 * It went unnoticed because the CI Runner sub-nav item was unreachable in
 * practice: clicking it dispatched `settings-ci-runner`, which `TabContent` had
 * no case for, so the fallback replaced the Settings tree before this panel
 * could settle. Fixing the routing (iter-2 R1) made the page reachable — and
 * immediately surfaced this crash on the UI Bridge. Fields are optional because
 * the supervisor may be down or older than this build.
 */
interface CiRunnerStatus {
  installed?: boolean;
  runner_status?: CiRunnerState;
  labels?: string[];
  service_names?: string[];
}

interface CiRunnerSettingsProps {
  onLog: LogFunction;
}

const SUPERVISOR_BASE = "http://localhost:9875";
const POLL_INTERVAL_MS = 10_000;
// Upper bound on a single status fetch. The supervisor now serves
// `/ci-runner/status` from its cached probe state in <100 ms, but if it is
// briefly slow (e.g. a cold WSL probe on the first tick after boot) we abort
// and surface a soft "Checking supervisor…" state rather than letting the
// fetch hang or the browser abort it into a hard "Failed to fetch" banner.
const STATUS_FETCH_TIMEOUT_MS = 8_000;

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

/**
 * The states in which the runner's service state is a FACT we can act on.
 * Everything else — the transport-level `"unknown"`, and the supervisor's own
 * `distro_down` / `probe_failed` — is either an absence of evidence or a
 * condition no button on this panel can clear, so the controls are disabled
 * rather than firing at a guess.
 */
function isServiceStateKnown(status: CiRunnerDisplayState): boolean {
  return status === "idle" || status === "busy" || status === "offline";
}

/**
 * Operator-facing explanation for the two REPORTED degraded states. Neither is
 * actionable by retry from this panel: nothing the Enable/Start/Stop controls
 * do reaches a distro that is not running, and a failed probe is an absence of
 * evidence, not a verdict to act on.
 */
const DEGRADED_STATE_NOTES: Record<
  "distro_down" | "probe_failed",
  { accent: AccentColor; text: string }
> = {
  distro_down: {
    accent: "orange",
    text:
      "The WSL distro hosting the CI runner is not running, so no runner can be online. " +
      "The supervisor will not start it — a monitor that wakes its subject cannot observe it. " +
      "Bring the distro up on the host (or check the WSL keepalive); runner controls stay " +
      "disabled until it is.",
  },
  probe_failed: {
    accent: "red",
    text:
      "The supervisor was reached, but its CI-runner probe failed — the runner's state is " +
      "unknown, which is NOT the same as Offline. Runner controls are disabled rather than " +
      "acting on a guess; the supervisor log carries the probe error.",
  },
};

function StatusBadge({ status }: { status: CiRunnerDisplayState }) {
  const colors: Record<CiRunnerDisplayState, string> = {
    idle: "text-green-400",
    busy: "text-yellow-400",
    offline: "text-zinc-400",
    // Reported degraded states: a host-level condition (orange) and a failed
    // measurement (red). Kept off amber, which is the supervisor-unreachable
    // "unknown" colour — different hop, different signal.
    distro_down: "text-orange-400",
    probe_failed: "text-red-400",
    unknown: "text-amber-400",
  };
  const labels: Record<CiRunnerDisplayState, string> = {
    idle: "Idle",
    busy: "Busy",
    offline: "Offline",
    distro_down: "Distro Down",
    probe_failed: "Probe Failed",
    unknown: "Unknown",
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
  const [checking, setChecking] = useState(false);
  const [actionSuccess, setActionSuccess] = useState<string | null>(null);
  // NO-DOWNGRADE: true whenever the most recent poll failed. The previously
  // fetched `status` is then STALE and must not be rendered as fact.
  const [statusUnknown, setStatusUnknown] = useState(true);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetchStatus = useCallback(async () => {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), STATUS_FETCH_TIMEOUT_MS);
    try {
      const resp = await fetch(`${SUPERVISOR_BASE}/ci-runner/status`, {
        signal: controller.signal,
      });
      if (!resp.ok) {
        throw new Error(`HTTP ${resp.status}`);
      }
      const data: CiRunnerStatus = await resp.json();
      setStatus(data);
      setStatusUnknown(false);
      setError(null);
      setChecking(false);
    } catch (err) {
      // Either failure mode leaves the previously-fetched `status` STALE, so
      // mark it unknown (NO-DOWNGRADE) — it must not be rendered as current
      // fact. A timeout additionally means the supervisor is reachable but
      // slow, not down: surface a soft "checking…" state and keep the last
      // status for display rather than flashing a hard "Failed to reach
      // supervisor" banner that the next poll would immediately clear.
      setStatusUnknown(true);
      if (err instanceof DOMException && err.name === "AbortError") {
        setChecking(true);
      } else {
        setError(`Failed to reach supervisor: ${err instanceof Error ? err.message : String(err)}`);
        setChecking(false);
      }
    } finally {
      clearTimeout(timeoutId);
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

  // Every read is defaulted so a supervisor that is down, older, or returns a
  // partial body never throws during render. But "defaulted" must not mean
  // "asserted": when the supervisor is unreachable the state is UNKNOWN, not
  // offline, and the actions are disabled rather than acting on a guess.
  const installed = status?.installed ?? false;
  const runnerStatus: CiRunnerDisplayState = statusUnknown
    ? "unknown"
    : (status?.runner_status ?? "offline");
  const labels = status?.labels ?? [];
  // The supervisor reported a degraded state (as opposed to us failing to
  // reach it). Rendered with its own note so the operator sees the CAUSE, and
  // gates the controls the same way `statusUnknown` does.
  const degradedNote =
    runnerStatus === "distro_down" || runnerStatus === "probe_failed"
      ? DEGRADED_STATE_NOTES[runnerStatus]
      : null;
  // Enable/disable, start and stop all mutate the runner through WSL. Allow
  // them only when the runner's service state is an observed fact.
  const controlsEnabled = !actionLoading && isServiceStateKnown(runnerStatus);

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

      {checking && !error && (
        <div className={`p-3 ${getAccentColors("amber").bg} rounded-lg flex items-start gap-2`}>
          <CircleDot className={`w-4 h-4 ${getAccentColors("amber").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("amber").text} text-xs`}>Checking supervisor…</span>
        </div>
      )}

      {degradedNote && (
        <div
          className={`p-3 ${getAccentColors(degradedNote.accent).bg} rounded-lg flex items-start gap-2`}
        >
          <Info
            className={`w-4 h-4 ${getAccentColors(degradedNote.accent).text} shrink-0 mt-0.5`}
          />
          <span className={`${getAccentColors(degradedNote.accent).text} text-xs`}>
            {degradedNote.text}
          </span>
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
            {(installed || statusUnknown || degradedNote !== null) && (
              <StatusBadge status={runnerStatus} />
            )}
            <ToggleSwitch
              checked={installed}
              onChange={(checked) => {
                if (checked) {
                  void handleEnable();
                } else {
                  void handleDisable();
                }
              }}
              disabled={!controlsEnabled}
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
                    disabled={!controlsEnabled || runnerStatus !== "offline"}
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
                    disabled={!controlsEnabled || runnerStatus === "offline"}
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
