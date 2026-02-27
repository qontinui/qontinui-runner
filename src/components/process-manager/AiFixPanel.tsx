import { useState, useEffect, useRef, useCallback } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { X, Send, Square } from "lucide-react";
import { cn } from "../../lib/utils";
import type { ChatMessage, ChatSessionState } from "@qontinui/shared-types";

interface AiFixPanelProps {
  messages: ChatMessage[];
  streamingContent: string;
  sessionState: ChatSessionState;
  processName: string;
  onSendFollowUp: (message: string) => void;
  onInterrupt: () => void;
  onClose: () => void;
}

export function AiFixPanel({
  messages,
  streamingContent,
  sessionState,
  processName,
  onSendFollowUp,
  onInterrupt,
  onClose,
}: AiFixPanelProps) {
  const [input, setInput] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);
  const inputRef = useRef<HTMLInputElement>(null);

  // Auto-scroll on new content
  useEffect(() => {
    if (autoScroll && scrollRef.current) {
      requestAnimationFrame(() => {
        const el = scrollRef.current;
        if (el) el.scrollTop = el.scrollHeight;
      });
    }
  }, [messages, streamingContent, autoScroll]);

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setAutoScroll(nearBottom);
  }, []);

  const handleSend = useCallback(() => {
    const trimmed = input.trim();
    if (!trimmed) return;
    onSendFollowUp(trimmed);
    setInput("");
  }, [input, onSendFollowUp]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  const isProcessing = sessionState === "processing";
  const isReady = sessionState === "ready";
  const canSend = isReady && input.trim().length > 0;

  // Skip the auto-generated prompt if present (first user message), show everything else
  const visibleMessages =
    messages.length > 0 && messages[0].role === "user" ? messages.slice(1) : messages;
  const hasAiContent = visibleMessages.some((m) => m.role === "ai");

  return (
    <div className="flex flex-col h-full border-t border-white/10 bg-zinc-950">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-white/10 bg-zinc-900/50">
        <div className="flex items-center gap-2 min-w-0">
          <span className="text-xs font-medium text-amber-300 truncate">
            AI Analysis: {processName}
          </span>
          <StatusBadge state={sessionState} />
        </div>
        <button
          onClick={onClose}
          className="p-0.5 text-zinc-500 hover:text-zinc-300 transition-colors"
          title="Close AI panel"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Message area */}
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto px-3 py-2 space-y-3 min-h-0"
      >
        {!hasAiContent && !streamingContent && (
          <div className="flex items-center justify-center h-full">
            <div className="flex items-center gap-1.5">
              <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:0ms]" />
              <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:150ms]" />
              <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:300ms]" />
              <span className="text-xs text-zinc-500 ml-2">Analyzing errors...</span>
            </div>
          </div>
        )}

        {visibleMessages.map((msg, i) =>
          msg.role === "ai" ? (
            <div
              key={i}
              className="prose prose-invert prose-sm max-w-none text-xs [&_pre]:bg-black/30 [&_pre]:rounded-md [&_pre]:p-2 [&_pre]:overflow-x-auto [&_code]:text-amber-300 [&_code]:text-xs [&_a]:text-cyan-400 [&_h1]:text-sm [&_h2]:text-sm [&_h3]:text-xs [&_p]:text-xs [&_li]:text-xs [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1"
            >
              <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
            </div>
          ) : (
            <div key={i} className="flex justify-end">
              <div className="max-w-[85%] rounded-md px-3 py-1.5 bg-cyan-900/30 border border-cyan-700/30 text-xs text-zinc-300">
                {msg.content}
              </div>
            </div>
          ),
        )}

        {/* Streaming content */}
        {isProcessing && streamingContent && (
          <div className="prose prose-invert prose-sm max-w-none text-xs [&_pre]:bg-black/30 [&_pre]:rounded-md [&_pre]:p-2 [&_pre]:overflow-x-auto [&_code]:text-amber-300 [&_code]:text-xs [&_a]:text-cyan-400 [&_h1]:text-sm [&_h2]:text-sm [&_h3]:text-xs [&_p]:text-xs [&_li]:text-xs [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{streamingContent}</ReactMarkdown>
            <span className="inline-block w-1.5 h-3 bg-amber-400 animate-pulse ml-0.5" />
          </div>
        )}

        {/* Processing with no content yet (after initial response) */}
        {isProcessing && !streamingContent && hasAiContent && (
          <div className="flex items-center gap-1.5 py-1">
            <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:0ms]" />
            <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:150ms]" />
            <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:300ms]" />
          </div>
        )}
      </div>

      {/* Input bar */}
      <div className="flex items-center gap-2 px-3 py-2 border-t border-white/10">
        <input
          ref={inputRef}
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={isProcessing ? "AI is analyzing..." : "Ask a follow-up question..."}
          disabled={!isReady}
          className="flex-1 px-2 py-1 text-xs bg-zinc-900 border border-white/10 rounded text-zinc-300 placeholder:text-zinc-600 focus:outline-none focus:border-amber-500/50 disabled:opacity-50"
        />
        {isProcessing ? (
          <button
            onClick={onInterrupt}
            className="p-1.5 text-red-400 hover:text-red-300 hover:bg-red-500/10 rounded transition-colors"
            title="Stop"
          >
            <Square className="w-3.5 h-3.5" />
          </button>
        ) : (
          <button
            onClick={handleSend}
            disabled={!canSend}
            className={cn(
              "p-1.5 rounded transition-colors",
              canSend
                ? "text-amber-300 hover:text-amber-200 hover:bg-amber-500/10"
                : "text-zinc-600 cursor-not-allowed",
            )}
            title="Send"
          >
            <Send className="w-3.5 h-3.5" />
          </button>
        )}
      </div>
    </div>
  );
}

function StatusBadge({ state }: { state: ChatSessionState }) {
  const config: Record<string, { label: string; color: string }> = {
    disconnected: { label: "disconnected", color: "text-zinc-500" },
    initializing: { label: "starting...", color: "text-amber-400" },
    ready: { label: "ready", color: "text-green-400" },
    processing: { label: "analyzing...", color: "text-amber-400" },
    restoring: { label: "restoring...", color: "text-blue-400" },
    closed: { label: "closed", color: "text-zinc-500" },
  };

  const { label, color } = config[state] ?? { label: state, color: "text-zinc-500" };

  return <span className={cn("text-[10px]", color)}>{label}</span>;
}
