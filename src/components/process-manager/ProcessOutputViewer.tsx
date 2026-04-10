import { useEffect, useRef, useState, useCallback, useMemo, startTransition, type ReactElement } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { List, useListRef, type RowComponentProps } from "react-window";
import { cn } from "../../lib/utils";
import { Search, ArrowDown, Trash2, Wand2, Hammer, History } from "lucide-react";

interface OutputLine {
  timestamp: string;
  stream: "stdout" | "stderr";
  line: string;
}

interface ProcessSession {
  id: string;
  process_config_id: string;
  process_name: string;
  started_at: string;
  stopped_at: string | null;
  exit_code: number | null;
  state: string;
  error_count: number;
}

interface ProcessSessionOutputLine {
  id: number;
  session_id: string;
  timestamp: string;
  stream: string;
  line: string;
}

const LIVE_SESSION = "__live__";

function formatSessionLabel(s: ProcessSession): string {
  const started = new Date(s.started_at);
  const now = Date.now();
  const ageMs = now - started.getTime();
  const ageMin = Math.floor(ageMs / 60_000);
  let when: string;
  if (ageMin < 1) when = "just now";
  else if (ageMin < 60) when = `${ageMin}m ago`;
  else if (ageMin < 1440) when = `${Math.floor(ageMin / 60)}h ago`;
  else when = `${Math.floor(ageMin / 1440)}d ago`;

  const status =
    s.state === "stopped"
      ? `exit ${s.exit_code ?? 0}`
      : s.state === "failed"
        ? `failed ${s.exit_code ?? "?"}`
        : s.state;
  return `${when} · ${status}`;
}

interface OutputRowProps {
  lines: OutputLine[];
}

function OutputRow({ index, style, lines }: RowComponentProps<OutputRowProps>): ReactElement {
  const line = lines[index];
  return (
    <div
      style={style}
      className={cn(
        "whitespace-nowrap overflow-hidden text-ellipsis px-2",
        line.stream === "stderr" ? "text-red-400/80" : "text-zinc-300",
      )}
    >
      <span className="text-zinc-600 select-none mr-2">
        {new Date(line.timestamp).toLocaleTimeString()}
      </span>
      {line.line}
    </div>
  );
}

interface ProcessOutputViewerProps {
  processId: string;
  processName: string;
  className?: string;
  processState?: string;
  errorCount?: number;
  onFixWithAi?: () => void;
  onRebuildAndRestart?: () => void;
  hasBuildCommand?: boolean;
  isFixActive?: boolean;
}

export function ProcessOutputViewer({
  processId,
  processName: _processName,
  className,
  processState,
  errorCount = 0,
  onFixWithAi,
  onRebuildAndRestart,
  hasBuildCommand = false,
  isFixActive = false,
}: ProcessOutputViewerProps) {
  const [lines, setLines] = useState<OutputLine[]>([]);
  const [searchText, setSearchText] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const [sessions, setSessions] = useState<ProcessSession[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string>(LIVE_SESSION);
  const [searchAllSessions, setSearchAllSessions] = useState(false);
  const [globalHits, setGlobalHits] = useState<
    Array<{
      id: number;
      session_id: string;
      timestamp: string;
      stream: string;
      line: string;
      process_config_id: string;
      process_name: string;
    }>
  >([]);
  const listRef = useListRef(null);
  const scrollableRef = listRef as React.RefObject<{
    scrollToRow: (config: { index: number; align?: string }) => void;
  } | null>;
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const LINE_HEIGHT = 20;

  const isLive = selectedSessionId === LIVE_SESSION;

  // Reset selection when switching processes
  useEffect(() => {
    startTransition(() => setSelectedSessionId(LIVE_SESSION));
  }, [processId]);

  // Load historical session list (refreshes when process state changes)
  useEffect(() => {
    invoke<ProcessSession[]>("get_process_sessions_from_db", {
      configId: processId,
      limit: 20,
    })
      .then((s) => setSessions(s ?? []))
      .catch((e) => console.error("Failed to load sessions:", e));
  }, [processId, processState]);

  // Load output: live ring buffer OR historical session
  useEffect(() => {
    if (isLive) {
      invoke<OutputLine[]>("get_process_output", {
        id: processId,
        tail: 500,
      })
        .then(setLines)
        .catch(console.error);
    } else {
      invoke<ProcessSessionOutputLine[]>("get_process_session_output_from_db", {
        sessionId: selectedSessionId,
        limit: 5000,
        offset: 0,
      })
        .then((rows) => {
          setLines(
            rows.map((r) => ({
              timestamp: r.timestamp,
              stream: r.stream === "stderr" ? "stderr" : "stdout",
              line: r.line,
            })),
          );
        })
        .catch(console.error);
    }
  }, [processId, isLive, selectedSessionId]);

  // Listen for real-time output events (only when in live mode)
  useEffect(() => {
    if (!isLive) {
      return;
    }
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
        unlistenRef.current = null;
      }
    };
  }, [processId, isLive]);

  const filteredLines = useMemo(
    () =>
      searchText
        ? lines.filter((l) => l.line.toLowerCase().includes(searchText.toLowerCase()))
        : lines,
    [lines, searchText],
  );

  // Run global search when searchAllSessions is enabled and there's a query
  useEffect(() => {
    if (!searchAllSessions || !searchText.trim()) {
      startTransition(() => setGlobalHits([]));
      return;
    }
    let cancelled = false;
    invoke<typeof globalHits>("search_process_logs", {
      query: searchText,
      configId: processId,
      limit: 200,
    })
      .then((hits) => {
        if (!cancelled) setGlobalHits(hits);
      })
      .catch((e) => {
        console.error("Global search failed:", e);
        if (!cancelled) setGlobalHits([]);
      });
    return () => {
      cancelled = true;
    };
  }, [searchAllSessions, searchText, processId]);

  // Auto-scroll to bottom using react-window
  useEffect(() => {
    if (autoScroll && filteredLines.length > 0) {
      scrollableRef.current?.scrollToRow({ index: filteredLines.length - 1, align: "end" });
    }
  }, [filteredLines.length, autoScroll, scrollableRef]);

  const handleRowsRendered = useCallback(
    (
      _visibleRows: { startIndex: number; stopIndex: number },
      allRows: { startIndex: number; stopIndex: number },
    ) => {
      // If the last rendered row is the last item, we're at the bottom
      const isAtBottom = allRows.stopIndex >= filteredLines.length - 1;
      setAutoScroll(isAtBottom);
    },
    [filteredLines.length],
  );

  const clearOutput = useCallback(() => {
    setLines([]);
  }, []);

  const scrollToBottom = useCallback(() => {
    if (filteredLines.length > 0) {
      scrollableRef.current?.scrollToRow({ index: filteredLines.length - 1, align: "end" });
    }
    setAutoScroll(true);
  }, [filteredLines.length, scrollableRef]);

  return (
    <div className={cn("flex flex-col h-full relative", className)}>
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-white/10">
        <div className="relative flex items-center">
          <History className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-zinc-500 pointer-events-none" />
          <select
            value={selectedSessionId}
            onChange={(e) => setSelectedSessionId(e.target.value)}
            className="pl-7 pr-2 py-1 text-xs bg-zinc-900 border border-white/10 rounded text-zinc-300 focus:outline-hidden focus:border-cyan-500/50 max-w-[180px]"
            title="Choose Live or a historical session"
          >
            <option value={LIVE_SESSION}>Live</option>
            {sessions.map((s) => (
              <option key={s.id} value={s.id}>
                {formatSessionLabel(s)}
              </option>
            ))}
          </select>
        </div>
        <div className="relative flex-1">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-zinc-500" />
          <input
            type="text"
            value={searchText}
            onChange={(e) => setSearchText(e.target.value)}
            placeholder={searchAllSessions ? "Search all sessions..." : "Search output..."}
            className="w-full pl-7 pr-3 py-1 text-xs bg-zinc-900 border border-white/10 rounded text-zinc-300 placeholder:text-zinc-600 focus:outline-hidden focus:border-cyan-500/50"
          />
        </div>
        <button
          onClick={() => setSearchAllSessions((v) => !v)}
          className={cn(
            "px-2 py-0.5 text-xs rounded border transition-colors",
            searchAllSessions
              ? "text-cyan-300 border-cyan-700/50 bg-cyan-900/30"
              : "text-zinc-500 border-white/10 hover:text-zinc-300",
          )}
          title="Toggle: search across all historical sessions vs current view"
        >
          All sessions
        </button>
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
        {onRebuildAndRestart && hasBuildCommand && (
          <button
            onClick={onRebuildAndRestart}
            disabled={processState === "building"}
            className={cn(
              "flex items-center gap-1 px-2 py-0.5 text-xs rounded border transition-colors",
              processState === "building"
                ? "text-zinc-500 border-zinc-700 cursor-not-allowed"
                : "text-cyan-300 border-cyan-700/50 bg-cyan-900/30 hover:bg-cyan-800/40",
            )}
            title="Run build command then restart"
          >
            <Hammer className="w-3 h-3" />
            Rebuild
          </button>
        )}
        <span className="text-xs text-zinc-600">{lines.length} lines</span>
      </div>

      {/* Virtualized output area */}
      <div className="flex-1 font-mono text-xs leading-5 bg-zinc-950 overflow-auto">
        {searchAllSessions && searchText.trim() ? (
          // Global search results panel
          globalHits.length === 0 ? (
            <div className="flex items-center justify-center h-full text-zinc-600">
              No matches across sessions.
            </div>
          ) : (
            <div className="px-2 py-1">
              <div className="text-xs text-zinc-500 mb-2">
                {globalHits.length} hit{globalHits.length === 1 ? "" : "s"} across sessions
              </div>
              {globalHits.map((hit) => (
                <button
                  key={hit.id}
                  onClick={() => {
                    setSelectedSessionId(hit.session_id);
                    setSearchAllSessions(false);
                  }}
                  className={cn(
                    "block w-full text-left px-2 py-1 rounded hover:bg-zinc-900 transition-colors",
                    hit.stream === "stderr" ? "text-red-400/80" : "text-zinc-300",
                  )}
                >
                  <div className="text-zinc-600 text-[10px]">
                    {new Date(hit.timestamp).toLocaleString()} · {hit.process_name} ·{" "}
                    {hit.session_id.slice(0, 8)}
                  </div>
                  <div className="whitespace-nowrap overflow-hidden text-ellipsis">
                    {hit.line}
                  </div>
                </button>
              ))}
            </div>
          )
        ) : filteredLines.length === 0 ? (
          <div className="flex items-center justify-center h-full text-zinc-600">
            {lines.length === 0
              ? "No output yet. Start the process to see output."
              : "No matching lines."}
          </div>
        ) : (
          <List
            listRef={listRef}
            rowCount={filteredLines.length}
            rowHeight={LINE_HEIGHT}
            rowComponent={OutputRow}
            rowProps={{ lines: filteredLines }}
            overscanCount={20}
            onRowsRendered={handleRowsRendered}
            style={{ width: "100%", height: "100%" }}
          />
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
