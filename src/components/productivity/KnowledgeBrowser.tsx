/**
 * KnowledgeBrowser
 *
 * V1 surface for `productivity_knowledge`. Two modes:
 *
 *   - `modal` (default) — rendered as a centred modal over the app, with
 *     a backdrop. Triggered globally via `Ctrl+Shift+K` and via a
 *     "Knowledge" button on the Productivity tab top bar.
 *   - `inline` — embedded as a full-height panel in the Productivity
 *     tab's `knowledge` sub-view. No backdrop, no close button.
 *
 * v1 supports FTS-only search (per plan §6 Knowledge Browser). Vector
 * search is exposed via the PG layer for advanced callers but is not
 * surfaced in this component yet.
 *
 * Each result row shows: summary, area, source-session id (link), and a
 * disclosure toggle for the body.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, Search, X, Loader2, AlertCircle, ChevronDown, ChevronRight } from "lucide-react";
import { searchKnowledge, type KnowledgeHit } from "./knowledgeApi";

export interface KnowledgeBrowserProps {
  /**
   * `modal` overlays the page with a backdrop and a centred dialog.
   * `inline` renders flush as a full-height panel (used by the
   * Productivity tab's `knowledge` sub-view). Defaults to `modal`.
   */
  mode?: "modal" | "inline";
  /**
   * Whether the modal is open. Ignored in `inline` mode.
   */
  open?: boolean;
  /** Called when the user dismisses the modal (Esc, backdrop, X). */
  onClose?: () => void;
}

const DEFAULT_TOP_K = 15;

export function KnowledgeBrowser({ mode = "modal", open = true, onClose }: KnowledgeBrowserProps) {
  const [query, setQuery] = useState("");
  const [areaFilter, setAreaFilter] = useState<string>("");
  const [topK, _setTopK] = useState(DEFAULT_TOP_K);
  const [hits, setHits] = useState<KnowledgeHit[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const inputRef = useRef<HTMLInputElement | null>(null);

  // Distinct areas across the current hit set, used to populate the
  // optional area-filter dropdown. Keeps it dynamic — we don't need to
  // hardcode the list of valid areas.
  const distinctAreas = useMemo(() => {
    const set = new Set<string>();
    for (const h of hits) set.add(h.row.area);
    return Array.from(set).sort();
  }, [hits]);

  // Auto-focus the input when modal opens or inline mounts.
  useEffect(() => {
    if (mode === "inline" || open) {
      const t = setTimeout(() => inputRef.current?.focus(), 50);
      return () => clearTimeout(t);
    }
  }, [mode, open]);

  // Esc-to-close in modal mode.
  useEffect(() => {
    if (mode !== "modal" || !open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose?.();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [mode, open, onClose]);

  const runSearch = useCallback(async () => {
    if (!query.trim()) {
      setHits([]);
      setError(null);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const results = await searchKnowledge(query, areaFilter ? areaFilter : null, topK);
      setHits(results);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setHits([]);
    } finally {
      setLoading(false);
    }
  }, [query, areaFilter, topK]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      runSearch();
    }
  };

  const toggleExpanded = (id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (mode === "modal" && !open) {
    return null;
  }

  const body = (
    <div
      className="flex flex-col h-full bg-background"
      data-page-id={mode === "modal" ? "productivity-knowledge-modal" : "productivity-knowledge"}
      data-ui-bridge-id="productivity.knowledge-browser"
    >
      {/* Header / search bar */}
      <div className="flex items-center gap-2 px-4 py-3 border-b border-border bg-card/40">
        {mode === "modal" && <BookOpen className="w-4 h-4 text-primary" aria-hidden="true" />}
        <div className="flex-1 flex items-center gap-2">
          <div className="relative flex-1">
            <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground pointer-events-none" />
            <input
              ref={inputRef}
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="Search knowledge…"
              data-ui-bridge-id="productivity.knowledge-search-input"
              className="w-full pl-7 pr-3 py-1.5 text-sm rounded-md border border-border bg-background text-foreground focus:outline-none focus:ring-1 focus:ring-primary"
            />
          </div>
          <select
            value={areaFilter}
            onChange={(e) => setAreaFilter(e.target.value)}
            data-ui-bridge-id="productivity.knowledge-area-filter"
            className="px-2 py-1.5 text-sm rounded-md border border-border bg-background text-foreground focus:outline-none focus:ring-1 focus:ring-primary"
            aria-label="Filter by area"
          >
            <option value="">All areas</option>
            {distinctAreas.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
          <button
            onClick={runSearch}
            disabled={loading || !query.trim()}
            data-ui-bridge-id="productivity.knowledge-search-submit"
            className="px-3 py-1.5 text-sm font-medium rounded-md bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {loading ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : "Search"}
          </button>
        </div>
        {mode === "modal" && (
          <button
            onClick={() => onClose?.()}
            data-ui-bridge-id="productivity.knowledge-close"
            className="p-1.5 rounded-md text-muted-foreground hover:text-foreground hover:bg-muted/30"
            aria-label="Close knowledge browser"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Results */}
      <div
        className="flex-1 min-h-0 overflow-y-auto"
        data-ui-bridge-id="productivity.knowledge-results"
      >
        {error && (
          <div className="m-4 flex items-start gap-2 p-3 rounded-md border border-destructive/30 bg-destructive/10 text-destructive">
            <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
            <div className="text-sm">{error}</div>
          </div>
        )}
        {!loading && !error && hits.length === 0 && query.trim() && (
          <div className="m-4 text-sm text-muted-foreground">
            No results for &ldquo;{query}&rdquo;
            {areaFilter ? ` in area "${areaFilter}"` : ""}.
          </div>
        )}
        {!loading && !query.trim() && hits.length === 0 && (
          <div className="m-4 text-sm text-muted-foreground">
            Type a query and press Enter. The knowledge table is populated by{" "}
            <code>/summarize-session</code> after each task completion.
          </div>
        )}
        <ul className="divide-y divide-border">
          {hits.map((hit) => {
            const isOpen = expanded.has(hit.row.id);
            return (
              <li
                key={hit.row.id}
                data-ui-bridge-id="productivity.knowledge-result-row"
                className="px-4 py-3 hover:bg-muted/20"
              >
                <button
                  onClick={() => toggleExpanded(hit.row.id)}
                  className="w-full flex items-start gap-2 text-left"
                  aria-expanded={isOpen}
                >
                  {isOpen ? (
                    <ChevronDown className="w-4 h-4 mt-0.5 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 mt-0.5 text-muted-foreground flex-shrink-0" />
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 text-xs">
                      <span className="px-1.5 py-0.5 rounded-sm bg-primary/10 text-primary font-medium">
                        {hit.row.area}
                      </span>
                      {hit.row.sessionId && (
                        <span className="text-muted-foreground truncate" title={hit.row.sessionId}>
                          session: {hit.row.sessionId.slice(0, 8)}…
                        </span>
                      )}
                      <span className="text-muted-foreground ml-auto">
                        score {hit.score.toFixed(3)}
                      </span>
                    </div>
                    <div className="mt-1 text-sm text-foreground">{hit.row.summary}</div>
                  </div>
                </button>
                {isOpen && (
                  <pre className="mt-2 ml-6 p-3 rounded-md bg-muted/30 text-xs whitespace-pre-wrap font-mono text-foreground overflow-x-auto">
                    {hit.row.body}
                  </pre>
                )}
              </li>
            );
          })}
        </ul>
      </div>

      {/* Footer */}
      <div className="px-4 py-2 border-t border-border bg-card/40 text-xs text-muted-foreground flex items-center gap-2">
        <span>
          {hits.length} result{hits.length === 1 ? "" : "s"}
        </span>
        {mode === "modal" && <span className="ml-auto">Press Esc to close</span>}
      </div>
    </div>
  );

  if (mode === "inline") {
    return body;
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6"
      role="dialog"
      aria-modal="true"
      aria-label="Knowledge browser"
    >
      <div
        className="absolute inset-0 bg-black/40"
        onClick={() => onClose?.()}
        aria-hidden="true"
      />
      <div
        className="relative w-full max-w-3xl h-[70vh] rounded-lg border border-border bg-background shadow-2xl overflow-hidden"
        data-ui-bridge-id="productivity.knowledge-modal"
      >
        {body}
      </div>
    </div>
  );
}

export default KnowledgeBrowser;

// ============================================================================
// Global Ctrl+Shift+K trigger
// ============================================================================

/**
 * Hook that toggles a state value when the user presses Ctrl+Shift+K
 * anywhere in the app. Mounted once at the App.tsx level (or wherever
 * we want the global shortcut to live). Returns `[open, setOpen]`.
 *
 * UI Bridge automation can also trigger the modal by dispatching a
 * `productivity-open-knowledge-modal` CustomEvent on `window`, since
 * synthetic KeyboardEvents from `/control/page/evaluate` carry
 * `isTrusted=false` and may be filtered out by browser-level checks.
 */
export function useKnowledgeBrowserHotkey(): [boolean, (v: boolean) => void] {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const keyHandler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && (e.key === "K" || e.key === "k")) {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    const eventHandler = (e: Event) => {
      const detail = (e as CustomEvent<{ open?: boolean }>).detail;
      if (detail && typeof detail.open === "boolean") {
        setOpen(detail.open);
      } else {
        setOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", keyHandler);
    window.addEventListener("productivity-open-knowledge-modal", eventHandler);
    return () => {
      window.removeEventListener("keydown", keyHandler);
      window.removeEventListener("productivity-open-knowledge-modal", eventHandler);
    };
  }, []);
  return [open, setOpen];
}
