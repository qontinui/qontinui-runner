/**
 * AI-powered session reorganization dialog.
 *
 * Analyzes terminal sessions across pages and proposes topic-based groupings.
 * Users can preview the proposal, edit page names, and apply the reorganization.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, Loader2, Check, AlertTriangle } from "lucide-react";
import { executeAiTask } from "@/hooks/useAiTaskPolling";
import { getApiPort } from "@/lib/runner-api";
import type { TerminalPageConfig } from "./useTerminalPages";

interface ReorganizeDialogProps {
  pages: TerminalPageConfig[];
  onClose: () => void;
  onApply: (plan: ReorganizePlan) => Promise<void>;
}

export interface ReorganizePlan {
  pages: Array<{
    name: string;
    sessionIds: string[];
  }>;
}

interface SessionMeta {
  id: string;
  title: string;
  page_id: string;
  working_dir: string;
  is_alive: boolean;
  scrollback_preview: string;
}

type DialogState = "selecting" | "analyzing" | "previewing" | "applying" | "done" | "error";

export function ReorganizeDialog({ pages, onClose, onApply }: ReorganizeDialogProps) {
  const [state, setState] = useState<DialogState>("selecting");
  const [selectedPages, setSelectedPages] = useState<Set<string>>(
    () => new Set(pages.map((p) => p.id)),
  );
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [proposal, setProposal] = useState<ReorganizePlan | null>(null);
  const [error, setError] = useState<string | null>(null);

  const togglePage = (pageId: string) => {
    setSelectedPages((prev) => {
      const next = new Set(prev);
      if (next.has(pageId)) next.delete(pageId);
      else next.add(pageId);
      return next;
    });
  };

  const getPageName = (pageId: string): string =>
    pages.find((p) => p.id === pageId)?.name ?? pageId;

  // Phase 1: Collect metadata
  const collectMetadata = useCallback(async (): Promise<SessionMeta[]> => {
    const result = await invoke<{ success: boolean; data?: { sessions: SessionMeta[] } }>(
      "terminal_collect_session_metadata",
      { pageIds: Array.from(selectedPages), maxScrollbackLines: 20 },
    );
    if (!result.success || !result.data) throw new Error("Failed to collect session metadata");
    return result.data.sessions;
  }, [selectedPages]);

  // Phase 2: Fetch file registry for context
  const fetchFileRegistry = useCallback(async () => {
    try {
      const port = getApiPort();
      const resp = await fetch(`http://127.0.0.1:${port}/file-registry/info`);
      if (!resp.ok) return [];
      return (await resp.json()) as Array<{
        file_path: string;
        holder_name: string;
      }>;
    } catch {
      return [];
    }
  }, []);

  // Phase 3: Build AI prompt and analyze
  const analyze = useCallback(async () => {
    setState("analyzing");
    setError(null);

    try {
      const [meta, fileRegistry] = await Promise.all([collectMetadata(), fetchFileRegistry()]);
      setSessions(meta);

      if (meta.length < 2) {
        setError("Need at least 2 sessions to reorganize.");
        setState("error");
        return;
      }

      // Build file edit map: session title → files edited
      const filesBySession = new Map<string, string[]>();
      for (const entry of fileRegistry) {
        const files = filesBySession.get(entry.holder_name) ?? [];
        files.push(entry.file_path);
        filesBySession.set(entry.holder_name, files);
      }

      const sessionDescriptions = meta
        .map((s) => {
          const files = filesBySession.get(s.title) ?? [];
          const pageName = getPageName(s.page_id);
          return [
            `- Session "${s.title}"`,
            `  Current page: "${pageName}"`,
            `  Working dir: ${s.working_dir || "(unknown)"}`,
            s.is_alive ? "  Status: active" : "  Status: inactive",
            files.length > 0 ? `  Files edited: ${files.slice(0, 5).join(", ")}` : null,
            s.scrollback_preview
              ? `  Recent output (last lines):\n    ${s.scrollback_preview.split("\n").slice(-5).join("\n    ")}`
              : null,
          ]
            .filter(Boolean)
            .join("\n");
        })
        .join("\n\n");

      const prompt = `Analyze these terminal sessions and group them by topic/theme.
Each group should become a terminal page with a descriptive name (2-4 words).

Sessions:
${sessionDescriptions}

Instructions:
- Group sessions that work on related topics onto the same page
- Name each page descriptively (e.g., "Runner Frontend", "Supervisor Backend", "Web API")
- Every session must appear in exactly one group
- Keep the number of groups between 2 and ${Math.min(meta.length, 6)}
- Return ONLY valid JSON, no other text

Return this exact JSON format:
{
  "pages": [
    { "name": "Page Name", "sessions": ["session-title-1", "session-title-2"] }
  ]
}`;

      const task = await executeAiTask(
        {
          name: "session-reorganization",
          content: prompt,
          display_prompt: "Analyzing sessions for topic-based reorganization...",
          max_sessions: 1,
          timeout_seconds: 60,
        },
        { timeoutMs: 90_000 },
      );

      // Parse the AI response
      const output = task.output_log || "";
      const jsonMatch = output.match(/\{[\s\S]*"pages"[\s\S]*\}/);
      if (!jsonMatch) {
        throw new Error("AI did not return valid JSON. Response: " + output.slice(0, 200));
      }

      const parsed = JSON.parse(jsonMatch[0]) as {
        pages: Array<{ name: string; sessions: string[] }>;
      };

      // Map session titles to IDs
      const titleToId = new Map(meta.map((s) => [s.title, s.id]));
      const plan: ReorganizePlan = {
        pages: parsed.pages.map((g) => ({
          name: g.name,
          sessionIds: g.sessions
            .map((title) => titleToId.get(title))
            .filter((id): id is string => id != null),
        })),
      };

      // Validate: every session should appear exactly once
      const assignedIds = new Set(plan.pages.flatMap((p) => p.sessionIds));
      const unassigned = meta.filter((s) => !assignedIds.has(s.id));
      if (unassigned.length > 0) {
        // Add unassigned sessions to the first group
        plan.pages[0].sessionIds.push(...unassigned.map((s) => s.id));
      }

      setProposal(plan);
      setState("previewing");
    } catch (err) {
      setError(String(err));
      setState("error");
    }
  }, [collectMetadata, fetchFileRegistry, getPageName]);

  // Phase 4: Apply the reorganization
  const apply = useCallback(async () => {
    if (!proposal) return;
    setState("applying");
    try {
      await onApply(proposal);
      setState("done");
    } catch (err) {
      setError(String(err));
      setState("error");
    }
  }, [proposal, onApply]);

  const updatePageName = (index: number, name: string) => {
    if (!proposal) return;
    setProposal({
      ...proposal,
      pages: proposal.pages.map((p, i) => (i === index ? { ...p, name } : p)),
    });
  };

  const getSessionTitle = (sessionId: string): string =>
    sessions.find((s) => s.id === sessionId)?.title ?? sessionId.slice(0, 8);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-xl shadow-2xl w-[500px] max-h-[80vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[#2a2d3d]">
          <h2 className="text-sm font-semibold text-[#c0caf5]">Reorganize Pages by Topic</h2>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4">
          {/* Selecting state */}
          {state === "selecting" && (
            <div className="space-y-3">
              <p className="text-xs text-[#a9b1d6]">
                Select which pages to include in the reorganization:
              </p>
              {pages.map((page) => (
                <label
                  key={page.id}
                  className="flex items-center gap-2 px-3 py-2 rounded-lg bg-[#13141f] hover:bg-[#2a2d3d]/50 cursor-pointer transition-colors"
                >
                  <input
                    type="checkbox"
                    checked={selectedPages.has(page.id)}
                    onChange={() => togglePage(page.id)}
                    className="accent-[#7aa2f7]"
                  />
                  <span className="text-xs text-[#c0caf5] flex-1">{page.name}</span>
                </label>
              ))}
            </div>
          )}

          {/* Analyzing state */}
          {state === "analyzing" && (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <Loader2 className="w-8 h-8 text-[#7aa2f7] animate-spin" />
              <p className="text-xs text-[#a9b1d6]">Analyzing session topics...</p>
            </div>
          )}

          {/* Previewing state */}
          {state === "previewing" && proposal && (
            <div className="space-y-4">
              <p className="text-xs text-[#a9b1d6]">
                Proposed reorganization ({proposal.pages.length} pages):
              </p>
              {proposal.pages.map((group, idx) => (
                <div key={idx} className="rounded-lg bg-[#13141f] p-3 space-y-2">
                  <input
                    value={group.name}
                    onChange={(e) => updatePageName(idx, e.target.value)}
                    className="w-full text-xs font-medium bg-transparent border-b border-[#2a2d3d] text-[#c0caf5] pb-1 outline-none focus:border-[#7aa2f7] transition-colors"
                  />
                  <div className="space-y-1">
                    {group.sessionIds.map((sid) => (
                      <div
                        key={sid}
                        className="flex items-center gap-2 text-[10px] text-[#a9b1d6] px-2 py-0.5 rounded bg-[#1a1b26]"
                      >
                        <span className="w-1.5 h-1.5 rounded-full bg-[#7aa2f7] shrink-0" />
                        <span className="truncate">{getSessionTitle(sid)}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Applying state */}
          {state === "applying" && (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <Loader2 className="w-8 h-8 text-[#9ece6a] animate-spin" />
              <p className="text-xs text-[#a9b1d6]">Moving sessions...</p>
            </div>
          )}

          {/* Done state */}
          {state === "done" && (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <Check className="w-8 h-8 text-[#9ece6a]" />
              <p className="text-xs text-[#9ece6a]">Pages reorganized successfully!</p>
            </div>
          )}

          {/* Error state */}
          {state === "error" && (
            <div className="flex flex-col items-center justify-center py-8 gap-3">
              <AlertTriangle className="w-8 h-8 text-[#f7768e]" />
              <p className="text-xs text-[#f7768e] text-center max-w-[300px]">{error}</p>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-[#2a2d3d]">
          {state === "selecting" && (
            <button
              onClick={analyze}
              disabled={selectedPages.size < 2}
              className="px-3 py-1.5 text-xs rounded-md bg-[#7aa2f7] text-[#1a1b26] font-medium hover:bg-[#7aa2f7]/80 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
            >
              Analyze
            </button>
          )}
          {state === "previewing" && (
            <>
              <button
                onClick={() => setState("selecting")}
                className="px-3 py-1.5 text-xs rounded-md bg-[#2a2d3d] text-[#a9b1d6] hover:bg-[#2a2d3d]/80 transition-colors"
              >
                Back
              </button>
              <button
                onClick={apply}
                className="px-3 py-1.5 text-xs rounded-md bg-[#9ece6a] text-[#1a1b26] font-medium hover:bg-[#9ece6a]/80 transition-colors"
              >
                Apply
              </button>
            </>
          )}
          {(state === "done" || state === "error") && (
            <button
              onClick={state === "error" ? () => setState("selecting") : onClose}
              className="px-3 py-1.5 text-xs rounded-md bg-[#2a2d3d] text-[#a9b1d6] hover:bg-[#2a2d3d]/80 transition-colors"
            >
              {state === "error" ? "Try Again" : "Close"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
