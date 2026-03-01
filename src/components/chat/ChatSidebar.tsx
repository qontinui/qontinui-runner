import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface ChatSessionEntry {
  id: string;
  task_name: string;
  status: string;
  updated_at: string;
  created_at: string;
  is_live: boolean;
}

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: Record<string, unknown>;
}

interface ChatSidebarProps {
  activeSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onNewChat: () => void;
  /** Incremented externally to trigger a refresh (e.g. after create/close). */
  refreshKey?: number;
}

function formatRelativeTime(dateStr: string): string {
  try {
    // DB stores as "YYYY-MM-DD HH:MM:SS" (UTC)
    const date = new Date(dateStr.replace(" ", "T") + "Z");
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMin = Math.floor(diffMs / 60000);

    if (diffMin < 1) return "just now";
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffHr = Math.floor(diffMin / 60);
    if (diffHr < 24) return `${diffHr}h ago`;
    const diffDays = Math.floor(diffHr / 24);
    if (diffDays < 7) return `${diffDays}d ago`;
    return date.toLocaleDateString();
  } catch {
    return "";
  }
}

export function ChatSidebar({
  activeSessionId,
  onSelectSession,
  onNewChat,
  refreshKey,
}: ChatSidebarProps) {
  const [sessions, setSessions] = useState<ChatSessionEntry[]>([]);

  const fetchSessions = useCallback(async () => {
    try {
      const response = await invoke<CommandResponse>("list_chat_sessions");
      if (response.success && response.data?.sessions) {
        setSessions(response.data.sessions as ChatSessionEntry[]);
      }
    } catch (e) {
      console.error("[ChatSidebar] Failed to fetch sessions:", e);
    }
  }, []);

  // Fetch on mount and when refreshKey changes
  useEffect(() => {
    fetchSessions();
  }, [fetchSessions, refreshKey]);

  // Poll every 5 seconds to keep status up to date
  useEffect(() => {
    const interval = setInterval(fetchSessions, 5000);
    return () => clearInterval(interval);
  }, [fetchSessions]);

  return (
    <div className="w-60 h-full flex flex-col border-r border-border bg-background">
      {/* New Chat button */}
      <div className="p-3 border-b border-border">
        <button
          onClick={onNewChat}
          className="w-full flex items-center justify-center gap-2 px-3 py-2 rounded-lg bg-primary hover:bg-primary/90 text-white text-sm font-medium transition-colors"
        >
          <svg
            className="size-4"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
          >
            <path d="M12 5v14" />
            <path d="M5 12h14" />
          </svg>
          New Chat
        </button>
      </div>

      {/* Session list */}
      <div className="flex-1 overflow-y-auto">
        {sessions.length === 0 ? (
          <div className="p-4 text-center text-foreground/40 text-xs">No chat sessions yet</div>
        ) : (
          <div className="py-1">
            {sessions.map((session) => {
              const isActive = session.id === activeSessionId;
              const isLive = session.is_live;

              return (
                <button
                  key={session.id}
                  onClick={() => onSelectSession(session.id)}
                  className={`w-full text-left px-3 py-2.5 transition-colors ${
                    isActive
                      ? "bg-primary/15 border-l-2 border-primary"
                      : "hover:bg-foreground/5 border-l-2 border-transparent"
                  }`}
                >
                  <div className="flex items-center gap-2 min-w-0">
                    {/* Status dot */}
                    <span
                      className={`shrink-0 size-2 rounded-full ${
                        isLive
                          ? "bg-green-400"
                          : session.status === "failed"
                            ? "bg-red-400"
                            : "bg-foreground/30"
                      }`}
                    />
                    {/* Session name */}
                    <span
                      className={`truncate text-sm ${
                        isActive ? "text-foreground font-medium" : "text-foreground/70"
                      }`}
                    >
                      {session.task_name}
                    </span>
                  </div>
                  {/* Timestamp */}
                  <div className="mt-0.5 pl-4 text-xs text-foreground/40">
                    {formatRelativeTime(session.updated_at)}
                  </div>
                </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
