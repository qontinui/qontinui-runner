import React from "react";
import type { TraceSpan } from "./types";
import { PHASE_COLORS } from "./types";
import { inferPhase, formatDuration } from "./trace-utils";

interface SpanDetailPanelProps {
  span: TraceSpan | null;
  onClose: () => void;
}

export const SpanDetailPanel: React.FC<SpanDetailPanelProps> = ({ span, onClose }) => {
  if (!span) return null;

  const phase = inferPhase(span.name);
  const colors = PHASE_COLORS[phase];

  return (
    <div className="w-[280px] min-w-[280px] border-l border-zinc-700 bg-zinc-900 overflow-y-auto">
      <div className="flex items-center justify-between p-3 border-b border-zinc-700">
        <h3 className="text-sm font-medium text-zinc-200 truncate">
          {span.name.replace(/^qontinui\./, "")}
        </h3>
        <button
          onClick={onClose}
          className="text-zinc-500 hover:text-zinc-300 text-xs"
        >
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
            <span className="text-zinc-400 ml-1">
              {new Date(span.end_ts).toLocaleTimeString()}
            </span>
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

        {/* Attributes */}
        {Object.keys(span.attributes).length > 0 && (
          <div className="pt-2 border-t border-zinc-800">
            <span className="text-zinc-500 font-medium">Attributes:</span>
            <div className="mt-1 space-y-1">
              {Object.entries(span.attributes).map(([key, value]) => (
                <div key={key} className="flex gap-1">
                  <span className="text-zinc-500 flex-shrink-0">{key}:</span>
                  <span className="text-zinc-400 break-all">
                    {typeof value === "string" ? value : JSON.stringify(value)}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
