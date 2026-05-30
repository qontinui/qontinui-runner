/**
 * Barrel for the result-card surface.
 */

export {
  ResultCardProvider,
  useResultCard,
  ResultCardContext,
  resultCardReducer,
} from "./ResultCardContext";
export type {
  ResultCardSpec,
  ResultCardSection,
  ResultCardContextValue,
  ResultCardAction,
} from "./ResultCardContext";
export { ResultCardMount } from "./ResultCardMount";
export { buildMetricsCardSpec, buildHistoryCardSpec } from "./builders";
export type { BuildMetricsCardInput, HistoryCardEntry } from "./builders";
