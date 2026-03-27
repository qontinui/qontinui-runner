/**
 * AiStreamWidget Component
 *
 * Dashboard widget that shows live AI output as it streams in real-time.
 * Listens to ai-output-chunk events and displays a scrolling monospace
 * text panel with the accumulated AI output.
 */

import { useState, useEffect, useRef, useCallback } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ChevronDown, ChevronUp, Terminal, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { useCurrentTaskRunId } from "@/contexts";

/** Maximum characters to retain in the buffer to prevent memory issues */
const MAX_BUFFER_SIZE = 500_000;

/**
 * AiStreamWidget component.
 *
 * Listens for ai-output-chunk events and displays the accumulated
 * AI output in a scrollable monospace panel.
 */
export function AiStreamWidget() {
  const taskRunId = useCurrentTaskRunId();
  const [output, setOutput] = useState("");
  const [expanded, setExpanded] = useState(true);
  const [autoScroll, setAutoScroll] = useState(true);
  const containerRef = useRef<HTMLDivElement>(null);
  const charCount = output.length;

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    // Reset output when task run changes
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription callback
    setOutput("");

    const setup = async () => {
      unlisten = await listen("ai-output-chunk", (event: { payload: Record<string, unknown> }) => {
        const data = event.payload;
        // The AppEvent is serialized with serde tag="event_type", content="data"
        const inner = (data as Record<string, unknown>).data as Record<string, unknown> | undefined;
        const eventData = inner || data;
        const eventTaskRunId = eventData.task_run_id as string;
        if (taskRunId && eventTaskRunId !== taskRunId) return;

        const chunk = eventData.chunk as string;
        if (!chunk) return;

        setOutput((prev) => {
          const next = prev + chunk;
          // Trim from the front if we exceed the buffer limit
          if (next.length > MAX_BUFFER_SIZE) {
            return next.slice(next.length - MAX_BUFFER_SIZE);
          }
          return next;
        });
      });
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, [taskRunId]);

  // Auto-scroll to bottom when new content arrives
  useEffect(() => {
    if (autoScroll && containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [output, autoScroll]);

  // Detect manual scroll to disable auto-scroll
  const handleScroll = useCallback(() => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    const isAtBottom = scrollHeight - scrollTop - clientHeight < 40;
    setAutoScroll(isAtBottom);
  }, []);

  const handleClear = useCallback(() => {
    setOutput("");
  }, []);

  if (charCount === 0) {
    return null;
  }

  return (
    <div className="flex flex-col bg-zinc-800/50 rounded-lg border border-zinc-700/50 overflow-hidden">
      {/* Header */}
      <button
        onClick={() => setExpanded((prev) => !prev)}
        className="flex items-center justify-between px-3 py-2 hover:bg-zinc-700/30 transition-colors"
      >
        <div className="flex items-center gap-2">
          <Terminal className="w-3.5 h-3.5 text-zinc-400" />
          <span className="text-xs font-semibold text-zinc-300 uppercase tracking-wider">
            AI Output
          </span>
          <span className="text-[10px] text-zinc-500 font-mono">
            {charCount > 1000 ? `${(charCount / 1000).toFixed(1)}k` : charCount} chars
          </span>
        </div>
        <div className="flex items-center gap-1">
          {expanded && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                handleClear();
              }}
              className="p-1 hover:bg-zinc-600/50 rounded text-zinc-500 hover:text-zinc-300 transition-colors"
              title="Clear output"
            >
              <Trash2 className="w-3 h-3" />
            </button>
          )}
          {expanded ? (
            <ChevronUp className="w-3.5 h-3.5 text-zinc-500" />
          ) : (
            <ChevronDown className="w-3.5 h-3.5 text-zinc-500" />
          )}
        </div>
      </button>

      {/* Content */}
      {expanded && (
        <div
          ref={containerRef}
          onScroll={handleScroll}
          className={cn(
            "max-h-[300px] overflow-y-auto overflow-x-hidden",
            "px-3 py-2 border-t border-zinc-700/50",
            "bg-zinc-900/50",
          )}
        >
          <pre className="text-xs text-zinc-300 font-mono whitespace-pre-wrap break-words leading-relaxed">
            {output}
          </pre>
        </div>
      )}

      {/* Auto-scroll indicator */}
      {expanded && !autoScroll && (
        <button
          onClick={() => {
            setAutoScroll(true);
            if (containerRef.current) {
              containerRef.current.scrollTop = containerRef.current.scrollHeight;
            }
          }}
          className="flex items-center justify-center gap-1 py-1 text-[10px] text-zinc-500 hover:text-zinc-300 bg-zinc-800/80 border-t border-zinc-700/30 transition-colors"
        >
          <ChevronDown className="w-3 h-3" />
          <span>Scroll to bottom</span>
        </button>
      )}
    </div>
  );
}

export default AiStreamWidget;
