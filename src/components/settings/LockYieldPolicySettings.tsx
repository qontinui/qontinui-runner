/**
 * LockYieldPolicySettings — Auto-yield-on-idle policy controls.
 *
 * Wraps the runner's `lock_yield_policy` settings group (see
 * `settings::LockYieldPolicySettings` in src-tauri). When enabled, a
 * background task in the runner releases held file locks once the
 * holder has been stdout-idle for `idleThresholdSecs` AND the oldest
 * waiter has been blocked for at least `minWaitSecs`. Default off so
 * existing deployments stay opt-in.
 *
 * Implements §Open Q4 of the lock-yield-protocol-plan.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, X, Hourglass, Info } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import {
  IDLE_THRESHOLD_MAX,
  IDLE_THRESHOLD_MIN,
  MIN_WAIT_MAX,
  MIN_WAIT_MIN,
  clampNumber,
} from "./lockYieldPolicyHelpers";
import type { LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

export interface LockYieldPolicySettingsValue {
  enabled: boolean;
  idle_threshold_secs: number;
  min_wait_secs: number;
}

interface LockYieldPolicySettingsProps {
  onLog: LogFunction;
}

const DEFAULT_SETTINGS: LockYieldPolicySettingsValue = {
  enabled: false,
  idle_threshold_secs: 60,
  min_wait_secs: 30,
};

// Re-export pure helpers so callers can `import { clampNumber } from
// "./LockYieldPolicySettings"` without changing import surface. Tests
// pull directly from `./lockYieldPolicyHelpers` to keep the design-
// system module graph out of vitest's node-environment.
export {
  IDLE_THRESHOLD_MIN,
  IDLE_THRESHOLD_MAX,
  MIN_WAIT_MIN,
  MIN_WAIT_MAX,
  clampNumber,
};

export function LockYieldPolicySettings({ onLog }: LockYieldPolicySettingsProps) {
  const [settings, setSettings] = useState<LockYieldPolicySettingsValue>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const result = await invoke<TauriResult<LockYieldPolicySettingsValue>>(
          "get_lock_yield_policy_settings",
        );
        if (cancelled) return;
        if (result?.success && result.data) {
          setSettings({
            ...DEFAULT_SETTINGS,
            ...result.data,
          });
          onLog("debug", "Lock-yield policy settings loaded");
        } else {
          onLog("warning", "Using default lock-yield policy settings");
        }
      } catch (err) {
        if (cancelled) return;
        console.error("Failed to load lock-yield policy settings:", err);
        setError(`Failed to load settings: ${String(err)}`);
        onLog("error", `Failed to load lock-yield policy settings: ${String(err)}`);
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
    // onLog is stable in practice (parent useCallback), but we deliberately
    // skip it as a dep to avoid reload thrash if a parent re-creates it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const saveSettings = async () => {
    setSaving(true);
    setError(null);
    setSaveSuccess(false);
    try {
      const result = await invoke<TauriResult<null>>("save_lock_yield_policy_settings", {
        enabled: settings.enabled,
        idleThresholdSecs: settings.idle_threshold_secs,
        minWaitSecs: settings.min_wait_secs,
      });
      if (result?.success) {
        setSaveSuccess(true);
        onLog("success", "Lock-yield policy settings saved");
        setTimeout(() => setSaveSuccess(false), 3000);
      } else {
        const msg = result?.message || "Unknown error";
        setError(msg);
        onLog("error", `Failed to save lock-yield policy settings: ${msg}`);
      }
    } catch (err) {
      console.error("Failed to save lock-yield policy settings:", err);
      setError(`Failed to save settings: ${String(err)}`);
      onLog("error", `Failed to save lock-yield policy settings: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading lock-yield policy settings"
          className="text-muted-foreground"
        >
          Loading lock-yield policy settings...
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Lock-yield policy"
        description="Automatically yield a held file lock when the holder has been stdout-idle and another session has been waiting. Disabled by default; opt in for the productivity-stack §Open Q4 behaviour."
        icon={<Hourglass className="w-6 h-6" />}
      />

      {error && (
        <div className={`p-3 ${getAccentColors("red").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("red").text} text-xs`}>{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className={`p-3 ${getAccentColors("green").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("green").text} text-xs`}>
            Lock-yield policy saved.
          </span>
        </div>
      )}

      <div className="rounded-lg bg-card/50 p-4 space-y-4">
        {/* Enable toggle */}
        <div className="flex items-center justify-between">
          <div>
            <h4 className="font-medium text-sm">Auto-yield idle lock holders</h4>
            <p className="text-xs text-muted-foreground">
              Background task polls every 5 seconds.
            </p>
          </div>
          <ToggleSwitch
            data-ui-bridge-id="settings.lock-yield-auto-yield-enabled"
            checked={settings.enabled}
            onChange={(enabled) => setSettings((s) => ({ ...s, enabled }))}
          />
        </div>

        {/* Idle threshold */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="lock-yield-idle-threshold">
            Idle threshold (seconds)
          </label>
          <input
            id="lock-yield-idle-threshold"
            data-ui-bridge-id="settings.lock-yield-idle-threshold"
            type="number"
            min={IDLE_THRESHOLD_MIN}
            max={IDLE_THRESHOLD_MAX}
            step={5}
            value={settings.idle_threshold_secs}
            onChange={(e) =>
              setSettings((s) => ({
                ...s,
                idle_threshold_secs: clampNumber(
                  parseInt(e.target.value, 10),
                  IDLE_THRESHOLD_MIN,
                  IDLE_THRESHOLD_MAX,
                  DEFAULT_SETTINGS.idle_threshold_secs,
                ),
              }))
            }
            disabled={!settings.enabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            Time since the holder&apos;s last terminal output before they&apos;re eligible to
            auto-yield. Range {IDLE_THRESHOLD_MIN}-{IDLE_THRESHOLD_MAX}s.
          </p>
        </div>

        {/* Min wait */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="lock-yield-min-wait">
            Min waiter wait (seconds)
          </label>
          <input
            id="lock-yield-min-wait"
            data-ui-bridge-id="settings.lock-yield-min-wait"
            type="number"
            min={MIN_WAIT_MIN}
            max={MIN_WAIT_MAX}
            step={5}
            value={settings.min_wait_secs}
            onChange={(e) =>
              setSettings((s) => ({
                ...s,
                min_wait_secs: clampNumber(
                  parseInt(e.target.value, 10),
                  MIN_WAIT_MIN,
                  MIN_WAIT_MAX,
                  DEFAULT_SETTINGS.min_wait_secs,
                ),
              }))
            }
            disabled={!settings.enabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            Minimum time a waiter must be blocked before auto-yield fires. Prevents auto-yield on
            transient contention. Range {MIN_WAIT_MIN}-{MIN_WAIT_MAX}s.
          </p>
        </div>

        <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
          <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
          <p className={`text-xs ${getAccentColors("blue").text}`}>
            The runner releases a held lock when{" "}
            <strong>holder idle ≥ idle threshold</strong> AND{" "}
            <strong>oldest waiter waited ≥ min waiter wait</strong>. A
            {" "}<code>file-lock-auto-yielded</code> event is emitted alongside the standard
            {" "}<code>file-lock-released</code> event so the waiter unblocks normally.
          </p>
        </div>
      </div>

      <div className="flex justify-end">
        <button
          type="button"
          onClick={saveSettings}
          disabled={saving}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
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
              <Hourglass className="w-4 h-4" />
              Save Lock-Yield Policy
            </>
          )}
        </button>
      </div>
    </div>
  );
}

// ── Toggle (mirrors SelfHealingSettings.tsx pattern) ────────────────────────
interface ToggleSwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  "data-ui-bridge-id"?: string;
}

function ToggleSwitch({
  checked,
  onChange,
  disabled,
  "data-ui-bridge-id": dataUiBridgeId,
}: ToggleSwitchProps) {
  return (
    <button
      type="button"
      data-ui-bridge-id={dataUiBridgeId}
      onClick={() => {
        if (!disabled) onChange(!checked);
      }}
      disabled={disabled}
      aria-pressed={checked}
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
