import { useState } from "react";
import {
  Search,
  Filter,
  Clock,
  Eye,
  BookOpen,
  Wrench,
  AlertTriangle,
  Scale,
  GitBranch,
  Workflow,
  Layout,
  X,
  Loader2,
} from "lucide-react";
import { useMemorySearch } from "./useMemorySearch";
import { ALL_SOURCES, SOURCE_COLORS, type MemoryResult, type MemorySourceFilter } from "./types";

const SOURCE_ICON_MAP: Record<string, typeof Search> = {
  observation: Eye,
  activity_timeline: Clock,
  task_knowledge: BookOpen,
  finding: Search,
  fix: Wrench,
  error: AlertTriangle,
  rule: Scale,
  graph_node: GitBranch,
  workflow: Workflow,
  ui_element: Layout,
};

function SourceBadge({ source }: { source: string }) {
  const Icon = SOURCE_ICON_MAP[source] || Search;
  const color = SOURCE_COLORS[source] || "bg-muted text-muted-foreground";
  return (
    <span
      className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs border ${color}`}
    >
      <Icon className="w-3 h-3" />
      {source.replace(/_/g, " ")}
    </span>
  );
}

function ScoreBar({ score }: { score: number }) {
  const pct = Math.round(score * 100);
  return (
    <div className="flex items-center gap-1.5">
      <div className="w-16 h-1.5 rounded-full bg-muted overflow-hidden">
        <div
          className="h-full rounded-full bg-blue-500 transition-all"
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="text-xs font-mono text-muted-foreground whitespace-nowrap">{pct}%</span>
    </div>
  );
}

function ResultRow({ result }: { result: MemoryResult }) {
  const Icon = SOURCE_ICON_MAP[result.source] || Search;
  const color = SOURCE_COLORS[result.source] || "text-muted-foreground bg-muted";

  return (
    <div className="flex items-start gap-3 py-2.5 px-1">
      <div className={`p-1.5 rounded ${color}`}>
        <Icon className="w-3.5 h-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <p className="text-sm font-medium text-foreground truncate">{result.title}</p>
          <SourceBadge source={result.source} />
        </div>
        <p className="text-xs text-muted-foreground mt-0.5 line-clamp-2">{result.snippet}</p>
        <div className="flex items-center gap-3 mt-1.5">
          <ScoreBar score={result.fused_score} />
          {result.timestamp && (
            <span className="text-xs text-muted-foreground flex items-center gap-1">
              <Clock className="w-3 h-3" />
              {new Date(result.timestamp).toLocaleDateString()}
            </span>
          )}
          {result.found_by.length > 0 && (
            <div className="flex gap-1">
              {result.found_by.map((strategy) => (
                <span
                  key={strategy}
                  className="inline-flex items-center px-1 py-0.5 rounded text-[10px] bg-muted text-muted-foreground"
                >
                  {strategy}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export function MemorySearchPanel() {
  const [query, setQuery] = useState("");
  const [showFilters, setShowFilters] = useState(false);
  const [selectedSources, setSelectedSources] = useState<MemorySourceFilter[]>([]);
  const [dateFrom, setDateFrom] = useState("");
  const [dateTo, setDateTo] = useState("");
  const [minScore, setMinScore] = useState(0);
  const { results, total, loading, error, search, clear } = useMemorySearch();

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (query.trim().length < 2) return;
    search({
      query: query.trim(),
      sources: selectedSources.length > 0 ? selectedSources : undefined,
      from: dateFrom || undefined,
      to: dateTo || undefined,
      minScore: minScore > 0 ? minScore : undefined,
    });
  };

  const toggleSource = (source: MemorySourceFilter) => {
    setSelectedSources((prev) =>
      prev.includes(source) ? prev.filter((s) => s !== source) : [...prev, source],
    );
  };

  const handleClear = () => {
    setQuery("");
    setSelectedSources([]);
    setDateFrom("");
    setDateTo("");
    setMinScore(0);
    clear();
  };

  return (
    <div className="h-full flex flex-col overflow-hidden p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center gap-2">
        <Search className="w-5 h-5 text-blue-400" />
        <h2 className="text-lg font-semibold">Memory Search</h2>
        <span className="text-xs text-muted-foreground">RRF Fusion</span>
      </div>

      {/* Search form */}
      <form onSubmit={handleSubmit} className="flex gap-2">
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search observations, findings, rules, knowledge..."
          className="flex-1 px-3 py-1.5 text-sm bg-muted border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring"
        />
        <button
          type="button"
          onClick={() => setShowFilters((f) => !f)}
          className={`px-2.5 py-1.5 text-sm rounded-md border border-border ${showFilters ? "bg-blue-500/10 text-blue-400 border-blue-500/30" : "bg-muted text-muted-foreground"}`}
        >
          <Filter className="w-4 h-4" />
        </button>
        <button
          type="submit"
          disabled={query.trim().length < 2 || loading}
          className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md disabled:opacity-50"
        >
          {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : "Search"}
        </button>
        {(results.length > 0 || error) && (
          <button
            type="button"
            onClick={handleClear}
            className="px-2.5 py-1.5 text-sm rounded-md border border-border bg-muted text-muted-foreground hover:text-foreground"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </form>

      {/* Filters panel */}
      {showFilters && (
        <div className="bg-card rounded-lg border border-border p-3 space-y-3">
          {/* Source filters */}
          <div>
            <p className="text-xs font-medium text-muted-foreground mb-1.5">Sources</p>
            <div className="flex flex-wrap gap-1.5">
              {ALL_SOURCES.map((source) => {
                const active = selectedSources.includes(source);
                return (
                  <button
                    key={source}
                    type="button"
                    onClick={() => toggleSource(source)}
                    className={`inline-flex items-center gap-1 px-2 py-1 rounded text-xs border transition-colors ${
                      active
                        ? "bg-blue-500/10 text-blue-400 border-blue-500/30"
                        : "bg-muted text-muted-foreground border-border hover:text-foreground"
                    }`}
                  >
                    {source.replace(/_/g, " ")}
                  </button>
                );
              })}
            </div>
          </div>

          {/* Date range */}
          <div className="flex gap-3">
            <div className="flex-1">
              <label className="text-xs font-medium text-muted-foreground block mb-1">From</label>
              <input
                type="date"
                value={dateFrom}
                onChange={(e) => setDateFrom(e.target.value)}
                className="w-full px-2 py-1 text-xs bg-muted border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring"
              />
            </div>
            <div className="flex-1">
              <label className="text-xs font-medium text-muted-foreground block mb-1">To</label>
              <input
                type="date"
                value={dateTo}
                onChange={(e) => setDateTo(e.target.value)}
                className="w-full px-2 py-1 text-xs bg-muted border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring"
              />
            </div>
          </div>

          {/* Min score slider */}
          <div>
            <label className="text-xs font-medium text-muted-foreground block mb-1">
              Min score: {Math.round(minScore * 100)}%
            </label>
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              value={minScore}
              onChange={(e) => setMinScore(parseFloat(e.target.value))}
              className="w-full accent-blue-500"
            />
          </div>
        </div>
      )}

      {/* Results */}
      <div className="flex-1 overflow-y-auto">
        {loading && (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="w-4 h-4 animate-spin" />
            <span>Searching memory...</span>
          </div>
        )}
        {error && <p className="text-sm text-red-400">Search failed: {error}</p>}
        {!loading && !error && results.length === 0 && query.length > 0 && total === 0 && (
          <div className="text-center py-8 text-muted-foreground">
            <Search className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">No results found</p>
          </div>
        )}
        {!loading && results.length === 0 && query.length === 0 && (
          <div className="text-center py-8 text-muted-foreground">
            <Search className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">Search across all memory sources</p>
            <p className="text-xs mt-1">Observations, findings, rules, knowledge, and more</p>
          </div>
        )}
        {results.length > 0 && (
          <div className="bg-card rounded-lg border border-border">
            <div className="p-3 border-b border-border flex items-center justify-between">
              <h3 className="text-sm font-medium">
                {total} result{total !== 1 ? "s" : ""}
              </h3>
              <div className="flex gap-1">
                {Object.entries(
                  results.reduce<Record<string, number>>((acc, r) => {
                    acc[r.source] = (acc[r.source] || 0) + 1;
                    return acc;
                  }, {}),
                ).map(([source, count]) => (
                  <span
                    key={source}
                    className={`inline-flex items-center px-1.5 py-0.5 rounded text-xs ${SOURCE_COLORS[source] || "bg-muted text-muted-foreground"}`}
                  >
                    {count} {source.replace(/_/g, " ")}
                  </span>
                ))}
              </div>
            </div>
            <div className="divide-y divide-border px-3">
              {results.map((r) => (
                <ResultRow key={r.id} result={r} />
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
