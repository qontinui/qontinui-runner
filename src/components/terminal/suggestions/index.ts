/**
 * Public surface of the suggestion-chip engine. See
 * `plans/2026-05-28-terminal-page-redesign-plan.md` §Phase 4 for
 * design context and `./rules.ts` for the rule list.
 */

export type { ChipCandidate, SuggestionContext } from "./rules";
export { reconcileChips } from "./rules";

export { SuggestionsProvider, useSuggestionForZone } from "./useSuggestions";

export { SuggestionChip } from "./SuggestionChip";
