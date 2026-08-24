import type { ErrorSeverity, ErrorStatus } from "../../types/errorMonitor";

/**
 * The single source of truth for what the Error Monitor's filter panel can
 * SHOW, plus the badge count that summarizes it.
 *
 * WHY this exists (manual-test-loop iter 13, item A): `ErrorMonitorTab` held two
 * independent status lists. `selectedStatuses` defaulted to
 * `["new", "acknowledged", "in_progress", "promoted"]` while `statusOptions` —
 * the array the pills are rendered from — listed six statuses and **omitted
 * `promoted`**. So on a fresh tab with no interaction the badge read "Filters 4"
 * next to a panel where only 3 pills could light. Worse than the arithmetic: a
 * status was actively filtering the operator's list with no pill to reveal it
 * and no pill to switch it off.
 *
 * The fix is to render `promoted` as a pill rather than to teach the badge to
 * ignore it. Deriving the badge from pill-representable statuses alone would
 * have made the number honest and left the invisible-unremovable filter in
 * place — the count would agree, and the operator still could not see or clear
 * the thing hiding rows from them. `promoted` is a real `ErrorStatus`, it is
 * already rendered by `StatusBadge`, and the backend query accepts it; the only
 * thing missing was its control.
 *
 * And the option lists are now DERIVED from an exhaustive display-order map
 * rather than hand-listed a second time. A `satisfies Record<ErrorStatus, …>`
 * map cannot omit a union member, and the array cannot omit a key of the map —
 * so a status added to `ErrorStatus` without a pill is a compile error, not
 * another silent disagreement between a default and a control.
 */

/**
 * Display order of the status pills. Exhaustive by construction: the compiler
 * requires exactly one entry per `ErrorStatus` member.
 */
const STATUS_PILL_ORDER = {
  new: 0,
  acknowledged: 1,
  in_progress: 2,
  resolved: 3,
  ignored: 4,
  recurring: 5,
  promoted: 6,
} satisfies Record<ErrorStatus, number>;

/** Display order of the severity pills. Exhaustive, same construction. */
const SEVERITY_PILL_ORDER = {
  critical: 0,
  error: 1,
  warning: 2,
  info: 3,
  debug: 4,
} satisfies Record<ErrorSeverity, number>;

function orderedKeys<K extends string>(order: Record<K, number>): readonly K[] {
  return (Object.keys(order) as K[]).sort((a, b) => order[a] - order[b]);
}

/** Every status the filter panel renders a pill for, in display order. */
export const ERROR_STATUS_FILTER_OPTIONS: readonly ErrorStatus[] = orderedKeys(STATUS_PILL_ORDER);

/** Every severity the filter panel renders a pill for, in display order. */
export const ERROR_SEVERITY_FILTER_OPTIONS: readonly ErrorSeverity[] =
  orderedKeys(SEVERITY_PILL_ORDER);

/** The tab's initial status selection. Must be representable as pills. */
export const DEFAULT_SELECTED_ERROR_STATUSES: readonly ErrorStatus[] = [
  "new",
  "acknowledged",
  "in_progress",
  "promoted",
];

/**
 * The number shown in the "Filters" badge.
 *
 * Defined as "selections the panel can actually show as a lit pill", which is
 * the property the operator verifies by looking: badge count == lit pill count.
 * With the options lists exhaustive above, that is every selection — but stating
 * it this way keeps the badge and the pills answering the SAME question instead
 * of two lengths that happened to agree.
 */
export function filterBadgeCount(
  selectedSeverities: readonly ErrorSeverity[],
  selectedStatuses: readonly ErrorStatus[],
): number {
  const severities = selectedSeverities.filter((s) =>
    ERROR_SEVERITY_FILTER_OPTIONS.includes(s),
  ).length;
  const statuses = selectedStatuses.filter((s) => ERROR_STATUS_FILTER_OPTIONS.includes(s)).length;
  return severities + statuses;
}
