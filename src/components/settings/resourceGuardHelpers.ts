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
export const MAX_CONCURRENT_BUILDS_MIN = 1;
export const MAX_CONCURRENT_BUILDS_MAX = 16;

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
