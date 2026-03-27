/**
 * CommandPalette.tsx
 *
 * Global unified search modal triggered by Cmd+K / Ctrl+K.
 * Searches across findings, fixes, rules, errors, and workflows
 * using the knowledge graph unified search endpoint.
 */

import { useEffect, useState } from "react";
import { Search, FileText, Wrench, Brain, AlertCircle, BookOpen, Workflow } from "lucide-react";
import { useUnifiedSearch } from "../../hooks/useGraphAnalytics";
import type { UnifiedSearchResult } from "../../hooks/useGraphAnalytics";

const ENTITY_ICONS: Record<string, typeof Search> = {
  finding: AlertCircle,
  fix: Wrench,
  knowledge: Brain,
  error: AlertCircle,
  rule: BookOpen,
  workflow: Workflow,
};

const ENTITY_COLORS: Record<string, string> = {
  finding: "text-red-400",
  fix: "text-green-400",
  knowledge: "text-blue-400",
  error: "text-orange-400",
  rule: "text-purple-400",
  workflow: "text-cyan-400",
};

export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const { data: results, isLoading } = useUnifiedSearch(open ? query : "");

  // Cmd+K / Ctrl+K handler
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setOpen(prev => !prev);
      }
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Reset query when closing
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset side-effect on close
    if (!open) setQuery("");
  }, [open]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]" onClick={() => setOpen(false)}>
      <div className="fixed inset-0 bg-black/50" />
      <div
        className="relative w-full max-w-lg bg-popover border border-border rounded-xl shadow-2xl overflow-hidden"
        onClick={e => e.stopPropagation()}
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-border">
          <Search className="w-4 h-4 text-muted-foreground" />
          <input
            autoFocus
            type="text"
            value={query}
            onChange={e => setQuery(e.target.value)}
            placeholder="Search findings, fixes, rules, errors, workflows..."
            className="flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
          />
          <kbd className="hidden sm:inline-flex items-center gap-0.5 px-1.5 py-0.5 rounded border border-border text-[10px] text-muted-foreground">
            ESC
          </kbd>
        </div>
        <div className="max-h-80 overflow-y-auto">
          {isLoading && query.length >= 2 && (
            <div className="p-4 text-sm text-muted-foreground text-center">Searching...</div>
          )}
          {!isLoading && query.length >= 2 && results && results.length === 0 && (
            <div className="p-4 text-sm text-muted-foreground text-center">No results</div>
          )}
          {query.length < 2 && (
            <div className="p-4 text-sm text-muted-foreground text-center">Type at least 2 characters to search</div>
          )}
          {results && results.length > 0 && results.map((r: UnifiedSearchResult, i: number) => {
            const Icon = ENTITY_ICONS[r.entity_type] || FileText;
            const color = ENTITY_COLORS[r.entity_type] || "text-muted-foreground";
            return (
              <button
                key={`${r.entity_type}-${r.entity_id}-${i}`}
                className="w-full flex items-center gap-3 px-4 py-2.5 hover:bg-accent text-left transition-colors"
                onClick={() => setOpen(false)}
              >
                <Icon className={`w-4 h-4 ${color}`} />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium truncate">{r.title}</p>
                  <p className="text-xs text-muted-foreground truncate">{r.snippet}</p>
                </div>
                <span className="text-xs text-muted-foreground capitalize">{r.entity_type}</span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
