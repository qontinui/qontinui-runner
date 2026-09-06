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
