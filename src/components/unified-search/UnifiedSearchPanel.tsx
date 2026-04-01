import { useState } from "react";
import { Search, FileText, Wrench, Brain, AlertCircle, BookOpen, Workflow } from "lucide-react";

interface UnifiedSearchResult {
  entity_type: string;
  entity_id: string;
  title: string;
  snippet: string;
  relevance_score: number;
  source_table: string;
}

const ENTITY_ICONS: Record<string, typeof Search> = {
  finding: AlertCircle,
  fix: Wrench,
  knowledge: Brain,
  error: AlertCircle,
  rule: BookOpen,
  workflow: Workflow,
};

const ENTITY_COLORS: Record<string, string> = {
  finding: "text-red-400 bg-red-500/10",
  fix: "text-green-400 bg-green-500/10",
  knowledge: "text-blue-400 bg-blue-500/10",
  error: "text-orange-400 bg-orange-500/10",
  rule: "text-purple-400 bg-purple-500/10",
  workflow: "text-cyan-400 bg-cyan-500/10",
};

function ResultRow({ result }: { result: UnifiedSearchResult }) {
  const Icon = ENTITY_ICONS[result.entity_type] || FileText;
  const color = ENTITY_COLORS[result.entity_type] || "text-muted-foreground bg-muted";
  const scorePct = Math.round(result.relevance_score * 100);

  return (
    <div className="flex items-start gap-3 py-2.5 px-1">
      <div className={`p-1.5 rounded ${color}`}>
        <Icon className="w-3.5 h-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <p className="text-sm font-medium text-foreground truncate">{result.title}</p>
          <span className="inline-flex items-center px-1.5 py-0.5 rounded text-xs bg-muted text-muted-foreground capitalize">
            {result.entity_type}
          </span>
        </div>
        <p className="text-xs text-muted-foreground mt-0.5 line-clamp-2">{result.snippet}</p>
      </div>
      <span className="text-xs font-mono text-muted-foreground whitespace-nowrap">{scorePct}%</span>
    </div>
  );
}

interface Props {
  results?: UnifiedSearchResult[];
  isLoading?: boolean;
  error?: Error | null;
  onSearch: (query: string) => void;
}

export default function UnifiedSearchPanel({ results, isLoading, error, onSearch }: Props) {
  const [query, setQuery] = useState("");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (query.trim().length >= 2) onSearch(query.trim());
  };

  return (
    <div className="h-full flex flex-col overflow-hidden p-4 space-y-4">
      <div className="flex items-center gap-2">
        <Search className="w-5 h-5 text-blue-400" />
        <h2 className="text-lg font-semibold">Search Everything</h2>
      </div>

      <form onSubmit={handleSubmit} className="flex gap-2">
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search findings, fixes, rules, errors..."
          className="flex-1 px-3 py-1.5 text-sm bg-muted border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring"
        />
        <button
          type="submit"
          disabled={query.trim().length < 2}
          className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md disabled:opacity-50"
        >
          Search
        </button>
      </form>

      <div className="flex-1 overflow-y-auto">
        {isLoading && <p className="text-sm text-muted-foreground">Searching...</p>}
        {error && <p className="text-sm text-red-400">Search failed</p>}
        {!isLoading && !error && results && results.length === 0 && (
          <div className="text-center py-8 text-muted-foreground">
            <Search className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">No results found</p>
          </div>
        )}
        {results && results.length > 0 && (
          <div className="bg-card rounded-lg border border-border">
            <div className="p-3 border-b border-border flex items-center justify-between">
              <h3 className="text-sm font-medium">
                {results.length} result{results.length !== 1 ? "s" : ""}
              </h3>
              <div className="flex gap-1">
                {Object.entries(
                  results.reduce<Record<string, number>>((acc, r) => {
                    acc[r.entity_type] = (acc[r.entity_type] || 0) + 1;
                    return acc;
                  }, {}),
                ).map(([type, count]) => (
                  <span
                    key={type}
                    className={`inline-flex items-center px-1.5 py-0.5 rounded text-xs ${ENTITY_COLORS[type] || "bg-muted text-muted-foreground"}`}
                  >
                    {count} {type}
                  </span>
                ))}
              </div>
            </div>
            <div className="divide-y divide-border px-3">
              {results.map((r) => (
                <ResultRow key={`${r.entity_type}-${r.entity_id}`} result={r} />
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
