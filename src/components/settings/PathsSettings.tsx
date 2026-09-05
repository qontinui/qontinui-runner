/**
 * PathsSettings — the directories the runner reads, in one panel.
 *
 * Plan `2026-09-05-plans-dir-is-env-only-and-unreachable-in-the-product`,
 * Phase 3. Wraps the runner's `paths` settings group
 * (`settings::PathSettings` in src-tauri), which until this panel had NO
 * product surface: the plans directory was settable only by a
 * backward-compatibility env shim or by hand-editing `settings.json` at a path
 * nothing in the product named, and the two directories with no env override
 * at all were unreachable by any means. The markdown-plan tier is OFF by
 * default with no fallback path, so this panel is its only switch — which is
 * why the tab is deliberately not behind a feature disclosure.
 *
 * Wire contract (the D5 command pair, `commands/path_settings.rs`):
 *
 *   invoke<PathSettingsView>("get_path_settings")
 *   invoke<PathSettingsView>("save_path_settings", { settings: PathSettings })
 *
 * Both return the view directly (no `{ success, data }` wrapper, unlike
 * `get_session_guard_settings`); a failure rejects with a string. `save`
 * returns the FRESH view after the write, so the panel re-renders from what
 * the runner actually stored rather than from what it sent.
 *
 * Four fields are edited here — `plans_dir`, `prompts_dir`, `workspace_root`,
 * `dev_logs_dir`. `plans_archive_dir` is not shown (runner PR #1288 removes
 * it) and `strict_mode` is a behaviour flag that belongs with the workflow
 * settings; both round-trip through a save untouched
 * (`buildPathSettingsPayload`).
 *
 * Every field shows the value IN EFFECT beside the value CONFIGURED, because
 * the two can genuinely differ: `workspace_root` yields to `$QONTINUI_ROOT` /
 * `$QONTINUI_WORKSPACE_ROOT` (kept on purpose, plan D4), `dev_logs_dir` falls
 * back to a platform default, and the plan-corpus dirs are re-read by the
 * adapter once per scan interval, so a saved change is live within one
 * interval — not at the next runner start, which fleet policy forbids anyway.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Check, FolderOpen, FolderTree, Info, TriangleAlert, X } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import {
  PATH_FIELDS,
  buildPathSettingsPayload,
  divergenceKind,
  draftsAreDirty,
  draftsFrom,
  normalizePathInput,
  planScanStatusLabel,
  type PathDrafts,
  type PathField,
  type PathSettingsView,
  type ResolvedPaths,
} from "./pathsSettingsHelpers";
import type { LogFunction } from "./types";

interface PathsSettingsProps {
  onLog: LogFunction;
}

/**
 * What each field does, and what happens when it is unset. Rendered verbatim
 * under the input, so the panel — not a log line — is where "why is nothing
 * being scanned?" gets answered.
 */
interface PathFieldCopy {
  label: string;
  does: string;
  whenUnset: string;
  placeholder: string;
}

const FIELD_COPY: Record<PathField, PathFieldCopy> = {
  plans_dir: {
    label: "Plans directory",
    does: "The directory of markdown plans the plan adapter scans: each plan becomes a coord work unit, and every session launched by this runner receives it as QONTINUI_PLANS_DIR.",
    whenUnset:
      "Unset means plan scanning is off. No work units are pushed to coord and sessions get no QONTINUI_PLANS_DIR.",
    placeholder: "e.g. /home/you/qontinui-dev-notes/plans",
  },
  prompts_dir: {
    label: "Prompts directory",
    does: "The directory of prompt documents the adapter scans alongside the plans, exported to every session as QONTINUI_PROMPTS_DIR.",
    whenUnset: "Unset means the prompt scan is off and sessions get no QONTINUI_PROMPTS_DIR.",
    placeholder: "e.g. /home/you/qontinui-dev-notes/plans/prompts",
  },
  workspace_root: {
    label: "Workspace root",
    does: "The directory holding the repo checkouts side by side. Worktrees, build coordination and the scripts a session runs all resolve from it.",
    whenUnset:
      "Unset means it is resolved from $QONTINUI_ROOT, then $QONTINUI_WORKSPACE_ROOT, then an ancestor walk from the runner executable.",
    placeholder: "e.g. /home/you/qontinui-root",
  },
  dev_logs_dir: {
    label: "Dev logs directory",
    does: "Where the runner writes its own log files, and where debugging starts.",
    whenUnset: "Unset means the platform default, shown under “In effect” below.",
    placeholder: "platform default",
  },
};

/** The one honest answer to "when does this apply?" — see the module doc. */
const TAKES_EFFECT =
  "Changes apply within the next scan interval (60 s by default); newly launched sessions see them immediately. No runner restart is needed.";

export function PathsSettings({ onLog }: PathsSettingsProps) {
  // The whole loaded view. `null` until the first load resolves, and stays
  // `null` when it fails: the panel never fabricates a `strict_mode` or a
  // `plans_archive_dir` to save over the real ones.
  const [view, setView] = useState<PathSettingsView | null>(null);
  const [drafts, setDrafts] = useState<PathDrafts>(() => draftsFrom({ strict_mode: false }));
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadAttempt, setLoadAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      setLoading(true);
      try {
        const loaded = await invoke<PathSettingsView>("get_path_settings");
        if (cancelled) return;
        setView(loaded);
        setDrafts(draftsFrom(loaded.configured));
        setError(null);
        onLog("debug", "Path settings loaded");
      } catch (err) {
        if (cancelled) return;
        console.error("Failed to load path settings:", err);
        setError(`Failed to load settings: ${String(err)}`);
        onLog("error", `Failed to load path settings: ${String(err)}`);
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
    // `loadAttempt` is the Retry button's handle on this effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadAttempt]);

  const setDraft = useCallback((field: PathField, value: string) => {
    setDrafts((d) => ({ ...d, [field]: value }));
  }, []);

  /**
   * Native directory picker — the same call `ClaudeCliSection` and
   * `ProjectsPage` make. A cancelled dialog resolves `null` and changes
   * nothing; a picker that cannot open is reported, not swallowed.
   */
  const browse = useCallback(
    async (field: PathField) => {
      let selected: string | string[] | null;
      try {
        selected = await open({ directory: true, multiple: false });
      } catch (err) {
        console.error("Directory picker failed:", err);
        onLog("error", `Could not open the folder picker: ${String(err)}`);
        return;
      }
      if (typeof selected === "string" && selected.length > 0) {
        setDraft(field, selected);
      }
    },
    [onLog, setDraft],
  );

  const saveSettings = async () => {
    if (!view) return;
    setSaving(true);
    setError(null);
    setSaveSuccess(false);
    try {
      const payload = buildPathSettingsPayload(view.configured, drafts);
      const fresh = await invoke<PathSettingsView>("save_path_settings", { settings: payload });
      // Re-render from what the runner STORED, not from what was sent: the
      // resolved half is what tells the operator whether the change is in
      // effect yet, and only the runner can answer that.
      setView(fresh);
      setDrafts(draftsFrom(fresh.configured));
      setSaveSuccess(true);
      onLog("success", "Path settings saved");
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (err) {
      console.error("Failed to save path settings:", err);
      setError(`Failed to save settings: ${String(err)}`);
      onLog("error", `Failed to save path settings: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading path settings"
          className="text-muted-foreground"
        >
          Loading path settings...
        </div>
      </div>
    );
  }

  const dirty = view ? draftsAreDirty(view.configured, drafts) : false;

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Paths"
        description="The directories this runner reads: the plan and prompt corpus it scans for coord, the workspace the repos live in, and where its own logs go. Each field shows what is saved and what is in effect right now."
        icon={<FolderTree className="w-6 h-6" />}
      />

      {error && (
        <div
          data-ui-bridge-id="settings.paths-error"
          className={`p-3 ${getAccentColors("red").bg} rounded-lg flex items-start gap-2`}
        >
          <X className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("red").text} text-xs`}>{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className={`p-3 ${getAccentColors("green").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("green").text} text-xs`}>Path settings saved.</span>
        </div>
      )}

      {!view ? (
        // The load failed. No form: a save built on invented values would
        // overwrite the fields this panel does not edit.
        <div className="rounded-lg bg-card/50 p-4 space-y-3">
          <p className="text-xs text-muted-foreground">
            The current path settings could not be read, so there is nothing safe to edit yet.
          </p>
          <button
            type="button"
            data-ui-bridge-id="settings.paths-retry-load"
            onClick={() => setLoadAttempt((n) => n + 1)}
            className="px-3 py-1.5 bg-muted hover:bg-muted/70 rounded-md text-xs font-medium transition-colors"
          >
            Retry
          </button>
        </div>
      ) : (
        <>
          <PlanScanStatus resolved={view.resolved} />

          <div className="rounded-lg bg-card/50 p-4 space-y-5">
            {PATH_FIELDS.map((field) => (
              <PathFieldRow
                key={field}
                field={field}
                draft={drafts[field]}
                configured={view.configured[field]}
                resolved={view.resolved[field]}
                onChange={(value) => setDraft(field, value)}
                onBrowse={() => void browse(field)}
                onClear={() => setDraft(field, "")}
              />
            ))}

            <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("blue").text}`}>
                {TAKES_EFFECT} A blank field is saved as <strong>unset</strong>, never as an empty
                path. Stored in <code>settings.json</code> under <code>paths</code>, the same file
                every other section writes.
              </p>
            </div>
          </div>

          <div className="flex justify-end items-center gap-3">
            {dirty && !saving && (
              <span
                data-ui-bridge-id="settings.paths-unsaved"
                className="text-[10px] text-muted-foreground"
              >
                Unsaved changes
              </span>
            )}
            <button
              type="button"
              data-ui-bridge-id="settings.paths-save"
              onClick={saveSettings}
              disabled={saving || !dirty}
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
                  <FolderTree className="w-4 h-4" />
                  Save Paths
                </>
              )}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

// ── Plan-tier status ────────────────────────────────────────────────────────

/**
 * "Plan scanning: on (N scan roots)" / "off", from the adapter's own report.
 *
 * `plan_scan_roots` is `null` when the adapter has not completed a cycle (or
 * is not running), and that renders as UNKNOWN — a `0` here would claim the
 * adapter looked and found nothing, which is not what a missing report means.
 */
function PlanScanStatus({ resolved }: { resolved: ResolvedPaths }) {
  const accent = resolved.plan_tier_active ? getAccentColors("green") : getAccentColors("amber");
  return (
    <div
      data-ui-bridge-id="settings.paths-plan-scan-status"
      data-content-role="status"
      data-content-label="plan scanning status"
      className={`p-3 ${accent.bg} rounded-lg flex items-start gap-2`}
    >
      {resolved.plan_tier_active ? (
        <Check className={`w-4 h-4 ${accent.text} shrink-0 mt-0.5`} />
      ) : (
        <TriangleAlert className={`w-4 h-4 ${accent.text} shrink-0 mt-0.5`} />
      )}
      <div className="space-y-0.5">
        <p className={`text-xs font-medium ${accent.text}`}>
          {planScanStatusLabel(resolved.plan_tier_active, resolved.plan_scan_roots)}
        </p>
        <p className={`text-[10px] ${accent.text}`}>
          {resolved.plan_tier_active
            ? "The adapter is scanning the plans directory in effect and pushing work units to coord."
            : "No plans directory is in effect, so nothing is scanned and no work units reach coord. Set one below to turn the tier on."}
        </p>
      </div>
    </div>
  );
}

// ── One path field ──────────────────────────────────────────────────────────

interface PathFieldRowProps {
  field: PathField;
  /** The input-box value. */
  draft: string;
  /** The SAVED value (absent when unset). */
  configured: string | undefined;
  /** The value in effect (null when nothing is). */
  resolved: string | null;
  onChange: (value: string) => void;
  onBrowse: () => void;
  onClear: () => void;
}

function PathFieldRow({
  field,
  draft,
  configured,
  resolved,
  onChange,
  onBrowse,
  onClear,
}: PathFieldRowProps) {
  const copy = FIELD_COPY[field];
  const inputId = `paths-${field.replace(/_/g, "-")}`;
  const bridgeId = `settings.paths-${field.replace(/_/g, "-")}`;
  const kind = divergenceKind(field, configured, resolved);
  const isSet = normalizePathInput(draft) !== undefined;

  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium" htmlFor={inputId}>
        {copy.label}
      </label>
      <div className="flex gap-2">
        <input
          id={inputId}
          data-ui-bridge-id={bridgeId}
          type="text"
          spellCheck={false}
          autoComplete="off"
          value={draft}
          placeholder={copy.placeholder}
          onChange={(e) => onChange(e.target.value)}
          className="flex-1 min-w-0 px-2.5 py-1.5 text-sm font-mono bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
        />
        <button
          type="button"
          data-ui-bridge-id={`${bridgeId}-browse`}
          onClick={onBrowse}
          title="Choose a directory"
          className="px-3 py-1.5 bg-muted hover:bg-muted/70 rounded-md text-xs font-medium transition-colors flex items-center gap-1.5 shrink-0"
        >
          <FolderOpen className="w-3.5 h-3.5" />
          Browse…
        </button>
        <button
          type="button"
          data-ui-bridge-id={`${bridgeId}-clear`}
          onClick={onClear}
          disabled={!isSet}
          title="Unset this directory"
          className="px-3 py-1.5 bg-muted hover:bg-muted/70 rounded-md text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
        >
          Clear
        </button>
      </div>
      <p className="text-[10px] text-muted-foreground">
        {copy.does} <strong>{copy.whenUnset}</strong>
      </p>
      <InEffect field={field} kind={kind} configured={configured} resolved={resolved} />
    </div>
  );
}

// ── Configured vs. in effect ────────────────────────────────────────────────

interface InEffectProps {
  field: PathField;
  kind: ReturnType<typeof divergenceKind>;
  configured: string | undefined;
  resolved: string | null;
}

/**
 * The value the runner is USING, beside the one that is saved — always shown,
 * and visibly flagged when they differ, with the reason for this field.
 *
 * Compares the SAVED value, not the draft: an unsaved edit is "Unsaved
 * changes" by the Save button, not a discrepancy between the runner and its
 * own settings file.
 */
function InEffect({ field, kind, configured, resolved }: InEffectProps) {
  const bridgeId = `settings.paths-${field.replace(/_/g, "-")}-in-effect`;
  const inEffect = resolved ?? "(none)";
  const saved = normalizePathInput(configured) ?? "(unset)";

  if (kind === "none") {
    return (
      <p data-ui-bridge-id={bridgeId} className="text-[10px] text-muted-foreground">
        In effect: <code>{inEffect}</code>
      </p>
    );
  }

  // A fallback is provenance, not a problem; the other two are worth a flag.
  const accent = kind === "fallback" ? getAccentColors("blue") : getAccentColors("amber");
  const reason = {
    fallback: "Nothing is configured, so the runner is using its platform default.",
    override:
      "$QONTINUI_ROOT / $QONTINUI_WORKSPACE_ROOT in the runner's environment override this setting (or, with nothing configured, the ancestor walk from the executable supplied it). That precedence is deliberate; to change what is in effect, change the environment the runner was started with.",
    lag: "Differs from what is saved. The adapter re-reads this setting once per scan interval, so the saved value is in effect within one interval (60 s by default); newly launched sessions already see it.",
  }[kind];

  return (
    <div data-ui-bridge-id={bridgeId} className={`p-2 ${accent.bg} rounded-md flex gap-2`}>
      {kind === "fallback" ? (
        <Info className={`w-3.5 h-3.5 ${accent.text} shrink-0 mt-0.5`} />
      ) : (
        <TriangleAlert className={`w-3.5 h-3.5 ${accent.text} shrink-0 mt-0.5`} />
      )}
      <p className={`text-[10px] ${accent.text}`}>
        In effect: <code>{inEffect}</code>
        {kind !== "fallback" && (
          <>
            {" "}
            (saved: <code>{saved}</code>)
          </>
        )}
        <br />
        {reason}
      </p>
    </div>
  );
}
