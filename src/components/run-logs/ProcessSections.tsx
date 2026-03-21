import { useState, useMemo, Fragment } from "react";
import {
  Loader2,
  AlertCircle,
  Activity,
  ChevronDown,
  ChevronRight,
  Clock,
  CheckCircle,
  XCircle,
  Timer,
  Terminal,
  Search,
} from "lucide-react";
import { useProcessSessions, useProcessSessionOutput } from "../../hooks/useAiData";
import type { ProcessSession } from "../../types/aiData";
import { getAccentColors } from "@/design-system";
import { formatTimestamp, formatSessionDuration } from "./ai-data-viewer-utils";

function ProcessSessionDetailPanel({ session }: { session: ProcessSession }) {
  const stateIcon =
    session.state === "running" ? (
      <Activity className="w-3.5 h-3.5 text-green-500" />
    ) : session.state === "failed" ? (
      <XCircle className="w-3.5 h-3.5 text-red-500" />
    ) : session.state === "stopped" ? (
      <CheckCircle className="w-3.5 h-3.5 text-muted-foreground" />
    ) : (
      <Clock className="w-3.5 h-3.5 text-muted-foreground" />
    );

  const stateBadgeClass =
    session.state === "running"
      ? "bg-green-500/10 text-green-500"
      : session.state === "failed"
        ? "bg-red-500/10 text-red-500"
        : "bg-muted text-muted-foreground";

  return (
    <div className="border-t border-border bg-muted/30 px-4 py-3 space-y-3">
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3 text-xs">
        <div>
          <span className="text-muted-foreground block mb-0.5">Process</span>
          <span className="font-medium flex items-center gap-1.5">
            <Terminal className="w-3 h-3 text-muted-foreground" />
            {session.process_name}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">State</span>
          <span
            className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 font-medium ${stateBadgeClass}`}
          >
            {stateIcon}
            {session.state}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Duration</span>
          <span className="font-mono flex items-center gap-1.5">
            <Timer className="w-3 h-3 text-muted-foreground" />
            {formatSessionDuration(session.started_at, session.stopped_at)}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Exit Code</span>
          <span
            className={`font-mono ${
              session.exit_code !== null && session.exit_code !== 0 ? "text-red-500" : ""
            }`}
          >
            {session.exit_code !== null ? session.exit_code : "\u2014"}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Started</span>
          <span className="font-mono">{formatTimestamp(session.started_at)}</span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Stopped</span>
          <span className="font-mono">
            {session.stopped_at ? formatTimestamp(session.stopped_at) : "\u2014"}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Errors</span>
          <span className={session.error_count > 0 ? "text-red-500 font-medium" : ""}>
            {session.error_count > 0 ? (
              <span className="flex items-center gap-1">
                <AlertCircle className="w-3 h-3" />
                {session.error_count}
              </span>
            ) : (
              "0"
            )}
          </span>
        </div>
        <div>
          <span className="text-muted-foreground block mb-0.5">Session ID</span>
          <span
            className="font-mono text-[10px] text-muted-foreground truncate block"
            title={session.id}
          >
            {session.id}
          </span>
        </div>
      </div>

      <div className="border-t border-border pt-2">
        <ProcessSessionOutputSection sessionId={session.id} compact />
      </div>
    </div>
  );
}

export function ProcessSessionsSection({
  onSelectSession,
}: {
  onSelectSession?: (sessionId: string) => void;
}) {
  const { data: sessions, isLoading, error } = useProcessSessions();
  const [expandedSessionId, setExpandedSessionId] = useState<string | null>(null);

  if (isLoading) {
    return (
      <div className="p-4 text-muted-foreground flex items-center gap-2">
        <Loader2 className="w-4 h-4 animate-spin" />
        Loading sessions...
      </div>
    );
  }
  if (error) {
    return <div className="p-4 text-red-500">Error: {String(error)}</div>;
  }
  if (!sessions || sessions.length === 0) {
    return <div className="p-4 text-muted-foreground">No process sessions found.</div>;
  }

  const handleRowClick = (session: ProcessSession) => {
    setExpandedSessionId((prev) => (prev === session.id ? null : session.id));
    onSelectSession?.(session.id);
  };

  return (
    <div className="space-y-1">
      <h3 className="px-4 pt-4 pb-2 text-sm font-medium text-muted-foreground">
        Process Sessions ({sessions.length})
      </h3>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b text-left text-muted-foreground">
              <th className="px-2 py-2 w-8"></th>
              <th className="px-4 py-2">Process</th>
              <th className="px-4 py-2">Started</th>
              <th className="px-4 py-2">Duration</th>
              <th className="px-4 py-2">State</th>
              <th className="px-4 py-2">Exit Code</th>
              <th className="px-4 py-2">Errors</th>
            </tr>
          </thead>
          <tbody>
            {sessions.map((session) => {
              const isExpanded = expandedSessionId === session.id;
              return (
                <Fragment key={session.id}>
                  <tr
                    className={`border-b hover:bg-muted/50 cursor-pointer ${
                      isExpanded ? "bg-muted/30" : ""
                    }`}
                    onClick={() => handleRowClick(session)}
                  >
                    <td className="px-2 py-2">
                      {isExpanded ? (
                        <ChevronDown className="w-4 h-4 text-muted-foreground" />
                      ) : (
                        <ChevronRight className="w-4 h-4 text-muted-foreground" />
                      )}
                    </td>
                    <td className="px-4 py-2 font-medium">{session.process_name}</td>
                    <td className="px-4 py-2 text-muted-foreground">
                      {formatTimestamp(session.started_at)}
                    </td>
                    <td className="px-4 py-2 text-muted-foreground font-mono text-xs">
                      {formatSessionDuration(session.started_at, session.stopped_at)}
                    </td>
                    <td className="px-4 py-2">
                      <span
                        className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${
                          session.state === "running"
                            ? "bg-green-500/10 text-green-500"
                            : session.state === "failed"
                              ? "bg-red-500/10 text-red-500"
                              : "bg-muted text-muted-foreground"
                        }`}
                      >
                        {session.state}
                      </span>
                    </td>
                    <td className="px-4 py-2 font-mono">
                      {session.exit_code !== null ? session.exit_code : "\u2014"}
                    </td>
                    <td className="px-4 py-2">
                      {session.error_count > 0 ? (
                        <span className="text-red-500">{session.error_count}</span>
                      ) : (
                        "0"
                      )}
                    </td>
                  </tr>
                  {isExpanded && (
                    <tr>
                      <td colSpan={7} className="p-0">
                        <ProcessSessionDetailPanel session={session} />
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export function ProcessSessionOutputSection({
  sessionId,
  compact = false,
}: {
  sessionId: string | null;
  compact?: boolean;
}) {
  const { data: lines, isLoading, error } = useProcessSessionOutput(sessionId);
  const [streamFilter, setStreamFilter] = useState<"all" | "stdout" | "stderr">("all");
  const [searchText, setSearchText] = useState("");

  const filteredLines = useMemo(() => {
    if (!lines) return [];
    return lines.filter((line) => {
      if (streamFilter !== "all" && line.stream !== streamFilter) return false;
      if (searchText && !line.line.toLowerCase().includes(searchText.toLowerCase())) return false;
      return true;
    });
  }, [lines, streamFilter, searchText]);

  const stdoutCount = useMemo(
    () => lines?.filter((l) => l.stream === "stdout").length ?? 0,
    [lines],
  );
  const stderrCount = useMemo(
    () => lines?.filter((l) => l.stream === "stderr").length ?? 0,
    [lines],
  );

  if (!sessionId) {
    return (
      <div className={`${compact ? "p-2" : "p-4"} text-muted-foreground`}>
        Select a session from the Sessions tab to view output.
      </div>
    );
  }
  if (isLoading) {
    return (
      <div className={`${compact ? "p-2" : "p-4"} text-muted-foreground flex items-center gap-2`}>
        <Loader2 className="w-3 h-3 animate-spin" />
        Loading output...
      </div>
    );
  }
  if (error) {
    return <div className={`${compact ? "p-2" : "p-4"} text-red-500`}>Error: {String(error)}</div>;
  }
  if (!lines || lines.length === 0) {
    return (
      <div className={`${compact ? "p-2" : "p-4"} text-muted-foreground`}>
        No output captured for this session.
      </div>
    );
  }

  const isFiltered = streamFilter !== "all" || searchText.length > 0;

  return (
    <div className={compact ? "flex flex-col" : "flex flex-col h-full"}>
      <div className={compact ? "pb-1 space-y-2" : "px-4 pt-4 pb-2 space-y-3"}>
        <h3 className="text-sm font-medium text-muted-foreground">
          Session Output
          {isFiltered
            ? ` \u2014 Showing ${filteredLines.length} of ${lines.length} lines`
            : ` (${lines.length} lines)`}
        </h3>

        <div className="flex flex-wrap items-center gap-3">
          <div className="flex gap-1.5">
            <button
              onClick={() => setStreamFilter("all")}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                streamFilter === "all"
                  ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text} border ${getAccentColors("blue").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              All ({lines.length})
            </button>
            <button
              onClick={() => setStreamFilter("stdout")}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                streamFilter === "stdout"
                  ? `${getAccentColors("green").bg} ${getAccentColors("green").text} border ${getAccentColors("green").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              stdout ({stdoutCount})
            </button>
            <button
              onClick={() => setStreamFilter("stderr")}
              className={`px-3 py-1.5 text-xs font-medium rounded-md transition-colors ${
                streamFilter === "stderr"
                  ? `${getAccentColors("red").bg} ${getAccentColors("red").text} border ${getAccentColors("red").border}`
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              stderr ({stderrCount})
            </button>
          </div>

          <div className="relative flex-1 min-w-[200px]">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground pointer-events-none" />
            <input
              type="text"
              placeholder="Filter lines by text..."
              value={searchText}
              onChange={(e) => setSearchText(e.target.value)}
              className="w-full pl-8 pr-3 py-1.5 text-xs rounded-md border border-border bg-muted/50 text-foreground placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-ring"
            />
            {searchText && (
              <button
                onClick={() => setSearchText("")}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground text-xs"
                title="Clear search"
              >
                &times;
              </button>
            )}
          </div>
        </div>
      </div>

      <div className={compact ? "overflow-auto" : "flex-1 overflow-auto px-4 pb-4"}>
        {filteredLines.length === 0 ? (
          <div className="text-sm text-muted-foreground py-4">
            No lines match the current filters.
          </div>
        ) : (
          <pre
            className={`text-xs font-mono bg-black/80 text-green-400 rounded-lg overflow-auto ${
              compact ? "p-2 max-h-[40vh]" : "p-4 max-h-[70vh]"
            }`}
          >
            {filteredLines.map((line) => (
              <div key={line.id} className={line.stream === "stderr" ? "text-red-400" : ""}>
                <span className="text-muted-foreground select-none">
                  {new Date(line.timestamp).toLocaleTimeString()}{" "}
                </span>
                {line.line}
              </div>
            ))}
          </pre>
        )}
      </div>
    </div>
  );
}
