import { useState, useEffect, useRef, useCallback } from "react";
import { TerminalSquare, Eye, Copy, AlertTriangle, Pin, Tag } from "lucide-react";
import type { UnifiedSession, SessionLiveStatus } from "./useSessionManager";

interface SessionCardProps {
  session: UnifiedSession;
  isSelected: boolean;
  isChecked: boolean;
  selectionMode: boolean;
  isPinned: boolean;
  sessionLabel: string | null;
  onResume: (session: UnifiedSession) => void;
  onViewTranscript: (session: UnifiedSession) => void;
  onCopyId: (session: UnifiedSession) => void;
  onToggleSelect: (sessionId: string) => void;
  onTogglePin: (sessionId: string) => void;
  onSetLabel: (sessionId: string, label: string | null) => void;
}

const STATUS_DOT: Record<SessionLiveStatus, { color: string; pulse: boolean; label: string }> = {
  "active-in-zone": { color: "#9ece6a", pulse: true, label: "Active in zone" },
  "active-external": { color: "#73daca", pulse: true, label: "Active externally" },
  frozen: { color: "#f7768e", pulse: false, label: "Frozen" },
  "needs-input": { color: "#e0af68", pulse: true, label: "Needs input" },
  completed: { color: "#565f89", pulse: false, label: "Completed" },
  error: { color: "#f7768e", pulse: false, label: "Error" },
  dormant: { color: "#414868", pulse: false, label: "Dormant" },
};

const ACCOUNT_BADGE_COLORS: Record<string, { bg: string; text: string }> = {
  gmail: { bg: "bg-[#7aa2f7]/10", text: "text-[#7aa2f7]" },
  hotmail: { bg: "bg-[#bb9af7]/10", text: "text-[#bb9af7]" },
};

function formatTimeAgo(iso: string): string {
  if (!iso) return "";
  try {
    const diff = Date.now() - new Date(iso).getTime();
    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return "just now";
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days === 1) return "yesterday";
    return `${days}d ago`;
  } catch {
    return "";
  }
}

function formatDuration(ms: number | null): string | null {
  if (ms == null || ms <= 0) return null;
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remainMin = minutes % 60;
  if (hours < 24) return `${hours}h${remainMin > 0 ? `${remainMin}m` : ""}`;
  const days = Math.floor(hours / 24);
  return `${days}d${hours % 24 > 0 ? `${hours % 24}h` : ""}`;
}

export function SessionCard({
  session,
  isSelected,
  isChecked,
  selectionMode,
  isPinned,
  sessionLabel,
  onResume,
  onViewTranscript,
  onCopyId,
  onToggleSelect,
  onTogglePin,
  onSetLabel,
}: SessionCardProps) {
  const statusInfo = STATUS_DOT[session.liveStatus];
  const accountBadge = ACCOUNT_BADGE_COLORS[session.accountLabel];
  const durationLabel = formatDuration(session.durationMs);

  // Context menu
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [labelInput, setLabelInput] = useState("");
  const [showLabelInput, setShowLabelInput] = useState(false);
  const contextRef = useRef<HTMLDivElement>(null);

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY });
  }, []);

  // Close on click outside
  useEffect(() => {
    if (!contextMenu) return;
    const handler = (e: MouseEvent) => {
      if (contextRef.current && !contextRef.current.contains(e.target as Node)) {
        setContextMenu(null);
        setShowLabelInput(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [contextMenu]);

  return (
    <div
      onContextMenu={handleContextMenu}
      className={`
        group relative border-l-2 transition-colors
        ${
          isSelected
            ? "border-l-[#7aa2f7] bg-[#7aa2f7]/5"
            : session.liveStatus === "frozen"
              ? "border-l-[#f7768e]/50 hover:bg-[#1a1b26]"
              : session.liveStatus === "needs-input"
                ? "border-l-[#e0af68]/50 hover:bg-[#1a1b26]"
                : session.liveStatus === "active-in-zone"
                  ? "border-l-[#9ece6a]/50 hover:bg-[#1a1b26]"
                  : session.liveStatus === "active-external"
                    ? "border-l-[#73daca]/50 hover:bg-[#1a1b26]"
                    : "border-l-transparent hover:bg-[#1a1b26]"
        }
      `}
    >
      <button
        onClick={() => onViewTranscript(session)}
        className="w-full text-left px-3 py-2 pr-20"
        title={`Session ${session.sessionId}\n${session.workSummaryHint || session.displayName}`}
      >
        {/* Row 1: Status dot + Name + Account badge */}
        <div className="flex items-center gap-1.5 mb-0.5">
          {selectionMode && (
            <input
              type="checkbox"
              checked={isChecked}
              onChange={(e) => {
                e.stopPropagation();
                onToggleSelect(session.sessionId);
              }}
              onClick={(e) => e.stopPropagation()}
              className="w-3 h-3 rounded border-[#414868] bg-[#1a1b26] text-[#7aa2f7] shrink-0 cursor-pointer"
            />
          )}
          <span
            className={`w-2 h-2 rounded-full shrink-0 ${statusInfo.pulse ? "animate-pulse" : ""}`}
            style={{ backgroundColor: statusInfo.color }}
            title={statusInfo.label}
          />
          <span className="text-xs text-[#a9b1d6] truncate flex-1">{session.displayName}</span>
          {accountBadge && (
            <span
              className={`px-1 py-0 rounded text-[9px] font-medium shrink-0 ${accountBadge.bg} ${accountBadge.text}`}
            >
              {session.accountLabel}
            </span>
          )}
          {!accountBadge && session.accountLabel !== "default" && (
            <span className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-[#565f89]/10 text-[#565f89]">
              {session.accountLabel}
            </span>
          )}
          {isPinned && (
            <Pin className="w-3 h-3 text-[#e0af68] shrink-0" />
          )}
          {sessionLabel && (
            <span className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-[#73daca]/10 text-[#73daca] truncate max-w-[60px]">
              {sessionLabel}
            </span>
          )}
        </div>

        {/* Row 2: Meta info */}
        <div className="flex items-center gap-1 text-[10px] text-[#414868] ml-3.5">
          <span>{formatTimeAgo(session.lastModified)}</span>
          <span>&middot;</span>
          <span>{session.messageCount} msgs</span>
          {durationLabel && (
            <>
              <span>&middot;</span>
              <span title="Session duration">{durationLabel}</span>
            </>
          )}
          <span>&middot;</span>
          <span className="truncate">{session.projectLabel}</span>
        </div>

        {/* Row 3: Work summary hint (only for frozen/needs-input/active) */}
        {session.workSummaryHint &&
          session.liveStatus !== "dormant" &&
          session.liveStatus !== "completed" && (
            <div className="mt-1 ml-3.5 text-[10px] text-[#565f89] truncate leading-tight">
              {session.lastMessagePreview || session.workSummaryHint}
            </div>
          )}

        {/* Frozen warning badge */}
        {session.liveStatus === "frozen" && (
          <div className="mt-1 ml-3.5 flex items-center gap-1 text-[10px] text-[#f7768e]/80">
            <AlertTriangle className="w-3 h-3" />
            <span>Session appears frozen — may need resume</span>
          </div>
        )}
      </button>

      {/* Quick action buttons — visible on hover */}
      <div className="absolute right-2 top-1/2 -translate-y-1/2 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
        <button
          onClick={(e) => {
            e.stopPropagation();
            onResume(session);
          }}
          className="p-1 rounded text-[#565f89] hover:text-[#9ece6a] hover:bg-[#9ece6a]/10 transition-colors"
          title={`Resume: claude --resume ${session.sessionId.slice(0, 8)}...`}
        >
          <TerminalSquare className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onViewTranscript(session);
          }}
          className="p-1 rounded text-[#565f89] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10 transition-colors"
          title="View transcript"
        >
          <Eye className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onCopyId(session);
          }}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title={`Copy session ID: ${session.sessionId}`}
        >
          <Copy className="w-3 h-3" />
        </button>
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={contextRef}
          className="fixed z-50 py-1 min-w-[160px] bg-[#1a1b26] border border-[#2a2d3d] rounded-md shadow-xl"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          {showLabelInput ? (
            <div className="px-2 py-1">
              <input
                autoFocus
                value={labelInput}
                onChange={(e) => setLabelInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    onSetLabel(session.sessionId, labelInput.trim() || null);
                    setContextMenu(null);
                    setShowLabelInput(false);
                    setLabelInput("");
                  } else if (e.key === "Escape") {
                    setShowLabelInput(false);
                    setLabelInput("");
                  }
                }}
                placeholder="Enter label..."
                className="w-full px-2 py-1 text-xs bg-[#13141f] border border-[#2a2d3d] rounded text-[#a9b1d6] placeholder-[#414868] focus:outline-none focus:border-[#7aa2f7]/50"
              />
            </div>
          ) : (
            <>
              <button
                onClick={() => { onResume(session); setContextMenu(null); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <TerminalSquare className="w-3 h-3 text-[#9ece6a]" /> Resume
              </button>
              <button
                onClick={() => { onViewTranscript(session); setContextMenu(null); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Eye className="w-3 h-3 text-[#7aa2f7]" /> View Transcript
              </button>
              <button
                onClick={() => { onCopyId(session); setContextMenu(null); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Copy className="w-3 h-3" /> Copy Session ID
              </button>
              <div className="my-1 h-px bg-[#2a2d3d]" />
              <button
                onClick={() => { onTogglePin(session.sessionId); setContextMenu(null); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Pin className={`w-3 h-3 ${isPinned ? "text-[#e0af68]" : ""}`} />
                {isPinned ? "Unpin" : "Pin to Top"}
              </button>
              <button
                onClick={() => { setShowLabelInput(true); setLabelInput(sessionLabel ?? ""); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
              >
                <Tag className="w-3 h-3 text-[#73daca]" />
                {sessionLabel ? "Edit Label" : "Add Label"}
              </button>
              {!selectionMode && (
                <button
                  onClick={() => { onToggleSelect(session.sessionId); setContextMenu(null); }}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
                >
                  <Copy className="w-3 h-3" /> Select for Bulk
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
