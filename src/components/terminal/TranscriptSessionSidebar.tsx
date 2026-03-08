import { useMemo } from "react";
import { RefreshCw, TerminalSquare } from "lucide-react";
import type { TranscriptSession } from "./useTranscriptSessions";

interface TranscriptSessionSidebarProps {
  sessions: TranscriptSession[];
  loading: boolean;
  selectedSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onRefresh: () => void;
  onResume: (session: TranscriptSession) => void;
}

function formatTime(iso: string): string {
  if (!iso) return "";
  try {
    const d = new Date(iso);
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}

/** Get a date-only key like "2026-03-07" for grouping. */
function dateKey(iso: string): string {
  if (!iso) return "";
  try {
    const d = new Date(iso);
    return d.toISOString().slice(0, 10);
  } catch {
    return "";
  }
}

/** Format a date key as a human-readable label (e.g. "Today", "Yesterday", "Mar 5"). */
function dateLabel(key: string): string {
  if (!key) return "";
  const d = new Date(key + "T00:00:00");
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const target = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = (today.getTime() - target.getTime()) / 86400000;

  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Extract just the last path segment for a compact project label. */
function projectLabel(projectPath: string): string {
  if (!projectPath) return "";
  const normalized = projectPath.replace(/\\/g, "/").replace(/\/$/, "");
  const last = normalized.split("/").pop() ?? "";
  return last;
}

export function TranscriptSessionSidebar({
  sessions,
  loading,
  selectedSessionId,
  onSelectSession,
  onRefresh,
  onResume,
}: TranscriptSessionSidebarProps) {
  // Filter out empty sessions and compute date groups
  const filtered = useMemo(() => sessions.filter((s) => s.message_count > 0), [sessions]);

  return (
    <div className="w-[280px] h-full flex flex-col border-r border-[#2a2d3d] bg-[#13141f] shrink-0">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-[#2a2d3d]">
        <span className="text-xs font-semibold text-[#c0caf5]">Claude Code Sessions</span>
        <button
          onClick={onRefresh}
          disabled={loading}
          className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors"
          title="Refresh sessions"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Session list */}
      <div className="flex-1 overflow-y-auto scrollbar-dark">
        {loading && filtered.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            <div className="w-3 h-3 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
            Loading...
          </div>
        ) : filtered.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#565f89] text-xs">No sessions found</div>
        ) : (
          (() => {
            let lastDateKey = "";
            return filtered.map((session) => {
              const isActive = session.session_id === selectedSessionId;
              const dk = dateKey(session.last_modified);
              const showSeparator = dk !== lastDateKey;
              lastDateKey = dk;

              return (
                <div key={session.session_id}>
                  {/* Date separator */}
                  {showSeparator && (
                    <div className="flex items-center gap-2 px-3 pt-3 pb-1">
                      <div className="flex-1 h-px bg-[#2a2d3d]" />
                      <span className="text-[10px] font-medium text-[#565f89] whitespace-nowrap">
                        {dateLabel(dk)}
                      </span>
                      <div className="flex-1 h-px bg-[#2a2d3d]" />
                    </div>
                  )}

                  <div
                    data-ui-bridge-id={`session-${session.session_id}`}
                    className={`
                      group relative border-l-2 transition-colors
                      ${
                        isActive
                          ? "border-l-[#7aa2f7] bg-[#7aa2f7]/5"
                          : "border-l-transparent hover:bg-[#1a1b26]"
                      }
                    `}
                  >
                    <button
                      onClick={() => onSelectSession(session.session_id)}
                      className="w-full text-left px-3 py-2 pr-8"
                      title={`Session ${session.session_id}`}
                    >
                      <div className="flex items-center gap-1.5 mb-0.5">
                        {session.has_plans && (
                          <span
                            className="w-1.5 h-1.5 rounded-full bg-[#e0af68] shrink-0"
                            title="Contains plan content"
                          />
                        )}
                        <span className="text-xs text-[#a9b1d6] truncate">
                          {session.display_name}
                        </span>
                      </div>
                      <div className="flex items-center gap-1 text-[10px] text-[#414868]">
                        <span>{formatTime(session.last_modified)}</span>
                        <span>&middot;</span>
                        <span>{session.message_count} msgs</span>
                        <span>&middot;</span>
                        <span className="truncate">{projectLabel(session.project_path)}</span>
                      </div>
                    </button>

                    {/* Quick-resume button — visible on hover */}
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        onResume(session);
                      }}
                      className="absolute right-2 top-1/2 -translate-y-1/2 p-1 rounded opacity-0 group-hover:opacity-100 text-[#565f89] hover:text-[#9ece6a] hover:bg-[#9ece6a]/10 transition-all"
                      title={`Resume: claude --resume ${session.session_id}`}
                    >
                      <TerminalSquare className="w-3.5 h-3.5" />
                    </button>
                  </div>
                </div>
              );
            });
          })()
        )}
      </div>
    </div>
  );
}
