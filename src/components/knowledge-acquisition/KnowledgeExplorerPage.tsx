/**
 * KnowledgeExplorerPage — search and browse the knowledge acquisition system.
 *
 * Two tabs:
 * - **Search**: Manual search with provider selection, results display with markdown
 * - **Stats**: Full dashboard with per-provider metrics and system health
 */

import { useState } from "react";
import {
  Search,
  ShieldAlert,
  Globe,
  Bug,
  BookOpen,
  BarChart3,
  ExternalLink,
  Clock,
  Loader2,
  AlertCircle,
  FileSearch,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  useKnowledgeSearch,
  useResearchError,
  useVulnerabilitySearch,
  useKnowledgeProviders,
} from "@/hooks/useKnowledgeAcquisition";
import { KnowledgeStatsWidget } from "./KnowledgeStatsWidget";
import type { KnowledgeResult } from "@/services/knowledge-acquisition-service";

type Tab = "search" | "stats";
type SearchMode = "web" | "error" | "vulnerability";

const SEARCH_MODES: {
  id: SearchMode;
  label: string;
  icon: React.ElementType;
  placeholder: string;
}[] = [
  { id: "web", label: "Web Search", icon: Globe, placeholder: "Search the web for anything..." },
  { id: "error", label: "Error Research", icon: Bug, placeholder: "Paste an error message..." },
  {
    id: "vulnerability",
    label: "Vuln Search",
    icon: ShieldAlert,
    placeholder: "CVE ID or software name + version...",
  },
];

export function KnowledgeExplorerPage() {
  const [tab, setTab] = useState<Tab>("search");

  return (
    <div className="flex flex-col h-full">
      {/* Tab bar */}
      <div className="flex items-center gap-1 px-3 pt-3 pb-1 border-b border-border">
        <TabButton
          active={tab === "search"}
          onClick={() => setTab("search")}
          icon={<Search className="w-3.5 h-3.5" />}
        >
          Search
        </TabButton>
        <TabButton
          active={tab === "stats"}
          onClick={() => setTab("stats")}
          icon={<BarChart3 className="w-3.5 h-3.5" />}
        >
          Stats
        </TabButton>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto">
        {tab === "search" && <SearchTab />}
        {tab === "stats" && <StatsTab />}
      </div>
    </div>
  );
}

// ============================================================================
// Search Tab
// ============================================================================

function SearchTab() {
  const [mode, setMode] = useState<SearchMode>("web");
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<KnowledgeResult[]>([]);
  const [providersUsed, setProvidersUsed] = useState<string[]>([]);

  const webSearch = useKnowledgeSearch();
  const errorSearch = useResearchError();
  const vulnSearch = useVulnerabilitySearch();

  const isLoading = webSearch.isPending || errorSearch.isPending || vulnSearch.isPending;

  const handleSearch = async () => {
    if (!query.trim() || query.trim().length < 2) return;

    try {
      let response;
      if (mode === "web") {
        response = await webSearch.mutateAsync({ query: query.trim() });
      } else if (mode === "error") {
        response = await errorSearch.mutateAsync({
          errorMessage: query.trim(),
        });
      } else {
        response = await vulnSearch.mutateAsync(query.trim());
      }
      setResults(response.results);
      setProvidersUsed(response.provider_used);
    } catch {
      setResults([]);
      setProvidersUsed([]);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSearch();
    }
  };

  const currentMode = SEARCH_MODES.find((m) => m.id === mode)!;
  const ModeIcon = currentMode.icon;
  const error = webSearch.error || errorSearch.error || vulnSearch.error;

  return (
    <div className="flex flex-col h-full">
      {/* Search Controls */}
      <div className="flex-none p-4 space-y-3 border-b border-border/50">
        {/* Mode selector */}
        <div className="flex gap-1">
          {SEARCH_MODES.map((m) => {
            const Icon = m.icon;
            return (
              <button
                key={m.id}
                onClick={() => setMode(m.id)}
                className={cn(
                  "flex items-center gap-1.5 px-2.5 py-1 text-xs rounded-md transition-colors",
                  mode === m.id
                    ? "bg-primary/10 text-primary border border-primary/30"
                    : "text-muted-foreground hover:text-foreground hover:bg-muted",
                )}
              >
                <Icon className="w-3 h-3" />
                {m.label}
              </button>
            );
          })}
        </div>

        {/* Search input */}
        <div className="flex gap-2">
          <div className="relative flex-1">
            <ModeIcon className="absolute left-2.5 top-2.5 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              aria-label="Search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={currentMode.placeholder}
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-ring"
            />
          </div>
          <button
            onClick={handleSearch}
            disabled={isLoading || query.trim().length < 2}
            className="px-4 py-2 text-sm font-medium bg-primary text-primary-foreground rounded-md hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-1.5"
          >
            {isLoading ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Search className="w-3.5 h-3.5" />
            )}
            Search
          </button>
        </div>
      </div>

      {/* Results */}
      <div className="flex-1 overflow-auto p-4">
        {error && (
          <div className="flex items-center gap-2 p-3 mb-4 bg-red-500/10 border border-red-500/20 rounded-lg text-red-400 text-sm">
            <AlertCircle className="w-4 h-4 flex-shrink-0" />
            {(error as Error).message}
          </div>
        )}

        {!isLoading && results.length === 0 && !error && (
          <div className="flex flex-col items-center justify-center h-48 text-muted-foreground">
            <FileSearch className="w-8 h-8 mb-2 opacity-50" />
            <p className="text-sm">
              {query ? "No results found" : "Enter a search query to explore knowledge"}
            </p>
          </div>
        )}

        {results.length > 0 && (
          <div className="space-y-1 mb-3">
            <div className="flex items-center justify-between text-xs text-muted-foreground">
              <span>
                {results.length} result{results.length !== 1 ? "s" : ""}
              </span>
              <span>via {providersUsed.map((p) => formatProvider(p)).join(", ")}</span>
            </div>
          </div>
        )}

        <div className="space-y-3">
          {results.map((result, i) => (
            <ResultCard key={`${result.provider}-${i}`} result={result} />
          ))}
        </div>
      </div>
    </div>
  );
}

// ============================================================================
// Stats Tab
// ============================================================================

function StatsTab() {
  const { data: providers } = useKnowledgeProviders();

  return (
    <div className="p-4 space-y-4">
      {/* Full stats widget */}
      <KnowledgeStatsWidget />

      {/* Provider availability */}
      {providers && (
        <div className="bg-card rounded-lg border border-border">
          <div className="px-3 py-2 border-b border-border/50 bg-muted/30">
            <span className="text-xs font-medium">Provider Availability</span>
          </div>
          <div className="p-3 space-y-2">
            {providers.map((p) => (
              <div key={p.name} className="flex items-center justify-between text-xs">
                <span className="text-foreground">{formatProvider(p.name)}</span>
                <div className="flex items-center gap-2">
                  {p.requires_api_key && (
                    <span className="text-[10px] text-muted-foreground">API key required</span>
                  )}
                  <span
                    className={cn(
                      "w-2 h-2 rounded-full",
                      p.available ? "bg-green-400" : "bg-zinc-600",
                    )}
                  />
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Result Card
// ============================================================================

function ResultCard({ result }: { result: KnowledgeResult }) {
  const [expanded, setExpanded] = useState(false);
  const hasLongContent = result.content.length > 500;
  const displayContent = expanded ? result.content : result.content.slice(0, 500);

  return (
    <div className="bg-card rounded-lg border border-border overflow-hidden">
      {/* Header */}
      <div className="px-3 py-2 flex items-start gap-3">
        <div
          className={cn("mt-0.5 p-1.5 rounded flex-shrink-0", getProviderColor(result.provider))}
        >
          {getProviderIcon(result.provider)}
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h4 className="text-sm font-medium text-foreground truncate">{result.title}</h4>
          </div>
          <div className="flex items-center gap-2 mt-0.5">
            <span className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] bg-muted text-muted-foreground">
              {formatProvider(result.provider)}
            </span>
            {result.relevance_score != null && (
              <span className="text-[10px] font-mono text-muted-foreground">
                {Math.round(result.relevance_score * 100)}% relevant
              </span>
            )}
            {result.metadata?.cvss_score != null && (
              <span
                className={cn(
                  "inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-semibold",
                  getCvssColor(result.metadata.cvss_score),
                )}
              >
                CVSS {result.metadata.cvss_score.toFixed(1)}
              </span>
            )}
          </div>
        </div>
        {result.url && (
          <a
            href={result.url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-muted-foreground hover:text-foreground flex-shrink-0"
          >
            <ExternalLink className="w-3.5 h-3.5" />
          </a>
        )}
      </div>

      {/* Content */}
      <div className="px-3 pb-3">
        <pre className="text-xs text-muted-foreground whitespace-pre-wrap font-sans leading-relaxed">
          {displayContent}
          {hasLongContent && !expanded && "..."}
        </pre>
        {hasLongContent && (
          <button
            onClick={() => setExpanded(!expanded)}
            className="text-[10px] text-primary hover:underline mt-1"
          >
            {expanded ? "Show less" : "Show more"}
          </button>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Shared Components & Helpers
// ============================================================================

function TabButton({
  active,
  onClick,
  icon,
  children,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-t-md transition-colors",
        active
          ? "bg-card text-foreground border border-b-0 border-border"
          : "text-muted-foreground hover:text-foreground hover:bg-muted/50",
      )}
    >
      {icon}
      {children}
    </button>
  );
}

function formatProvider(name: string): string {
  const names: Record<string, string> = {
    duckduckgo: "DuckDuckGo",
    tavily: "Tavily",
    ui_bridge: "UI Bridge",
    sploitus: "Sploitus",
    osv_dev: "OSV.dev",
    perplexity: "Perplexity",
    local_knowledge: "Local Cache",
  };
  return names[name] || name;
}

function getProviderIcon(provider: string) {
  switch (provider) {
    case "duckduckgo":
      return <Globe className="w-3.5 h-3.5" />;
    case "tavily":
      return <BookOpen className="w-3.5 h-3.5" />;
    case "sploitus":
      return <ShieldAlert className="w-3.5 h-3.5" />;
    case "osv_dev":
      return <ShieldAlert className="w-3.5 h-3.5" />;
    case "perplexity":
      return <Search className="w-3.5 h-3.5" />;
    case "local_knowledge":
      return <Clock className="w-3.5 h-3.5" />;
    default:
      return <Search className="w-3.5 h-3.5" />;
  }
}

function getProviderColor(provider: string): string {
  switch (provider) {
    case "duckduckgo":
      return "bg-orange-500/10 text-orange-400";
    case "tavily":
      return "bg-blue-500/10 text-blue-400";
    case "sploitus":
      return "bg-red-500/10 text-red-400";
    case "osv_dev":
      return "bg-red-500/10 text-red-400";
    case "perplexity":
      return "bg-purple-500/10 text-purple-400";
    case "local_knowledge":
      return "bg-green-500/10 text-green-400";
    default:
      return "bg-muted text-muted-foreground";
  }
}

function getCvssColor(score: number): string {
  if (score >= 9.0) return "bg-red-500/20 text-red-400";
  if (score >= 7.0) return "bg-orange-500/20 text-orange-400";
  if (score >= 4.0) return "bg-yellow-500/20 text-yellow-400";
  return "bg-green-500/20 text-green-400";
}
