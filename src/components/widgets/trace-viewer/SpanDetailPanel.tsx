import React from "react";
import type { TraceSpan, CriticalPathInfo } from "./types";
import { PHASE_COLORS } from "./types";
import { inferPhase, formatDuration } from "./trace-utils";

interface SpanDetailPanelProps {
  span: TraceSpan | null;
  onClose: () => void;
  criticalPath?: CriticalPathInfo | null;
  allSpans?: TraceSpan[];
}

export const SpanDetailPanel: React.FC<SpanDetailPanelProps> = ({
  span,
  onClose,
  criticalPath,
  allSpans = [],
}) => {
  if (!span) return null;

  const phase = inferPhase(span.name);
  const colors = PHASE_COLORS[phase];

  return (
    <div
      className="w-[280px] min-w-[280px] border-l border-zinc-700 bg-zinc-900 overflow-y-auto"
      data-tutorial-id="trace-span-detail-panel"
    >
      <div className="flex items-center justify-between p-3 border-b border-zinc-700">
        <h3 className="text-sm font-medium text-zinc-200 truncate">
          {span.name.replace(/^qontinui\./, "")}
        </h3>
        <button onClick={onClose} className="text-zinc-500 hover:text-zinc-300 text-xs">
          ✕
        </button>
      </div>

      <div className="p-3 space-y-3 text-xs">
        {/* Status */}
        <div className="flex items-center gap-2">
          <span className="text-zinc-500">Status:</span>
          <span className={span.success ? "text-emerald-400" : "text-red-400"}>
            {span.success ? "Success" : "Error"}
          </span>
        </div>

        {/* Phase */}
        <div className="flex items-center gap-2">
          <span className="text-zinc-500">Phase:</span>
          <span className={`px-1.5 rounded ${colors.badge} ${colors.text}`}>{phase}</span>
        </div>

        {/* Duration */}
        <div className="flex items-center gap-2">
          <span className="text-zinc-500">Duration:</span>
          <span className="text-zinc-300">{formatDuration(span.duration_ms ?? 0)}</span>
        </div>

        {/* Timestamps */}
        <div>
          <span className="text-zinc-500">Start:</span>
          <span className="text-zinc-400 ml-1">{new Date(span.start_ts).toLocaleTimeString()}</span>
        </div>
        {span.end_ts && (
          <div>
            <span className="text-zinc-500">End:</span>
            <span className="text-zinc-400 ml-1">{new Date(span.end_ts).toLocaleTimeString()}</span>
          </div>
        )}

        {/* IDs */}
        <div className="space-y-1 pt-2 border-t border-zinc-800">
          <div>
            <span className="text-zinc-500">Trace ID:</span>
            <span className="text-zinc-400 ml-1 font-mono">{span.trace_id}</span>
          </div>
          <div>
            <span className="text-zinc-500">Span ID:</span>
            <span className="text-zinc-400 ml-1 font-mono">{span.span_id}</span>
          </div>
          {span.parent_span_id && (
            <div>
              <span className="text-zinc-500">Parent:</span>
              <span className="text-zinc-400 ml-1 font-mono">{span.parent_span_id}</span>
            </div>
          )}
        </div>

        {/* Error */}
        {span.error && (
          <div className="pt-2 border-t border-zinc-800">
            <span className="text-red-400 font-medium">Error:</span>
            <pre className="mt-1 text-red-300/80 whitespace-pre-wrap break-words bg-red-900/20 rounded p-2">
              {span.error}
            </pre>
          </div>
        )}

        {/* Critical path insight */}
        {criticalPath && criticalPath.pathDurationMs > 0 && (
          <div className="pt-2 border-t border-zinc-800">
            {criticalPath.spanIds.has(span.span_id) ? (
              <>
                <span className="text-red-300 font-medium text-[10px]">On critical path</span>
                <p className="text-zinc-400 mt-1">
                  Reducing this span's duration would directly reduce total trace time.
                </p>
              </>
            ) : (
              <>
                <span className="text-zinc-500 font-medium text-[10px]">Off critical path</span>
                <p className="text-zinc-400 mt-1">
                  Slack: {formatDuration(criticalPath.slackBySpanId.get(span.span_id) ?? 0)} — this
                  span could take that much longer without affecting total time.
                </p>
              </>
            )}
          </div>
        )}

        {/* AI session section */}
        {span.name.includes("ai.session") && (
          <div className="pt-2 border-t border-zinc-800">
            <span className="text-zinc-500 font-medium text-[10px]">Token Usage:</span>
            <div className="mt-1 grid grid-cols-2 gap-1">
              {span.input_tokens != null && (
                <div className="bg-blue-500/10 rounded px-1.5 py-0.5 text-[10px]">
                  In:{" "}
                  <span className="font-mono text-zinc-300">
                    {span.input_tokens.toLocaleString()}
                  </span>
                </div>
              )}
              {span.output_tokens != null && (
                <div className="bg-green-500/10 rounded px-1.5 py-0.5 text-[10px]">
                  Out:{" "}
                  <span className="font-mono text-zinc-300">
                    {span.output_tokens.toLocaleString()}
                  </span>
                </div>
              )}
              {span.cost_cents != null && (
                <div className="bg-amber-500/10 rounded px-1.5 py-0.5 text-[10px] col-span-2">
                  Cost:{" "}
                  <span className="font-mono text-zinc-300">
                    ${(span.cost_cents / 100).toFixed(4)}
                  </span>
                </div>
              )}
            </div>
            {/* Child tool calls */}
            {allSpans.length > 0 &&
              (() => {
                const childTools = allSpans.filter(
                  (s) => s.parent_span_id === span.span_id && s.name.includes("tool_call"),
                );
                if (childTools.length === 0) return null;
                return (
                  <div className="mt-2">
                    <span className="text-zinc-500 font-medium text-[10px]">
                      Tool Calls ({childTools.length}):
                    </span>
                    <div className="mt-1 space-y-0.5">
                      {childTools.map((tool) => (
                        <div key={tool.span_id} className="flex items-center gap-1 text-[10px]">
                          <span className={tool.success ? "text-emerald-400" : "text-red-400"}>
                            {tool.success ? "\u2713" : "\u2717"}
                          </span>
                          <span className="text-zinc-400 truncate">
                            {((tool.attributes ?? {})["ai.tool_name"] as string) ||
                              tool.name.replace(/^qontinui\./, "")}
                          </span>
                          {tool.duration_ms != null && (
                            <span className="text-zinc-600 ml-auto shrink-0">
                              {formatDuration(tool.duration_ms)}
                            </span>
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                );
              })()}
          </div>
        )}

        {/* GenAI attributes */}
        {(() => {
          const attrs = span.attributes ?? {};
          const genAiEntries = Object.entries(attrs).filter(([key]) => key.startsWith("gen_ai."));
          if (genAiEntries.length === 0) return null;
          return (
            <div className="pt-2 border-t border-zinc-800">
              <span className="text-zinc-500 font-medium">GenAI (OpenTelemetry):</span>
              <div className="mt-1 space-y-1">
                {genAiEntries.map(([key, value]) => (
                  <div key={key} className="flex gap-1">
                    <span className="text-zinc-500 shrink-0">{key.replace(/^gen_ai\./, "")}:</span>
                    <span className="text-zinc-400 break-all">
                      {typeof value === "string" ? value : JSON.stringify(value)}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          );
        })()}

        {/* Attributes (excluding gen_ai.* which are shown above) */}
        {(() => {
          const attrs = span.attributes ?? {};
          const nonGenAiEntries = Object.entries(attrs).filter(
            ([key]) => !key.startsWith("gen_ai."),
          );
          if (nonGenAiEntries.length === 0) return null;
          return (
            <div className="pt-2 border-t border-zinc-800">
              <span className="text-zinc-500 font-medium">Attributes:</span>
              <div className="mt-1 space-y-1">
                {nonGenAiEntries.map(([key, value]) => (
                  <div key={key} className="flex gap-1">
                    <span className="text-zinc-500 shrink-0">{key}:</span>
                    <span className="text-zinc-400 break-all">
                      {typeof value === "string" ? value : JSON.stringify(value)}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          );
        })()}
      </div>
    </div>
  );
};
