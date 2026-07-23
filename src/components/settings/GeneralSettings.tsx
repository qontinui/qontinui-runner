import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Settings as SettingsIcon, FileText, Layers } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import {
  DISCLOSURE_LABELS,
  useFeatureDisclosure,
  type FeatureDisclosure,
} from "@/contexts/FeatureDisclosureContext";
import type { AppSettings, LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

interface AutoLoadConfigData {
  enabled: boolean;
}

interface IncludeSummaryStepData {
  enabled: boolean;
}

interface AutostartStatusData {
  enabled: boolean;
}

interface GeneralSettingsProps {
  onLog: LogFunction;
}

export function GeneralSettings({ onLog }: GeneralSettingsProps) {
  const [appSettings, setAppSettings] = useState<AppSettings>({
    auto_load_last_config: false,
  });
  const [includeSummaryStep, setIncludeSummaryStep] = useState<boolean>(true);
  // Phase F.1 — "Launch on system startup" toggle. Backed by
  // `tauri-plugin-autostart`'s registry/LaunchAgent/.desktop write through the
  // `get_autostart_enabled` / `set_autostart_enabled` Tauri commands. Durable
  // solution for overnight scheduled tasks: the OS auto-starts the runner at
  // login, so scheduled tasks fire even if the user closed the runner.
  const [autostartEnabled, setAutostartEnabled] = useState<boolean>(false);

  // The two optional feature sets. Persisted in localStorage by
  // FeatureDisclosureContext and consumed reactively by the Sidebar (which
  // calls setShowHiddenItems() for "advanced" and gates the AI Dev / Visual
  // switcher on "visual") and by the Settings sub-nav.
  const { isEnabled, setEnabled } = useFeatureDisclosure();
  const showAdvancedAutomation = isEnabled("advanced");
  const showVisualAutomation = isEnabled("visual");

  const loadAppSettings = async () => {
    try {
      const result = await invoke<TauriResult<AutoLoadConfigData>>("get_auto_load_last_config");
      if (result && result.success && result.data) {
        setAppSettings({
          auto_load_last_config: result.data.enabled || false,
        });
      }
    } catch (err) {
      console.error("Failed to load app settings:", err);
    }
  };

  const loadIncludeSummaryStep = async () => {
    try {
      const result = await invoke<TauriResult<IncludeSummaryStepData>>(
        "get_include_summary_step_by_default",
      );
      if (result && result.success && result.data) {
        setIncludeSummaryStep(result.data.enabled);
      }
    } catch (err) {
      console.error("Failed to load include summary step setting:", err);
    }
  };

  const loadAutostartEnabled = async () => {
    try {
      const result = await invoke<TauriResult<AutostartStatusData>>("get_autostart_enabled");
      if (result && result.success && result.data) {
        setAutostartEnabled(result.data.enabled);
      }
    } catch (err) {
      console.error("Failed to load autostart setting:", err);
    }
  };

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async init from Tauri backend
    loadAppSettings();
    loadIncludeSummaryStep();
    loadAutostartEnabled();
  }, []);

  const handleToggleAutoLoad = async () => {
    const newValue = !appSettings.auto_load_last_config;
    setAppSettings((prev) => ({
      ...prev,
      auto_load_last_config: newValue,
    }));

    try {
      const result = await invoke<TauriResult<null>>("save_auto_load_last_config", {
        enabled: newValue,
      });
      if (result && result.success) {
        onLog("success", `Auto-load last config ${newValue ? "enabled" : "disabled"}`);
      } else {
        onLog("error", "Failed to save auto-load setting");
        setAppSettings((prev) => ({
          ...prev,
          auto_load_last_config: !newValue,
        }));
      }
    } catch (err) {
      console.error("Failed to save auto-load setting:", err);
      onLog("error", `Failed to save auto-load setting: ${err}`);
      setAppSettings((prev) => ({
        ...prev,
        auto_load_last_config: !newValue,
      }));
    }
  };

  const handleToggleAutostart = async () => {
    const newValue = !autostartEnabled;
    setAutostartEnabled(newValue);

    try {
      const result = await invoke<TauriResult<AutostartStatusData>>("set_autostart_enabled", {
        enabled: newValue,
      });
      if (result && result.success && result.data) {
        // Reconcile against actual OS state — the registry/LaunchAgent write
        // can succeed-but-not-stick on hardened systems, so we believe the
        // post-toggle `is_enabled` readback over the optimistic update.
        setAutostartEnabled(result.data.enabled);
        onLog(
          "success",
          `Launch on system startup ${result.data.enabled ? "enabled" : "disabled"}`,
        );
      } else {
        onLog("error", "Failed to save launch-on-startup setting");
        setAutostartEnabled(!newValue);
      }
    } catch (err) {
      console.error("Failed to save autostart setting:", err);
      onLog("error", `Failed to save launch-on-startup setting: ${err}`);
      setAutostartEnabled(!newValue);
    }
  };

  const handleToggleDisclosure = (disclosure: FeatureDisclosure, current: boolean) => {
    const newValue = !current;
    setEnabled(disclosure, newValue);
    onLog("success", `${DISCLOSURE_LABELS[disclosure]} ${newValue ? "shown" : "hidden"}`);
  };

  const handleToggleIncludeSummaryStep = async () => {
    const newValue = !includeSummaryStep;
    setIncludeSummaryStep(newValue);

    try {
      const result = await invoke<TauriResult<null>>("save_include_summary_step_by_default", {
        enabled: newValue,
      });
      if (result && result.success) {
        onLog("success", `Include AI Summary step ${newValue ? "enabled" : "disabled"}`);
      } else {
        onLog("error", "Failed to save include summary step setting");
        setIncludeSummaryStep(!newValue);
      }
    } catch (err) {
      console.error("Failed to save include summary step setting:", err);
      onLog("error", `Failed to save include summary step setting: ${err}`);
      setIncludeSummaryStep(!newValue);
    }
  };

  return (
    <div className="space-y-6">
      <SectionHeader
        title="General"
        description="Application-level preferences that control startup behavior and defaults."
        icon={<SettingsIcon className="w-6 h-6" />}
      />

      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm">Application</h4>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Auto-load Last Configuration on Startup</div>
              <div className="text-xs text-muted-foreground">
                Automatically load the last used configuration file and workflow when the
                application starts
              </div>
            </div>
            <button
              onClick={handleToggleAutoLoad}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                appSettings.auto_load_last_config ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  appSettings.auto_load_last_config ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>

          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Launch on System Startup</div>
              <div className="text-xs text-muted-foreground">
                Auto-start the runner at user login so scheduled tasks fire reliably without leaving
                the runner manually open.
              </div>
            </div>
            <button
              onClick={handleToggleAutostart}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                autostartEnabled ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  autostartEnabled ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>

        <div className="p-3 bg-primary/5 rounded-lg">
          <div className="text-xs text-muted-foreground">
            <strong className="text-foreground">Tip:</strong> When enabled, the runner will
            automatically load your last configuration and selected workflow, saving time when you
            restart the application.
          </div>
        </div>
      </div>

      {/*
        Optional feature sets. The runner ships two large paradigms alongside
        its Terminal-first default — the verification-agentic loop workflow and
        visual GUI automation — and each is revealed here. Turning one on adds
        its pages to the sidebar AND its panels to this Settings sub-nav; both
        default off so a session-driven user is not asked to navigate machinery
        they never use. Nothing is uninstalled either way: every route, tab and
        settings panel stays reachable by deep-link regardless.
      */}
      <div className="space-y-4 rounded-lg bg-card/50 p-4">
        <h4 className="font-medium text-sm flex items-center gap-2">
          <Layers className="w-4 h-4" />
          Optional Feature Sets
        </h4>

        <div className="space-y-2">
          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Show advanced automation features</div>
              <div className="text-xs text-muted-foreground">
                Reveals the verification-agentic loop workflow: the Home prompt page, the Active
                execution dashboard, the Workflow Builder, DAG Editor, Orchestration loop, Step
                Builders and Specs — plus their settings panels (Advanced AI, Playwright, Execution
                Variables, Discovery). The Terminal is the primary way to run AI work; these are
                surfaces for building reusable, gated, scheduled workflows.
              </div>
            </div>
            <button
              onClick={() => handleToggleDisclosure("advanced", showAdvancedAutomation)}
              aria-pressed={showAdvancedAutomation}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                showAdvancedAutomation ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  showAdvancedAutomation ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>

          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Show visual GUI automation</div>
              <div className="text-xs text-muted-foreground">
                Adds the AI Dev / Visual switcher to the top of the sidebar. Switching to Visual
                swaps the menu to the image-matching, state-machine and device-control surfaces
                (Dashboard, Execute, Visual GUI) and reveals their settings panels (Self-Healing,
                Mobile, image-match debugging).
              </div>
            </div>
            <button
              onClick={() => handleToggleDisclosure("visual", showVisualAutomation)}
              aria-pressed={showVisualAutomation}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                showVisualAutomation ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  showVisualAutomation ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>
        </div>
      </div>

      {/*
        Workflow Builder defaults — only meaningful once the loop workflow is
        revealed, so the section follows its disclosure rather than describing a
        builder the user cannot open.

        Unlike the image-match debug section in AdvancedSettings, this one needs
        no "…or the setting is on" escape hatch — but for a narrower reason than
        it looks: `include_summary_step_by_default` currently has NO reader. It
        appears only in its own getter/setter chain (`settings.rs`,
        `config_facade.rs`, `commands/config.rs`) and the MCP settings
        passthrough; nothing in workflow generation or the step builders
        consults it. So the value cannot strand anything, because it does not
        do anything yet. If a consumer is ever wired up — the Terminal's
        "save as workflow" flow is the obvious candidate, and it deliberately
        opens the builder WITHOUT this disclosure — revisit this gate and apply
        the union rule the way AdvancedSettings does.
      */}
      {showAdvancedAutomation && (
        <div className="space-y-4 rounded-lg bg-card/50 p-4">
          <h4 className="font-medium text-sm flex items-center gap-2">
            <FileText className="w-4 h-4" />
            Workflow Builder
          </h4>

          <label className="flex items-center justify-between cursor-pointer p-3 rounded-lg bg-muted/30 hover:bg-muted/50 transition-colors">
            <div className="space-y-1">
              <div className="text-sm font-medium">Include AI Summary in New Workflows</div>
              <div className="text-xs text-muted-foreground">
                Automatically add an AI Summary step to the completion phase of new workflows
              </div>
            </div>
            <button
              onClick={handleToggleIncludeSummaryStep}
              aria-pressed={includeSummaryStep}
              className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
                includeSummaryStep ? "bg-primary" : "bg-muted"
              }`}
            >
              <span
                className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
                  includeSummaryStep ? "translate-x-4" : "translate-x-1"
                }`}
              />
            </button>
          </label>

          <div className="p-3 bg-primary/5 rounded-lg">
            <div className="text-xs text-muted-foreground">
              <strong className="text-foreground">Tip:</strong> The AI Summary step generates a
              comprehensive summary of your workflow execution, including what was accomplished and
              any issues encountered. It runs at the end of the completion phase.
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
