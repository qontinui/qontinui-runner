import { RefreshCw } from "lucide-react";
import type { TranscriptSession } from "./useTranscriptSessions";

interface TranscriptSessionSidebarProps {
  sessions: TranscriptSession[];
  loading: boolean;
  selectedSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onRefresh: () => void;
}

function formatDate(iso: string): string {
  if (!iso) return "";
  try {
    const d = new Date(iso);
    return d.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return iso;
  }
}

export function TranscriptSessionSidebar({
  sessions,
  loading,
  selectedSessionId,
  onSelectSession,
  onRefresh,
}: TranscriptSessionSidebarProps) {
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
        {loading && sessions.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            <div className="w-3 h-3 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
            Loading...
          </div>
        ) : sessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#565f89] text-xs">No sessions found</div>
        ) : (
          sessions.map((session) => {
            const isActive = session.session_id === selectedSessionId;
            return (
              <button
                key={session.session_id}
                onClick={() => onSelectSession(session.session_id)}
                className={`
                  w-full text-left px-3 py-2 border-l-2 transition-colors
                  ${
                    isActive
                      ? "border-l-[#7aa2f7] bg-[#7aa2f7]/5"
                      : "border-l-transparent hover:bg-[#1a1b26]"
                  }
                `}
              >
                <div className="flex items-center gap-1.5 mb-0.5">
                  {session.has_plans && (
                    <span
                      className="w-1.5 h-1.5 rounded-full bg-[#e0af68] shrink-0"
                      title="Contains plan content"
                    />
                  )}
                  <span className="text-xs text-[#a9b1d6] truncate font-mono">
                    {session.session_id.slice(0, 12)}...
                  </span>
                </div>
                {session.first_message_preview && (
                  <p className="text-[11px] text-[#565f89] truncate mb-0.5">
                    &ldquo;{session.first_message_preview}&rdquo;
                  </p>
                )}
                <div className="flex items-center gap-1 text-[10px] text-[#414868]">
                  <span>{formatDate(session.last_modified)}</span>
                  <span>&middot;</span>
                  <span>{session.message_count} records</span>
                </div>
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}
