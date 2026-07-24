/**
 * Prompt-library modal — the `/prompt` command surface.
 *
 * Plan `2026-07-24-terminal-prompt-library-command` Phase 4. Lists the
 * tenant's curated prompt templates (coord `kind=prompt_template`
 * documents, fetched + frontmatter-parsed by the Rust
 * `list_prompt_templates` command) with a parameter form and three
 * actions:
 *
 * - PRIMARY "Start new session" — spawn an AI session with the rendered
 *   prompt auto-typed (the one-obvious-button path for non-programmers).
 * - "Insert into session ▾" — prefill the rendered text into an open
 *   terminal WITHOUT a trailing `\r`, so the operator reviews before
 *   submitting.
 * - "Copy" — clipboard.
 *
 * Overlay skeleton (fixed inset-0 z-50, Escape + backdrop close) copied
 * from `DocFinderModal.tsx`. Degraded auth states render an explicit
 * not-paired / not-authorized panel — never a silent empty list.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Copy,
  Loader2,
  Play,
  RefreshCw,
  Search,
  Sparkles,
  X,
} from "lucide-react";

import { writeClipboard } from "@/lib/clipboard";

import type { PromptLibraryAuth, PromptParameter, PromptTemplate } from "./promptLibraryApi";
import {
  initialParamValues,
  isParameterVisible,
  missingRequired,
  renderPromptTemplate,
  type PromptParamValues,
} from "./renderPromptTemplate";

/* ── Types ────────────────────────────────────────────────────────────── */

export interface PromptModalSession {
  id: string;
  title: string;
}

interface PromptModalProps {
  prompts: PromptTemplate[];
  auth: PromptLibraryAuth;
  reason?: string;
  loading: boolean;
  onRefresh: () => void;
  /** Slug to preselect (the dynamic `/slug` route with parameters). */
  initialSlug?: string | null;
  /** Open terminal sessions for the "Insert into session" dropdown. */
  sessions: PromptModalSession[];
  /** Default insert target (the focused session). */
  focusedSessionId?: string | null;
  /** Spawn a fresh AI session with the rendered text auto-typed. */
  onSpawn: (text: string) => void | Promise<void>;
  /** Prefill (NO trailing `\r`) the rendered text into a session. */
  onInsert: (tabId: string, text: string) => void;
  onClose: () => void;
}

/* ── Component ────────────────────────────────────────────────────────── */

export function PromptModal({
  prompts,
  auth,
  reason,
  loading,
  onRefresh,
  initialSlug,
  sessions,
  focusedSessionId,
  onSpawn,
  onInsert,
  onClose,
}: PromptModalProps) {
  const [search, setSearch] = useState("");
  const [selectedSlug, setSelectedSlug] = useState<string | null>(initialSlug ?? null);
  const [values, setValues] = useState<PromptParamValues>({});
  const [showPreview, setShowPreview] = useState(false);
  const [showInsertMenu, setShowInsertMenu] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);

  const overlayRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // Close on Escape (capture-phase, matching DocFinderModal).
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [onClose]);

  useEffect(() => {
    setTimeout(() => searchInputRef.current?.focus(), 50);
  }, []);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return prompts;
    return prompts.filter(
      (p) =>
        p.title.toLowerCase().includes(q) ||
        p.name.toLowerCase().includes(q) ||
        p.description.toLowerCase().includes(q) ||
        p.category.toLowerCase().includes(q),
    );
  }, [prompts, search]);

  // Group by category, stable insertion order.
  const grouped = useMemo(() => {
    const map = new Map<string, PromptTemplate[]>();
    for (const p of filtered) {
      const list = map.get(p.category);
      if (list) list.push(p);
      else map.set(p.category, [p]);
    }
    return Array.from(map.entries());
  }, [filtered]);

  // Effective selection, derived at render (no effect needed): an explicit
  // click wins; otherwise `initialSlug` (the dynamic `/<slug>` route);
  // otherwise the first prompt so the right pane is never blank.
  const selected = useMemo(() => {
    if (selectedSlug) {
      const clicked = prompts.find((p) => p.name === selectedSlug);
      if (clicked) return clicked;
    }
    if (initialSlug) {
      const wanted = prompts.find((p) => p.name === initialSlug);
      if (wanted) return wanted;
    }
    return prompts[0] ?? null;
  }, [prompts, selectedSlug, initialSlug]);

  // Reset form values + feedback when the selection changes.
  const [prevSelected, setPrevSelected] = useState<string | null>(null);
  if (selected?.name !== prevSelected) {
    setPrevSelected(selected?.name ?? null);
    setValues(selected ? initialParamValues(selected.parameters) : {});
    setShowPreview(false);
    setShowInsertMenu(false);
    setFeedback(null);
  }

  const missing = selected ? missingRequired(selected.parameters, values) : [];
  const rendered = selected ? renderPromptTemplate(selected.body, values) : "";
  const canRun = !!selected && missing.length === 0;

  const defaultInsertTarget =
    (focusedSessionId ? sessions.find((s) => s.id === focusedSessionId) : undefined) ??
    sessions[0] ??
    null;

  const handleSpawn = async () => {
    if (!canRun) return;
    await onSpawn(rendered);
    onClose();
  };

  const handleInsert = (tabId: string) => {
    if (!canRun) return;
    onInsert(tabId, rendered);
    setShowInsertMenu(false);
    onClose();
  };

  const handleCopy = async () => {
    if (!canRun) return;
    const okCopy = await writeClipboard(rendered);
    setFeedback(okCopy ? "Copied to clipboard" : "Copy failed");
  };

  const handleBackdropClick = (e: React.MouseEvent) => {
    if (e.target === overlayRef.current) onClose();
  };

  const degraded = auth.state !== "ok";

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm"
      onClick={handleBackdropClick}
    >
      <div className="bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-2xl w-[760px] max-h-[80vh] h-[560px] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[#2a2d3d]">
          <Sparkles className="w-4 h-4 text-[#7aa2f7]" />
          <h2 className="text-sm font-medium text-[#c0caf5] flex-1">Prompts</h2>
          <button
            onClick={onRefresh}
            title="Refresh prompt library"
            className="p-1 rounded hover:bg-[#2a2d3d] transition-colors text-[#565f89] hover:text-[#c0caf5]"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-[#2a2d3d] transition-colors text-[#565f89] hover:text-[#c0caf5]"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {degraded ? (
          <DegradedState auth={auth} reason={reason} onRefresh={onRefresh} />
        ) : (
          <div className="flex-1 flex min-h-0">
            {/* Left: search + grouped list */}
            <div className="w-[280px] border-r border-[#2a2d3d] flex flex-col min-h-0">
              <div className="px-3 pt-3 pb-2">
                <div className="flex items-center gap-2 bg-[#13141f] border border-[#2a2d3d] rounded px-3 py-2 focus-within:border-[#7aa2f7] transition-colors">
                  <Search className="w-3.5 h-3.5 text-[#565f89] shrink-0" />
                  <input
                    ref={searchInputRef}
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    placeholder="Search prompts..."
                    className="flex-1 min-w-0 bg-transparent text-sm text-[#c0caf5] placeholder-[#565f89] outline-hidden"
                  />
                </div>
              </div>
              <div className="flex-1 overflow-y-auto scrollbar-dark pb-2">
                {loading && prompts.length === 0 && (
                  <div className="px-4 py-8 flex items-center justify-center gap-2 text-[12px] text-[#565f89]">
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    Loading prompts...
                  </div>
                )}
                {!loading && prompts.length === 0 && (
                  <div className="px-4 py-8 text-center text-[12px] text-[#565f89]">
                    {reason ? (
                      <span className="text-[#e0af68]">{reason}</span>
                    ) : (
                      "No prompts in the library yet. Author prompt templates on the web to see them here."
                    )}
                  </div>
                )}
                {prompts.length > 0 && filtered.length === 0 && (
                  <div className="px-4 py-8 text-center text-[12px] text-[#565f89]">No matches</div>
                )}
                {grouped.map(([category, items]) => (
                  <div key={category}>
                    <div className="px-3 pt-3 pb-1 text-[10px] font-semibold uppercase tracking-wider text-[#565f89]">
                      {category}
                    </div>
                    {items.map((p) => (
                      <button
                        key={p.name}
                        onClick={() => setSelectedSlug(p.name)}
                        className={`w-full text-left px-3 py-2 flex items-start gap-2 transition-colors ${
                          p.name === selectedSlug ? "bg-[#24283b]" : "hover:bg-[#24283b]/50"
                        }`}
                      >
                        <span className="text-[14px] leading-5 shrink-0 w-5 text-center">
                          {p.icon ?? "📄"}
                        </span>
                        <span className="flex-1 min-w-0">
                          <span className="flex items-center gap-1.5">
                            <span className="text-[12px] font-medium text-[#c0caf5] truncate">
                              {p.title}
                            </span>
                            <span className="text-[9px] font-mono bg-[#2a2d3d] text-[#7aa2f7] rounded px-1 py-px shrink-0">
                              v{p.version}
                            </span>
                          </span>
                          <span className="block text-[10px] text-[#565f89] truncate">
                            {p.description || `/${p.name}`}
                          </span>
                        </span>
                      </button>
                    ))}
                  </div>
                ))}
                {reason && prompts.length > 0 && (
                  <div className="px-3 pt-3 text-[10px] text-[#e0af68]">{reason}</div>
                )}
              </div>
            </div>

            {/* Right: detail + form + actions */}
            <div className="flex-1 min-w-0 flex flex-col min-h-0">
              {selected ? (
                <div className="flex-1 overflow-y-auto scrollbar-dark px-4 py-3 flex flex-col gap-3">
                  <div>
                    <div className="flex items-center gap-2">
                      <h3 className="text-[14px] font-medium text-[#c0caf5]">{selected.title}</h3>
                      <span className="text-[9px] font-mono bg-[#2a2d3d] text-[#7aa2f7] rounded px-1.5 py-0.5">
                        v{selected.version}
                      </span>
                      <span className="text-[10px] text-[#565f89]">· {selected.category}</span>
                    </div>
                    <div className="text-[10px] text-[#565f89] font-mono mt-0.5">
                      /{selected.name}
                    </div>
                    {selected.description && (
                      <p className="text-[12px] text-[#a9b1d6] mt-2">{selected.description}</p>
                    )}
                    {selected.parse_error && (
                      <div className="mt-2 flex items-start gap-1.5 text-[11px] text-[#e0af68]">
                        <AlertTriangle className="w-3.5 h-3.5 shrink-0 mt-px" />
                        <span>
                          This prompt&apos;s argument definition could not be read (
                          {selected.parse_error}); it runs as plain text.
                        </span>
                      </div>
                    )}
                  </div>

                  {/* Parameter form */}
                  {selected.parameters.length > 0 && (
                    <div className="flex flex-col gap-2.5">
                      {selected.parameters
                        .filter((p) => isParameterVisible(p, values))
                        .map((p) => (
                          <ParameterField
                            key={p.name}
                            param={p}
                            value={values[p.name]}
                            onChange={(v) => setValues((prev) => ({ ...prev, [p.name]: v }))}
                          />
                        ))}
                    </div>
                  )}

                  {/* Actions */}
                  <div className="flex flex-col gap-2 mt-1">
                    <button
                      onClick={() => void handleSpawn()}
                      disabled={!canRun}
                      title={
                        canRun
                          ? "Start a new AI session with this prompt"
                          : `Fill required fields: ${missing.join(", ")}`
                      }
                      className="flex items-center justify-center gap-2 px-4 py-2.5 bg-[#7aa2f7] hover:bg-[#6a92e7] disabled:bg-[#2a2d3d] disabled:text-[#565f89] text-[#1a1b26] text-sm font-medium rounded transition-colors"
                    >
                      <Play className="w-4 h-4" />
                      Start new session
                    </button>
                    <div className="flex items-center gap-3 text-[11px]">
                      <div className="relative">
                        <button
                          onClick={() => setShowInsertMenu((v) => !v)}
                          disabled={!canRun || sessions.length === 0}
                          title={
                            sessions.length === 0
                              ? "No open terminal sessions"
                              : "Prefill the prompt into an open session (you press Enter)"
                          }
                          className="flex items-center gap-1 text-[#7aa2f7] hover:text-[#9bb8fb] disabled:text-[#565f89] transition-colors"
                        >
                          Insert into session
                          <ChevronDown className="w-3 h-3" />
                        </button>
                        {showInsertMenu && sessions.length > 0 && (
                          <div className="absolute bottom-full left-0 mb-1 min-w-[220px] max-h-[200px] overflow-y-auto scrollbar-dark bg-[#13141f] border border-[#2a2d3d] rounded shadow-xl z-10">
                            {sessions.map((s) => (
                              <button
                                key={s.id}
                                onClick={() => handleInsert(s.id)}
                                className="w-full text-left px-3 py-1.5 text-[11px] text-[#c0caf5] hover:bg-[#24283b] transition-colors truncate"
                              >
                                {s.title}
                                {s.id === defaultInsertTarget?.id && (
                                  <span className="text-[#565f89]"> · focused</span>
                                )}
                              </button>
                            ))}
                          </div>
                        )}
                      </div>
                      <button
                        onClick={() => void handleCopy()}
                        disabled={!canRun}
                        className="flex items-center gap-1 text-[#7aa2f7] hover:text-[#9bb8fb] disabled:text-[#565f89] transition-colors"
                      >
                        <Copy className="w-3 h-3" />
                        Copy
                      </button>
                      {feedback && <span className="text-[#9ece6a]">{feedback}</span>}
                    </div>
                    {missing.length > 0 && (
                      <div className="text-[10px] text-[#565f89]">
                        Fill the required fields to continue: {missing.join(", ")}
                      </div>
                    )}
                  </div>

                  {/* Read-only rendered preview, collapsed by default */}
                  <div className="border-t border-[#2a2d3d] pt-2">
                    <button
                      onClick={() => setShowPreview((v) => !v)}
                      className="flex items-center gap-1 text-[11px] text-[#565f89] hover:text-[#a9b1d6] transition-colors"
                    >
                      {showPreview ? (
                        <ChevronDown className="w-3 h-3" />
                      ) : (
                        <ChevronRight className="w-3 h-3" />
                      )}
                      Show what will run
                    </button>
                    {showPreview && (
                      <pre className="mt-2 p-3 bg-[#13141f] border border-[#2a2d3d] rounded text-[11px] text-[#a9b1d6] whitespace-pre-wrap break-words max-h-[180px] overflow-y-auto scrollbar-dark">
                        {rendered}
                      </pre>
                    )}
                  </div>
                </div>
              ) : (
                <div className="flex-1 flex items-center justify-center text-[12px] text-[#565f89]">
                  Select a prompt
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Degraded auth state ──────────────────────────────────────────────── */

function DegradedState({
  auth,
  reason,
  onRefresh,
}: {
  auth: PromptLibraryAuth;
  reason?: string;
  onRefresh: () => void;
}) {
  const title =
    auth.state === "unpaired" ? "Runner not paired" : "Not authorized to read the prompt library";
  const hint =
    auth.state === "unpaired"
      ? "Pair this runner with coord to load your prompt library."
      : "The runner's device credential was rejected — re-pairing usually fixes this.";
  return (
    <div className="flex-1 flex flex-col items-center justify-center gap-2 px-8 text-center">
      <AlertTriangle className="w-8 h-8 text-[#e0af68]" />
      <div className="text-[13px] font-medium text-[#c0caf5]">{title}</div>
      <div className="text-[11px] text-[#a9b1d6]">{hint}</div>
      {reason && <div className="text-[10px] text-[#565f89]">{reason}</div>}
      <button
        onClick={onRefresh}
        className="mt-2 flex items-center gap-1.5 px-3 py-1.5 text-[11px] text-[#7aa2f7] hover:text-[#9bb8fb] border border-[#2a2d3d] rounded transition-colors"
      >
        <RefreshCw className="w-3 h-3" />
        Retry
      </button>
    </div>
  );
}

/* ── Parameter field ──────────────────────────────────────────────────── */

function ParameterField({
  param,
  value,
  onChange,
}: {
  param: PromptParameter;
  value: string | number | boolean | undefined;
  onChange: (v: string | number | boolean | undefined) => void;
}) {
  const inputClass =
    "w-full bg-[#13141f] border border-[#2a2d3d] rounded px-3 py-1.5 text-[12px] text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors";

  return (
    <label className="flex flex-col gap-1">
      <span className="text-[11px] text-[#a9b1d6]">
        {param.label}
        {param.required && <span className="text-[#f7768e]"> *</span>}
      </span>
      {param.type === "boolean" ? (
        <span className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={Boolean(value)}
            onChange={(e) => onChange(e.target.checked)}
            className="accent-[#7aa2f7]"
          />
          {param.description && (
            <span className="text-[10px] text-[#565f89]">{param.description}</span>
          )}
        </span>
      ) : param.type === "select" ? (
        <select
          value={value === undefined ? "" : String(value)}
          onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
          className={inputClass}
        >
          <option value="">— select —</option>
          {(param.options ?? []).map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      ) : param.type === "number" ? (
        <input
          type="number"
          value={value === undefined ? "" : String(value)}
          min={param.min}
          max={param.max}
          placeholder={param.placeholder}
          onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))}
          className={inputClass}
        />
      ) : (
        <input
          type="text"
          value={value === undefined ? "" : String(value)}
          placeholder={param.placeholder}
          onChange={(e) => onChange(e.target.value)}
          className={inputClass}
        />
      )}
      {param.type !== "boolean" && param.description && (
        <span className="text-[10px] text-[#565f89]">{param.description}</span>
      )}
    </label>
  );
}
