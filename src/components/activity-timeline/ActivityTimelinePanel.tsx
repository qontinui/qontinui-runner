/**
 * Activity Timeline search panel — screenpipe-inspired searchable capture history.
 *
 * Provides full-text search over captured screen content from UI Bridge snapshots,
 * OCR, and accessibility trees. Results are ranked by relevance.
 *
 * All interactive elements are registered with UI Bridge for AI control.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIComponent, useUIElement } from "ui-bridge";
import { Search, Clock, Filter, ExternalLink } from "lucide-react";

interface TimelineSearchResult {
  id: number;
  textPreview: string;
  sourceType: string;
  captureMode: string;
  appName: string | null;
  windowTitle: string | null;
  url: string | null;
  screenshotPath: string | null;
  elementCount: number | null;
  confidence: number | null;
  createdAt: string;
  rank: number | null;
}

interface TimelineStat {
  sourceType: string;
  captureMode: string;
  count: number;
  latestCapture: string;
}

export function ActivityTimelinePanel() {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<TimelineSearchResult[]>([]);
  const [stats, setStats] = useState<TimelineStat[]>([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);
  const [showFilters, setShowFilters] = useState(false);
  const [appFilter, setAppFilter] = useState("");
  const [sourceFilter, setSourceFilter] = useState("");

  const handleSearch = useCallback(async () => {
    if (!query.trim()) return;
    setLoading(true);
    setSearched(true);
    try {
      const res = await invoke<TimelineSearchResult[]>(
        appFilter || sourceFilter
          ? "search_activity_timeline_filtered"
          : "search_activity_timeline",
        appFilter || sourceFilter
          ? {
              query: query.trim(),
              appName: appFilter || null,
              sourceType: sourceFilter || null,
              taskRunId: null,
              maxResults: 50,
            }
          : { query: query.trim(), maxResults: 50 }
      );
      setResults(res);
    } catch (err) {
      console.error("Timeline search failed:", err);
      setResults([]);
    } finally {
      setLoading(false);
    }
  }, [query, appFilter, sourceFilter]);

  const loadStats = useCallback(async () => {
    try {
      const s = await invoke<TimelineStat[]>("get_activity_timeline_stats");
      setStats(s);
    } catch {
      // PG not available
    }
  }, []);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") handleSearch();
  };

  const formatTime = (iso: string) => {
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  };

  const sourceColor = (source: string) => {
    switch (source) {
      case "ui_bridge": return "bg-blue-500/20 text-blue-400";
      case "accessibility": return "bg-green-500/20 text-green-400";
      case "ocr": return "bg-amber-500/20 text-amber-400";
      default: return "bg-gray-500/20 text-gray-400";
    }
  };

  // ---- UI Bridge registrations ----

  useUIComponent({
    id: "activity-timeline",
    name: "Activity Timeline",
    description: "Searchable capture history — full-text search over screen content from UI Bridge, accessibility trees, and OCR",
    actions: [
      {
        id: "search",
        label: "Search Timeline",
        handler: async () => { await handleSearch(); },
      },
      {
        id: "load-stats",
        label: "Load Stats",
        handler: async () => { await loadStats(); },
      },
      {
        id: "toggle-filters",
        label: "Toggle Filters",
        handler: async () => { setShowFilters((v) => !v); },
      },
      {
        id: "clear-search",
        label: "Clear Search",
        handler: async () => { setQuery(""); setResults([]); setSearched(false); },
      },
    ],
  });

  const { ref: searchInputRef } = useUIElement({
    id: "timeline-search-input",
    type: "input",
    label: "Timeline search query input",
  });

  const { ref: searchButtonRef } = useUIElement({
    id: "timeline-search-button",
    type: "button",
    label: "Search button",
    actions: ["click"],
  });

  const { ref: filterToggleRef } = useUIElement({
    id: "timeline-filter-toggle",
    type: "button",
    label: "Toggle filter panel",
    actions: ["click"],
  });

  const { ref: statsButtonRef } = useUIElement({
    id: "timeline-stats-button",
    type: "button",
    label: "Load capture statistics",
    actions: ["click"],
  });

  const { ref: appFilterRef } = useUIElement({
    id: "timeline-app-filter",
    type: "input",
    label: "Filter by application name",
  });

  const { ref: sourceFilterRef } = useUIElement({
    id: "timeline-source-filter",
    type: "select",
    label: "Filter by capture source type",
  });

  const { ref: resultsRef } = useUIElement({
    id: "timeline-results",
    type: "generic",
    label: `Search results (${results.length} entries)`,
  });

  return (
    <div className="flex flex-col h-full p-4 gap-3" data-tutorial-id="timeline-page">
      {/* Search bar */}
      <div className="flex gap-2" data-tutorial-id="timeline-search-bar">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
          <input
            ref={searchInputRef}
            type="text"
            placeholder="Search activity timeline... (e.g., 'login button disabled')"
            className="w-full pl-9 pr-3 py-2 rounded-md border border-border bg-background text-sm focus:outline-none focus:ring-1 focus:ring-ring"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
          />
        </div>
        <button
          ref={searchButtonRef}
          onClick={handleSearch}
          disabled={loading || !query.trim()}
          className="px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
        >
          {loading ? "Searching..." : "Search"}
        </button>
        <button
          ref={filterToggleRef}
          onClick={() => setShowFilters(!showFilters)}
          className="px-3 py-2 rounded-md border border-border text-sm hover:bg-accent"
        >
          <Filter className="h-4 w-4" />
        </button>
        <button
          ref={statsButtonRef}
          onClick={loadStats}
          className="px-3 py-2 rounded-md border border-border text-sm hover:bg-accent"
          title="Load stats"
        >
          <Clock className="h-4 w-4" />
        </button>
      </div>

      {/* Filters (collapsible) */}
      {showFilters && (
        <div className="flex gap-2 text-sm">
          <input
            ref={appFilterRef}
            type="text"
            placeholder="App name filter"
            className="flex-1 px-3 py-1.5 rounded-md border border-border bg-background text-sm"
            value={appFilter}
            onChange={(e) => setAppFilter(e.target.value)}
          />
          <select
            ref={sourceFilterRef}
            className="px-3 py-1.5 rounded-md border border-border bg-background text-sm"
            value={sourceFilter}
            onChange={(e) => setSourceFilter(e.target.value)}
          >
            <option value="">All sources</option>
            <option value="ui_bridge">UI Bridge</option>
            <option value="accessibility">Accessibility</option>
            <option value="ocr">OCR</option>
          </select>
        </div>
      )}

      {/* Stats row */}
      {stats.length > 0 && (
        <div className="flex gap-2 flex-wrap">
          {stats.map((s) => (
            <span
              key={`${s.sourceType}-${s.captureMode}`}
              className={`px-2 py-0.5 rounded text-xs ${sourceColor(s.sourceType)}`}
            >
              {s.sourceType} ({s.captureMode}): {s.count.toLocaleString()} entries
            </span>
          ))}
        </div>
      )}

      {/* Results */}
      <div ref={resultsRef} className="flex-1 overflow-auto" data-tutorial-id="timeline-results">
        {searched && results.length === 0 && !loading && (
          <div className="text-center text-muted-foreground py-8 text-sm">
            No results found. Try a different query.
          </div>
        )}
        {results.map((r) => (
          <div
            key={r.id}
            className="border border-border rounded-md p-3 mb-2 hover:bg-accent/50 transition-colors"
          >
            <div className="flex items-center gap-2 mb-1">
              <span className={`px-1.5 py-0.5 rounded text-xs font-medium ${sourceColor(r.sourceType)}`}>
                {r.sourceType}
              </span>
              {r.appName && (
                <span className="text-xs text-muted-foreground">{r.appName}</span>
              )}
              <span className="text-xs text-muted-foreground ml-auto">
                {formatTime(r.createdAt)}
              </span>
              {r.rank != null && (
                <span className="text-xs text-muted-foreground">
                  rank: {r.rank.toFixed(2)}
                </span>
              )}
            </div>
            <p className="text-sm text-foreground line-clamp-3">{r.textPreview}</p>
            {r.url && (
              <div className="flex items-center gap-1 mt-1 text-xs text-muted-foreground">
                <ExternalLink className="h-3 w-3" />
                <span className="truncate">{r.url}</span>
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
