/**
 * ObservationBrowser.tsx
 *
 * Dedicated panel for browsing, searching, and managing persistent observations
 * (Engram-inspired cross-session memory). Supports full-text search, type filtering,
 * and progressive disclosure (previews with expand-for-detail).
 */

import { useState, useCallback, useEffect } from "react";
import {
  Search,
  BookOpen,
  Trash2,
  ChevronDown,
  ChevronRight,
  RefreshCw,
  Loader2,
  Brain,
  AlertCircle,
} from "lucide-react";
import { useObservationSearch, type ObservationSearchResult } from "@/hooks/useObservationMemory";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { getAccentColors, getStatusColors } from "@/design-system";

// ============================================================================
// Types
// ============================================================================

interface FullObservation {
  id: number;
  title: string;
  content: string;
  observationType: string;
  scope: string;
  topicKey?: string | null;
  contentHash: string;
  revisionCount: number;
  duplicateCount: number;
  projectId?: string | null;
  workflowId?: string | null;
  taskRunId?: string | null;
  sessionId?: string | null;
  createdAt: string;
  updatedAt: string;
}

interface ObservationStats {
  observationType: string;
  count: number;
  latestUpdated: string;
}

const TYPE_COLORS: Record<string, string> = {
  decision: "blue",
  architecture: "violet",
  bugfix: "red",
  pattern: "amber",
  learning: "green",
  discovery: "cyan",
};

// ============================================================================
// Main Component
// ============================================================================

export function ObservationBrowser({ projectId }: { projectId?: string | null }) {
  const [searchQuery, setSearchQuery] = useState("");
  const [stats, setStats] = useState<ObservationStats[]>([]);
  const [statsLoading, setStatsLoading] = useState(false);
  const [expandedId, setExpandedId] = useState<number | null>(null);
  const [fullObservation, setFullObservation] = useState<FullObservation | null>(null);
  const [fullLoading, setFullLoading] = useState(false);

  const [showCreateForm, setShowCreateForm] = useState(false);
  const [createTitle, setCreateTitle] = useState("");
  const [createContent, setCreateContent] = useState("");
  const [createType, setCreateType] = useState("learning");
  const [createLoading, setCreateLoading] = useState(false);

  const { results, loading: searchLoading, search } = useObservationSearch();

  // Load stats callback (must be declared before useEffect that references it)
  const loadStats = useCallback(async () => {
    setStatsLoading(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/observations/stats`);
      if (response.ok) {
        const result = await response.json();
        setStats(result.data ?? []);
      }
    } catch {
      // Ignore errors
    } finally {
      setStatsLoading(false);
    }
  }, []);

  // Auto-load stats on mount. Deferred into a microtask so the setState
  // inside loadStats doesn't fire synchronously from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void loadStats();
    });
    return () => {
      cancelled = true;
    };
  }, [loadStats]);

  // Create handler
  const handleCreate = useCallback(async () => {
    if (!createTitle.trim() || !createContent.trim()) return;
    setCreateLoading(true);
    try {
      const response = await tracedFetch(`${getApiBase()}/observations`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          title: createTitle,
          content: createContent,
          observationType: createType,
          scope: "project",
          ...(projectId ? { projectId } : {}),
        }),
      });
      if (response.ok) {
        setCreateTitle("");
        setCreateContent("");
        setShowCreateForm(false);
        loadStats();
        if (searchQuery.trim()) {
          search(searchQuery, projectId ?? undefined);
        }
      }
    } catch {
      // Ignore
    } finally {
      setCreateLoading(false);
    }
  }, [createTitle, createContent, createType, projectId, searchQuery, search, loadStats]);

  // Search handler
  const handleSearch = useCallback(() => {
    if (searchQuery.trim()) {
      search(searchQuery, projectId ?? undefined);
    }
  }, [searchQuery, projectId, search]);

  // Load full observation for detail view
  const handleExpand = useCallback(
    async (id: number) => {
      if (expandedId === id) {
        setExpandedId(null);
        setFullObservation(null);
        return;
      }
      setExpandedId(id);
      setFullLoading(true);
      try {
        const response = await tracedFetch(`${getApiBase()}/observations/${id}`);
        if (response.ok) {
          const result = await response.json();
          setFullObservation(result.data ?? null);
        }
      } catch {
        setFullObservation(null);
      } finally {
        setFullLoading(false);
      }
    },
    [expandedId],
  );

  // Delete observation
  const handleDelete = useCallback(
    async (id: number) => {
      try {
        const response = await tracedFetch(`${getApiBase()}/observations/${id}`, {
          method: "DELETE",
        });
        if (response.ok) {
          // Re-search to refresh
          if (searchQuery.trim()) {
            search(searchQuery, projectId ?? undefined);
          }
          if (expandedId === id) {
            setExpandedId(null);
            setFullObservation(null);
          }
        }
      } catch {
        // Ignore
      }
    },
    [searchQuery, projectId, search, expandedId],
  );

  const totalCount = stats.reduce((sum, s) => sum + s.count, 0);

  return (
    <div
      className="flex flex-col h-full"
      data-ui-element="observation-browser"
      data-tutorial-id="observation-browser"
    >
      {/* Header */}
      <div className="flex items-center gap-3 p-4 border-b border-border">
        <Brain className="w-5 h-5 text-primary" />
        <div className="flex-1">
          <h2
            className="font-semibold text-sm"
            data-ui-element="observation-heading"
            data-tutorial-id="observation-heading"
          >
            Observation Memory
          </h2>
          <p className="text-xs text-muted-foreground" data-ui-element="observation-subtitle">
            Cross-session knowledge from past runs
          </p>
        </div>
        <button
          onClick={() => setShowCreateForm(!showCreateForm)}
          className="px-2 py-1 text-xs bg-primary text-primary-foreground rounded-md"
          data-ui-element="new-observation-btn"
          data-tutorial-id="observation-create-btn"
        >
          + New
        </button>
        <button
          onClick={loadStats}
          disabled={statsLoading}
          className="p-1.5 hover:bg-muted rounded-md"
          title="Refresh stats"
          data-ui-element="refresh-stats-btn"
        >
          {statsLoading ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <RefreshCw className="w-4 h-4" />
          )}
        </button>
      </div>

      {/* Create Form */}
      {showCreateForm && (
        <div className="px-4 py-3 border-b border-border space-y-2 bg-muted/30">
          <input
            type="text"
            aria-label="Observation title"
            value={createTitle}
            onChange={(e) => setCreateTitle(e.target.value)}
            placeholder="Observation title..."
            className="w-full px-3 py-1.5 text-sm bg-background border border-border rounded-md outline-none focus:ring-1 focus:ring-primary"
            data-ui-element="create-observation-title"
          />
          <textarea
            aria-label="Observation content"
            value={createContent}
            onChange={(e) => setCreateContent(e.target.value)}
            placeholder="Content (markdown supported)..."
            rows={3}
            className="w-full px-3 py-1.5 text-sm bg-background border border-border rounded-md outline-none focus:ring-1 focus:ring-primary resize-y"
            data-ui-element="create-observation-content"
          />
          <div className="flex items-center gap-2">
            <select
              aria-label="Observation type"
              value={createType}
              onChange={(e) => setCreateType(e.target.value)}
              className="px-2 py-1 text-xs bg-background border border-border rounded-md"
            >
              <option value="decision">Decision</option>
              <option value="architecture">Architecture</option>
              <option value="bugfix">Bug Fix</option>
              <option value="pattern">Pattern</option>
              <option value="learning">Learning</option>
              <option value="discovery">Discovery</option>
            </select>
            <div className="flex-1" />
            <button
              onClick={() => setShowCreateForm(false)}
              className="px-3 py-1 text-xs text-muted-foreground hover:text-foreground"
            >
              Cancel
            </button>
            <button
              onClick={handleCreate}
              disabled={createLoading || !createTitle.trim() || !createContent.trim()}
              className="px-3 py-1 text-xs bg-primary text-primary-foreground rounded-md disabled:opacity-50"
              data-ui-element="save-observation-btn"
            >
              {createLoading ? "Saving..." : "Save"}
            </button>
          </div>
        </div>
      )}

      {/* Stats bar */}
      {stats.length > 0 && (
        <div
          className="flex items-center gap-2 px-4 py-2 border-b border-border overflow-x-auto"
          data-tutorial-id="observation-stats"
        >
          <span className="text-xs text-muted-foreground shrink-0">{totalCount} total</span>
          {stats.map((s) => {
            const color = TYPE_COLORS[s.observationType] || "zinc";
            return (
              <button
                key={s.observationType}
                onClick={() => {
                  setSearchQuery(s.observationType);
                  search(s.observationType, projectId ?? undefined);
                }}
                className={`shrink-0 px-2 py-0.5 text-[10px] rounded-full ${getAccentColors(color as Parameters<typeof getAccentColors>[0]).bg} ${getAccentColors(color as Parameters<typeof getAccentColors>[0]).text}`}
              >
                {s.observationType}: {s.count}
              </button>
            );
          })}
        </div>
      )}

      {/* Search bar */}
      <div className="flex items-center gap-2 px-4 py-3 border-b border-border">
        <div className="flex-1 flex items-center gap-2 bg-muted rounded-md px-3 py-1.5">
          <Search className="w-4 h-4 text-muted-foreground shrink-0" />
          <input
            type="text"
            aria-label="Search observations"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleSearch()}
            placeholder="Search observations..."
            className="flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
            data-ui-element="observation-search-input"
            data-tutorial-id="observation-search"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={searchLoading || !searchQuery.trim()}
          className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md disabled:opacity-50"
          data-ui-element="observation-search-btn"
        >
          {searchLoading ? <Loader2 className="w-4 h-4 animate-spin" /> : "Search"}
        </button>
      </div>

      {/* Results */}
      <div className="flex-1 overflow-y-auto">
        {results.length === 0 && !searchLoading && (
          <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
            <BookOpen className="w-10 h-10 mb-3 opacity-30" />
            <p className="text-sm">
              {searchQuery ? "No observations found" : "Search to browse observations"}
            </p>
            <p className="text-xs mt-1">
              Observations are automatically captured from task runs and sessions
            </p>
          </div>
        )}

        {results.map((obs) => (
          <ObservationRow
            key={obs.id}
            observation={obs}
            isExpanded={expandedId === obs.id}
            fullObservation={expandedId === obs.id ? fullObservation : null}
            fullLoading={expandedId === obs.id && fullLoading}
            onExpand={() => handleExpand(obs.id)}
            onDelete={() => handleDelete(obs.id)}
          />
        ))}
      </div>
    </div>
  );
}

// ============================================================================
// Observation Row
// ============================================================================

function ObservationRow({
  observation,
  isExpanded,
  fullObservation,
  fullLoading,
  onExpand,
  onDelete,
}: {
  observation: ObservationSearchResult;
  isExpanded: boolean;
  fullObservation: FullObservation | null;
  fullLoading: boolean;
  onExpand: () => void;
  onDelete: () => void;
}) {
  const color = TYPE_COLORS[observation.observationType] || "zinc";
  const colorClasses = getAccentColors(color as Parameters<typeof getAccentColors>[0]);

  return (
    <div className="border-b border-border">
      {/* Summary row */}
      <button
        onClick={onExpand}
        className="w-full flex items-start gap-2 px-4 py-3 text-left hover:bg-muted/50 transition-colors"
      >
        {isExpanded ? (
          <ChevronDown className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
        ) : (
          <ChevronRight className="w-4 h-4 mt-0.5 shrink-0 text-muted-foreground" />
        )}

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            <span
              className={`px-1.5 py-0.5 text-[10px] rounded ${colorClasses.bg} ${colorClasses.text}`}
            >
              {observation.observationType}
            </span>
            <span className="font-medium text-sm truncate">{observation.title}</span>
            {observation.revisionCount > 1 && (
              <span className="text-[10px] text-muted-foreground">
                rev {observation.revisionCount}
              </span>
            )}
            {observation.rank != null && observation.rank > 0 && (
              <span className="text-[10px] text-muted-foreground ml-auto">
                relevance: {(observation.rank * 100).toFixed(0)}%
              </span>
            )}
          </div>
          <p className="text-xs text-muted-foreground line-clamp-2">{observation.contentPreview}</p>
          <div className="flex items-center gap-3 mt-1 text-[10px] text-muted-foreground">
            {observation.topicKey && <span>[{observation.topicKey}]</span>}
            <span>{new Date(observation.updatedAt).toLocaleDateString()}</span>
          </div>
        </div>
      </button>

      {/* Expanded detail */}
      {isExpanded && (
        <div className="px-4 pb-3 pl-10">
          {fullLoading ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground py-2">
              <Loader2 className="w-4 h-4 animate-spin" />
              Loading full content...
            </div>
          ) : fullObservation ? (
            <div className="space-y-2">
              <pre className="text-xs bg-muted rounded-md p-3 whitespace-pre-wrap max-h-64 overflow-y-auto">
                {fullObservation.content}
              </pre>
              <div className="flex items-center gap-4 text-[10px] text-muted-foreground">
                <span>Hash: {fullObservation.contentHash.slice(0, 12)}...</span>
                <span>Scope: {fullObservation.scope}</span>
                {fullObservation.duplicateCount > 0 && (
                  <span>Duplicates prevented: {fullObservation.duplicateCount}</span>
                )}
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete();
                  }}
                  className={`ml-auto flex items-center gap-1 px-2 py-1 rounded ${getStatusColors("error").text} hover:${getStatusColors("error").bg}`}
                >
                  <Trash2 className="w-3 h-3" />
                  Delete
                </button>
              </div>
            </div>
          ) : (
            <div className="flex items-center gap-2 text-sm text-muted-foreground py-2">
              <AlertCircle className="w-4 h-4" />
              Could not load full content
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default ObservationBrowser;
