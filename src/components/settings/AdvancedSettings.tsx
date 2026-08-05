import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Check,
  X,
  Wrench,
  Settings as SettingsIcon,
  Monitor,
  Copy,
  RefreshCw,
  FlaskConical,
  Gauge,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { instanceStorage } from "@/lib/instance-storage";
import { SectionHeader } from "./SectionHeader";
import { getStatusColors } from "@/design-system";
import { useFeatureDisclosure } from "@/contexts/FeatureDisclosureContext";
import type { DebugSettings, LogFunction } from "./types";
import {
  DEFAULT_PERF_CAPS,
  getPerfCapsLoadState,
  loadPerfCaps,
  normalizePerfCaps,
  setPerfCaps,
  usePerfCaps,
  type PerfCaps,
  type PerfCapsLoadState,
} from "@/lib/perfCaps";
import {
  PERF_CAP_FIELDS,
  clampPerfCaps,
  clampPerfField,
  isOutOfRange,
} from "./performanceCapsConfig";

interface DeviceInfo {
  device_id: string;
  device_name: string;
  platform: string;
}

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface AdvancedSettingsProps {
  onLog: LogFunction;
  onDebugModeChange: (enabled: boolean) => void;
}

export function AdvancedSettings({ onLog, onDebugModeChange }: AdvancedSettingsProps) {
  const { isEnabled } = useFeatureDisclosure();
  const showVisualAutomation = isEnabled("visual");
  const [settings, setSettings] = useState<DebugSettings>({
    enable_image_debug: false,
    top_matches_count: 5,
  });
  const [loading, setLoading] = useState(true);
  /**
   * Did we actually learn the backend's debug settings?
   *
   * `settings` initialises to `enable_image_debug: false`, so a FAILED load
   * leaves it indistinguishable from a genuine "off". That matters because the
   * image-match section below renders on `enable_image_debug` being true — a
   * failed load would hide the only switch that turns a live debug capture off.
   * Unknown is not OFF.
   */
  const [debugSettingsKnown, setDebugSettingsKnown] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Experimental: terminal backend
  const [terminalBackend, setTerminalBackend] = useState<"xterm" | "ghostty">(
    () => (instanceStorage.getItem("terminal-backend") as "xterm" | "ghostty" | null) || "xterm",
  );

  // Unattended session-restore policy for the Claude CLI's "Resume from
  // summary?" picker (#548 item 3). Default "full": context loss is silent
  // and unrecoverable; token cost is visible and /compact-able.
  const [resumePolicy, setResumePolicy] = useState<"full" | "summary">(() =>
    instanceStorage.getItem("resume-summary-policy") === "summary" ? "summary" : "full",
  );

  /*
   * Performance & Caps (many-sessions plan Phase 8).
   *
   * Collapsed by default and parked at the bottom of the Advanced page: the
   * discoverability gate wants these findable without adding a seventh
   * top-level settings entry, and an operator who never opens the section
   * sees one extra header line rather than eight extra inputs.
   *
   * `perfDraft` is a local edit buffer so typing never writes settings.json
   * per keystroke; `perfCaps` stays the committed truth, which is also what
   * "Discard changes" restores to.
   */
  const perfCaps = usePerfCaps();
  const [perfOpen, setPerfOpen] = useState(false);
  const [perfDraft, setPerfDraft] = useState<PerfCaps>(() => ({ ...DEFAULT_PERF_CAPS }));
  const [perfLoadState, setPerfLoadState] = useState<PerfCapsLoadState>("unloaded");
  const [perfSaving, setPerfSaving] = useState(false);
  const [perfSaved, setPerfSaved] = useState(false);
  const [perfError, setPerfError] = useState<string | null>(null);

  // Device info state
  const [deviceInfo, setDeviceInfo] = useState<DeviceInfo | null>(null);
  const [deviceInfoLoading, setDeviceInfoLoading] = useState(true);
  const [copiedField, setCopiedField] = useState<string | null>(null);

  const loadSettings = async () => {
    try {
      setLoading(true);
      setError(null);

      const result = await invoke<TauriResult<DebugSettings>>("get_debug_settings");

      if (result && result.success && result.data) {
        const loadedSettings: DebugSettings = {
          enable_image_debug: result.data.enable_image_debug || false,
          top_matches_count: result.data.top_matches_count || 5,
        };
        setSettings(loadedSettings);
        setDebugSettingsKnown(true);
        onLog("debug", "Debug settings loaded");
      } else {
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

  const loadDeviceInfo = async () => {
    try {
      setDeviceInfoLoading(true);
      const info: DeviceInfo = await invoke("get_device_info");
      setDeviceInfo(info);
      onLog("debug", "Device info loaded");
    } catch (err) {
      console.error("Failed to load device info:", err);
      onLog("warning", `Failed to load device info: ${err}`);
    } finally {
      setDeviceInfoLoading(false);
    }
  };

  const perfSavedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  /**
   * Read the caps and seed the edit buffer. Also the Retry affordance for a
   * failed read — without it a panel that lost the race with app startup
   * stays wrong until the whole settings page is remounted.
   */
  const loadPerfCapsIntoDraft = async () => {
    const loaded = await loadPerfCaps();
    // Seed the edit buffer from the committed values. Called on mount and on
    // an explicit Retry only — re-seeding on every store change would wipe an
    // in-progress edit the moment another surface saved.
    setPerfDraft({ ...loaded });
    setPerfLoadState(getPerfCapsLoadState());
  };

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (cancelled) return;
      void loadSettings();
      void loadDeviceInfo();
      void loadPerfCaps().then((loaded) => {
        if (cancelled) return;
        setPerfDraft({ ...loaded });
        setPerfLoadState(getPerfCapsLoadState());
      });
    });
    return () => {
      cancelled = true;
      if (perfSavedTimerRef.current !== null) clearTimeout(perfSavedTimerRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const setPerfField = (key: keyof PerfCaps, value: number | boolean | null) => {
    setPerfSaved(false);
    setPerfDraft((prev) => ({ ...prev, [key]: value }) as PerfCaps);
  };

  const perfDirty = (Object.keys(perfDraft) as Array<keyof PerfCaps>).some(
    (k) => perfDraft[k] !== perfCaps[k],
  );

  const savePerfCaps = async () => {
    try {
      setPerfSaving(true);
      setPerfError(null);
      setPerfSaved(false);
      // Clamp ONCE, here. Clamping per keystroke makes multi-digit entry
      // impossible on a field whose minimum has more digits than the first
      // character typed (scrollback's is 65536), so the inputs stay unclamped
      // while editing and the bounds are enforced at the save.
      const clamped = clampPerfCaps(perfDraft, PERF_CAP_FIELDS);
      const stored = await invoke<unknown>("save_performance_settings", {
        settings: clamped,
      });
      // Publish what the backend accepted rather than the draft.
      const normalized = normalizePerfCaps(stored);
      setPerfCaps(normalized);
      setPerfDraft({ ...normalized });
      setPerfLoadState(getPerfCapsLoadState());
      setPerfSaved(true);
      onLog("success", "Performance caps saved");
      if (perfSavedTimerRef.current !== null) clearTimeout(perfSavedTimerRef.current);
      perfSavedTimerRef.current = setTimeout(() => setPerfSaved(false), 3000);
    } catch (err) {
      console.error("Failed to save performance caps:", err);
      setPerfError(`Failed to save performance caps: ${err}`);
      onLog("error", `Failed to save performance caps: ${err}`);
    } finally {
      setPerfSaving(false);
    }
  };

  const copyToClipboard = async (text: string, fieldName: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedField(fieldName);
      onLog("info", `${fieldName} copied to clipboard`);
      setTimeout(() => setCopiedField(null), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
      onLog("error", "Failed to copy to clipboard");
    }
  };

  const saveSettings = async () => {
    try {
      setSaving(true);
      setError(null);
      setSaveSuccess(false);

      const result = await invoke<TauriResult<null>>("set_debug_settings", {
        enableImageDebug: settings.enable_image_debug,
        topMatchesCount: settings.top_matches_count,
      });

      if (result && result.success) {
        setSaveSuccess(true);
        onLog("success", "Debug settings saved successfully");
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

  const handleToggleDebug = () => {
    setSettings((prev) => ({
      ...prev,
      enable_image_debug: !prev.enable_image_debug,
    }));
  };

  const handleTopMatchesChange = (value: number) => {
    const clampedValue = Math.max(1, Math.min(10, value));
    setSettings((prev) => ({
      ...prev,
      top_matches_count: clampedValue,
    }));
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading settings"
          className="text-muted-foreground"
        >
          Loading settings...
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Advanced"
        description="Developer and debugging options: device information, performance caps, terminal backend, and large-session resume behavior."
        icon={<Wrench className="w-6 h-6" />}
      />

      {error && (
        <div className={`p-3 ${getStatusColors("error").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
          <span className={`${getStatusColors("error").text} text-xs`}>{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className={`p-3 ${getStatusColors("success").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getStatusColors("success").icon} shrink-0 mt-0.5`} />
          <span className={`${getStatusColors("success").text} text-xs`}>
            Settings saved successfully!
          </span>
        </div>
      )}

      {/*
        Image-match diagnostics are a VISUAL GUI AUTOMATION concern — top match
        candidates, confidence scores and the Image Recognition Debug tab all
        belong to the template-matching engine. The rest of this panel (device
        info, terminal backend, session resume) is core, so only this section
        follows the "visual" disclosure rather than the whole Debug page.

        `|| settings.enable_image_debug` is the same "visible UNION active" rule
        the nav surfaces use, applied to a SETTING rather than a page. Hiding a
        page is harmless because an unvisited page does nothing; hiding a
        control whose value is ON is not — image debug keeps capturing top-match
        candidates on every recognition attempt, and the only switch that turns
        it off would have just disappeared. So the section stays while the
        setting is engaged, whatever the disclosure says.

        `|| !debugSettingsKnown` matters for the same reason: `settings`
        initialises to `enable_image_debug: false` and is filled by an async
        Tauri call. A load that FAILS leaves that default in place, which is
        indistinguishable from a real "off" — so the rule would hide the
        off-switch precisely when we cannot prove the capture is stopped.
        Unknown is not OFF. (The `loading` early-return above means this is
        about a failed or empty load, not the in-flight window.)
      */}
      {(showVisualAutomation || !debugSettingsKnown || settings.enable_image_debug) && (
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Image Match Debug</h4>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Enable Image Match Debug Mode</div>
              <div className="text-xs text-muted-foreground">
                Collect and display detailed match information in the Images tab, including top
                match candidates, confidence scores, and failure diagnostics
              </div>
            </div>
            <button
              onClick={handleToggleDebug}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                settings.enable_image_debug ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  settings.enable_image_debug ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        <div className="space-y-2 p-3 rounded-lg bg-muted/30">
          <label className="block">
            <div className="text-sm font-medium mb-1">Top Matches to Display</div>
            <div className="text-xs text-muted-foreground mb-3">
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
                className="w-14 px-2 py-1.5 bg-muted/50 rounded-md text-center text-sm outline-hidden focus:ring-1 focus:ring-primary/50"
              />
            </div>
          </label>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Note:</strong> Debug settings take effect on the
            next image recognition attempt. These settings control the level of detail shown in the
            Image Recognition Debug tab.
          </div>
        </div>

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
      )}

      {/* Device Information Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <div className="flex items-center justify-between">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <Monitor className="w-4 h-4 text-primary" />
            Device Information
          </h4>
          <button
            onClick={loadDeviceInfo}
            disabled={deviceInfoLoading}
            className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded-md transition-colors disabled:opacity-50"
            title="Refresh device info"
          >
            <RefreshCw className={`w-3.5 h-3.5 ${deviceInfoLoading ? "animate-spin" : ""}`} />
          </button>
        </div>

        <p className="text-xs text-muted-foreground">
          System information useful for debugging and support. Click to copy values.
        </p>

        {deviceInfoLoading && !deviceInfo ? (
          <div className="flex items-center gap-2 text-muted-foreground">
            <RefreshCw className="w-4 h-4 animate-spin" />
            <span data-content-role="status" data-content-label="loading device info">
              Loading device info...
            </span>
          </div>
        ) : deviceInfo ? (
          <div className="space-y-2">
            {/* Device ID */}
            <div className="space-y-1">
              <label className="text-xs font-medium text-muted-foreground">Device ID</label>
              <button
                onClick={() => copyToClipboard(deviceInfo.device_id, "Device ID")}
                className="w-full flex items-center justify-between px-2.5 py-1.5 bg-muted/50 rounded-md hover:bg-muted transition-colors group"
              >
                <span className="font-mono text-xs truncate">{deviceInfo.device_id}</span>
                {copiedField === "Device ID" ? (
                  <Check
                    className={`w-3.5 h-3.5 ${getStatusColors("success").icon} shrink-0 ml-2`}
                  />
                ) : (
                  <Copy className="w-3.5 h-3.5 text-muted-foreground group-hover:text-primary shrink-0 ml-2" />
                )}
              </button>
            </div>

            {/* Device Name */}
            <div className="space-y-1">
              <label className="text-xs font-medium text-muted-foreground">Device Name</label>
              <button
                onClick={() => copyToClipboard(deviceInfo.device_name, "Device Name")}
                className="w-full flex items-center justify-between px-2.5 py-1.5 bg-muted/50 rounded-md hover:bg-muted transition-colors group"
              >
                <span className="text-xs truncate">{deviceInfo.device_name}</span>
                {copiedField === "Device Name" ? (
                  <Check
                    className={`w-3.5 h-3.5 ${getStatusColors("success").icon} shrink-0 ml-2`}
                  />
                ) : (
                  <Copy className="w-3.5 h-3.5 text-muted-foreground group-hover:text-primary shrink-0 ml-2" />
                )}
              </button>
            </div>

            {/* Platform */}
            <div className="space-y-1">
              <label className="text-xs font-medium text-muted-foreground">Platform</label>
              <button
                onClick={() => copyToClipboard(deviceInfo.platform, "Platform")}
                className="w-full flex items-center justify-between px-2.5 py-1.5 bg-muted/50 rounded-md hover:bg-muted transition-colors group"
              >
                <span className="text-xs capitalize">{deviceInfo.platform}</span>
                {copiedField === "Platform" ? (
                  <Check
                    className={`w-3.5 h-3.5 ${getStatusColors("success").icon} shrink-0 ml-2`}
                  />
                ) : (
                  <Copy className="w-3.5 h-3.5 text-muted-foreground group-hover:text-primary shrink-0 ml-2" />
                )}
              </button>
            </div>
          </div>
        ) : (
          <div
            data-content-role="status"
            data-content-label="device info unavailable"
            className="text-xs text-muted-foreground italic"
          >
            Unable to load device information.
          </div>
        )}

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> Include your Device ID when reporting
            issues to help identify your runner in the system.
          </div>
        </div>
      </div>

      {/* Session Restore Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <RefreshCw className="w-4 h-4 text-primary" />
          Session Restore
        </h4>

        <div className="space-y-2 p-3 rounded-lg bg-muted/30">
          <div className="text-sm font-medium">Large-session resume behavior</div>
          <div className="text-xs text-muted-foreground mb-2">
            When the runner auto-resumes a large Claude session after a restart, the CLI can offer
            to resume from a compacted summary. <strong>Full</strong> (default) resumes the
            complete conversation as-is — context loss is silent and unrecoverable, while token
            cost is visible and can be /compact-ed later. <strong>Summary</strong> opts in to the
            compacted resume for cost-sensitive setups.
          </div>
          <div className="flex gap-2">
            {(["full", "summary"] as const).map((policy) => (
              <button
                key={policy}
                onClick={() => {
                  setResumePolicy(policy);
                  instanceStorage.setItem("resume-summary-policy", policy);
                  onLog("info", `Unattended resume policy set to ${policy}.`);
                }}
                className={`px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
                  resumePolicy === policy
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                }`}
              >
                {policy === "full" ? "Resume full (default)" : "Resume from summary"}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Performance & Caps Section (many-sessions plan Phase 8) */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <button
          type="button"
          onClick={() => setPerfOpen((v) => !v)}
          aria-expanded={perfOpen}
          data-ui-bridge-id="settings.performance-caps-toggle"
          className="w-full flex items-center justify-between text-left"
        >
          <span className="font-medium text-sm flex items-center gap-2">
            <Gauge className="w-4 h-4 text-primary" />
            Performance &amp; Caps
          </span>
          {perfOpen ? (
            <ChevronDown className="w-4 h-4 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-4 h-4 text-muted-foreground" />
          )}
        </button>

        <p className="text-xs text-muted-foreground">
          Tuning knobs for running many sessions at once. Every default matches the behavior the
          runner had before these were configurable, so leaving this section alone changes nothing.
          {!perfOpen && " Expand to adjust."}
        </p>

        {perfOpen && (
          <div className="space-y-3" data-ui-bridge-id="settings.performance-caps">
            {perfError && (
              <div
                className={`p-3 ${getStatusColors("error").bg} rounded-lg flex items-start gap-2`}
              >
                <X className={`w-4 h-4 ${getStatusColors("error").icon} shrink-0 mt-0.5`} />
                <span className={`${getStatusColors("error").text} text-xs`}>{perfError}</span>
              </div>
            )}

            {/*
              Honesty: the panel must not present compiled-in defaults as "your
              current configuration". While the read is in flight it says so;
              if the read FAILED, it says that too, because settings.json may
              hold values that differ from what is on screen — and saving from
              here would then overwrite them.
            */}
            {perfLoadState === "unloaded" && (
              <div
                data-content-role="status"
                data-content-label="loading performance caps"
                className="text-xs text-muted-foreground italic"
              >
                Loading current values… (showing defaults meanwhile)
              </div>
            )}
            {perfLoadState === "failed" && (
              <div className="p-3 bg-yellow-500/10 rounded-lg space-y-2">
                <div className="text-xs text-yellow-200/80">
                  Could not read the current caps from the runner, so the values below are the
                  built-in defaults — they may not match what is in settings.json. Saving would
                  overwrite whatever is stored there.
                </div>
                <button
                  type="button"
                  onClick={() => void loadPerfCapsIntoDraft()}
                  className="px-3 py-1.5 rounded-md text-xs font-medium bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                >
                  Retry read
                </button>
              </div>
            )}

            {PERF_CAP_FIELDS.map((field) => (
              <div
                key={field.key}
                className="space-y-1 p-3 rounded-lg bg-muted/30"
                data-ui-bridge-id={`settings.perf-cap.${field.key}`}
              >
                <div className="text-sm font-medium">{field.label}</div>
                <div className="text-xs text-muted-foreground">{field.description}</div>

                {field.requiresRestart && (
                  <div className="text-[11px] text-yellow-300/90">
                    Takes effect after a runner restart.
                  </div>
                )}
                {field.pendingPhase && (
                  <div className="text-[11px] text-yellow-300/90">
                    Stored now, but not consumed yet — this value only does something once{" "}
                    {field.pendingPhase} lands.
                  </div>
                )}

                {field.type === "number" && (
                  <div className="flex items-center gap-2 pt-1 flex-wrap">
                    <input
                      type="number"
                      min={field.min}
                      max={field.max}
                      value={perfDraft[field.key] as number}
                      onChange={(e) => {
                        // Unclamped while typing (see savePerfCaps). An empty
                        // or partial entry leaves the draft alone rather than
                        // snapping it, so a field can be cleared and retyped.
                        const parsed = parseInt(e.target.value, 10);
                        if (Number.isNaN(parsed) || parsed < 0) return;
                        setPerfField(field.key, parsed);
                      }}
                      onBlur={() =>
                        setPerfField(field.key, clampPerfField(field, perfDraft[field.key]))
                      }
                      className="w-32 px-2 py-1.5 bg-muted/50 rounded-md text-sm outline-hidden focus:ring-1 focus:ring-primary/50"
                    />
                    {field.unit && (
                      <span className="text-xs text-muted-foreground">{field.unit}</span>
                    )}
                    {isOutOfRange(field, perfDraft[field.key]) && (
                      <span className="text-[11px] text-yellow-300/90">
                        outside {field.min}-{field.max}; will be adjusted on save
                      </span>
                    )}
                  </div>
                )}

                {field.type === "boolean" && (
                  <div className="pt-1">
                    <button
                      type="button"
                      role="switch"
                      aria-checked={perfDraft[field.key] === true}
                      onClick={() => setPerfField(field.key, !(perfDraft[field.key] as boolean))}
                      className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                        perfDraft[field.key] ? "bg-primary" : "bg-muted"
                      }`}
                    >
                      <span
                        className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                          perfDraft[field.key] ? "translate-x-4" : "translate-x-1"
                        }`}
                      />
                    </button>
                  </div>
                )}

                {field.type === "tristate" && (
                  <div className="flex gap-2 pt-1">
                    {(
                      [
                        [null, field.inheritLabel],
                        [true, "Always redact"],
                        [false, "Never redact"],
                      ] as const
                    ).map(([value, label]) => (
                      <button
                        key={String(value)}
                        type="button"
                        onClick={() => setPerfField(field.key, value)}
                        className={`px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
                          perfDraft[field.key] === value
                            ? "bg-primary text-primary-foreground"
                            : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                        }`}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            ))}

            <div className="flex items-center justify-between gap-2">
              {/*
                Reversibility: one click back to the shipped defaults, and one
                back to the last saved values. Neither writes anything until
                Save, so both are undoable in turn.
              */}
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => setPerfDraft({ ...DEFAULT_PERF_CAPS })}
                  className="px-3 py-1.5 rounded-md text-xs font-medium bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                >
                  Reset to defaults
                </button>
                <button
                  type="button"
                  disabled={!perfDirty}
                  onClick={() => setPerfDraft({ ...perfCaps })}
                  className="px-3 py-1.5 rounded-md text-xs font-medium bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  Discard changes
                </button>
              </div>
              <button
                type="button"
                onClick={savePerfCaps}
                disabled={perfSaving || !perfDirty}
                data-ui-bridge-id="settings.performance-caps-save"
                className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
              >
                {perfSaving ? (
                  <>
                    <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
                    Saving...
                  </>
                ) : perfSaved ? (
                  <>
                    <Check className="w-4 h-4" />
                    Saved!
                  </>
                ) : (
                  <>
                    <SettingsIcon className="w-4 h-4" />
                    Save Caps
                  </>
                )}
              </button>
            </div>
          </div>
        )}
      </div>

      {/* Experimental Section */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <FlaskConical className="w-4 h-4 text-yellow-500" />
          Experimental
        </h4>

        <p className="text-xs text-muted-foreground">
          These features are experimental and may be unstable. Changes take effect for new terminals
          only.
        </p>

        <div className="space-y-2 p-3 rounded-lg bg-muted/30">
          <div className="text-sm font-medium">Terminal Backend</div>
          <div className="text-xs text-muted-foreground mb-2">
            Choose the terminal rendering engine. <strong>ghostty-web</strong> uses Ghostty&apos;s
            WASM-compiled VT parser with better Unicode handling but Canvas 2D rendering (no WebGL).
          </div>
          <div className="flex gap-2">
            {(["xterm", "ghostty"] as const).map((backend) => (
              <button
                key={backend}
                onClick={() => {
                  setTerminalBackend(backend);
                  instanceStorage.setItem("terminal-backend", backend);
                  onLog(
                    "info",
                    `Terminal backend set to ${backend}. Open new terminals to see the change.`,
                  );
                }}
                className={`px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
                  terminalBackend === backend
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
                }`}
              >
                {backend === "xterm" ? "xterm.js (default)" : "ghostty-web (experimental)"}
              </button>
            ))}
          </div>
        </div>

        {terminalBackend === "ghostty" && (
          <div className="p-3 bg-yellow-500/10 rounded-lg">
            <div className="text-xs text-yellow-200/80">
              <strong className="text-yellow-300">Warning:</strong> ghostty-web uses Canvas 2D
              rendering (no WebGL acceleration) and may have different behavior for some escape
              sequences. If you experience issues, switch back to xterm.js.
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
