/**
 * Pure helpers for `LockYieldPolicySettings`.
 *
 * Lives outside the JSX module so vitest can import these without
 * dragging in the design-system / SectionHeader module graph (which
 * pulls in `qontinui-workflow-utils` and fails to resolve under
 * `environment: "node"`). Same pattern used by sibling
 * `HoldingLockBanner.tsx` — the banner's pure helpers ship alongside
 * the JSX, but only because their import surface is already
 * test-friendly. For settings panels we factor the helpers out.
 */

// Bounds the panel enforces on the number inputs. The runner will
// accept any u64, but the UI keeps the user away from values that
// aren't useful (e.g. 0s thresholds that would yield every transient
// contention).
export const IDLE_THRESHOLD_MIN = 10;
export const IDLE_THRESHOLD_MAX = 3600;
export const MIN_WAIT_MIN = 5;
export const MIN_WAIT_MAX = 600;

/**
 * Clamp `n` to `[min, max]`. Non-finite / non-numeric values resolve
 * to `fallback`. Used to gate number inputs against typo'd values
 * (e.g. user types "abc" → falls back to default).
 */
export function clampNumber(
  n: number,
  min: number,
  max: number,
  fallback: number,
): number {
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, Math.floor(n)));
}
