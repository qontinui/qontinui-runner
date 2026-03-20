import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { cn } from "../../lib/utils";
import { Search, ArrowDown, Trash2, Wand2 } from "lucide-react";

interface OutputLine {
  timestamp: string;
  stream: "stdout" | "stderr";
  line: string;
}

interface ProcessOutputViewerProps {
  processId: string;
  processName: string;
  className?: string;
  processState?: string;
  errorCount?: number;
  onFixWithAi?: () => void;
  isFixActive?: boolean;
}

export function ProcessOutputViewer({
  processId,
  processName: _processName,
  className,
  processState,
  errorCount = 0,
  onFixWithAi,
  isFixActive = false,
}: ProcessOutputViewerProps) {
  const [lines, setLines] = useState<OutputLine[]>([]);
  const [searchText, setSearchText] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const scrollRef = useRef<HTMLDivElement>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  // Load initial output
  useEffect(() => {
    invoke<OutputLine[]>("get_process_output", {
      id: processId,
      tail: 500,
    })
      .then(setLines)
      .catch(console.error);
  }, [processId]);

  // Listen for real-time output events
  useEffect(() => {
    const setup = async () => {
      unlistenRef.current = await listen<{
        id: string;
        line: OutputLine;
      }>("process-output", (event) => {
        if (event.payload.id === processId) {
          setLines((prev) => {
            const next = [...prev, event.payload.line];
            // Keep last 2000 lines in frontend
            if (next.length > 2000) {
              return next.slice(-2000);
            }
            return next;
          });
        }
      });
    };
    setup();

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
      }
    };
  }, [processId]);

  // Auto-scroll to bottom
  useEffect(() => {
    if (autoScroll && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [lines, autoScroll]);

  const handleScroll = useCallback(() => {
    if (!scrollRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
    const isNearBottom = scrollHeight - scrollTop - clientHeight < 50;
    setAutoScroll(isNearBottom);
  }, []);

  const clearOutput = useCallback(() => {
    setLines([]);
  }, []);

  const scrollToBottom = useCallback(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
      setAutoScroll(true);
    }
  }, []);

  const filteredLines = searchText
    ? lines.filter((l) => l.line.toLowerCase().includes(searchText.toLowerCase()))
    : lines;

  return (
    <div className={cn("flex flex-col h-full", className)}>
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-white/10">
        <div className="relative flex-1">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-zinc-500" />
          <input
            type="text"
            value={searchText}
            onChange={(e) => setSearchText(e.target.value)}
            placeholder="Search output..."
            className="w-full pl-7 pr-3 py-1 text-xs bg-zinc-900 border border-white/10 rounded text-zinc-300 placeholder:text-zinc-600 focus:outline-none focus:border-cyan-500/50"
          />
        </div>
        <button
          onClick={scrollToBottom}
          className="p-1 text-zinc-500 hover:text-zinc-300 transition-colors"
          title="Scroll to bottom"
        >
          <ArrowDown className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={clearOutput}
          className="p-1 text-zinc-500 hover:text-zinc-300 transition-colors"
          title="Clear output"
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
        {onFixWithAi && (processState === "failed" || errorCount > 0) && (
          <button
            onClick={onFixWithAi}
            disabled={isFixActive}
            className={cn(
              "flex items-center gap-1 px-2 py-0.5 text-xs rounded border transition-colors",
              isFixActive
                ? "text-zinc-500 border-zinc-700 cursor-not-allowed"
                : "text-amber-300 border-amber-700/50 bg-amber-900/30 hover:bg-amber-800/40",
            )}
            title="Analyze errors with AI"
          >
            <Wand2 className="w-3 h-3" />
            Fix with AI
          </button>
        )}
        <span className="text-xs text-zinc-600">{lines.length} lines</span>
      </div>

      {/* Output area */}
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="flex-1 overflow-auto font-mono text-xs leading-5 p-2 bg-zinc-950"
      >
        {filteredLines.length === 0 ? (
          <div className="flex items-center justify-center h-full text-zinc-600">
            {lines.length === 0
              ? "No output yet. Start the process to see output."
              : "No matching lines."}
          </div>
        ) : (
          filteredLines.map((line, i) => (
            <div
              key={`${line.timestamp}-${i}`}
              className={cn(
                "whitespace-pre-wrap break-all",
                line.stream === "stderr" ? "text-red-400/80" : "text-zinc-300",
              )}
            >
              <span className="text-zinc-600 select-none mr-2">
                {new Date(line.timestamp).toLocaleTimeString()}
              </span>
              {line.line}
            </div>
          ))
        )}
      </div>

      {/* Auto-scroll indicator */}
      {!autoScroll && lines.length > 0 && (
        <button
          onClick={scrollToBottom}
          className="absolute bottom-4 right-4 px-3 py-1 text-xs bg-cyan-900/80 text-cyan-300 rounded-full border border-cyan-700 hover:bg-cyan-800 transition-colors"
        >
          Scroll to bottom
        </button>
      )}
    </div>
  );
}
