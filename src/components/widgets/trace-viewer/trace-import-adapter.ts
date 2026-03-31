import type { TraceSpan, TraceTreeNode } from "./types";
import { buildTraceTree } from "./trace-utils";

/**
 * Generic adapter interface for importing traces from external sources.
 *
 * Inspired by Agent-Prism's SpanAdapter pattern but outputs qontinui-native
 * types directly, avoiding a double-translation layer.
 *
 * Each adapter implementation maps a specific external format (Langfuse,
 * Phoenix/OTLP, etc.) to qontinui's TraceSpan + TraceTreeNode types.
 */
export interface TraceImportAdapter<TRawTrace, TRawSpan = unknown> {
  /** Human-readable name for the adapter (e.g., "Langfuse", "OTLP JSON"). */
  readonly name: string;

  /** Parse raw trace data into individual raw spans. */
  extractRawSpans(rawTrace: TRawTrace): TRawSpan[];

  /** Convert a single raw span to a qontinui TraceSpan. */
  convertSpan(rawSpan: TRawSpan, index: number): TraceSpan;

  /** Get the span category for UI classification. */
  classifySpan(rawSpan: TRawSpan): "llm" | "tool" | "agent" | "retrieval" | "chain" | "other";

  /** Extract cost from a raw span (in cents), if available. */
  extractCost(rawSpan: TRawSpan): number | undefined;

  /** Extract token counts from a raw span, if available. */
  extractTokens(rawSpan: TRawSpan): { input?: number; output?: number } | undefined;
}

/**
 * Import traces using an adapter, producing qontinui-native TraceSpan[] and TraceTreeNode[].
 */
export function importTrace<TRawTrace, TRawSpan>(
  adapter: TraceImportAdapter<TRawTrace, TRawSpan>,
  rawTrace: TRawTrace,
): { spans: TraceSpan[]; tree: TraceTreeNode[] } {
  const rawSpans = adapter.extractRawSpans(rawTrace);
  const spans = rawSpans.map((raw, i) => {
    const span = adapter.convertSpan(raw, i);
    const tokens = adapter.extractTokens(raw);
    const cost = adapter.extractCost(raw);
    return {
      ...span,
      input_tokens: tokens?.input ?? span.input_tokens,
      output_tokens: tokens?.output ?? span.output_tokens,
      cost_cents: cost ?? span.cost_cents,
    };
  });
  const tree = buildTraceTree(spans);
  return { spans, tree };
}
