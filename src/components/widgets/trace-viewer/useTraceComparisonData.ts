import { useMemo } from "react";
import { useTraceViewerData } from "./useTraceViewerData";
import { alignSpans, computeComparisonSummary } from "./trace-utils";
import type { TraceSpan, SpanMatch, ComparisonSummary } from "./types";

export interface TraceComparisonData {
  spansA: TraceSpan[];
  spansB: TraceSpan[];
  matches: SpanMatch[];
  summary: ComparisonSummary;
  isLoading: boolean;
  error: string | null;
}

/** Fetch and compare execution spans from two runs. */
export function useTraceComparisonData(
  runAId: string | null,
  runBId: string | null,
): TraceComparisonData {
  const queryA = useTraceViewerData(runAId);
  const queryB = useTraceViewerData(runBId);

  const spansA = useMemo(() => queryA.data ?? [], [queryA.data]);
  const spansB = useMemo(() => queryB.data ?? [], [queryB.data]);

  const matches = useMemo(() => alignSpans(spansA, spansB), [spansA, spansB]);
  const summary = useMemo(
    () => computeComparisonSummary(spansA, spansB, matches),
    [spansA, spansB, matches],
  );

  return {
    spansA,
    spansB,
    matches,
    summary,
    isLoading: queryA.isLoading || queryB.isLoading,
    error: queryA.error ? String(queryA.error) : queryB.error ? String(queryB.error) : null,
  };
}
