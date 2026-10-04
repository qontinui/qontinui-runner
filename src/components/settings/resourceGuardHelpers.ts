/**
 * Pure helpers for `ResourceGuardSettings`.
 *
 * Lives outside the JSX module for the reason `lockYieldPolicyHelpers.ts`
 * documents: vitest can import these under `environment: "node"` without
 * dragging in the design-system / SectionHeader module graph.
 *
 * ## Bytes on the wire, GiB in the input box
 *
 * `settings::SessionGuardSettings` stores the two floors in **bytes** — the
 * critical default is 1.5 GiB, which has no integer-GiB spelling, and coord's
 * fleet-policy floors are `*_bytes` columns, so bytes is the only unit that can
 * carry the value end to end. Nobody wants to type `1610612736` into a settings
 * field, so the panel edits GiB and converts at the boundary. The conversion
 * lives here, in one place, tested — not inline in two `onChange` handlers where
 * the two directions can drift apart.
 */

/** One gibibyte in bytes. The unit every floor in this panel is expressed in. */
export const GIB = 1024 * 1024 * 1024;

// Bounds the panel enforces on the GiB inputs. The runner accepts any u64; the
// UI keeps the operator away from values that are not useful. A 0 floor would
// disable the guard it names rather than relaxing it — the same reason
// `ci_node`'s remote configuration door refuses `min_free_disk_gb: 0` — so the
// minimum is a real, if small, quantity.
export const SESSION_FLOOR_MIN_GIB = 0.25;
export const SESSION_FLOOR_MAX_GIB = 128;
export const DISK_FLOOR_MIN_GIB = 1;
export const DISK_FLOOR_MAX_GIB = 2000;
// Concurrent CI builds. 64 is the Rust validator's bound
// (`src-tauri/src/ci_node/settings_directive.rs` `MAX_CONCURRENT_BUILDS_MAX`),
// and qontinui-web's CI-node panel and backend schema both allow 1-64 too. A UI
// that refuses what the validator accepts is a second, undocumented policy —
// which is what the old hardware-independent 16 was. The per-host recommendation
// is the `host_suggestion` the runner returns, shown as a warning, never a
// clamp. `resourceGuardHelpers.test.ts` reads the Rust constant from source so
// the two cannot drift apart again.
export const MAX_CONCURRENT_BUILDS_MIN = 1;
export const MAX_CONCURRENT_BUILDS_MAX = 64;

/**
 * The runner's host-derived capacity suggestion, returned beside
 * `settings::CiNodeSettings` by `get_ci_node_settings` (Rust:
 * `commands::resource_guard_settings::host_suggestion_json`, over
 * `qontinui_ci_exec::host_sizing::suggestion_with_limit`). `mem_gib` and
 * `limiting_term` are null when the host's memory could not be read.
 */
export interface CiNodeHostSuggestion {
  suggested: number;
  cpus: number;
  mem_gib: number | null;
  limiting_term: "cores" | "memory" | null;
}

/**
 * Commit the concurrency input's text draft (called on blur, never per
 * keystroke). Anything numeric is an explicit override clamped to the
 * validator's range; empty or non-numeric text REVERTS to `previous` — it never
 * silently switches to "use the host suggestion". The "Use suggested" button is
 * the only way to write `null`.
 */
export function parseConcurrencyInput(raw: string, previous: number | null): number | null {
  const n = parseInt(raw, 10);
  if (!Number.isFinite(n)) return previous;
  return clampInt(
    n,
    MAX_CONCURRENT_BUILDS_MIN,
    MAX_CONCURRENT_BUILDS_MAX,
    MAX_CONCURRENT_BUILDS_MIN,
  );
}

/**
 * The inline warning for an explicit value above the host suggestion, naming
 * the term that bounds it — or `null` when there is nothing to warn about
 * (unset, at or below the suggestion, or no suggestion loaded). A
 * recommendation, never a clamp: the value still saves.
 */
export function concurrencyAboveSuggestionWarning(
  value: number | null,
  suggestion: CiNodeHostSuggestion | null,
): string | null {
  if (value === null || suggestion === null || value <= suggestion.suggested) return null;
  const term =
    suggestion.limiting_term === "memory"
      ? `memory (${suggestion.mem_gib ?? "?"} GB at 12 GB per build)`
      : suggestion.limiting_term === "cores"
        ? `cores (${suggestion.cpus} at 4 per build)`
        : "an unreadable memory probe";
  return `${value} is above the suggested ${suggestion.suggested} for this host, which is bounded by ${term}. Each build also gets a smaller share of the host.`;
}

// Typing bounds on the two thread-ceiling inputs.
//
// The top is the ENFORCED bound, not just a typing one: it mirrors
// `resource_guard::THREAD_CEILING_ABS_MAX` (2048, four times tokio's 512-slot
// blocking pool, past which a ceiling could not fire before the pool it
// protects was already exhausted), which the save command refuses above.
// `resourceGuardHelpers.test.ts` reads the Rust constant from source so the two
// sides cannot drift apart.
//
// 50 at the bottom is a typing bound only: well under a measured idle runner
// (150-151 threads), so an operator can watch the runner's lower clamp —
// `THREAD_CEILING_MIN + shift`, served in `thread_ceilings.clampMin` — raise it.
export const THREAD_CEILING_INPUT_MIN = 50;
export const THREAD_CEILING_ABS_MAX = 2048;

/**
 * The runner's own hardcoded floors and ceiling — the terms that decide what a
 * configured value actually ENFORCES.
 *
 * Mirrors `settings::SessionGuardSettings::default()` (3 GiB warn / 1.5 GiB
 * critical) and `resource_guard::SESSION_FLOOR_MAX_BYTES` (12 GiB). They are
 * duplicated here rather than fetched because the panel needs them to render a
 * number BEFORE any invoke resolves, and because a settings panel that could
 * only explain itself when the backend answered would be silent in exactly the
 * case the operator is trying to debug. `effectiveSessionFloorsGib` is the one
 * place they are used, and its tests pin the arithmetic against the Rust rules.
 */
export const SESSION_FLOOR_DEFAULT_WARN_GIB = 3;
export const SESSION_FLOOR_DEFAULT_CRITICAL_GIB = 1.5;
export const SESSION_FLOOR_CAP_GIB = 12;

/** Bytes → GiB, rounded to 2 decimals so 1.5 GiB round-trips as `1.5`. */
export function bytesToGib(bytes: number): number {
  if (!Number.isFinite(bytes) || bytes <= 0) return 0;
  return Math.round((bytes / GIB) * 100) / 100;
}

/** GiB → bytes, rounded to a whole byte (the Rust side is a `u64`). */
export function gibToBytes(gib: number): number {
  if (!Number.isFinite(gib) || gib <= 0) return 0;
  return Math.round(gib * GIB);
}

/**
 * Clamp `n` to `[min, max]`, resolving non-numeric input to `fallback`.
 *
 * Unlike `lockYieldPolicyHelpers.clampNumber` this does NOT floor to an
 * integer: the floors here are fractional GiB (the shipped critical default is
 * 1.5), and flooring would silently rewrite the default the moment the panel
 * loaded it.
 */
export function clampGib(n: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, n));
}

/** Clamp an integer field (build slots, disk GiB) to `[min, max]`. */
export function clampInt(n: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, Math.floor(n)));
}

/**
 * `true` when the two session floors are transposed.
 *
 * Mirrors `commands::resource_guard_settings::session_floors_are_inverted`
 * exactly, including that equal floors are legal. The Rust side is the
 * authority — it refuses the write — but a panel that lets the operator hit
 * Save and then reports a backend error for something it could see in the input
 * box is a worse experience than one that says so inline.
 */
export function sessionFloorsAreInverted(warnGib: number, criticalGib: number): boolean {
  return criticalGib > warnGib;
}

/**
 * What the runner will ACTUALLY enforce for a pair of configured floors.
 *
 * Mirrors `resource_guard::merge_floors` minus its fleet term, in order:
 *
 *   1. `max(configured, hardcoded default)` — the local value can only ever
 *      TIGHTEN, so the whole 0.25–3 GiB warn range and the 0.25–1.5 GiB critical
 *      range are inert. That is the discrepancy this helper exists to surface:
 *      the input accepts them, the runner discards them, and until now nothing
 *      on the page said so.
 *   2. `min(…, SESSION_FLOOR_CAP_GIB)` — an unreachable floor would refuse every
 *      unattended spawn on this machine forever, so the enforcing side caps it.
 *   3. `critical = min(critical, warn)` — the warn verdict is the lighter one
 *      and must fire first, so the ladder is coerced rather than inverted.
 *
 * The FLEET term is deliberately absent: a tenant-wide floor can only raise
 * these further, the panel has no cache of it, and inventing a zero for an
 * unknown would be the one lie the whole guard is arranged to avoid. The panel
 * says so in words beside the numbers.
 */
export function effectiveSessionFloorsGib(
  warnGib: number,
  criticalGib: number,
): { warnGib: number; criticalGib: number } {
  const warn = clampGib(
    Math.max(warnGib, SESSION_FLOOR_DEFAULT_WARN_GIB),
    SESSION_FLOOR_DEFAULT_WARN_GIB,
    SESSION_FLOOR_CAP_GIB,
    SESSION_FLOOR_DEFAULT_WARN_GIB,
  );
  const critical = clampGib(
    Math.max(criticalGib, SESSION_FLOOR_DEFAULT_CRITICAL_GIB),
    SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
    SESSION_FLOOR_CAP_GIB,
    SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
  );
  return { warnGib: warn, criticalGib: Math.min(critical, warn) };
}

/**
 * `true` when the two thread ceilings are transposed.
 *
 * Mirrors `commands::resource_guard_settings::thread_ceilings_are_inverted`,
 * which is the MIRROR of the floors' predicate rather than a copy of it: on a
 * ceiling lane the lighter verdict is the LOWER number, so the transposition is
 * `critical < warn` — the opposite comparison to
 * {@link sessionFloorsAreInverted}. Equal ceilings are legal, exactly as equal
 * floors are.
 *
 * Takes the operator's OPTIONAL overrides (`null` = machine default): only a
 * pair the operator stated in full can be transposed. With one half on the
 * machine default its partner moves with the box, so the runner's fold coerces
 * that case instead of the writer refusing it.
 */
export function threadCeilingsAreInverted(
  warnThreads: number | null,
  criticalThreads: number | null,
): boolean {
  return warnThreads !== null && criticalThreads !== null && criticalThreads < warnThreads;
}

/**
 * Commit a thread-ceiling input's text draft (on blur, and on save). Anything
 * numeric is an explicit override clamped to the typing range; empty or
 * non-numeric text REVERTS to `previous` — it never silently switches to the
 * machine default. The "Use machine default" button is the only way to write
 * `null`. The same contract as {@link parseConcurrencyInput}.
 */
export function parseThreadCeilingInput(raw: string, previous: number | null): number | null {
  const n = parseInt(raw, 10);
  if (!Number.isFinite(n)) return previous;
  return clampInt(n, THREAD_CEILING_INPUT_MIN, THREAD_CEILING_ABS_MAX, THREAD_CEILING_INPUT_MIN);
}

/**
 * Which term decided one enforced thread ceiling — the runner's
 * `resource_guard::CeilingSource::wire_name`.
 */
export type ThreadCeilingSource =
  | "local"
  | "scaled"
  | "floor"
  | "fleet"
  | "clamp_min"
  | "clamp_max"
  | "ladder"
  | "census_misread";

interface ThreadCeilingPair {
  warn: number;
  critical: number;
}

/**
 * The thread ceilings the runner ENFORCES and how it reached them, served by
 * `get_session_guard_settings` as `thread_ceilings` (Rust:
 * `resource_guard::EffectiveThreadCeilings::to_json` — the same projection
 * `/health` serves as `threadCeilings`).
 *
 * The panel renders this instead of re-deriving the fold. It used to carry a
 * TypeScript copy (`effectiveThreadCeilings`), which had already drifted — it
 * never knew the machine shift — and once the default became a function of the
 * machine (cores, memory, the measured at-rest floor) no copy could be right.
 * Every UNKNOWN is `null`, never a zero.
 */
export interface ThreadCeilingsReport {
  enabled: boolean;
  warn: number;
  critical: number;
  provenance: { warn: ThreadCeilingSource; critical: ThreadCeilingSource };
  local: { warn: number | null; critical: number | null };
  fleet: { warn: number | null; critical: number | null };
  floor: ThreadCeilingPair;
  shift: number;
  clampMin: number;
  absMax: number;
  scaled:
    | (ThreadCeilingPair & {
        sessionArm: ThreadCeilingPair;
        poolArm: ThreadCeilingPair;
        sessionCapacity: ThreadCeilingPair;
        baselineUsed: number;
        perSessionThreadsUsed: number;
      })
    | null;
  scaledUnknown:
    | "cores_unknown"
    | "mem_total_unknown"
    | "session_threads_unknown"
    | "session_census_misread"
    | null;
  inputs: {
    cores: number | null;
    memTotalBytes: number | null;
    baseline: number | null;
    perSessionThreads: number | null;
    sessionThreadsNow: number | null;
    sessionCensusMisread: boolean;
  };
  ladderCoerced: boolean;
}

/**
 * One sentence saying where an enforced thread ceiling came from, for the line
 * under each input. Pure, so each arm's wording is pinned by a test; the
 * numbers in it are the runner's, never recomputed here.
 */
export function threadCeilingSourceText(
  which: "warn" | "critical",
  report: ThreadCeilingsReport,
): string {
  const source = report.provenance[which];
  switch (source) {
    case "local":
      return "your value — it replaces this machine's default, looser or tighter";
    case "scaled": {
      const s = report.scaled;
      if (!s) return "this machine's default";
      const arm = s[which] === s.sessionArm[which] ? "session capacity" : "blocking-pool headroom";
      return `this machine's default, sized from ${report.inputs.cores ?? "?"} cores and memory (bound by ${arm})`;
    }
    case "floor":
      return report.scaledUnknown
        ? `the built-in floor — the machine-sized default is unavailable (${report.scaledUnknown.replace(/_/g, " ")})`
        : "the built-in floor — this machine's sized default is below it";
    case "fleet":
      return "your tenant's fleet ceiling, which can only tighten";
    case "clamp_min":
      return `raised to ${report.clampMin}, the lowest ceiling this machine can still spawn under`;
    case "clamp_max":
      return `cut to the ${report.absMax}-thread bound`;
    case "ladder":
      return "raised to the warn ceiling — the lighter verdict has to fire first";
    case "census_misread":
      return "the built-in floor — the thread census disagreed with the live session count, so this machine's sized default is unknown right now";
  }
}

/**
 * Split a comma/newline-separated allowlist textarea into repo entries.
 *
 * Blank entries are dropped rather than persisted: `ci_node`'s contract is that
 * an EMPTY allowlist means nothing is runnable, so an accidental trailing comma
 * must not turn into an `""` entry that matches nothing but reads as a
 * configured repo.
 */
export function parseRepoAllowlist(raw: string): string[] {
  return raw
    .split(/[\n,]/)
    .map((r) => r.trim())
    .filter((r) => r.length > 0);
}
