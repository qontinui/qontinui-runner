/**
 * Pure helpers and wire types for `PathsSettings`.
 *
 * Lives outside the JSX module for the reason `lockYieldPolicyHelpers.ts` and
 * `resourceGuardHelpers.ts` document: vitest runs under `environment: "node"`
 * and can import these without dragging in the design-system / SectionHeader
 * module graph.
 *
 * ## Absent, not empty
 *
 * `settings::PathSettings` stores every directory as an `Option<String>` with
 * `skip_serializing_if`, so an unset field is ABSENT on the wire and must be
 * sent back absent. An empty string is not "unset": for `plans_dir` it would
 * be a directory named `""` that the adapter tries to scan, and for
 * `workspace_root` it would shadow the `$QONTINUI_ROOT` fallback with nothing.
 * `normalizePathInput` is the one place a blank input box becomes `undefined`,
 * and `buildPathSettingsPayload` is the one place a payload is assembled, so
 * the two directions cannot drift apart.
 *
 * ## Configured vs. in effect
 *
 * The panel shows the value the runner is USING beside the value that is
 * SAVED. They can legitimately differ — `workspace_root` yields to two env
 * overrides, `dev_logs_dir` falls back to a platform default, and the
 * plan-corpus dirs are re-read once per scan interval — so the comparison
 * normalises the two spellings a path picks up on its way through Rust
 * (`\` vs `/`, a trailing separator) rather than flagging every cosmetic
 * difference as a discrepancy.
 */

/**
 * Wire shape of `settings::PathSettings` (serde snake_case).
 *
 * `plans_archive_dir` and `strict_mode` are NOT edited by the panel — the
 * former is being removed by runner PR #1288, the latter is a behaviour flag
 * that belongs with the workflow settings — but both must round-trip through
 * a save untouched, which `buildPathSettingsPayload` guarantees by spreading
 * the loaded struct before overwriting only the edited fields.
 */
export interface PathSettings {
  dev_logs_dir?: string;
  plans_dir?: string;
  plans_archive_dir?: string;
  prompts_dir?: string;
  workspace_root?: string;
  strict_mode: boolean;
}

/** What is in effect right now, as reported by `get_path_settings`. */
export interface ResolvedPaths {
  plans_dir: string | null;
  prompts_dir: string | null;
  /**
   * May differ from the configured value: `$QONTINUI_ROOT` /
   * `$QONTINUI_WORKSPACE_ROOT` in the runner's environment beat the setting,
   * deliberately (plan D4 keeps that precedence).
   */
  workspace_root: string | null;
  /** Always resolves — the platform default when unset. */
  dev_logs_dir: string;
  /** `true` when a plans dir is in effect, i.e. the markdown-plan tier is on. */
  plan_tier_active: boolean;
  /**
   * The adapter's scan-root count; `null` when the adapter is not running or
   * has not completed a cycle yet — UNKNOWN, never zero.
   */
  plan_scan_roots: number | null;
  /**
   * How far the directory the adapter actually scans has drifted from its
   * default branch; `null` when the adapter has not ticked yet — UNKNOWN,
   * never "in step". See {@link scanSourceStatus}.
   */
  plan_scan_divergence: ScanDivergenceView | null;
}

/**
 * Wire shape of `ScanDivergenceView` (`commands/path_settings.rs`): the plan
 * adapter's last scan-source divergence reading.
 *
 * Only `state === "measured"` carries `behind` / `ahead`; every absent number
 * is UNKNOWN. `counts_are_floors` is the runner's floor rule, computed there
 * so the panel never re-derives it: the ref the counts were taken against is
 * older than the adapter's freshness window, or of unknown age, so the counts
 * are LOWER BOUNDS.
 */
export interface ScanDivergenceView {
  state: "not_scanning" | "not_a_git_work_tree" | "measured" | "unknown";
  plans_dir: string | null;
  repo_root: string | null;
  /** The repo's own default branch as resolved at scan time, e.g. `origin/main`. */
  default_ref: string | null;
  ref_sha: string | null;
  head_sha: string | null;
  behind: number | null;
  ahead: number | null;
  /** Seconds since `default_ref` was last known refreshed; `null` is UNKNOWN. */
  ref_age_secs: number | null;
  counts_are_floors: boolean;
  /** Why the state is `unknown` / `not_a_git_work_tree`, or why the age is absent. */
  detail: string | null;
}

/** Return shape of both `get_path_settings` and `save_path_settings`. */
export interface PathSettingsView {
  configured: PathSettings;
  resolved: ResolvedPaths;
}

/** The fields the panel edits, in the order they are rendered. */
export const PATH_FIELDS = ["plans_dir", "prompts_dir", "workspace_root", "dev_logs_dir"] as const;

export type PathField = (typeof PATH_FIELDS)[number];

/** The text-input values, one per edited field. `""` means "unset". */
export type PathDrafts = Record<PathField, string>;

/**
 * Blank → `undefined`, otherwise the trimmed path.
 *
 * The ONLY door from an input box to a settings value. An empty string is
 * never persisted (see the module doc), and surrounding whitespace — the
 * usual residue of a pasted path — is not part of a directory name.
 */
export function normalizePathInput(raw: string | null | undefined): string | undefined {
  if (raw === null || raw === undefined) return undefined;
  const trimmed = raw.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

/**
 * One spelling for a path that may have passed through Rust's `PathBuf` on
 * either platform: `\` → `/`, trailing separators stripped (a bare root such
 * as `/` or `C:/` keeps its one separator), a Windows drive letter upper-cased.
 *
 * Used for COMPARISON only — the value shown and saved is the operator's own.
 */
export function canonicalPath(path: string): string {
  let s = path.trim().replace(/\\/g, "/");
  s = s.replace(/^([a-z]):/, (_m, drive: string) => `${drive.toUpperCase()}:`);
  while (s.length > 1 && s.endsWith("/") && !/^[A-Z]:\/$/.test(s)) {
    s = s.slice(0, -1);
  }
  return s;
}

/**
 * `true` when the value in effect is not the value that is configured.
 *
 * Absent on both sides is agreement; absent on exactly one side is a
 * difference (a fallback, an override, or a scan-interval lag — see
 * {@link divergenceKind}); otherwise the two canonical spellings decide.
 */
export function resolvedDiffers(
  configured: string | undefined,
  resolved: string | null | undefined,
): boolean {
  const c = normalizePathInput(configured);
  const r = normalizePathInput(resolved);
  if (c === undefined && r === undefined) return false;
  if (c === undefined || r === undefined) return true;
  return canonicalPath(c) !== canonicalPath(r);
}

/**
 * WHY the in-effect value differs from the configured one — the explanation
 * the panel renders beside the flag.
 *
 * - `"none"`     — they agree.
 * - `"fallback"` — nothing is configured and the runner is using its own
 *                  default (`dev_logs_dir`'s platform default). Not a
 *                  discrepancy; shown as provenance.
 * - `"fallback"` — also `workspace_root` with nothing configured: the
 *                  runner resolved it from `$QONTINUI_ROOT` /
 *                  `$QONTINUI_WORKSPACE_ROOT` or the ancestor walk from the
 *                  executable. Provenance, not a discrepancy.
 * - `"override"` — `workspace_root` with a CONFIGURED value that is not the
 *                  one in effect: `$QONTINUI_ROOT` / `$QONTINUI_WORKSPACE_ROOT`
 *                  in the runner's environment beat the setting. Deliberate;
 *                  plan D4.
 * - `"lag"`      — the plan-corpus dirs: the adapter re-reads the setting once
 *                  per scan interval, so a saved change is in effect within one
 *                  interval.
 * - `"restart"`  — a configured `dev_logs_dir` the process has not picked up:
 *                  the runner resolves that directory once, at first use, so a
 *                  change is honestly in effect only at the next runner start.
 */
export type DivergenceKind = "none" | "fallback" | "override" | "lag" | "restart";

export function divergenceKind(
  field: PathField,
  configured: string | undefined,
  resolved: string | null | undefined,
): DivergenceKind {
  if (!resolvedDiffers(configured, resolved)) return "none";
  const unconfigured = normalizePathInput(configured) === undefined;
  if (field === "workspace_root") return unconfigured ? "fallback" : "override";
  if (field === "dev_logs_dir") return unconfigured ? "fallback" : "restart";
  return "lag";
}

/** The input-box values for a loaded (or freshly saved) struct. */
export function draftsFrom(configured: PathSettings): PathDrafts {
  return {
    plans_dir: configured.plans_dir ?? "",
    prompts_dir: configured.prompts_dir ?? "",
    workspace_root: configured.workspace_root ?? "",
    dev_logs_dir: configured.dev_logs_dir ?? "",
  };
}

/**
 * The struct to send to `save_path_settings`.
 *
 * Starts from the LOADED struct so every field the panel does not edit
 * (`plans_archive_dir`, `strict_mode`) round-trips untouched, then overwrites
 * only the four edited fields — DELETING a key whose draft is blank rather
 * than writing `""`, because absent is the wire form of unset.
 */
export function buildPathSettingsPayload(saved: PathSettings, drafts: PathDrafts): PathSettings {
  const next: PathSettings = { ...saved };
  for (const field of PATH_FIELDS) {
    const value = normalizePathInput(drafts[field]);
    if (value === undefined) {
      delete next[field];
    } else {
      next[field] = value;
    }
  }
  return next;
}

/** `true` when a draft would persist something other than what is saved. */
export function draftsAreDirty(saved: PathSettings, drafts: PathDrafts): boolean {
  return PATH_FIELDS.some(
    (field) => normalizePathInput(drafts[field]) !== normalizePathInput(saved[field]),
  );
}

/**
 * The one-line plan-tier status: "Plan scanning: on (3 scan roots)" / "off".
 *
 * A `null` root count renders as UNKNOWN, never as 0 — the adapter has not
 * reported a cycle yet, which says nothing about how many roots it will scan.
 */
export function planScanStatusLabel(active: boolean, scanRoots: number | null): string {
  if (!active) return "Plan scanning: off";
  if (scanRoots === null) return "Plan scanning: on (scan roots: unknown)";
  return `Plan scanning: on (${scanRoots} scan ${scanRoots === 1 ? "root" : "roots"})`;
}

/** `45s`, `12m`, `7h`, `3d` — whole units, rounded down. */
export function formatRefAge(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

/**
 * How the scan-source reading is shown:
 * - `"ok"`      — measured in step against a ref proven current;
 * - `"warn"`    — a measured divergence (current or a lower bound);
 * - `"unknown"` — nothing that proves the distance either way;
 * - `"off"`     — nothing is scanned.
 */
export type ScanSourceTone = "ok" | "warn" | "unknown" | "off";

export interface ScanSourceReading {
  tone: ScanSourceTone;
  headline: string;
  /** The sentence under the headline; `null` when there is nothing to add. */
  detail: string | null;
}

/**
 * The panel's reading of {@link ScanDivergenceView}.
 *
 * The one rule this exists to keep is the runner's FLOOR RULE: when
 * `counts_are_floors` is set, `behind` is a lower bound, so it renders as
 * "at least N behind" — and a floor of `0 behind` (whatever `ahead` is) is
 * UNKNOWN, never "in step". A `0/0` is the reading that LOOKS like agreement,
 * which is exactly why it is the one that must not be trusted when the ref it
 * was compared against is stale or of unknown age. Only a `0/0` against a ref
 * proven current reads "in step". On a floor `ahead` moves the other way — a
 * stale ref can make it OVERSTATE — so it renders as "up to N ahead".
 *
 * The age is the ref's age when the reading was TAKEN (the panel holds a
 * snapshot), so it is worded "as of this reading", never as a live "ago".
 *
 * `inEffect` is what the runner resolves NOW. The adapter re-reads the plans
 * dir once per scan interval, so straight after a save the reading can
 * describe the PREVIOUS directory — or be `not_scanning` when the tier was
 * just turned on. A reading for another directory is not this directory's
 * reading, so it renders as not measured yet, never as the old verdict.
 */
export function scanSourceStatus(
  view: ScanDivergenceView | null,
  inEffect: Pick<ResolvedPaths, "plans_dir" | "plan_tier_active">,
): ScanSourceReading {
  if (!inEffect.plan_tier_active) {
    return { tone: "off", headline: "Scan source: nothing is scanned", detail: null };
  }
  const notYet = (detail: string): ScanSourceReading => ({
    tone: "unknown",
    headline: "Scan source: not measured yet for this directory",
    detail,
  });
  if (view === null) {
    return notYet(
      "The adapter has not completed a cycle since the runner started, so how far the scanned directory has drifted is unknown.",
    );
  }
  if (
    view.state === "not_scanning" ||
    (view.plans_dir !== null && resolvedDiffers(view.plans_dir, inEffect.plans_dir))
  ) {
    return notYet(
      "The adapter re-reads the plans directory once per scan interval, and its last reading is for a different directory. Reopen this panel after the next interval.",
    );
  }
  if (view.state === "not_a_git_work_tree") {
    return {
      tone: "unknown",
      headline: "Scan source: not a git work tree — drift cannot be measured",
      detail: view.detail,
    };
  }
  const { behind, ahead } = view;
  if (view.state !== "measured" || behind === null || ahead === null) {
    return { tone: "unknown", headline: "Scan source: drift unknown", detail: view.detail };
  }

  const ref = view.default_ref ?? "the default branch";
  const age =
    view.ref_age_secs === null
      ? null
      : `As of this reading, ${ref} had last been refreshed ${formatRefAge(view.ref_age_secs)} earlier`;

  if (view.counts_are_floors) {
    const why =
      age === null
        ? `Nothing proves when ${ref} was last refreshed${view.detail ? ` (${view.detail})` : ""}`
        : `${age}, outside the adapter's freshness window`;
    const detail = `${why}, so the true distance behind can only be larger and an ahead count may overstate. The adapter never fetches.`;
    if (behind === 0) {
      return ahead > 0
        ? {
            tone: "warn",
            headline: `Scan source: up to ${ahead} ahead of ${ref}; behind unknown (0 is a lower bound, not agreement)`,
            detail,
          }
        : {
            tone: "unknown",
            headline: `Scan source: drift unknown (0 behind ${ref} is a lower bound, not agreement)`,
            detail,
          };
    }
    return {
      tone: "warn",
      headline: `Scan source: at least ${behind} behind ${ref}${ahead > 0 ? `, up to ${ahead} ahead` : ""}`,
      detail,
    };
  }

  const ageSentence = age === null ? "" : ` ${age}.`;
  if (behind === 0 && ahead === 0) {
    return {
      tone: "ok",
      headline: `Scan source: in step with ${ref}`,
      detail: ageSentence === "" ? null : ageSentence.trim(),
    };
  }
  const consequences: string[] = [];
  if (behind > 0) {
    consequences.push(
      `plans on ${ref} that are not in this tree are missing from what this machine feeds the corpus`,
    );
  }
  if (ahead > 0) {
    consequences.push(`it publishes ${ahead} commit(s) of plan content that ${ref} does not carry`);
  }
  return {
    tone: "warn",
    headline:
      behind > 0
        ? `Scan source: ${behind} behind ${ref}${ahead > 0 ? `, ${ahead} ahead` : ""}`
        : `Scan source: ${ahead} ahead of ${ref}`,
    detail: `The adapter publishes this working tree, not ${ref}: ${consequences.join("; and ")}.${ageSentence}`,
  };
}
