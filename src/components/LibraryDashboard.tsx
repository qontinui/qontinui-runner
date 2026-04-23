/**
 * LibraryDashboard.tsx
 *
 * Cross-category dashboard showing recent items from all library types.
 * Provides quick access to recently modified items and cross-category search.
 * Clicking an item navigates to the appropriate builder page for editing.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import {
  Search,
  Clock,
  FileText,
  Puzzle,
  BookOpen,
  ShieldCheck,
  Globe,
  TestTube,
  Sparkles,
  Layers,
  Loader2,
  ChevronRight,
  Filter,
  CheckCircle,
  Star,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { isDevelopmentMode } from "qontinui-navigation";
import { getAccentColors } from "@/design-system";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Item types that can appear in the dashboard
type ItemType =
  | "task"
  | "prompt-snippet"
  | "context"
  | "verification"
  | "api-request"
  | "script"
  | "unified-workflow"
  | "macro"
  | "check";

interface DashboardItem {
  id: string;
  type: ItemType;
  name: string;
  description?: string;
  category?: string;
  tags?: string[];
  modifiedAt: string;
  createdAt: string;
  isFavorite?: boolean;
}

// Raw API response item (flexible shape from various endpoints)
interface ApiResponseItem {
  id: string;
  name: string;
  description?: string;
  content?: string;
  category?: string;
  tags?: string | string[];
  modified_at?: string;
  modifiedAt?: string;
  created_at?: string;
  createdAt?: string;
  updated_at?: string;
  method?: string;
  url?: string;
  target_url?: string;
  enabled?: boolean;
  check_type?: string;
  tool?: string;
  is_favorite?: boolean;
}

// Normalize tags from API response (can be JSON string or array)
function normalizeTags(tags: string | string[] | undefined): string[] | undefined {
  if (!tags) return undefined;
  if (Array.isArray(tags)) return tags;
  try {
    const parsed = JSON.parse(tags);
    return Array.isArray(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

// Category configuration for display
const CATEGORY_CONFIG: Record<
  ItemType,
  {
    label: string;
    icon: React.ComponentType<{ className?: string; style?: React.CSSProperties }>;
    accentColor: string;
    builderTab: string;
  }
> = {
  task: {
    label: "Task",
    icon: FileText,
    accentColor: "amber",
    builderTab: "task-builder",
  },
  "prompt-snippet": {
    label: "Prompt Snippet",
    icon: Puzzle,
    accentColor: "cyan",
    builderTab: "playwright-test-builder",
  },
  context: {
    label: "Context",
    icon: BookOpen,
    accentColor: "blue",
    builderTab: "context-builder",
  },
  verification: {
    label: "State Exploration",
    icon: ShieldCheck,
    accentColor: "emerald",
    builderTab: "state-explorer-builder",
  },
  "api-request": {
    label: "API Request",
    icon: Globe,
    accentColor: "indigo",
    builderTab: "api-request-builder",
  },
  script: {
    label: "Playwright Test",
    icon: TestTube,
    accentColor: "purple",
    builderTab: "playwright-test-builder",
  },
  "unified-workflow": {
    label: "Workflow",
    icon: Sparkles,
    accentColor: "green",
    builderTab: "unified-workflow-builder",
  },
  macro: {
    label: "Macro",
    icon: Layers,
    accentColor: "pink",
    builderTab: "macro-builder",
  },
  check: {
    label: "Check",
    icon: CheckCircle,
    accentColor: "teal",
    builderTab: "check-builder",
  },
};

interface LibraryDashboardProps {
  onNavigateToBuilder?: (builderTab: string, itemId: string, itemType: ItemType) => void;
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
}

export function LibraryDashboard({ onNavigateToBuilder, onLog }: LibraryDashboardProps) {
  const [items, setItems] = useState<DashboardItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<ItemType | "all">("all");
  const [showFavoritesOnly, setShowFavoritesOnly] = useState(false);

  // Fetch all items from all categories
  const fetchAllItems = useCallback(async () => {
    setLoading(true);
    const allItems: DashboardItem[] = [];

    try {
      // Fetch all item types in parallel
      const [
        tasksRes,
        promptSnippetsRes,
        contextsRes,
        verificationsRes,
        apiRequestsRes,
        scriptsRes,
        unifiedWorkflowsRes,
        macrosRes,
        checksRes,
      ] = await Promise.allSettled([
        tracedFetch(`${getApiBase()}/prompts`),
        tracedFetch(`${getApiBase()}/prompt-snippets`),
        tracedFetch(`${getApiBase()}/contexts`),
        tracedFetch(`${getApiBase()}/saved-verifications`),
        tracedFetch(`${getApiBase()}/saved-api-requests`),
        tracedFetch(`${getApiBase()}/playwright-scripts`),
        tracedFetch(`${getApiBase()}/unified-workflows`),
        tracedFetch(`${getApiBase()}/macros`),
        invoke<{ success: boolean; data?: ApiResponseItem[] }>("list_checks", {
          enabledOnly: false,
        }),
      ]);

      // Process tasks
      if (tasksRes.status === "fulfilled" && tasksRes.value.ok) {
        const result = await tasksRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "task",
            name: item.name,
            description: item.description,
            category: item.category,
            tags: normalizeTags(item.tags),
            modifiedAt: item.modified_at || item.modifiedAt || item.created_at || "",
            createdAt: item.created_at || item.createdAt || "",
          });
        });
      }

      // Process prompt snippets
      if (promptSnippetsRes.status === "fulfilled" && promptSnippetsRes.value.ok) {
        const result = await promptSnippetsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "prompt-snippet",
            name: item.name,
            description: item.content?.slice(0, 100),
            category: item.category,
            tags: normalizeTags(item.tags),
            modifiedAt: item.modified_at || item.modifiedAt || item.created_at || "",
            createdAt: item.created_at || item.createdAt || "",
          });
        });
      }

      // Process contexts
      if (contextsRes.status === "fulfilled" && contextsRes.value.ok) {
        const result = await contextsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "context",
            name: item.name,
            description: item.content?.slice(0, 100),
            category: item.category,
            tags: normalizeTags(item.tags),
            modifiedAt: item.modifiedAt || item.modified_at || item.createdAt || "",
            createdAt: item.createdAt || item.created_at || "",
          });
        });
      }

      // Process verifications
      if (verificationsRes.status === "fulfilled" && verificationsRes.value.ok) {
        const result = await verificationsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "verification",
            name: item.name,
            description: item.description,
            tags: normalizeTags(item.tags),
            modifiedAt: item.updated_at || item.modified_at || item.created_at || "",
            createdAt: item.created_at || "",
          });
        });
      }

      // Process API requests
      if (apiRequestsRes.status === "fulfilled" && apiRequestsRes.value.ok) {
        const result = await apiRequestsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "api-request",
            name: item.name,
            description: `${item.method} ${item.url}`,
            category: item.category,
            tags: normalizeTags(item.tags),
            modifiedAt: item.updated_at || item.modified_at || item.created_at || "",
            createdAt: item.created_at || "",
          });
        });
      }

      // Process scripts
      if (scriptsRes.status === "fulfilled" && scriptsRes.value.ok) {
        const result = await scriptsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "script",
            name: item.name,
            description: item.description || item.target_url,
            modifiedAt: item.modified_at || item.created_at || "",
            createdAt: item.created_at || "",
          });
        });
      }

      // Process unified workflows
      if (unifiedWorkflowsRes.status === "fulfilled" && unifiedWorkflowsRes.value.ok) {
        const result = await unifiedWorkflowsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "unified-workflow",
            name: item.name,
            description: item.description,
            category: item.category,
            tags: normalizeTags(item.tags),
            modifiedAt: item.modified_at || item.updated_at || item.created_at || "",
            createdAt: item.created_at || "",
            isFavorite: item.is_favorite,
          });
        });
      }

      // Process macros
      if (macrosRes.status === "fulfilled" && macrosRes.value.ok) {
        const result = await macrosRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "macro",
            name: item.name,
            description: item.description,
            modifiedAt: item.modified_at || item.updated_at || item.created_at || "",
            createdAt: item.created_at || "",
          });
        });
      }

      // Process checks (from Tauri invoke, not HTTP)
      if (checksRes.status === "fulfilled") {
        const result = checksRes.value;
        const data = result.success ? result.data : [];
        data?.forEach((item: ApiResponseItem) => {
          allItems.push({
            id: item.id,
            type: "check",
            name: item.name,
            description: item.description || `${item.check_type} - ${item.tool}`,
            tags: normalizeTags(item.tags),
            modifiedAt: item.updated_at || item.created_at || "",
            createdAt: item.created_at || "",
          });
        });
      }

      // In production, hide automation-specific item types (macros, state exploration)
      const isDevMode = isDevelopmentMode();
      const hiddenTypes: ItemType[] = isDevMode ? [] : ["macro", "verification"];
      const visibleItems =
        hiddenTypes.length > 0
          ? allItems.filter((item) => !hiddenTypes.includes(item.type))
          : allItems;

      setItems(visibleItems);
    } catch (error) {
      console.error("Failed to fetch library items:", error);
      onLog?.("error", `Failed to fetch library items: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside fetchAllItems.
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchAllItems();
    });
    return () => {
      cancelled = true;
    };
  }, [fetchAllItems]);

  // Toggle favorite on a workflow (optimistic update with rollback)
  const toggleFavorite = useCallback(async (itemId: string) => {
    // Optimistic toggle
    setItems((prev) =>
      prev.map((item) => (item.id === itemId ? { ...item, isFavorite: !item.isFavorite } : item)),
    );
    try {
      const response = await tracedFetch(`${getApiBase()}/unified-workflows/${itemId}/favorite`, {
        method: "POST",
      });
      const result = await response.json();
      if (result.success) {
        // Sync with server state in case of mismatch
        const newState = result.data.is_favorite;
        setItems((prev) =>
          prev.map((item) => (item.id === itemId ? { ...item, isFavorite: newState } : item)),
        );
      } else {
        // Revert on failure
        setItems((prev) =>
          prev.map((item) =>
            item.id === itemId ? { ...item, isFavorite: !item.isFavorite } : item,
          ),
        );
      }
    } catch (error) {
      console.error("Failed to toggle favorite:", error);
      // Revert optimistic update
      setItems((prev) =>
        prev.map((item) => (item.id === itemId ? { ...item, isFavorite: !item.isFavorite } : item)),
      );
    }
  }, []);

  const hasFavorites = useMemo(
    () => items.some((item) => item.type === "unified-workflow" && item.isFavorite),
    [items],
  );

  // Filter and sort items
  const filteredItems = useMemo(() => {
    let result = [...items];

    // Filter by favorites
    if (showFavoritesOnly) {
      result = result.filter((item) => item.type === "unified-workflow" && item.isFavorite);
    }

    // Filter by type
    if (typeFilter !== "all") {
      result = result.filter((item) => item.type === typeFilter);
    }

    // Filter by search query
    if (searchQuery) {
      const query = searchQuery.toLowerCase();
      result = result.filter(
        (item) =>
          item.name.toLowerCase().includes(query) ||
          item.description?.toLowerCase().includes(query) ||
          item.category?.toLowerCase().includes(query) ||
          item.tags?.some((t) => t.toLowerCase().includes(query)),
      );
    }

    // Sort favorites first, then by modified date
    result.sort((a, b) => {
      const favA = a.isFavorite ? 1 : 0;
      const favB = b.isFavorite ? 1 : 0;
      if (favA !== favB) return favB - favA;
      const dateA = new Date(a.modifiedAt || a.createdAt).getTime();
      const dateB = new Date(b.modifiedAt || b.createdAt).getTime();
      return dateB - dateA;
    });

    return result;
  }, [items, typeFilter, searchQuery, showFavoritesOnly]);

  // Group items by type for the type filter counts
  const typeCounts = useMemo(() => {
    const counts: Record<ItemType | "all", number> = {
      all: items.length,
      task: 0,
      "prompt-snippet": 0,
      context: 0,
      verification: 0,
      "api-request": 0,
      script: 0,
      "unified-workflow": 0,
      macro: 0,
      check: 0,
    };
    items.forEach((item) => {
      counts[item.type]++;
    });
    return counts;
  }, [items]);

  // Format relative time
  const formatRelativeTime = (dateStr: string) => {
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) return "just now";
    if (diffMins < 60) return `${diffMins}m ago`;
    if (diffHours < 24) return `${diffHours}h ago`;
    if (diffDays < 7) return `${diffDays}d ago`;
    return date.toLocaleDateString();
  };

  const handleItemClick = (item: DashboardItem) => {
    const config = CATEGORY_CONFIG[item.type];
    onNavigateToBuilder?.(config.builderTab, item.id, item.type);
  };

  // Available type filters (only show types that have items)
  const availableTypes = useMemo(() => {
    const types: (ItemType | "all")[] = ["all"];
    (Object.keys(CATEGORY_CONFIG) as ItemType[]).forEach((type) => {
      if (typeCounts[type] > 0) {
        types.push(type);
      }
    });
    return types;
  }, [typeCounts]);

  return (
    <div className="h-full flex flex-col bg-card">
      {/* Header */}
      <div className="p-4 border-b border-border">
        <div className="flex items-center justify-between mb-4">
          <div>
            <h2 className="text-xl font-semibold flex items-center gap-2">
              <Clock className="w-5 h-5 text-muted-foreground" />
              Library Dashboard
            </h2>
            <p className="text-sm text-muted-foreground mt-1">
              Recently modified items across all categories
            </p>
          </div>
          <div className="text-sm text-muted-foreground">{items.length} total items</div>
        </div>

        {/* Search */}
        <div className="relative mb-4">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search across all categories..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-10 pr-4 py-2.5 bg-muted border border-border rounded-lg text-sm focus:outline-hidden focus:border-border"
          />
        </div>

        {/* Type filters */}
        <div className="flex items-center gap-2 overflow-x-auto pb-1">
          <Filter className="w-4 h-4 text-muted-foreground shrink-0" />
          {hasFavorites && (
            <button
              onClick={() => setShowFavoritesOnly((prev) => !prev)}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs font-medium transition-colors whitespace-nowrap ${
                showFavoritesOnly
                  ? "bg-amber-500/20 text-amber-400 border border-amber-500/50"
                  : "bg-muted text-muted-foreground hover:bg-muted/80 hover:text-foreground border border-transparent"
              }`}
            >
              <Star className={`w-3.5 h-3.5 ${showFavoritesOnly ? "fill-amber-400" : ""}`} />
              Favorites
            </button>
          )}
          {availableTypes.map((type) => {
            const isAll = type === "all";
            const config = isAll ? null : CATEGORY_CONFIG[type];
            const Icon = config?.icon;
            const isActive = typeFilter === type;

            return (
              <button
                key={type}
                onClick={() => setTypeFilter(type)}
                className={`flex items-center gap-1.5 px-3 py-1.5 rounded-full text-xs font-medium transition-colors whitespace-nowrap ${
                  isActive
                    ? "bg-muted/80 text-white"
                    : "bg-muted text-muted-foreground hover:bg-muted/80 hover:text-foreground"
                }`}
              >
                {Icon && <Icon className="w-3.5 h-3.5" />}
                {isAll ? "All" : config?.label}
                <span className="text-muted-foreground">({typeCounts[type]})</span>
              </button>
            );
          })}
        </div>
      </div>

      {/* Item list */}
      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
          </div>
        ) : filteredItems.length === 0 ? (
          <div className="text-center py-12 text-muted-foreground">
            <Clock className="w-12 h-12 mx-auto mb-3 opacity-30" />
            <p className="text-lg mb-1">No items found</p>
            <p className="text-sm">
              {searchQuery
                ? "Try adjusting your search or filters"
                : "Create items using the Builders menu"}
            </p>
          </div>
        ) : (
          <div className="divide-y divide-muted">
            {filteredItems.map((item) => {
              const config = CATEGORY_CONFIG[item.type];
              const Icon = config.icon;
              const accentColors = getAccentColors(config.accentColor);

              return (
                <button
                  key={`${item.type}-${item.id}`}
                  onClick={() => handleItemClick(item)}
                  className="w-full text-left px-4 py-3 hover:bg-muted/50 transition-colors group"
                >
                  <div className="flex items-start gap-3">
                    {/* Icon */}
                    <div
                      className="w-8 h-8 rounded-lg flex items-center justify-center shrink-0 mt-0.5"
                      style={{ backgroundColor: `${accentColors.bgSolid}20` }}
                    >
                      <Icon className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    </div>

                    {/* Content */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="font-medium text-sm truncate">{item.name}</span>
                        <span
                          className="text-xs px-1.5 py-0.5 rounded"
                          style={{
                            backgroundColor: `${accentColors.bgSolid}20`,
                            color: accentColors.bgSolid,
                          }}
                        >
                          {config.label}
                        </span>
                      </div>
                      {item.description && (
                        <p className="text-xs text-muted-foreground truncate mt-0.5">
                          {item.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5">
                        {item.category && (
                          <span className="text-xs text-muted-foreground">{item.category}</span>
                        )}
                        <span className="text-xs text-muted-foreground">
                          {formatRelativeTime(item.modifiedAt || item.createdAt)}
                        </span>
                      </div>
                    </div>

                    {/* Star toggle for workflows */}
                    {item.type === "unified-workflow" && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          toggleFavorite(item.id);
                        }}
                        className="shrink-0 p-1 rounded transition-colors hover:bg-white/10 mt-0.5"
                        title={item.isFavorite ? "Remove from favorites" : "Add to favorites"}
                      >
                        <Star
                          className={`w-4 h-4 ${item.isFavorite ? "text-amber-400 fill-amber-400" : "text-border group-hover:text-muted-foreground"}`}
                        />
                      </button>
                    )}

                    {/* Arrow */}
                    <ChevronRight className="w-4 h-4 text-border group-hover:text-muted-foreground transition-colors shrink-0 mt-1" />
                  </div>
                </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
