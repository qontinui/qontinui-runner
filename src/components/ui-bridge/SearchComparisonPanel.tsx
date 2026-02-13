/**
 * Search Comparison Panel
 *
 * Compares different search strategies for finding UI Bridge elements:
 * - Exact Match: Direct ID lookup
 * - Fuzzy Match: Levenshtein distance on IDs, labels, text
 * - Attribute Match: Search by label, text, type, or tag
 *
 * Part of Phase 1 of the AI-Native UI Bridge enhancements.
 */

import { useState, useCallback, useMemo } from "react";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import {
  Search,
  Check,
  X,
  Copy,
  Eye,
  Zap,
  Target,
  Tag,
  ChevronDown,
  ChevronRight,
  Sparkles,
} from "lucide-react";
import type { ExternalElement } from "../../types/ui-bridge-types";

interface SearchComparisonPanelProps {
  elements: ExternalElement[];
  onSelectElement: (elementId: string) => void;
  onHighlightElement: (elementId: string) => void;
  disabled?: boolean;
}

interface SearchResult {
  element: ExternalElement;
  score: number;
  matchType: "exact" | "fuzzy" | "attribute";
  matchDetails: string;
}

interface SearchStrategy {
  id: string;
  name: string;
  icon: React.ReactNode;
  description: string;
  search: (query: string, elements: ExternalElement[]) => SearchResult[];
}

/**
 * Calculate Levenshtein distance between two strings
 */
function levenshteinDistance(a: string, b: string): number {
  const matrix: number[][] = [];

  for (let i = 0; i <= b.length; i++) {
    matrix[i] = [i];
  }
  for (let j = 0; j <= a.length; j++) {
    matrix[0][j] = j;
  }

  for (let i = 1; i <= b.length; i++) {
    for (let j = 1; j <= a.length; j++) {
      if (b.charAt(i - 1) === a.charAt(j - 1)) {
        matrix[i][j] = matrix[i - 1][j - 1];
      } else {
        matrix[i][j] = Math.min(
          matrix[i - 1][j - 1] + 1, // substitution
          matrix[i][j - 1] + 1, // insertion
          matrix[i - 1][j] + 1, // deletion
        );
      }
    }
  }

  return matrix[b.length][a.length];
}

/**
 * Calculate similarity score (0-1) based on Levenshtein distance
 */
function similarityScore(a: string, b: string): number {
  if (a === b) return 1;
  if (a.length === 0 || b.length === 0) return 0;

  const distance = levenshteinDistance(a.toLowerCase(), b.toLowerCase());
  const maxLength = Math.max(a.length, b.length);
  return 1 - distance / maxLength;
}

/**
 * Check if string contains query (case-insensitive)
 */
function containsQuery(text: string | undefined, query: string): boolean {
  if (!text) return false;
  return text.toLowerCase().includes(query.toLowerCase());
}

/**
 * Exact match strategy - finds elements with exact ID match
 */
function exactMatchSearch(query: string, elements: ExternalElement[]): SearchResult[] {
  const results: SearchResult[] = [];
  const lowerQuery = query.toLowerCase();

  for (const element of elements) {
    if (element.id.toLowerCase() === lowerQuery) {
      results.push({
        element,
        score: 1.0,
        matchType: "exact",
        matchDetails: "Exact ID match",
      });
    }
  }

  return results;
}

/**
 * Fuzzy match strategy - finds elements with similar IDs using Levenshtein distance
 */
function fuzzyMatchSearch(query: string, elements: ExternalElement[]): SearchResult[] {
  const results: SearchResult[] = [];
  const lowerQuery = query.toLowerCase();
  const THRESHOLD = 0.4; // Minimum similarity to include

  for (const element of elements) {
    // Check ID similarity
    const idScore = similarityScore(element.id, lowerQuery);
    if (idScore >= THRESHOLD) {
      results.push({
        element,
        score: idScore,
        matchType: "fuzzy",
        matchDetails: `ID similarity: ${(idScore * 100).toFixed(0)}%`,
      });
      continue;
    }

    // Check label similarity
    if (element.label) {
      const labelScore = similarityScore(element.label, lowerQuery);
      if (labelScore >= THRESHOLD) {
        results.push({
          element,
          score: labelScore * 0.9, // Slightly lower weight for label match
          matchType: "fuzzy",
          matchDetails: `Label similarity: ${(labelScore * 100).toFixed(0)}%`,
        });
        continue;
      }
    }

    // Check text similarity
    if (element.text) {
      const textScore = similarityScore(element.text, lowerQuery);
      if (textScore >= THRESHOLD) {
        results.push({
          element,
          score: textScore * 0.8, // Lower weight for text match
          matchType: "fuzzy",
          matchDetails: `Text similarity: ${(textScore * 100).toFixed(0)}%`,
        });
      }
    }
  }

  // Sort by score descending
  return results.sort((a, b) => b.score - a.score).slice(0, 10);
}

/**
 * Attribute match strategy - finds elements containing the query in various attributes
 */
function attributeMatchSearch(query: string, elements: ExternalElement[]): SearchResult[] {
  const results: SearchResult[] = [];

  for (const element of elements) {
    const matches: string[] = [];

    // Check various attributes
    if (containsQuery(element.id, query)) {
      matches.push("ID contains query");
    }
    if (containsQuery(element.label, query)) {
      matches.push("Label contains query");
    }
    if (containsQuery(element.text, query)) {
      matches.push("Text contains query");
    }
    if (containsQuery(element.placeholder, query)) {
      matches.push("Placeholder contains query");
    }
    if (containsQuery(element.type, query)) {
      matches.push("Type matches");
    }
    if (containsQuery(element.tagName, query)) {
      matches.push("Tag matches");
    }

    if (matches.length > 0) {
      results.push({
        element,
        score: matches.length / 6, // Normalize by number of attributes checked
        matchType: "attribute",
        matchDetails: matches.join(", "),
      });
    }
  }

  // Sort by number of matches (score) descending
  return results.sort((a, b) => b.score - a.score).slice(0, 10);
}

/**
 * Search strategies
 */
const SEARCH_STRATEGIES: SearchStrategy[] = [
  {
    id: "exact",
    name: "Exact Match",
    icon: <Target className="w-4 h-4" />,
    description: "Find element with exact ID",
    search: exactMatchSearch,
  },
  {
    id: "fuzzy",
    name: "Fuzzy Match",
    icon: <Sparkles className="w-4 h-4" />,
    description: "Find similar IDs using Levenshtein distance",
    search: fuzzyMatchSearch,
  },
  {
    id: "attribute",
    name: "Attribute Search",
    icon: <Tag className="w-4 h-4" />,
    description: "Search by label, text, type, or tag",
    search: attributeMatchSearch,
  },
];

export function SearchComparisonPanel({
  elements,
  onSelectElement,
  onHighlightElement,
  disabled = false,
}: SearchComparisonPanelProps) {
  const [query, setQuery] = useState("");
  const [isSearching, setIsSearching] = useState(false);
  const [expandedStrategy, setExpandedStrategy] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  // Run all search strategies
  const searchResults = useMemo(() => {
    if (!query.trim() || elements.length === 0) {
      return new Map<string, SearchResult[]>();
    }

    const results = new Map<string, SearchResult[]>();
    for (const strategy of SEARCH_STRATEGIES) {
      results.set(strategy.id, strategy.search(query, elements));
    }
    return results;
  }, [query, elements]);

  // Find the best overall result
  const bestResult = useMemo(() => {
    let best: { result: SearchResult; strategyId: string } | null = null;

    for (const [strategyId, results] of searchResults) {
      if (results.length > 0 && (!best || results[0].score > best.result.score)) {
        best = { result: results[0], strategyId };
      }
    }

    return best;
  }, [searchResults]);

  // Handle search
  const handleSearch = useCallback(() => {
    setIsSearching(true);
    // Search is synchronous via useMemo, just show loading briefly
    setTimeout(() => setIsSearching(false), 100);
  }, []);

  // Handle copy ID
  const handleCopyId = useCallback(async (elementId: string) => {
    try {
      await navigator.clipboard.writeText(elementId);
      setCopiedId(elementId);
      setTimeout(() => setCopiedId(null), 2000);
    } catch {
      // Ignore errors
    }
  }, []);

  // Toggle strategy expansion
  const toggleStrategy = useCallback((strategyId: string) => {
    setExpandedStrategy((prev) => (prev === strategyId ? null : strategyId));
  }, []);

  // Render a search result
  const renderResult = (result: SearchResult, isRecommended: boolean = false) => (
    <div
      key={result.element.id}
      className={`p-2 rounded-md border transition-colors ${
        isRecommended
          ? "bg-accent/10 border-accent/30"
          : "bg-muted/20 border-border/30 hover:bg-muted/30"
      }`}
    >
      <div className="flex items-center gap-2">
        {isRecommended && (
          <Badge variant="default" className="text-[10px] px-1 py-0">
            Recommended
          </Badge>
        )}
        <Badge variant="muted" className="text-[10px] px-1 py-0">
          {result.element.type}
        </Badge>
        <span className="font-mono text-sm truncate flex-1">{result.element.id}</span>
        <span className="text-xs text-muted-foreground">{(result.score * 100).toFixed(0)}%</span>
      </div>

      <div className="text-xs text-muted-foreground mt-1">{result.matchDetails}</div>

      {(result.element.label || result.element.text) && (
        <div className="text-xs text-muted-foreground mt-1 truncate">
          {result.element.label || result.element.text}
        </div>
      )}

      <div className="flex gap-1 mt-2">
        <Button
          variant="outline"
          size="sm"
          className="text-xs h-6 px-2"
          onClick={() => onSelectElement(result.element.id)}
        >
          <Check className="w-3 h-3 mr-1" />
          Select
        </Button>
        <Button
          variant="outline"
          size="sm"
          className="text-xs h-6 px-2"
          onClick={() => onHighlightElement(result.element.id)}
        >
          <Eye className="w-3 h-3 mr-1" />
          Highlight
        </Button>
        <Button
          variant="outline"
          size="sm"
          className="text-xs h-6 px-2"
          onClick={() => handleCopyId(result.element.id)}
        >
          {copiedId === result.element.id ? (
            <>
              <Check className="w-3 h-3 mr-1" />
              Copied
            </>
          ) : (
            <>
              <Copy className="w-3 h-3 mr-1" />
              Copy ID
            </>
          )}
        </Button>
      </div>
    </div>
  );

  if (disabled || elements.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Search className="w-8 h-8 opacity-50" />
        <p>Connect to a browser tab to search elements</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Search input */}
      <div className="mb-4">
        <div className="flex gap-2">
          <div className="relative flex-1">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSearch()}
              placeholder="Search by ID, label, text, or type..."
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md focus:outline-none focus:ring-1 focus:ring-primary"
            />
          </div>
          <Button onClick={handleSearch} disabled={!query.trim() || isSearching}>
            <Zap className="w-4 h-4 mr-1" />
            Search
          </Button>
        </div>
        <p className="text-xs text-muted-foreground mt-1">
          {elements.length} elements available for search
        </p>
      </div>

      {/* Best result recommendation */}
      {bestResult && (
        <div className="mb-4 p-3 bg-accent/5 border border-accent/20 rounded-lg">
          <div className="flex items-center gap-2 mb-2">
            <Sparkles className="w-4 h-4 text-accent" />
            <span className="text-sm font-medium">Best Match</span>
            <Badge variant="outline" className="text-[10px]">
              {SEARCH_STRATEGIES.find((s) => s.id === bestResult.strategyId)?.name}
            </Badge>
          </div>
          {renderResult(bestResult.result, true)}
        </div>
      )}

      {/* Strategy comparison */}
      <div className="flex-1 overflow-auto space-y-2">
        {query.trim() ? (
          SEARCH_STRATEGIES.map((strategy) => {
            const results = searchResults.get(strategy.id) || [];
            const isExpanded = expandedStrategy === strategy.id;
            const hasResults = results.length > 0;

            return (
              <div key={strategy.id} className="border border-border/50 rounded-lg overflow-hidden">
                {/* Strategy header */}
                <button
                  className="w-full flex items-center gap-2 p-3 bg-muted/20 hover:bg-muted/30 transition-colors"
                  onClick={() => toggleStrategy(strategy.id)}
                >
                  {isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground" />
                  )}
                  <span className={hasResults ? "text-foreground" : "text-muted-foreground"}>
                    {strategy.icon}
                  </span>
                  <span className="font-medium text-sm">{strategy.name}</span>
                  <span className="text-xs text-muted-foreground">{strategy.description}</span>
                  <div className="flex-1" />
                  {hasResults ? (
                    <Badge variant="success" className="text-xs">
                      {results.length} found
                    </Badge>
                  ) : (
                    <Badge variant="muted" className="text-xs">
                      No results
                    </Badge>
                  )}
                </button>

                {/* Strategy results */}
                {isExpanded && (
                  <div className="p-3 space-y-2 border-t border-border/30">
                    {hasResults ? (
                      results.map((result) => renderResult(result))
                    ) : (
                      <div className="text-sm text-muted-foreground text-center py-4">
                        <X className="w-5 h-5 mx-auto mb-1 opacity-50" />
                        No matches found with this strategy
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })
        ) : (
          <div className="text-center py-8 text-muted-foreground">
            <Search className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">Enter a search query to compare strategies</p>
            <p className="text-xs mt-1">Try searching for element IDs, labels, or text content</p>
          </div>
        )}
      </div>

      {/* Help section */}
      <div className="mt-3 pt-3 border-t border-border/50">
        <div className="text-xs text-muted-foreground space-y-1">
          <p className="flex items-center gap-1">
            <Target className="w-3 h-3" />
            <strong>Exact Match:</strong> Requires exact ID (case-insensitive)
          </p>
          <p className="flex items-center gap-1">
            <Sparkles className="w-3 h-3" />
            <strong>Fuzzy Match:</strong> Finds similar IDs using edit distance
          </p>
          <p className="flex items-center gap-1">
            <Tag className="w-3 h-3" />
            <strong>Attribute Search:</strong> Searches ID, label, text, type, and tag
          </p>
        </div>
      </div>
    </div>
  );
}

export default SearchComparisonPanel;
