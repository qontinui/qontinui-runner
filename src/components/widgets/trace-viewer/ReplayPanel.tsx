import type { ReplayStep } from "./useReplayData";
import { formatDuration } from "./trace-utils";

interface ReplayPanelProps {
  step: ReplayStep;
  previousStep?: ReplayStep;
}

const TYPE_COLORS: Record<string, string> = {
  phase_transition: "text-blue-400",
  ai_session: "text-purple-400",
  tool_call: "text-amber-400",
  decision: "text-green-400",
};

const TYPE_LABELS: Record<string, string> = {
  phase_transition: "Phase Transition",
  ai_session: "AI Session",
  tool_call: "Tool Call",
  decision: "Agent Decision",
};

export function ReplayPanel({ step, previousStep }: ReplayPanelProps) {
  const { span } = step;

  return (
    <div className="w-[280px] border-l border-zinc-700 overflow-y-auto p-3 space-y-3 text-sm">
      {/* Header */}
      <div>
        <div
          className={`text-[10px] font-medium uppercase tracking-wider ${TYPE_COLORS[step.type] || "text-muted-foreground"}`}
        >
          {TYPE_LABELS[step.type] || step.type}
        </div>
        <div className="font-medium text-sm mt-0.5">{step.title}</div>
        <div className="text-xs text-muted-foreground">{step.description}</div>
      </div>

      {/* Timing */}
      <div className="space-y-1">
        <div className="text-xs font-medium text-muted-foreground">Timing</div>
        {span.duration_ms != null && (
          <div className="text-xs">
            Duration:{" "}
            <span className="font-mono">{formatDuration(span.duration_ms)}</span>
          </div>
        )}
        {span.start_ts && (
          <div className="text-xs text-muted-foreground">
            Started: {new Date(span.start_ts).toLocaleTimeString()}
          </div>
        )}
      </div>

      {/* Status */}
      <div className="space-y-1">
        <div className="text-xs font-medium text-muted-foreground">Status</div>
        <div
          className={`text-xs ${span.success === false || span.error ? "text-red-400" : "text-green-400"}`}
        >
          {span.success === false || span.error ? "Failed" : "Success"}
        </div>
        {span.error && (
          <div className="text-xs text-red-300 bg-red-500/10 rounded p-1.5 font-mono break-all">
            {span.error}
          </div>
        )}
      </div>

      {/* Token usage (for AI spans) */}
      {(span.input_tokens != null || span.output_tokens != null) && (
        <div className="space-y-1">
          <div className="text-xs font-medium text-muted-foreground">Tokens</div>
          <div className="grid grid-cols-2 gap-1 text-xs">
            {span.input_tokens != null && (
              <div className="bg-blue-500/10 rounded px-1.5 py-0.5">
                In:{" "}
                <span className="font-mono">
                  {span.input_tokens.toLocaleString()}
                </span>
              </div>
            )}
            {span.output_tokens != null && (
              <div className="bg-green-500/10 rounded px-1.5 py-0.5">
                Out:{" "}
                <span className="font-mono">
                  {span.output_tokens.toLocaleString()}
                </span>
              </div>
            )}
          </div>
        </div>
      )}

      {/* Snapshot context */}
      {step.snapshot && (
        <div className="space-y-1">
          <div className="text-xs font-medium text-muted-foreground">Context Snapshot</div>
          {step.snapshot.summary && (
            <div className="text-xs">{step.snapshot.summary}</div>
          )}
          {step.snapshot.context_json && (() => {
            try {
              const ctx = JSON.parse(step.snapshot.context_json);
              return (
                <div className="space-y-0.5 text-xs">
                  {Object.entries(ctx).map(([k, v]) => (
                    <div key={k} className="flex justify-between">
                      <span className="text-muted-foreground">{k}:</span>
                      <span className="font-mono truncate ml-2">{String(v)}</span>
                    </div>
                  ))}
                </div>
              );
            } catch { return null; }
          })()}
        </div>
      )}

      {/* Context change */}
      {previousStep && (
        <div className="space-y-1">
          <div className="text-xs font-medium text-muted-foreground">
            Previous Step
          </div>
          <div className="text-xs text-muted-foreground">
            {previousStep.title}
            {previousStep.span.duration_ms != null && (
              <span className="ml-1 font-mono">
                ({formatDuration(previousStep.span.duration_ms)})
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
