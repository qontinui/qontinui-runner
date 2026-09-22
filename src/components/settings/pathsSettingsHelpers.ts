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
 * ## The SAVE is a patch, and absent means "do not touch"
 *
 * That rule governs what `get_path_settings` REPORTS. What a save SENDS is a
 * different type — {@link PathSettingsPatch} — with one rule of its own:
 * **absent means "leave the stored value alone"**, an explicit value sets, and
 * `null` (scalars) or `{}` (maps) clears (plan
 * `2026-09-22-plans-dir-is-a-single-path-so-a-multi-bound-device-cannot-author-per-tenant`
 * P3 — the server-side merge that closes the erasure hazard a caller unaware of
 * a field used to open by omitting it).
 *
 * Two consequences this file exists to get right, both of which have already
 * been got wrong once:
 *
 * - **A shown-but-emptied box sends `null`, not nothing.** Under the merge,
 *   omission is a no-op; this helper used to `delete` the key to mean "cleared",
 *   which now means the opposite.
 * - **The payload is built from an EMPTY object**, never by spreading the loaded
 *   struct. A key present only because it was spread asserts a value the panel
 *   never edited, so the save writes the panel's mount-time snapshot back over
 *   whatever a peer changed meanwhile — a last-writer-wins on every field the
 *   panel does not display, invisible to any single-writer test.
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
  /**
   * Where a repo that is NOT under the workspace root is checked out on this
   * device, keyed by coord slug (`owner/name`). Absent when empty — the Rust
   * side skips serializing an empty map. Edited as text through
   * {@link formatRepoCheckouts} / {@link parseRepoCheckouts}.
   */
  repo_checkouts?: Record<string, string>;
  /**
   * Per-tenant override of {@link PathSettings.plans_dir}, keyed by tenant UUID
   * string. Absent from the wire when empty (the Rust side skips serializing an
   * empty map), and the scalar beside it REMAINS the device-wide default:
   * resolution is `by_tenant[tenant]`, then the scalar, then unset.
   *
   * A key the device is not currently bound to is preserved, never dropped (the
   * plan's D2) — the operator may be re-pairing, and losing the path silently
   * would be a config reset dressed as a cleanup.
   */
  plans_dir_by_tenant?: Record<string, string>;
  /** Per-tenant override of {@link PathSettings.plans_archive_dir}. See {@link PathSettings.plans_dir_by_tenant}. */
  plans_archive_dir_by_tenant?: Record<string, string>;
  /** Per-tenant override of {@link PathSettings.prompts_dir}. See {@link PathSettings.plans_dir_by_tenant}. */
  prompts_dir_by_tenant?: Record<string, string>;
  strict_mode: boolean;
}

/**
 * Wire shape of `commands::path_settings::PathSettingsPatch` — what a SAVE
 * sends, which is NOT a `PathSettings`.
 *
 * **One rule for every field: absent means "leave the stored value alone".** An
 * explicit value sets; `null` (scalars) or `{}` (maps) clears. So a payload
 * states only what the caller actually edited, and silence changes nothing —
 * which is what makes the door safe for a caller that knows about some fields
 * and not others, the shape every non-UI caller has.
 *
 * Every field is optional for that reason, and `buildPathSettingsPayload`
 * assembles one from an EMPTY object rather than from the loaded struct: a key
 * present only because it was spread is an assertion nobody made, and the save
 * would write the panel's mount-time snapshot over a peer's change.
 */
export interface PathSettingsPatch {
  dev_logs_dir?: string | null;
  plans_dir?: string | null;
  plans_archive_dir?: string | null;
  prompts_dir?: string | null;
  workspace_root?: string | null;
  repo_checkouts?: Record<string, string>;
  plans_dir_by_tenant?: Record<string, string>;
  plans_archive_dir_by_tenant?: Record<string, string>;
  prompts_dir_by_tenant?: Record<string, string>;
  strict_mode?: boolean;
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
  /**
   * The same three plan/prompt directories resolved FOR ONE NAMED TENANT —
   * present only when the caller passed a `tenantId` to `get_path_settings`
   * (plan P3). Absent means "nobody named a tenant", which is the panel's own
   * case: it edits N tenants at once, so it asks for the device view and states
   * the fallback rule rather than claiming a resolution the runner did not make.
   */
  resolved_for_tenant?: ResolvedForTenant;
}

/**
 * `view.resolved.resolved_for_tenant` — what the three per-tenant directories
 * resolve to for the tenant the caller named. Each is `null` when that tenant
 * has neither an entry nor a device-wide default, i.e. genuinely unset.
 */
export interface ResolvedForTenant {
  tenant_id: string;
  plans_dir: string | null;
  plans_archive_dir: string | null;
  prompts_dir: string | null;
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
 * The directories that can be overridden PER TENANT, in the order they are
 * rendered. Only the plan/prompt corpus dirs are keyed by tenant: `workspace_root`,
 * `dev_logs_dir`, `repo_checkouts` and `strict_mode` are device-wide by design
 * (they describe the machine, not a tenant's authoring surface).
 *
 * `plans_archive_dir` has no device-wide box in this panel — it round-trips
 * untouched — but it does get per-tenant rows, because the same operator
 * requirement covers it (plan P5).
 */
export const TENANT_PATH_FIELDS = ["plans_dir", "plans_archive_dir", "prompts_dir"] as const;

export type TenantPathField = (typeof TENANT_PATH_FIELDS)[number];

/** Which `*_by_tenant` map holds each per-tenant directory. */
export const TENANT_MAP_FIELD = {
  plans_dir: "plans_dir_by_tenant",
  plans_archive_dir: "plans_archive_dir_by_tenant",
  prompts_dir: "prompts_dir_by_tenant",
} as const satisfies Record<TenantPathField, keyof PathSettings>;

/**
 * The per-tenant input-box values: one map of `tenantId -> box text` per
 * directory. `""` (or a missing key) means "no override — use the device-wide
 * value", which is what an absent map entry means on the wire.
 */
export type TenantPathDrafts = Record<TenantPathField, Record<string, string>>;

/** The saved map for one per-tenant directory; `{}` when none is stored. */
export function tenantPathMap(saved: PathSettings, field: TenantPathField): Record<string, string> {
  // A COPY, not the live reference. Every caller today is read-only, so this is
  // not a bug being fixed — it is a footgun being removed: `tenantDraftsFrom`
  // spreads the result into editable drafts, and a future caller that mutated
  // what it got back would be editing `view.configured` in place. The panel
  // would then compare its drafts against an already-changed "saved" and read
  // clean, which is the worst shape a dirty check can fail in.
  return { ...(saved[TENANT_MAP_FIELD[field]] ?? {}) };
}

/**
 * The tenant ids to render rows for, for one directory: every tenant the device
 * is bound to (in the order the context reports them), then every id present in
 * the SAVED map that is not among them, sorted.
 *
 * The second group is the plan's D2: an entry whose tenant the device is no
 * longer bound to still renders — labelled as not currently bound — because a
 * row nobody can see is a path the operator loses without being told.
 */
export function tenantIdsForField(
  saved: PathSettings,
  candidates: readonly string[],
  field: TenantPathField,
): string[] {
  const bound: string[] = [];
  for (const raw of candidates) {
    const id = raw.trim();
    if (id.length > 0 && !bound.includes(id)) bound.push(id);
  }
  const extra = Object.keys(tenantPathMap(saved, field))
    .filter((id) => !bound.includes(id))
    .sort((a, b) => a.localeCompare(b));
  return [...bound, ...extra];
}

/**
 * The per-tenant boxes for a loaded (or freshly saved) struct.
 *
 * Seeded from the SAVED maps only — a tenant the device is bound to but has no
 * entry for simply has no key here, which the panel renders as an empty box.
 * That keeps this pure and independent of when `TenantContext` finishes loading
 * its candidate list, which happens after this panel's first render.
 */
export function tenantDraftsFrom(saved: PathSettings): TenantPathDrafts {
  return {
    plans_dir: { ...tenantPathMap(saved, "plans_dir") },
    plans_archive_dir: { ...tenantPathMap(saved, "plans_archive_dir") },
    prompts_dir: { ...tenantPathMap(saved, "prompts_dir") },
  };
}

/**
 * One per-tenant box map as it goes on the wire: keys and values trimmed, every
 * blank value dropped (a blank box is "no override", never a directory named
 * `""`), keys sorted so a comparison of two normalisations is stable.
 */
export function normalizeTenantPathMap(
  draft: Record<string, string> | undefined,
): Record<string, string> {
  const out: Record<string, string> = {};
  if (!draft) return out;
  for (const key of Object.keys(draft).sort((a, b) => a.localeCompare(b))) {
    const tenantId = key.trim();
    const value = normalizePathInput(draft[key]);
    if (tenantId.length === 0 || value === undefined) continue;
    out[tenantId] = value;
  }
  return out;
}

/** `true` when the per-tenant boxes would persist maps other than the saved ones. */
export function tenantDraftsAreDirty(saved: PathSettings, drafts: TenantPathDrafts): boolean {
  return TENANT_PATH_FIELDS.some(
    (field) =>
      JSON.stringify(normalizeTenantPathMap(drafts[field])) !==
      JSON.stringify(normalizeTenantPathMap(tenantPathMap(saved, field))),
  );
}

/** How many per-tenant overrides are stored, across all three directories. */
export function storedTenantOverrideCount(saved: PathSettings): number {
  return TENANT_PATH_FIELDS.reduce(
    (total, field) => total + Object.keys(tenantPathMap(saved, field)).length,
    0,
  );
}

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
 * Built from an EMPTY object, stating only what this panel actually edited — see
 * the comment in the body for why the old `{ ...saved }` spread was the bug. A
 * field the panel does not edit (`plans_archive_dir`, `strict_mode`) is therefore
 * OMITTED, and the runner's patch merge is what leaves it untouched; nothing
 * round-trips through here. A shown-but-emptied box sends an explicit `null`,
 * which is the wire form of "clear".
 *
 * `saved` is not read by this function and is retained only to avoid churning
 * the call site and its tests; it carries no meaning. Do not reintroduce a
 * spread of it.
 *
 * The MAP fields go the other way, for the reason the module doc gives: the save
 * merges them, so `{}` is how an emptied box says "cleared" and an absent key
 * says "leave what is stored alone". A caller that passes `repoCheckouts` or
 * `tenantDrafts` is therefore asserting it edited that map; one that omits them
 * is asserting it did not show it.
 */
export function buildPathSettingsPayload(
  saved: PathSettings,
  drafts: PathDrafts,
  repoCheckouts?: Record<string, string>,
  tenantDrafts?: TenantPathDrafts,
): PathSettingsPatch {
  // ── Built from NOTHING, not from the loaded struct ───────────────────────
  //
  // This used to be `{ ...saved }` plus overwrites, and the spread was the bug.
  // Under the patch's one rule — ABSENT means "leave the stored value alone" —
  // a key that is present because it was spread is an ASSERTION the panel never
  // made: it carries the panel's mount-time snapshot, and the save writes it
  // back over whatever a peer changed in between. That is a last-writer-wins on
  // every field the panel does not display (`plans_archive_dir`, `strict_mode`,
  // and every map when the rows are hidden), and it is invisible with a single
  // writer, which is why it survived a review round.
  //
  // So the payload is assembled from an empty object and states only what this
  // panel actually edited. `null` is how a shown-but-emptied box says "clear";
  // omission says "I am not talking about this field".
  const next: PathSettingsPatch = {};
  for (const field of PATH_FIELDS) {
    // `normalizePathInput` gives `undefined` for a blank box, and a blank box
    // IS a clear — so it becomes an explicit `null`, never an omission.
    next[field] = normalizePathInput(drafts[field]) ?? null;
  }
  if (repoCheckouts !== undefined) {
    // `{}` is a DELIBERATE CLEAR: under the merge, omitting the key would leave
    // the stored map in place.
    next.repo_checkouts = { ...repoCheckouts };
  }
  if (tenantDrafts !== undefined) {
    // Same rule: a cleared per-tenant row must reach the runner as `{}` (or as
    // a map without that key) to actually be cleared.
    next.plans_dir_by_tenant = normalizeTenantPathMap(tenantDrafts.plans_dir);
    next.plans_archive_dir_by_tenant = normalizeTenantPathMap(tenantDrafts.plans_archive_dir);
    next.prompts_dir_by_tenant = normalizeTenantPathMap(tenantDrafts.prompts_dir);
  }
  // `plans_archive_dir` and `strict_mode` are deliberately NEVER sent: this
  // panel does not show them, so it has nothing to say about them, and saying
  // nothing is now how that is expressed.
  return next;
}

// ── Repo checkouts (repos outside the workspace root) ──────────────────────

/**
 * The saved map as editable text: one `owner/name = path` line per entry,
 * sorted by slug so a load-then-format is stable.
 */
export function formatRepoCheckouts(map: Record<string, string> | undefined): string {
  if (!map) return "";
  return Object.keys(map)
    .sort((a, b) => a.localeCompare(b))
    .map((slug) => `${slug} = ${map[slug]}`)
    .join("\n");
}

export interface ParsedRepoCheckouts {
  entries: Record<string, string>;
  /** One message per rejected line, naming the line number. Empty = valid. */
  errors: string[];
}

/** `owner/name`: two non-empty segments, no whitespace. */
const REPO_SLUG = /^[^\s/]+\/[^\s/]+$/;

/**
 * Parse the text box. Blank lines and `#` comments are ignored; every other
 * line must be `owner/name = path`. A malformed line or a slug repeated
 * (case-insensitively, as the runner matches it) is an error rather than a
 * silently dropped mapping — the panel refuses to save until it is fixed.
 */
export function parseRepoCheckouts(text: string): ParsedRepoCheckouts {
  const entries: Record<string, string> = {};
  const seen = new Set<string>();
  const errors: string[] = [];
  text.split(/\r?\n/).forEach((raw, index) => {
    const line = raw.trim();
    if (line.length === 0 || line.startsWith("#")) return;
    const lineNo = index + 1;
    const eq = line.indexOf("=");
    if (eq < 0) {
      errors.push(`Line ${lineNo}: expected "owner/name = path".`);
      return;
    }
    const slug = line.slice(0, eq).trim();
    const path = line.slice(eq + 1).trim();
    if (!REPO_SLUG.test(slug)) {
      errors.push(`Line ${lineNo}: "${slug}" is not an owner/name repo slug.`);
      return;
    }
    if (slug.toLowerCase().startsWith("qontinui/")) {
      errors.push(
        `Line ${lineNo}: qontinui repositories always use the workspace root; remove ${slug}.`,
      );
      return;
    }
    if (path.length === 0) {
      errors.push(`Line ${lineNo}: no path for ${slug}.`);
      return;
    }
    // Mirrors the runner's `has_root` test: a relative path would resolve
    // against the runner's own working directory, so it is never used.
    if (!/^(\/|\\|[A-Za-z]:[\\/])/.test(path)) {
      errors.push(`Line ${lineNo}: ${path} is not an absolute path.`);
      return;
    }
    const key = slug.toLowerCase();
    if (seen.has(key)) {
      errors.push(`Line ${lineNo}: ${slug} is listed more than once.`);
      return;
    }
    seen.add(key);
    entries[slug] = path;
  });
  return { entries, errors };
}

/** `true` when the text box would persist a map other than the saved one. */
export function repoCheckoutsDirty(saved: PathSettings, text: string): boolean {
  const { entries, errors } = parseRepoCheckouts(text);
  if (errors.length > 0) return true;
  return formatRepoCheckouts(entries) !== formatRepoCheckouts(saved.repo_checkouts);
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
  if (view.state === "not_scanning") {
    return notYet(
      "Plan scanning was just turned on; the adapter picks the directory up within one scan interval. Reopen this panel after the next interval.",
    );
  }
  if (view.plans_dir !== null && resolvedDiffers(view.plans_dir, inEffect.plans_dir)) {
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
