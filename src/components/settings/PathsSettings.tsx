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
 * `dev_logs_dir` — plus `repo_checkouts`, the map of repos that live outside
 * the workspace root (plan
 * `2026-09-12-continuation-for-a-repo-outside-the-workspace-root-spawns-into-an-empty-directory`). `plans_archive_dir` is not shown (runner PR #1288 removes
 * it) and `strict_mode` is a behaviour flag that belongs with the workflow
 * settings; both round-trip through a save untouched
 * (`buildPathSettingsPayload`).
 *
 * ## Per-tenant directories
 *
 * Plan
 * `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`,
 * P4. A device can be bound to N coord tenants, so the three plan/prompt
 * directories carry a `<field>_by_tenant` map beside the scalar: the map is the
 * override for one tenant's sessions, the scalar remains the DEVICE-WIDE
 * DEFAULT, and resolution is `by_tenant[tenant]` → scalar → unset.
 *
 * Those rows render only when `useTenant().showSwitcher` — the existing
 * `candidates.length > 1` gate (`TenantContext.tsx`), reused rather than
 * duplicated, so a single-tenant operator sees exactly the panel they saw
 * before. Rows are labelled with `shortTenantId` and the annotations this
 * frontend can actually derive; there is no tenant display name anywhere in it,
 * and inventing one here is explicitly out of scope (the plan's §5).
 *
 * The maps are PATCH fields on the save (`{}` clears, absent leaves the stored
 * map untouched), so this panel sends them only when it showed them —
 * `buildPathSettingsPayload`'s fourth argument.
 *
 * Every field shows the value IN EFFECT beside the value CONFIGURED, because
 * the two can genuinely differ: `workspace_root` yields to `$QONTINUI_ROOT` /
 * `$QONTINUI_WORKSPACE_ROOT` (kept on purpose, plan D4), `dev_logs_dir` falls
 * back to a platform default and is resolved once at start-up (so it alone is
 * honestly "next runner start"), and the plan-corpus dirs are re-read by the
 * adapter once per scan interval, so a saved change is live within one
 * interval.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Check, FolderOpen, FolderTree, Info, TriangleAlert, X } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import { useTenant } from "@/contexts/TenantContext";
import { shortTenantId } from "@/components/terminal/SpawnTenantPicker";
import {
  PATH_FIELDS,
  TENANT_PATH_FIELDS,
  buildPathSettingsPayload,
  divergenceKind,
  draftsAreDirty,
  draftsFrom,
  formatRepoCheckouts,
  normalizePathInput,
  parseRepoCheckouts,
  planScanStatusLabel,
  repoCheckoutsDirty,
  scanSourceStatus,
  storedTenantOverrideCount,
  tenantDraftsAreDirty,
  tenantDraftsFrom,
  tenantIdsForField,
  type PathDrafts,
  type PathField,
  type PathSettings,
  type PathSettingsView,
  type ResolvedPaths,
  type TenantPathDrafts,
  type TenantPathField,
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

/**
 * What a per-tenant row overrides, per directory. Deliberately shorter than
 * {@link FIELD_COPY}: the device-wide rows above already say what each directory
 * does, and repeating it once per tenant is the clutter the `showSwitcher` gate
 * exists to avoid.
 */
const TENANT_FIELD_COPY: Record<TenantPathField, { label: string; does: string }> = {
  plans_dir: {
    label: "Plans directory",
    does: "Sessions launched for this tenant get this directory as QONTINUI_PLANS_DIR.",
  },
  plans_archive_dir: {
    label: "Plans archive directory",
    does: "Where this tenant's archived plans live. The device-wide value is not edited in this panel and round-trips untouched.",
  },
  prompts_dir: {
    label: "Prompts directory",
    does: "Sessions launched for this tenant get this directory as QONTINUI_PROMPTS_DIR.",
  },
};

/** The one honest answer to "when does this apply?" — see the module doc. */
const TAKES_EFFECT =
  "Changes apply within the next scan interval (60 s by default); newly launched sessions see them immediately. No runner restart is needed — except the dev logs directory, which the runner resolves once at start-up.";

export function PathsSettings({ onLog }: PathsSettingsProps) {
  // The whole loaded view. `null` until the first load resolves, and stays
  // `null` when it fails: the panel never fabricates a `strict_mode` or a
  // `plans_archive_dir` to save over the real ones.
  const [view, setView] = useState<PathSettingsView | null>(null);
  const [drafts, setDrafts] = useState<PathDrafts>(() => draftsFrom({ strict_mode: false }));
  const [checkoutsDraft, setCheckoutsDraft] = useState("");
  // The per-tenant boxes. Seeded from the saved maps, so a tenant with no
  // override has no key here and renders as an empty box — which is also what
  // it means on the wire.
  const [tenantDrafts, setTenantDrafts] = useState<TenantPathDrafts>(() =>
    tenantDraftsFrom({ strict_mode: false }),
  );
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadAttempt, setLoadAttempt] = useState(0);
  // The tenants this device is bound to, and the existing no-clutter gate.
  // `candidates` is populated from a LOCAL `paired_user.json` read with no coord
  // round-trip (`commands/tenant.rs:193` → `pair::read_paired_binding_tenant_ids`),
  // so these rows work offline; an EMPTY list is UNKNOWN about the bindings,
  // never evidence of a single tenant.
  const { candidates, showSwitcher, defaultTenantIdForNewSessions } = useTenant();

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      setLoading(true);
      try {
        const loaded = await invoke<PathSettingsView>("get_path_settings");
        if (cancelled) return;
        setView(loaded);
        setDrafts(draftsFrom(loaded.configured));
        setCheckoutsDraft(formatRepoCheckouts(loaded.configured.repo_checkouts));
        setTenantDrafts(tenantDraftsFrom(loaded.configured));
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

  const setTenantDraft = useCallback(
    (field: TenantPathField, tenantId: string, value: string) => {
      setTenantDrafts((d) => ({ ...d, [field]: { ...d[field], [tenantId]: value } }));
    },
    [],
  );

  /**
   * Native directory picker — the same call `ClaudeCliSection` and
   * `ProjectsPage` make. A cancelled dialog resolves `null` and changes
   * nothing; a picker that cannot open is reported, not swallowed.
   *
   * One picker for both row kinds, so the device-wide and per-tenant boxes
   * cannot drift on how a cancellation or a failure is handled.
   */
  const pickDirectory = useCallback(async (): Promise<string | null> => {
    let selected: string | string[] | null;
    try {
      selected = await open({ directory: true, multiple: false });
    } catch (err) {
      console.error("Directory picker failed:", err);
      onLog("error", `Could not open the folder picker: ${String(err)}`);
      return null;
    }
    return typeof selected === "string" && selected.length > 0 ? selected : null;
  }, [onLog]);

  const browse = useCallback(
    async (field: PathField) => {
      const selected = await pickDirectory();
      if (selected !== null) setDraft(field, selected);
    },
    [pickDirectory, setDraft],
  );

  const browseTenant = useCallback(
    async (field: TenantPathField, tenantId: string) => {
      const selected = await pickDirectory();
      if (selected !== null) setTenantDraft(field, tenantId, selected);
    },
    [pickDirectory, setTenantDraft],
  );

  const saveSettings = async () => {
    if (!view) return;
    setSaving(true);
    setError(null);
    setSaveSuccess(false);
    try {
      const checkouts = parseRepoCheckouts(checkoutsDraft);
      if (checkouts.errors.length > 0) return;
      // The per-tenant maps are PATCH fields: they are sent only when the rows
      // were SHOWN. A panel that did not render them has no business asserting
      // what they should contain — omitting them leaves what is stored alone.
      const payload = buildPathSettingsPayload(
        view.configured,
        drafts,
        checkouts.entries,
        showSwitcher ? tenantDrafts : undefined,
      );
      const fresh = await invoke<PathSettingsView>("save_path_settings", { settings: payload });
      // Re-render from what the runner STORED, not from what was sent: the
      // resolved half is what tells the operator whether the change is in
      // effect yet, and only the runner can answer that.
      setView(fresh);
      setDrafts(draftsFrom(fresh.configured));
      setCheckoutsDraft(formatRepoCheckouts(fresh.configured.repo_checkouts));
      setTenantDrafts(tenantDraftsFrom(fresh.configured));
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

  const checkoutErrors = parseRepoCheckouts(checkoutsDraft).errors;
  const dirty = view
    ? draftsAreDirty(view.configured, drafts) ||
      repoCheckoutsDirty(view.configured, checkoutsDraft) ||
      // Only when the rows are shown: what is not rendered is not editable, and
      // an unrendered map is not sent.
      (showSwitcher && tenantDraftsAreDirty(view.configured, tenantDrafts))
    : false;

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
          <ScanSourceStatus resolved={view.resolved} />

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

            <RepoCheckoutsField
              draft={checkoutsDraft}
              errors={checkoutErrors}
              onChange={setCheckoutsDraft}
            />

            <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
              <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
              <p className={`text-xs ${getAccentColors("blue").text}`}>
                {TAKES_EFFECT} A blank field is saved as <strong>unset</strong>, never as an empty
                path. Stored in <code>settings.json</code> under <code>paths</code>, the same file
                every other section writes.
              </p>
            </div>
          </div>

          <PerTenantPaths
            showSwitcher={showSwitcher}
            candidates={candidates}
            defaultTenantId={defaultTenantIdForNewSessions}
            configured={view.configured}
            drafts={tenantDrafts}
            onChange={setTenantDraft}
            onBrowse={(field, tenantId) => void browseTenant(field, tenantId)}
          />

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
              disabled={saving || !dirty || checkoutErrors.length > 0}
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

// ── Scan-source drift ───────────────────────────────────────────────────────

const SCAN_SOURCE_ACCENT = {
  ok: "green",
  warn: "amber",
  unknown: "slate",
  off: "slate",
} as const;

/**
 * How far the directory the adapter scans has drifted from its default
 * branch — the reading that makes a plans dir parked on a stale branch
 * visible instead of silently authoritative. The wording, including the
 * floor rule, is `scanSourceStatus`'s; this only picks the accent. Nothing
 * is rendered while nothing is scanned — `PlanScanStatus` already says so.
 */
function ScanSourceStatus({ resolved }: { resolved: ResolvedPaths }) {
  const status = scanSourceStatus(resolved.plan_scan_divergence, resolved);
  if (status.tone === "off") return null;
  const accent = getAccentColors(SCAN_SOURCE_ACCENT[status.tone]);
  return (
    <div
      data-ui-bridge-id="settings.paths-scan-source-status"
      data-content-role="status"
      data-content-label="plan scan source drift"
      className={`p-3 ${accent.bg} rounded-lg flex items-start gap-2`}
    >
      {status.tone === "ok" ? (
        <Check className={`w-4 h-4 ${accent.text} shrink-0 mt-0.5`} />
      ) : status.tone === "warn" ? (
        <TriangleAlert className={`w-4 h-4 ${accent.text} shrink-0 mt-0.5`} />
      ) : (
        <Info className={`w-4 h-4 ${accent.text} shrink-0 mt-0.5`} />
      )}
      <div className="space-y-0.5">
        <p className={`text-xs font-medium ${accent.text}`}>{status.headline}</p>
        {status.detail && <p className={`text-[10px] ${accent.text}`}>{status.detail}</p>}
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

// ── Repos outside the workspace root ────────────────────────────────────────

interface RepoCheckoutsFieldProps {
  draft: string;
  errors: string[];
  onChange: (value: string) => void;
}

/**
 * `paths.repo_checkouts` as one `owner/name = path` line per repo. Without an
 * entry the runner looks for a repo owned by anyone but `qontinui` at
 * `<parent of workspace root>/<owner>/<name>`, then `<workspace root>/<name>`;
 * a continuation whose repo is at none of them is refused rather than started
 * in a directory that does not hold it. `qontinui/*` lines and relative paths
 * are rejected by `parseRepoCheckouts`, because the runner would ignore them.
 */
function RepoCheckoutsField({ draft, errors, onChange }: RepoCheckoutsFieldProps) {
  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium" htmlFor="paths-repo-checkouts">
        Repositories outside the workspace root
      </label>
      <textarea
        id="paths-repo-checkouts"
        data-ui-bridge-id="settings.paths-repo-checkouts"
        spellCheck={false}
        autoComplete="off"
        rows={3}
        value={draft}
        placeholder="e.g. your-org/your-app = /home/you/your-org/your-app"
        onChange={(e) => onChange(e.target.value)}
        className="w-full px-2.5 py-1.5 text-sm font-mono bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
      />
      <p className="text-[10px] text-muted-foreground">
        One <code>owner/name = path</code> line per repository this runner works on that is not
        checked out under the workspace root, as an absolute path. Sessions for that repository work
        in a worktree made from it, and are refused rather than started in the shared checkout when
        no worktree can be made.{" "}
        <strong>
          Without a line, a repository owned by anyone but qontinui is looked for at &lt;parent of
          the workspace root&gt;/&lt;owner&gt;/&lt;name&gt;, then &lt;workspace
          root&gt;/&lt;name&gt;; a session whose repository is in none of those places is refused
          instead of being started somewhere the repository is not.
        </strong>
      </p>
      {errors.length > 0 && (
        <div
          data-ui-bridge-id="settings.paths-repo-checkouts-errors"
          className={`p-2 ${getAccentColors("red").bg} rounded-md`}
        >
          {errors.map((e) => (
            <p key={e} className={`text-[10px] ${getAccentColors("red").text}`}>
              {e}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Per-tenant directories ──────────────────────────────────────────────────

interface PerTenantPathsProps {
  /** `useTenant().showSwitcher` — the ONLY gate on these rows. */
  showSwitcher: boolean;
  /** The tenants this device is bound to, as the context reports them. */
  candidates: readonly string[];
  /** The device's default tenant for new sessions, annotated on its row. */
  defaultTenantId: string | null;
  configured: PathSettings;
  drafts: TenantPathDrafts;
  onChange: (field: TenantPathField, tenantId: string, value: string) => void;
  onBrowse: (field: TenantPathField, tenantId: string) => void;
}

/**
 * One row per bound tenant, per per-tenant directory — behind `showSwitcher`.
 *
 * The gate is reused, not re-derived: `showSwitcher` is `candidates.length > 1`
 * in `TenantContext`, and a second predicate here would be a second answer to
 * "does this operator see tenant UI?". A single-tenant operator therefore sees
 * the panel exactly as it was, and when `candidates` is EMPTY — which is
 * UNKNOWN about the bindings, not evidence of one tenant — the note below says
 * so instead of the rows implying anything.
 */
function PerTenantPaths({
  showSwitcher,
  candidates,
  defaultTenantId,
  configured,
  drafts,
  onChange,
  onBrowse,
}: PerTenantPathsProps) {
  const bound = new Set(candidates.map((id) => id.trim()).filter((id) => id.length > 0));
  const storedOverrides = storedTenantOverrideCount(configured);

  if (!showSwitcher) {
    return <TenantBindingNote boundCount={bound.size} storedOverrides={storedOverrides} />;
  }

  const groups = TENANT_PATH_FIELDS.map((field) => ({
    field,
    ids: tenantIdsForField(configured, candidates, field),
  }));
  const hasUnbound = groups.some(({ ids }) => ids.some((id) => !bound.has(id)));
  const defaultId = defaultTenantId?.trim() ?? "";

  return (
    <div
      data-ui-bridge-id="settings.paths-per-tenant"
      className="rounded-lg bg-card/50 p-4 space-y-5"
    >
      <div className="space-y-1">
        <p className="text-xs font-medium">Per-tenant directories</p>
        <p className="text-[10px] text-muted-foreground">
          This device is bound to {bound.size} tenants, so the plan and prompt directories can differ
          per tenant. A path here is used for sessions launched for that tenant; a blank row means
          that tenant uses the device-wide directory above. A session&rsquo;s tenant is stamped when
          it is spawned and never changes, so a change here applies to future sessions.
        </p>
        {hasUnbound && (
          <p className="text-[10px] text-muted-foreground">
            A row marked <strong>not currently bound</strong> is a path stored for a tenant this
            device has no binding for right now. It is kept rather than dropped — the device may be
            re-pairing — and clearing the box is what removes it.
          </p>
        )}
      </div>

      {groups.map(({ field, ids }) => (
        <div key={field} className="space-y-1.5">
          <p className="text-xs font-medium">{TENANT_FIELD_COPY[field].label}</p>
          <p className="text-[10px] text-muted-foreground">
            {TENANT_FIELD_COPY[field].does} Device-wide default:{" "}
            <code>{normalizePathInput(configured[field]) ?? "(unset)"}</code>
          </p>
          {ids.map((tenantId) => (
            <TenantPathRow
              key={tenantId}
              field={field}
              tenantId={tenantId}
              draft={drafts[field][tenantId] ?? ""}
              bound={bound.has(tenantId)}
              isDeviceDefault={tenantId === defaultId}
              onChange={(value) => onChange(field, tenantId, value)}
              onBrowse={() => onBrowse(field, tenantId)}
              onClear={() => onChange(field, tenantId, "")}
            />
          ))}
        </div>
      ))}
    </div>
  );
}

/**
 * What the panel says when it shows no per-tenant rows.
 *
 * Two cases, and only the first is a statement the operator needs: an EMPTY
 * candidate list is UNKNOWN about this device's bindings —
 * `read_paired_binding_tenant_ids` returns an empty vec for an absent or
 * unreadable `paired_user.json` and never an error — so it must not read as
 * "this device has one tenant". A device that does report exactly one binding
 * gets nothing at all, unless per-tenant paths are stored, in which case saying
 * they survive the save is cheaper than an operator wondering.
 */
function TenantBindingNote({
  boundCount,
  storedOverrides,
}: {
  boundCount: number;
  storedOverrides: number;
}) {
  if (boundCount > 0 && storedOverrides === 0) return null;
  const accent = getAccentColors("slate");
  const stored =
    storedOverrides === 0
      ? null
      : `${storedOverrides} per-tenant ${storedOverrides === 1 ? "directory is" : "directories are"} stored; a save from this panel leaves them untouched.`;
  return (
    <div
      data-ui-bridge-id="settings.paths-per-tenant-note"
      data-content-role="status"
      data-content-label="per-tenant directories"
      className={`p-3 ${accent.bg} rounded-lg flex items-start gap-2`}
    >
      <Info className={`w-4 h-4 ${accent.text} shrink-0 mt-0.5`} />
      <div className="space-y-0.5">
        <p className={`text-xs font-medium ${accent.text}`}>
          {boundCount === 0
            ? "Per-tenant directories: this device's tenant bindings are unknown"
            : "Per-tenant directories: not shown for a single-tenant device"}
        </p>
        <p className={`text-[10px] ${accent.text}`}>
          {boundCount === 0
            ? "The bindings are read from this machine's own pairing record, and an absent or unreadable one reads as an empty list — which is not evidence that the device is bound to exactly one tenant. Until a binding is recorded, every directory above is device-wide."
            : "This device reports one tenant binding, so the directories above are the whole story."}
          {stored !== null && ` ${stored}`}
        </p>
      </div>
    </div>
  );
}

interface TenantPathRowProps {
  field: TenantPathField;
  tenantId: string;
  /** The input-box value. `""` means "no override for this tenant". */
  draft: string;
  /** `false` when the id is stored but the device is not bound to it (D2). */
  bound: boolean;
  isDeviceDefault: boolean;
  onChange: (value: string) => void;
  onBrowse: () => void;
  onClear: () => void;
}

/**
 * One tenant's override of one directory.
 *
 * Labelled with `shortTenantId` — the only labelling affordance this frontend
 * has, since `candidates` is raw UUIDs and no tenant display name exists
 * anywhere in it — plus the two annotations that ARE derivable here: the device
 * default, and D2's not-currently-bound. The full id is the label's `title`, so
 * nothing is hidden by the truncation. Resolving a human-readable name needs a
 * coord round-trip and a widened wire type, and is deliberately not started here.
 */
function TenantPathRow({
  field,
  tenantId,
  draft,
  bound,
  isDeviceDefault,
  onChange,
  onBrowse,
  onClear,
}: TenantPathRowProps) {
  const slug = `${field.replace(/_/g, "-")}-tenant-${tenantId}`;
  const inputId = `paths-${slug}`;
  const bridgeId = `settings.paths-${slug}`;
  const isSet = normalizePathInput(draft) !== undefined;

  return (
    <div className="flex gap-2 items-center">
      <label htmlFor={inputId} title={tenantId} className="w-44 shrink-0 text-[10px] leading-tight">
        <code className="font-mono">{shortTenantId(tenantId)}</code>
        {isDeviceDefault && <span className="text-muted-foreground"> · device default</span>}
        {!bound && (
          <span className={getAccentColors("amber").text}> · not currently bound</span>
        )}
      </label>
      <input
        id={inputId}
        data-ui-bridge-id={bridgeId}
        type="text"
        spellCheck={false}
        autoComplete="off"
        value={draft}
        placeholder="Uses the device-wide directory above"
        onChange={(e) => onChange(e.target.value)}
        className="flex-1 min-w-0 px-2.5 py-1.5 text-sm font-mono bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50"
      />
      <button
        type="button"
        data-ui-bridge-id={`${bridgeId}-browse`}
        onClick={onBrowse}
        title="Choose a directory for this tenant"
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
        title="Remove this tenant's override"
        className="px-3 py-1.5 bg-muted hover:bg-muted/70 rounded-md text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
      >
        Clear
      </button>
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
    fallback:
      field === "workspace_root"
        ? "Nothing is configured, so the runner resolved it from $QONTINUI_ROOT / $QONTINUI_WORKSPACE_ROOT or the ancestor walk from the executable."
        : "Nothing is configured, so the runner is using its platform default.",
    override:
      "$QONTINUI_ROOT / $QONTINUI_WORKSPACE_ROOT in the runner's environment override this setting. That precedence is deliberate; to change what is in effect, change the environment the runner was started with.",
    lag: "Differs from what is saved. The adapter re-reads this setting once per scan interval, so the saved value is in effect within one interval (60 s by default); newly launched sessions already see it.",
    restart:
      "Differs from what is saved. The runner resolves its dev logs directory once at start-up, so the saved value takes effect at the next runner start.",
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
