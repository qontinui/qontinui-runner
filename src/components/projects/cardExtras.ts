/**
 * Card extras (plan §5.1) — pinning order and the weekly spend line.
 *
 * Pure, so it is unit-tested; the JSX that uses it is not renderable under the
 * runner's `environment: "node"` vitest config.
 */

import type { SavedProject } from "@/hooks/useSavedProjects";

/**
 * Grid order: pinned first, then most-recently-worked-on, then by name.
 *
 * Recency before name matters more than it looks — the grid is the landing
 * page, so its top-left card is what the runner opens onto, and "what I touched
 * last" is a far better default than "whatever starts with A".
 *
 * Returns a new array; the caller's list is not mutated (React state).
 */
export function sortProjects(projects: SavedProject[]): SavedProject[] {
  return [...projects].sort((a, b) => {
    const pinDelta = Number(b.pinned ?? false) - Number(a.pinned ?? false);
    if (pinDelta !== 0) return pinDelta;

    // A project never opened sorts after every project that has been —
    // `undefined` is "no recency", not "recency zero".
    const aOpened = a.lastOpenedMs ?? -1;
    const bOpened = b.lastOpenedMs ?? -1;
    if (aOpened !== bOpened) return bOpened - aOpened;

    return a.name.localeCompare(b.name);
  });
}

/**
 * "$1.20 this week", or `null` when there is nothing honest to say.
 *
 * `undefined` means NOT MEASURED and renders nothing — distinct from `0`, which
 * means measured and free, and is worth showing: "$0.00 this week" is a real
 * answer to "is this costing me money?" and silence is not.
 */
export function formatWeeklySpend(spend7dUsd: number | undefined): string | null {
  if (spend7dUsd === undefined || !Number.isFinite(spend7dUsd) || spend7dUsd < 0) return null;
  // Sub-cent spend rounds to $0.00, which reads as "free" — true enough at this
  // resolution, and better than inventing a third decimal nobody asked for.
  return `$${spend7dUsd.toFixed(2)} this week`;
}
