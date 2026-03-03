import { useState, useCallback } from "react";
import { X, Check, Wand2, User, Bot } from "lucide-react";
import type { TranscriptMessage } from "./useTranscriptSessions";

interface TranscriptContentPanelProps {
  sessionId: string;
  messages: TranscriptMessage[];
  loading: boolean;
  onGenerate: (description: string, inlineContext: string) => void;
  onClose: () => void;
}

export function TranscriptContentPanel({
  sessionId,
  messages,
  loading,
  onGenerate,
  onClose,
}: TranscriptContentPanelProps) {
  const [selectedMessageIds, setSelectedMessageIds] = useState<Set<string>>(
    () => new Set(messages.map((m) => m.uuid)),
  );
  const [description, setDescription] = useState("");

  // Keep selection in sync when messages change
  const messageUuids = messages.map((m) => m.uuid).join(",");
  const [lastMessageKey, setLastMessageKey] = useState(messageUuids);
  if (messageUuids !== lastMessageKey) {
    setLastMessageKey(messageUuids);
    setSelectedMessageIds(new Set(messages.map((m) => m.uuid)));
  }

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

  const selectPlansOnly = useCallback(() => {
    setSelectedMessageIds(new Set(messages.filter((m) => m.plan_content).map((m) => m.uuid)));
  }, [messages]);

  const handleGenerate = useCallback(() => {
    const selected = messages.filter((m) => selectedMessageIds.has(m.uuid));
    if (selected.length === 0) return;

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
  }, [messages, selectedMessageIds, description, onGenerate]);

  const planCount = messages.filter((m) => m.plan_content).length;

  return (
    <div className="w-[420px] h-full flex flex-col border-l border-[#2a2d3d] bg-[#1a1b26] shrink-0">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-[#2a2d3d]">
        <div className="flex items-center gap-2 min-w-0">
          <span className="text-xs font-semibold text-[#c0caf5] truncate">
            {sessionId.slice(0, 12)}...
          </span>
          <span className="text-[10px] text-[#565f89] shrink-0">{messages.length} messages</span>
        </div>
        <button
          onClick={onClose}
          className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors shrink-0"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Selection controls */}
      {!loading && messages.length > 0 && (
        <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d] text-xs">
          <span className="text-[#565f89]">
            {selectedMessageIds.size}/{messages.length}
          </span>
          <button
            onClick={selectAll}
            className="text-[#7aa2f7] hover:text-[#89b4fa] transition-colors"
          >
            Select All
          </button>
          {planCount > 0 && (
            <button
              onClick={selectPlansOnly}
              className="text-[#e0af68] hover:text-[#e8b96e] transition-colors"
            >
              Plans Only
            </button>
          )}
          <button
            onClick={selectNone}
            className="text-[#7aa2f7] hover:text-[#89b4fa] transition-colors"
          >
            Clear
          </button>
        </div>
      )}

      {/* Message list */}
      <div className="flex-1 overflow-y-auto scrollbar-dark px-3 py-2 min-h-0">
        {loading ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            <div className="w-3 h-3 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
            Loading messages...
          </div>
        ) : messages.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            No messages in this session
          </div>
        ) : (
          <div className="space-y-1.5">
            {messages.map((msg) => {
              const isSelected = selectedMessageIds.has(msg.uuid);
              const isUser = msg.msg_type === "user";

              return (
                <button
                  key={msg.uuid}
                  onClick={() => toggleMessage(msg.uuid)}
                  className={`
                    w-full text-left px-2.5 py-2 rounded border transition-colors
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
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              // One-click: select only this plan message
                              setSelectedMessageIds(new Set([msg.uuid]));
                            }}
                            className="text-[10px] px-1 rounded bg-[#e0af68]/10 text-[#e0af68] hover:bg-[#e0af68]/20 transition-colors"
                            title="Click to select only this plan"
                          >
                            plan
                          </button>
                        )}
                      </div>
                      <p className="text-[11px] text-[#a9b1d6] line-clamp-3 whitespace-pre-wrap">
                        {msg.plan_content || msg.text}
                      </p>
                    </div>
                  </div>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="px-3 py-2.5 border-t border-[#2a2d3d] space-y-2">
        <div>
          <label className="block text-[10px] text-[#565f89] mb-1">Description (optional)</label>
          <input
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Describe the workflow to generate..."
            className="w-full px-2.5 py-1.5 bg-[#13141f] border border-[#2a2d3d] rounded text-xs text-[#c0caf5] placeholder-[#414868] focus:border-[#7aa2f7] focus:outline-none transition-colors"
          />
        </div>

        <button
          onClick={handleGenerate}
          disabled={selectedMessageIds.size === 0}
          className={`
            w-full flex items-center justify-center gap-1.5 px-3 py-1.5 rounded text-xs font-medium transition-colors
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
  );
}
