import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, FileText, ChevronDown, Check, Wand2, User, Bot } from "lucide-react";

interface TranscriptSession {
  session_id: string;
  project_path: string;
  config_dir: string;
  message_count: number;
  last_modified: string;
}

interface TranscriptMessage {
  uuid: string;
  msg_type: string;
  timestamp: string;
  text: string;
  plan_content: string | null;
  model: string | null;
  has_tool_use: boolean;
}

interface CommandResponse {
  success: boolean;
  message: string | null;
  data: unknown;
}

interface TranscriptImportDialogProps {
  open: boolean;
  onClose: () => void;
  onGenerate: (description: string, inlineContext: string) => void;
}

export function TranscriptImportDialog({ open, onClose, onGenerate }: TranscriptImportDialogProps) {
  const [sessions, setSessions] = useState<TranscriptSession[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<TranscriptMessage[]>([]);
  const [selectedMessageIds, setSelectedMessageIds] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [description, setDescription] = useState("");
  const [showSessionDropdown, setShowSessionDropdown] = useState(false);

  // Load sessions on open
  useEffect(() => {
    if (!open) return;

    const loadSessions = async () => {
      setLoading(true);
      setError(null);
      try {
        // Load latest session and all sessions in parallel
        const [latestResult, listResult] = await Promise.all([
          invoke<CommandResponse>("transcript_get_latest"),
          invoke<CommandResponse>("transcript_list_sessions"),
        ]);

        if (listResult.success && listResult.data) {
          const sessionList = listResult.data as TranscriptSession[];
          setSessions(sessionList);

          // Pre-select the latest session
          if (latestResult.success && latestResult.data) {
            const latest = latestResult.data as TranscriptSession;
            setSelectedSessionId(latest.session_id);
          } else if (sessionList.length > 0) {
            setSelectedSessionId(sessionList[0].session_id);
          }
        } else {
          setError(listResult.message || "Failed to load sessions");
        }
      } catch (err) {
        setError(`Failed to load sessions: ${err}`);
      } finally {
        setLoading(false);
      }
    };

    loadSessions();
  }, [open]);

  // Load messages when session changes
  useEffect(() => {
    if (!selectedSessionId) {
      setMessages([]);
      return;
    }

    const loadMessages = async () => {
      setLoadingMessages(true);
      setError(null);
      try {
        const result = await invoke<CommandResponse>("transcript_read_session", {
          sessionId: selectedSessionId,
        });

        if (result.success && result.data) {
          const msgs = result.data as TranscriptMessage[];
          setMessages(msgs);
          // Select all messages by default
          setSelectedMessageIds(new Set(msgs.map((m) => m.uuid)));
        } else {
          setError(result.message || "Failed to load messages");
          setMessages([]);
        }
      } catch (err) {
        setError(`Failed to load messages: ${err}`);
        setMessages([]);
      } finally {
        setLoadingMessages(false);
      }
    };

    loadMessages();
  }, [selectedSessionId]);

  // Reset state on close
  useEffect(() => {
    if (!open) {
      setSelectedSessionId(null);
      setMessages([]);
      setSelectedMessageIds(new Set());
      setDescription("");
      setError(null);
    }
  }, [open]);

  const toggleMessage = useCallback((uuid: string) => {
    setSelectedMessageIds((prev) => {
      const next = new Set(prev);
      if (next.has(uuid)) {
        next.delete(uuid);
      } else {
        next.add(uuid);
      }
      return next;
    });
  }, []);

  const selectAll = useCallback(() => {
    setSelectedMessageIds(new Set(messages.map((m) => m.uuid)));
  }, [messages]);

  const selectNone = useCallback(() => {
    setSelectedMessageIds(new Set());
  }, []);

  const handleGenerate = useCallback(() => {
    const selected = messages.filter((m) => selectedMessageIds.has(m.uuid));
    if (selected.length === 0) return;

    // Build inline context from selected messages
    const parts = selected.map((msg) => {
      const role = msg.msg_type === "user" ? "User" : "Assistant";
      const planPart = msg.plan_content ? `## ${role} (Plan)\n\n${msg.plan_content}\n\n` : "";
      const textPart = msg.text.trim() ? `## ${role}\n\n${msg.text}` : "";
      return `${planPart}${textPart}`.trim();
    });

    const inlineContext = parts.filter(Boolean).join("\n\n---\n\n");
    const desc =
      description.trim() ||
      `Generate workflow from Claude Code transcript (${selected.length} messages)`;

    onGenerate(desc, inlineContext);
    onClose();
  }, [messages, selectedMessageIds, description, onGenerate, onClose]);

  if (!open) return null;

  const selectedSession = sessions.find((s) => s.session_id === selectedSessionId);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[700px] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[#2a2d3d]">
          <div className="flex items-center gap-2">
            <FileText className="w-4 h-4 text-[#9ece6a]" />
            <h2 className="text-sm font-semibold text-[#c0caf5]">
              Import from Claude Code Transcript
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Session Selector */}
        <div className="px-4 py-3 border-b border-[#2a2d3d]">
          <label className="block text-xs text-[#565f89] mb-1.5">Session</label>
          <div className="relative">
            <button
              onClick={() => setShowSessionDropdown(!showSessionDropdown)}
              disabled={loading}
              className="w-full flex items-center justify-between px-3 py-1.5 bg-[#13141f] border border-[#2a2d3d] rounded text-sm text-[#c0caf5] hover:border-[#414868] transition-colors"
            >
              {loading ? (
                <span className="text-[#565f89]">Loading sessions...</span>
              ) : selectedSession ? (
                <span className="truncate">
                  {selectedSession.session_id.slice(0, 8)}... —{" "}
                  {formatDate(selectedSession.last_modified)} ({selectedSession.message_count}{" "}
                  records)
                </span>
              ) : (
                <span className="text-[#565f89]">
                  {sessions.length === 0 ? "No sessions found" : "Select a session"}
                </span>
              )}
              <ChevronDown className="w-3.5 h-3.5 text-[#565f89] shrink-0 ml-2" />
            </button>

            {showSessionDropdown && sessions.length > 0 && (
              <div className="absolute z-10 mt-1 w-full bg-[#13141f] border border-[#2a2d3d] rounded shadow-lg max-h-48 overflow-y-auto scrollbar-dark">
                {sessions.map((session) => (
                  <button
                    key={session.session_id}
                    onClick={() => {
                      setSelectedSessionId(session.session_id);
                      setShowSessionDropdown(false);
                    }}
                    className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs text-left hover:bg-[#1a1b26] transition-colors ${
                      session.session_id === selectedSessionId
                        ? "text-[#7aa2f7] bg-[#7aa2f7]/5"
                        : "text-[#a9b1d6]"
                    }`}
                  >
                    {session.session_id === selectedSessionId && (
                      <Check className="w-3 h-3 shrink-0" />
                    )}
                    <span className="truncate">
                      {session.session_id.slice(0, 8)}... — {formatDate(session.last_modified)} (
                      {session.message_count} records)
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        {/* Messages */}
        <div className="flex-1 overflow-y-auto scrollbar-dark px-4 py-2 min-h-0">
          {loadingMessages ? (
            <div className="flex items-center justify-center py-8 text-[#565f89] text-sm">
              <div className="w-4 h-4 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
              Loading messages...
            </div>
          ) : messages.length === 0 ? (
            <div className="flex items-center justify-center py-8 text-[#565f89] text-sm">
              {selectedSessionId
                ? "No messages found in this session"
                : "Select a session to view messages"}
            </div>
          ) : (
            <>
              {/* Selection controls */}
              <div className="flex items-center gap-3 mb-2 text-xs text-[#565f89]">
                <span>
                  {selectedMessageIds.size} of {messages.length} selected
                </span>
                <button
                  onClick={selectAll}
                  className="text-[#7aa2f7] hover:text-[#89b4fa] transition-colors"
                >
                  Select All
                </button>
                <button
                  onClick={selectNone}
                  className="text-[#7aa2f7] hover:text-[#89b4fa] transition-colors"
                >
                  Clear
                </button>
              </div>

              {/* Message list */}
              <div className="space-y-1.5">
                {messages.map((msg) => {
                  const isSelected = selectedMessageIds.has(msg.uuid);
                  const isUser = msg.msg_type === "user";

                  return (
                    <button
                      key={msg.uuid}
                      onClick={() => toggleMessage(msg.uuid)}
                      className={`
                        w-full text-left px-3 py-2 rounded border transition-colors
                        ${
                          isSelected
                            ? "border-[#7aa2f7]/40 bg-[#7aa2f7]/5"
                            : "border-[#2a2d3d] bg-[#13141f] hover:border-[#414868]"
                        }
                      `}
                    >
                      <div className="flex items-start gap-2">
                        {/* Checkbox */}
                        <div
                          className={`
                            mt-0.5 w-3.5 h-3.5 rounded border shrink-0 flex items-center justify-center
                            ${isSelected ? "bg-[#7aa2f7] border-[#7aa2f7]" : "border-[#414868]"}
                          `}
                        >
                          {isSelected && <Check className="w-2.5 h-2.5 text-white" />}
                        </div>

                        {/* Icon */}
                        <div className="shrink-0 mt-0.5">
                          {isUser ? (
                            <User className="w-3.5 h-3.5 text-[#9ece6a]" />
                          ) : (
                            <Bot className="w-3.5 h-3.5 text-[#bb9af7]" />
                          )}
                        </div>

                        {/* Content */}
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-2 mb-0.5">
                            <span
                              className={`text-xs font-medium ${
                                isUser ? "text-[#9ece6a]" : "text-[#bb9af7]"
                              }`}
                            >
                              {isUser ? "User" : "Assistant"}
                            </span>
                            {msg.model && (
                              <span className="text-[10px] text-[#414868]">{msg.model}</span>
                            )}
                            {msg.has_tool_use && (
                              <span className="text-[10px] px-1 rounded bg-[#414868]/30 text-[#565f89]">
                                tools
                              </span>
                            )}
                            {msg.plan_content && (
                              <span className="text-[10px] px-1 rounded bg-[#e0af68]/10 text-[#e0af68]">
                                plan
                              </span>
                            )}
                          </div>
                          <p className="text-xs text-[#a9b1d6] line-clamp-3 whitespace-pre-wrap">
                            {msg.plan_content || msg.text}
                          </p>
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-3 border-t border-[#2a2d3d] space-y-2">
          {error && (
            <div className="text-xs text-[#f7768e] bg-[#f7768e]/10 rounded px-2 py-1">{error}</div>
          )}

          <div>
            <label className="block text-xs text-[#565f89] mb-1">
              Workflow description (optional)
            </label>
            <input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Describe the workflow to generate..."
              className="w-full px-3 py-1.5 bg-[#13141f] border border-[#2a2d3d] rounded text-sm text-[#c0caf5] placeholder-[#414868] focus:border-[#7aa2f7] focus:outline-none transition-colors"
            />
          </div>

          <div className="flex items-center justify-end gap-2">
            <button
              onClick={onClose}
              className="px-3 py-1.5 rounded text-xs font-medium text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
            >
              Cancel
            </button>
            <button
              onClick={handleGenerate}
              disabled={selectedMessageIds.size === 0}
              className={`
                flex items-center gap-1.5 px-3 py-1.5 rounded text-xs font-medium transition-colors
                ${
                  selectedMessageIds.size > 0
                    ? "bg-[#7aa2f7] text-white hover:bg-[#89b4fa]"
                    : "bg-[#2a2d3d] text-[#414868] cursor-not-allowed"
                }
              `}
            >
              <Wand2 className="w-3 h-3" />
              Generate Workflow ({selectedMessageIds.size} messages)
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function formatDate(iso: string): string {
  if (!iso) return "Unknown";
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
