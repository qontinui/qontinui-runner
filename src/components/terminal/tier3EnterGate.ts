/**
 * tier3EnterGate — when may Enter on a line that matches nothing report
 * "No command matches"?
 *
 * Only once every tier has had its say. Tier 3 (the AI interpreter) runs
 * debounced and async, so for a line it is eligible for there is a window —
 * the debounce, then the subprocess call — in which "no match" is a verdict
 * from two tiers out of three. Painting the error there puts it on screen a
 * moment before the AI match lands in the dropdown under it; Enter stays
 * inert instead, which is what it always did.
 *
 * A leaf of `CommandBar.tsx` (which cannot be imported under the runner's
 * node test environment) so the rule is unit-testable, and one predicate for
 * the debounced fire and for the Enter branch so the two cannot disagree
 * about which lines are still awaiting an answer.
 */

import { matchPattern, resolve } from "./commands";

/** Minimum normalized-query length that's worth a Tier-3 subprocess call. */
export const TIER3_MIN_CHARS = 3;

/**
 * Whether Tier 3 would be asked about `query` at all: long enough, and not
 * already resolved exactly by Tier 1 or matched by a Tier-2 pattern.
 */
export function tier3Eligible(query: string, recents: Parameters<typeof resolve>[1]): boolean {
  if (query.trim().length < TIER3_MIN_CHARS) return false;
  if (resolve(query, recents).some((m) => m.exact)) return false;
  return !matchPattern(query);
}

export interface NoMatchEnterState {
  query: string;
  /** Tier 3's subprocess call is in flight. */
  interpreting: boolean;
  /** `tier3Eligible(query, recents)`. */
  eligible: boolean;
  /**
   * The query Tier 3 last answered FOR IN THE CURRENT RUN, or `null`. The
   * component resets it whenever the query changes, so a line recalled from
   * history or edited away and back is NOT remembered as settled.
   */
  settledFor: string | null;
}

/** `true` while Tier 3 has yet to answer for this line — Enter stays inert. */
export function noMatchEnterIsInert(s: NoMatchEnterState): boolean {
  return s.interpreting || (s.eligible && s.settledFor !== s.query);
}
