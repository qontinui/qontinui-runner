/**
 * TerminalFindingsPanel
 *
 * Right pane that surfaces "needs_input" findings as individual tabs.
 * Each tab shows the full decision context (question, options, code context).
 * Supports click-to-respond, fix-with-Claude, workflow generation, and cross-tab toggle.
 */

import { useEffect, useState, useCallback } from "react";
import { X, AlertCircle, FileCode, ChevronRight, Play, Wand2, Send } from "lucide-react";
import { getCategoryById } from "@/services/FindingCategories";
import { getSeverityColors } from "@/design-system";
import type { Finding } from "@/types/findings";

interface TerminalFindingsPanelProps {
  /** Findings to display (pre-filtered by the parent for the active tab). */
  findings: Finding[];
  /** All findings across all terminal tabs. */
  allFindings?: Finding[];
  onClose: () => void;
  /** Write a response to the terminal PTY for a given finding. */
  onRespond?: (findingId: string, text: string) => void;
  /** Open a new Claude session to fix a finding. */
  onFix?: (finding: Finding) => void;
  /** Generate a workflow from the currently displayed findings. */
  onGenerateWorkflow?: (findings: Finding[]) => void;
}

// Severity dot color mapping (inline for the tab indicators)
const SEVERITY_DOT: Record<string, string> = {
  critical: "bg-red-500",
  high: "bg-orange-500",
  medium: "bg-yellow-500",
  low: "bg-blue-500",
  info: "bg-slate-500",
};

export function TerminalFindingsPanel({
  findings,
  allFindings,
  onClose,
  onRespond,
  onFix,
  onGenerateWorkflow,
}: TerminalFindingsPanelProps) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showAllTabs, setShowAllTabs] = useState(false);
  const [responseText, setResponseText] = useState("");

  // Derive displayed findings from toggle state
  const displayedFindings = showAllTabs && allFindings ? allFindings : findings;

  // Auto-select first finding when findings change or selected is removed
  useEffect(() => {
    if (displayedFindings.length === 0) {
      setSelectedId(null);
      return;
    }
    const stillExists = selectedId && displayedFindings.some((f) => f.id === selectedId);
    if (!stillExists) {
      setSelectedId(displayedFindings[0].id);
    }
  }, [displayedFindings, selectedId]);

  const selected = displayedFindings.find((f) => f.id === selectedId) ?? null;
  const category = selected ? getCategoryById(selected.categoryId) : null;
  const severityColors = selected ? getSeverityColors(selected.severity) : null;

  const truncate = useCallback((text: string, max: number) => {
    return text.length > max ? text.slice(0, max) + "\u2026" : text;
  }, []);

  const handleSubmitResponse = useCallback(() => {
    if (!selected || !responseText.trim() || !onRespond) return;
    onRespond(selected.id, responseText.trim());
    setResponseText("");
  }, [selected, responseText, onRespond]);

  return (
    <div className="w-[460px] h-full shrink-0 flex flex-col bg-[#13141f] border-l border-[#2a2d3d]">
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2.5 border-b border-[#2a2d3d] bg-[#0d0e1a] shrink-0">
        <AlertCircle className="w-4 h-4 text-[#bb9af7]" />
        <span className="flex-1 text-sm font-semibold text-[#c0caf5]">Decisions Needing Input</span>

        {/* Cross-tab toggle */}
        {allFindings && (
          <div className="flex items-center rounded bg-[#1a1b26] border border-[#2a2d3d] overflow-hidden">
            <button
              onClick={() => setShowAllTabs(false)}
              className={`px-2 py-0.5 text-[10px] font-medium transition-colors ${
                !showAllTabs
                  ? "bg-[#bb9af7]/20 text-[#bb9af7]"
                  : "text-[#565f89] hover:text-[#a9b1d6]"
              }`}
            >
              Tab
            </button>
            <button
              onClick={() => setShowAllTabs(true)}
              className={`px-2 py-0.5 text-[10px] font-medium transition-colors ${
                showAllTabs
                  ? "bg-[#bb9af7]/20 text-[#bb9af7]"
                  : "text-[#565f89] hover:text-[#a9b1d6]"
              }`}
            >
              All
            </button>
          </div>
        )}

        {displayedFindings.length > 0 && (
          <span className="px-1.5 py-0.5 rounded-full text-[10px] font-bold bg-[#bb9af7]/20 text-[#bb9af7]">
            {displayedFindings.length}
          </span>
        )}

        {/* Generate workflow button */}
        {displayedFindings.length > 0 && onGenerateWorkflow && (
          <button
            onClick={() => onGenerateWorkflow(displayedFindings)}
            className="px-2 py-0.5 rounded text-[10px] font-medium bg-[#7aa2f7]/10 text-[#7aa2f7] hover:bg-[#7aa2f7]/20 transition-colors"
            title="Generate workflow to fix these findings"
          >
            <Wand2 className="w-3 h-3 inline mr-1" />
            Workflow
          </button>
        )}

        <button
          onClick={onClose}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title="Close"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      {displayedFindings.length === 0 ? (
        /* Empty state */
        <div className="flex-1 flex flex-col items-center justify-center gap-2 text-[#565f89]">
          <AlertCircle className="w-8 h-8 opacity-30" />
          <span className="text-sm">No decisions pending</span>
          <span className="text-xs text-[#414868]">
            Items will appear here when the AI needs your input
          </span>
        </div>
      ) : (
        <>
          {/* Tab bar */}
          <div className="flex overflow-x-auto border-b border-[#2a2d3d] bg-[#0d0e1a] shrink-0 scrollbar-thin">
            {displayedFindings.map((f) => {
              const isActive = f.id === selectedId;
              const dotColor = SEVERITY_DOT[f.severity] ?? "bg-slate-500";
              return (
                <button
                  key={f.id}
                  onClick={() => setSelectedId(f.id)}
                  className={`
                    flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium whitespace-nowrap shrink-0
                    border-b-2 transition-colors
                    ${
                      isActive
                        ? "border-[#bb9af7] text-[#c0caf5] bg-[#13141f]"
                        : "border-transparent text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#1a1b26]"
                    }
                  `}
                  title={f.title}
                >
                  <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${dotColor}`} />
                  {truncate(f.title, 20)}
                </button>
              );
            })}
          </div>

          {/* Tab content */}
          {selected && (
            <div className="flex-1 overflow-y-auto p-4 space-y-4">
              {/* Title */}
              <h3 className="text-sm font-semibold text-[#c0caf5] leading-snug">
                {selected.title}
              </h3>

              {/* Severity + Category badges + Fix button */}
              <div className="flex items-center gap-2 flex-wrap">
                {severityColors && (
                  <span
                    className={`px-2 py-0.5 rounded text-[10px] font-semibold uppercase tracking-wider ${severityColors.bg} ${severityColors.text}`}
                  >
                    {selected.severity}
                  </span>
                )}
                {category && (
                  <span className="px-2 py-0.5 rounded text-[10px] font-medium bg-[#2a2d3d] text-[#a9b1d6]">
                    {category.name}
                  </span>
                )}
                {onFix && selected.codeContext?.file && (
                  <button
                    onClick={() => onFix(selected)}
                    className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium bg-[#9ece6a]/10 text-[#9ece6a] hover:bg-[#9ece6a]/20 transition-colors ml-auto"
                  >
                    <Play className="w-3 h-3" /> Fix with Claude
                  </button>
                )}
              </div>

              {/* Description */}
              {selected.description && (
                <div className="text-xs text-[#a9b1d6] leading-relaxed whitespace-pre-wrap">
                  {selected.description}
                </div>
              )}

              {/* Question section */}
              {selected.pendingQuestion && (
                <div className="rounded-lg bg-[#bb9af7]/5 border border-[#bb9af7]/20 p-3 space-y-2">
                  <div className="text-xs font-semibold text-[#bb9af7] uppercase tracking-wider">
                    Question
                  </div>
                  <div className="text-sm text-[#c0caf5] leading-relaxed">
                    {selected.pendingQuestion.question}
                  </div>

                  {/* Context */}
                  {selected.pendingQuestion.context && (
                    <div className="text-xs text-[#565f89] leading-relaxed mt-1">
                      {selected.pendingQuestion.context}
                    </div>
                  )}

                  {/* Clickable options */}
                  {selected.pendingQuestion.options &&
                    selected.pendingQuestion.options.length > 0 && (
                      <div className="space-y-1.5 mt-2">
                        <div className="text-[10px] font-medium text-[#565f89] uppercase tracking-wider">
                          Options
                        </div>
                        {selected.pendingQuestion.options.map((opt, i) => (
                          <button
                            key={i}
                            onClick={() => onRespond?.(selected.id, opt.label)}
                            className="flex items-start gap-2 text-xs text-[#a9b1d6] bg-[#1a1b26] rounded px-2.5 py-1.5 hover:bg-[#2a2d3d] cursor-pointer w-full text-left transition-colors"
                          >
                            <ChevronRight className="w-3 h-3 mt-0.5 text-[#bb9af7] shrink-0" />
                            <div>
                              <span className="font-medium text-[#c0caf5]">{opt.label}</span>
                              {opt.description && (
                                <span className="text-[#565f89]"> — {opt.description}</span>
                              )}
                            </div>
                          </button>
                        ))}
                      </div>
                    )}

                  {/* Free-form text response */}
                  {onRespond && (
                    <div className="flex gap-1.5 mt-2">
                      <input
                        type="text"
                        value={responseText}
                        onChange={(e) => setResponseText(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") handleSubmitResponse();
                        }}
                        placeholder="Type a response..."
                        className="flex-1 px-2 py-1 text-xs bg-[#1a1b26] border border-[#2a2d3d] rounded text-[#c0caf5] placeholder-[#414868] focus:outline-none focus:border-[#bb9af7]/50"
                      />
                      <button
                        onClick={handleSubmitResponse}
                        disabled={!responseText.trim()}
                        className="px-2 py-1 rounded text-xs bg-[#bb9af7]/20 text-[#bb9af7] hover:bg-[#bb9af7]/30 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                        title="Send response"
                      >
                        <Send className="w-3 h-3" />
                      </button>
                    </div>
                  )}
                </div>
              )}

              {/* Code context */}
              {selected.codeContext && (
                <div className="rounded-lg bg-[#1a1b26] border border-[#2a2d3d] p-3 space-y-1.5">
                  <div className="flex items-center gap-1.5 text-[10px] font-medium text-[#565f89] uppercase tracking-wider">
                    <FileCode className="w-3 h-3" />
                    File Context
                  </div>
                  {selected.codeContext.file && (
                    <div
                      className="text-xs font-mono text-[#7dcfff] truncate"
                      title={selected.codeContext.file}
                    >
                      {selected.codeContext.file}
                      {selected.codeContext.line != null && (
                        <span className="text-[#565f89]">
                          :{selected.codeContext.line}
                          {selected.codeContext.endLine != null &&
                            selected.codeContext.endLine !== selected.codeContext.line &&
                            `-${selected.codeContext.endLine}`}
                        </span>
                      )}
                    </div>
                  )}
                  {selected.codeContext.snippet && (
                    <pre className="text-[11px] text-[#a9b1d6] bg-[#0d0e1a] rounded p-2 overflow-x-auto leading-relaxed">
                      {selected.codeContext.snippet}
                    </pre>
                  )}
                </div>
              )}

              {/* Hint to respond via terminal */}
              <div className="text-[10px] text-[#414868] italic pt-2 border-t border-[#2a2d3d]/50">
                {onRespond
                  ? "Click an option above, type a response, or type directly in the terminal"
                  : "Type your response in the terminal on the left"}
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}
