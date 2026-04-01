import { useState, useEffect, useCallback, useMemo } from "react";
import { Search, Clock, Lightbulb, Network, RefreshCw } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { TimelineView } from "./TimelineView";
import { ConceptView } from "./ConceptView";
import { DecisionGraphView } from "./DecisionGraphView";
import { DecisionDetailPanel } from "./DecisionDetailPanel";
import type {
  DecisionPreview,
  ConceptSummaryPreview,
  Decision,
  ConceptSummaryRecord,
  DecisionCategory,
} from "@/types/decision-trail";

type ViewMode = "timeline" | "concepts" | "graph";

const CATEGORY_OPTIONS: DecisionCategory[] = [
  "architecture",
  "technology",
  "design",
  "integration",
  "performance",
  "security",
  "ux",
  "data-model",
];

export function DecisionTrailPage() {
  const [viewMode, setViewMode] = useState<ViewMode>("timeline");
  const [decisions, setDecisions] = useState<DecisionPreview[]>([]);
  const [concepts, setConcepts] = useState<ConceptSummaryPreview[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [categoryFilter, setCategoryFilter] = useState<string>("");
  const [loading, setLoading] = useState(false);
  const [selectedDecision, setSelectedDecision] = useState<Decision | null>(null);
  const [selectedConcept, setSelectedConcept] = useState<ConceptSummaryRecord | null>(null);

  // Cache of full decisions fetched for graph edges (keyed by ID)
  const [fullDecisionCache, setFullDecisionCache] = useState<Map<string, Decision>>(new Map());

  const fetchDecisions = useCallback(async () => {
    setLoading(true);
    try {
      const params = new URLSearchParams({ max_results: "100" });
      if (categoryFilter) params.set("category", categoryFilter);

      let url: string;
      if (searchQuery.trim()) {
        // Include category filter in search if set
        const searchParams = new URLSearchParams({
          q: searchQuery,
          max_results: "100",
        });
        if (categoryFilter) searchParams.set("category", categoryFilter);
        url = `${getApiBase()}/decisions/search?${searchParams}`;
      } else {
        url = `${getApiBase()}/decisions?${params}`;
      }

      const response = await tracedFetch(url);
      const json = await response.json();
      if (json.data) setDecisions(json.data);
    } catch (e) {
      console.error("Failed to fetch decisions:", e);
    } finally {
      setLoading(false);
    }
  }, [searchQuery, categoryFilter]);

  const fetchConcepts = useCallback(async () => {
    try {
      let url: string;
      if (searchQuery.trim()) {
        url = `${getApiBase()}/concept-summaries/search?q=${encodeURIComponent(searchQuery)}&max_results=50`;
      } else {
        url = `${getApiBase()}/concept-summaries?max_results=50`;
      }

      const response = await tracedFetch(url);
      const json = await response.json();
      if (json.data) setConcepts(json.data);
    } catch (e) {
      console.error("Failed to fetch concepts:", e);
    }
  }, [searchQuery]);

  useEffect(() => {
    fetchDecisions();
    fetchConcepts();
  }, [fetchDecisions, fetchConcepts]);

  // When switching to graph view, fetch full decision data for edge rendering
  useEffect(() => {
    if (viewMode !== "graph" || decisions.length === 0) return;

    // Only fetch decisions not already in cache
    const missingIds = decisions.filter((d) => !fullDecisionCache.has(d.id)).map((d) => d.id);
    if (missingIds.length === 0) return;

    const fetchFull = async () => {
      const newEntries = new Map(fullDecisionCache);
      // Fetch up to 20 at a time to avoid overwhelming the API
      for (const id of missingIds.slice(0, 20)) {
        try {
          const response = await tracedFetch(`${getApiBase()}/decisions/${id}`);
          const json = await response.json();
          if (json.data) newEntries.set(id, json.data);
        } catch {
          // Skip failed fetches
        }
      }
      setFullDecisionCache(newEntries);
    };

    fetchFull();
  }, [viewMode, decisions]); // eslint-disable-line react-hooks/exhaustive-deps

  const handleSelectDecision = useCallback(async (id: string) => {
    setSelectedConcept(null);
    try {
      const response = await tracedFetch(`${getApiBase()}/decisions/${id}`);
      const json = await response.json();
      if (json.data) {
        setSelectedDecision(json.data);
        // Also add to cache for graph view
        setFullDecisionCache((prev) => new Map(prev).set(id, json.data));
      }
    } catch (e) {
      console.error("Failed to fetch decision:", e);
    }
  }, []);

  const handleSelectConcept = useCallback(async (id: string) => {
    setSelectedDecision(null);
    try {
      const response = await tracedFetch(`${getApiBase()}/concept-summaries/${id}`);
      const json = await response.json();
      if (json.data) setSelectedConcept(json.data);
    } catch (e) {
      console.error("Failed to fetch concept:", e);
    }
  }, []);

  const handleCloseDetail = useCallback(() => {
    setSelectedDecision(null);
    setSelectedConcept(null);
  }, []);

  const viewTabs: { id: ViewMode; label: string; icon: typeof Clock }[] = [
    { id: "timeline", label: "Timeline", icon: Clock },
    { id: "concepts", label: "Concepts", icon: Lightbulb },
    { id: "graph", label: "Graph", icon: Network },
  ];

  return (
    <div className="flex h-full overflow-hidden">
      {/* Main content */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Header */}
        <div className="p-4 border-b border-border space-y-3">
          <div className="flex items-center justify-between">
            <h1 className="text-lg font-semibold text-foreground">Decision Trail</h1>
            <div className="flex items-center gap-2">
              <button
                onClick={() => {
                  fetchDecisions();
                  fetchConcepts();
                }}
                className="p-1.5 rounded-md hover:bg-accent text-muted-foreground"
                title="Refresh"
              >
                <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
              </button>
            </div>
          </div>

          {/* Search + filters */}
          <div className="flex items-center gap-2">
            <div className="relative flex-1">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
              <input
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                placeholder="Search decisions and concepts..."
                className="w-full pl-9 pr-3 py-1.5 text-sm rounded-md border border-border bg-background text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary"
              />
            </div>

            <select
              value={categoryFilter}
              onChange={(e) => setCategoryFilter(e.target.value)}
              className="px-2 py-1.5 text-xs rounded-md border border-border bg-background text-foreground"
            >
              <option value="">All categories</option>
              {CATEGORY_OPTIONS.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </div>

          {/* View tabs */}
          <div className="flex items-center gap-1">
            {viewTabs.map(({ id, label, icon: Icon }) => (
              <button
                key={id}
                onClick={() => setViewMode(id)}
                className={`flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
                  viewMode === id
                    ? "bg-primary/10 text-primary"
                    : "text-muted-foreground hover:bg-accent"
                }`}
              >
                <Icon className="w-3.5 h-3.5" />
                {label}
              </button>
            ))}

            <div className="ml-auto flex items-center gap-1 text-xs text-muted-foreground">
              {decisions.length} decision{decisions.length !== 1 ? "s" : ""}
              {concepts.length > 0 &&
                ` · ${concepts.length} concept${concepts.length !== 1 ? "s" : ""}`}
            </div>
          </div>
        </div>

        {/* Content area */}
        <div className="flex-1 overflow-auto p-4">
          {viewMode === "timeline" && (
            <TimelineView decisions={decisions} onSelectDecision={handleSelectDecision} />
          )}
          {viewMode === "concepts" && (
            <ConceptView concepts={concepts} onSelectConcept={handleSelectConcept} />
          )}
          {viewMode === "graph" && (
            <DecisionGraphView
              decisions={decisions}
              fullDecisions={fullDecisionCache}
              onSelectDecision={handleSelectDecision}
            />
          )}
        </div>
      </div>

      {/* Detail sidebar */}
      {(selectedDecision || selectedConcept) && (
        <DecisionDetailPanel
          decision={selectedDecision}
          concept={selectedConcept}
          onClose={handleCloseDetail}
        />
      )}
    </div>
  );
}
