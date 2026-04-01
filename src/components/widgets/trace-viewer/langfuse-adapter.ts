import type { TraceImportAdapter } from "./trace-import-adapter";

/**
 * Raw Langfuse observation shape (from Langfuse API /api/public/observations).
 * This is a subset — only the fields we need for trace import.
 */
export interface LangfuseObservation {
  id: string;
  traceId: string;
  parentObservationId: string | null;
  name: string;
  type: "GENERATION" | "SPAN" | "EVENT";
  startTime: string;
  endTime?: string;
  model?: string;
  modelParameters?: Record<string, unknown>;
  input?: unknown;
  output?: unknown;
  usage?: {
    input?: number;
    output?: number;
    total?: number;
    unit?: string;
  };
  calculatedTotalCost?: number;
  level?: "DEBUG" | "DEFAULT" | "WARNING" | "ERROR";
  statusMessage?: string;
  metadata?: Record<string, unknown>;
}

export interface LangfuseTrace {
  id: string;
  name?: string;
  observations: LangfuseObservation[];
}

export const langfuseAdapter: TraceImportAdapter<LangfuseTrace, LangfuseObservation> = {
  name: "Langfuse",

  extractRawSpans(rawTrace) {
    return rawTrace.observations;
  },

  convertSpan(obs, index) {
    const startMs = new Date(obs.startTime).getTime();
    const endMs = obs.endTime ? new Date(obs.endTime).getTime() : startMs;
    const durationMs = endMs - startMs;

    const attributes: Record<string, unknown> = {
      "langfuse.type": obs.type,
      ...(obs.model && { "gen_ai.request.model": obs.model }),
      ...(obs.modelParameters && { "langfuse.model_parameters": obs.modelParameters }),
      ...(obs.metadata &&
        Object.fromEntries(
          Object.entries(obs.metadata).map(([k, v]) => [`langfuse.metadata.${k}`, v]),
        )),
    };

    return {
      id: index,
      execution_id: obs.traceId,
      trace_id: obs.traceId,
      span_id: obs.id,
      parent_span_id: obs.parentObservationId,
      name: obs.name || obs.type.toLowerCase(),
      target: null,
      level: obs.level?.toLowerCase() ?? null,
      start_ts: obs.startTime,
      end_ts: obs.endTime ?? null,
      duration_ms: durationMs,
      attributes,
      success: obs.level !== "ERROR",
      error: obs.level === "ERROR" ? (obs.statusMessage ?? "Error") : null,
      input_tokens: obs.usage?.input,
      output_tokens: obs.usage?.output,
      cost_cents:
        obs.calculatedTotalCost != null ? Math.round(obs.calculatedTotalCost * 100) : undefined,
    };
  },

  classifySpan(obs) {
    switch (obs.type) {
      case "GENERATION":
        return "llm";
      case "EVENT":
        return "other";
      case "SPAN": {
        const name = obs.name?.toLowerCase() ?? "";
        if (name.includes("tool") || name.includes("function")) return "tool";
        if (name.includes("agent")) return "agent";
        if (name.includes("retriev") || name.includes("rag")) return "retrieval";
        if (name.includes("chain")) return "chain";
        return "other";
      }
      default:
        return "other";
    }
  },

  extractCost(obs) {
    if (obs.calculatedTotalCost != null) {
      return Math.round(obs.calculatedTotalCost * 100); // dollars to cents
    }
    return undefined;
  },

  extractTokens(obs) {
    if (!obs.usage) return undefined;
    return { input: obs.usage.input, output: obs.usage.output };
  },
};
