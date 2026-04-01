import { useMemo } from "react";
import type { TraceSpan } from "./types";
import { inferPhase, formatDuration } from "./trace-utils";

export interface StateSnapshot {
  id: number;
  span_id: string;
  snapshot_ts: string;
  state_type: string;
  summary?: string;
  context_json?: string;
}

export interface ReplayStep {
  index: number;
  spanId: string;
  span: TraceSpan;
  type: "phase_transition" | "ai_session" | "tool_call" | "decision";
  title: string;
  description: string;
  timestamp: string;
  snapshot?: StateSnapshot;
}

/** Derive an ordered list of replay steps from trace spans. */
export function deriveReplaySteps(spans: TraceSpan[]): ReplayStep[] {
  const steps: ReplayStep[] = [];

  // Sort by start timestamp
  const sorted = [...spans].sort((a, b) => (a.start_ts || "").localeCompare(b.start_ts || ""));

  for (const span of sorted) {
    // Phase transitions
    if (span.name.includes("phase.")) {
      const phase = inferPhase(span.name);
      steps.push({
        index: steps.length,
        spanId: span.span_id,
        span,
        type: "phase_transition",
        title: `Phase: ${phase}`,
        description: `Entered ${phase} phase${span.duration_ms != null ? ` (${formatDuration(span.duration_ms)})` : ""}`,
        timestamp: span.start_ts || "",
      });
    }
    // AI sessions
    else if (span.name.includes("ai.session")) {
      const attrs: Record<string, unknown> =
        span.attributes && typeof span.attributes === "object"
          ? (span.attributes as Record<string, unknown>)
          : {};
      const stepName = (attrs["step_name"] as string) || "AI Session";
      steps.push({
        index: steps.length,
        spanId: span.span_id,
        span,
        type: "ai_session",
        title: stepName,
        description: `AI session${span.input_tokens ? ` (${span.input_tokens.toLocaleString()} in / ${(span.output_tokens ?? 0).toLocaleString()} out)` : ""}`,
        timestamp: span.start_ts || "",
      });
    }
    // Tool calls
    else if (span.name.includes("tool_call")) {
      const attrs: Record<string, unknown> =
        span.attributes && typeof span.attributes === "object"
          ? (span.attributes as Record<string, unknown>)
          : {};
      const toolName = (attrs["ai.tool_name"] as string) || "Tool call";
      steps.push({
        index: steps.length,
        spanId: span.span_id,
        span,
        type: "tool_call",
        title: toolName,
        description: `Tool: ${toolName}`,
        timestamp: span.start_ts || "",
      });
    }
    // Agent phases
    else if (span.name.includes("agent.phase")) {
      const attrs: Record<string, unknown> =
        span.attributes && typeof span.attributes === "object"
          ? (span.attributes as Record<string, unknown>)
          : {};
      const role = (attrs["agent.role"] as string) || "Agent";
      steps.push({
        index: steps.length,
        spanId: span.span_id,
        span,
        type: "decision",
        title: `Agent: ${role}`,
        description: `${role} agent${span.duration_ms != null ? ` (${formatDuration(span.duration_ms)})` : ""}`,
        timestamp: span.start_ts || "",
      });
    }
  }

  // Re-index
  steps.forEach((s, i) => {
    s.index = i;
  });
  return steps;
}

export function useReplayData(spans: TraceSpan[]) {
  const steps = useMemo(() => deriveReplaySteps(spans), [spans]);
  return { steps, totalSteps: steps.length };
}
