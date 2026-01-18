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
  MousePointer2,
  Loader2,
  ChevronRight,
  Filter,
  CheckCircle,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

// Item types that can appear in the dashboard
type ItemType =
  | "task"
  | "scriptlet"
  | "context"
  | "verification"
  | "api-request"
  | "script"
  | "workflow"
  | "unified-workflow"
  | "gui-workflow"
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
  scriptlet: {
    label: "Scriptlet",
    icon: Puzzle,
    accentColor: "cyan",
    builderTab: "script-builder",
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
    label: "Script",
    icon: TestTube,
    accentColor: "purple",
    builderTab: "script-builder",
  },
  workflow: {
    label: "AI Workflow",
    icon: Sparkles,
    accentColor: "green",
    builderTab: "workflow-builder",
  },
  "unified-workflow": {
    label: "Workflow",
    icon: Sparkles,
    accentColor: "green",
    builderTab: "workflow-builder",
  },
  "gui-workflow": {
    label: "GUI Workflow",
    icon: MousePointer2,
    accentColor: "orange",
    builderTab: "gui-workflow-builder",
  },
  check: {
    label: "Check",
    icon: CheckCircle,
    accentColor: "teal",
    builderTab: "check-builder",
  },
};

interface LibraryDashboardProps {
  onNavigateToBuilder: (builderTab: string, itemId: string, itemType: ItemType) => void;
  onLog?: (level: string, message: string) => void;
}

export function LibraryDashboard({ onNavigateToBuilder, onLog }: LibraryDashboardProps) {
  const [items, setItems] = useState<DashboardItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<ItemType | "all">("all");

  // Fetch all items from all categories
  const fetchAllItems = useCallback(async () => {
    setLoading(true);
    const allItems: DashboardItem[] = [];

    try {
      // Fetch all item types in parallel
      const [
        tasksRes,
        scriptletsRes,
        contextsRes,
        verificationsRes,
        apiRequestsRes,
        scriptsRes,
        workflowsRes,
        unifiedWorkflowsRes,
        guiWorkflowsRes,
        checksRes,
      ] = await Promise.allSettled([
        fetch(`${API_BASE}/prompts`),
        fetch(`${API_BASE}/scriptlets`),
        fetch(`${API_BASE}/contexts`),
        fetch(`${API_BASE}/saved-verifications`),
        fetch(`${API_BASE}/saved-api-requests`),
        fetch(`${API_BASE}/playwright-scripts`),
        fetch(`${API_BASE}/ai-workflows`),
        fetch(`${API_BASE}/unified-workflows`),
        fetch(`${API_BASE}/gui-workflows`),
        invoke<{ success: boolean; data?: any[] }>("list_checks", { enabledOnly: false }),
      ]);

      // Process tasks
      if (tasksRes.status === "fulfilled" && tasksRes.value.ok) {
        const result = await tasksRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "task",
            name: item.name,
            description: item.description,
            category: item.category,
            tags: item.tags,
            modifiedAt: item.modified_at || item.modifiedAt || item.created_at,
            createdAt: item.created_at || item.createdAt,
          });
        });
      }

      // Process scriptlets
      if (scriptletsRes.status === "fulfilled" && scriptletsRes.value.ok) {
        const result = await scriptletsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "scriptlet",
            name: item.name,
            description: item.content?.slice(0, 100),
            category: item.category,
            tags: item.tags,
            modifiedAt: item.modified_at || item.modifiedAt || item.created_at,
            createdAt: item.created_at || item.createdAt,
          });
        });
      }

      // Process contexts
      if (contextsRes.status === "fulfilled" && contextsRes.value.ok) {
        const result = await contextsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "context",
            name: item.name,
            description: item.content?.slice(0, 100),
            category: item.category,
            tags: item.tags,
            modifiedAt: item.modifiedAt || item.modified_at || item.createdAt,
            createdAt: item.createdAt || item.created_at,
          });
        });
      }

      // Process verifications
      if (verificationsRes.status === "fulfilled" && verificationsRes.value.ok) {
        const result = await verificationsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "verification",
            name: item.name,
            description: item.description,
            tags: item.tags,
            modifiedAt: item.updated_at || item.modified_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      // Process API requests
      if (apiRequestsRes.status === "fulfilled" && apiRequestsRes.value.ok) {
        const result = await apiRequestsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "api-request",
            name: item.name,
            description: `${item.method} ${item.url}`,
            category: item.category,
            tags: item.tags,
            modifiedAt: item.updated_at || item.modified_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      // Process scripts
      if (scriptsRes.status === "fulfilled" && scriptsRes.value.ok) {
        const result = await scriptsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "script",
            name: item.name,
            description: item.description || item.target_url,
            modifiedAt: item.modified_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      // Process AI workflows
      if (workflowsRes.status === "fulfilled" && workflowsRes.value.ok) {
        const result = await workflowsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "workflow",
            name: item.name,
            description: item.description,
            category: item.category,
            tags: item.tags,
            modifiedAt: item.modified_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      // Process unified workflows
      if (unifiedWorkflowsRes.status === "fulfilled" && unifiedWorkflowsRes.value.ok) {
        const result = await unifiedWorkflowsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "unified-workflow",
            name: item.name,
            description: item.description,
            category: item.category,
            tags: item.tags,
            modifiedAt: item.modified_at || item.updated_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      // Process GUI workflows
      if (guiWorkflowsRes.status === "fulfilled" && guiWorkflowsRes.value.ok) {
        const result = await guiWorkflowsRes.value.json();
        const data = result.success ? result.data : Array.isArray(result) ? result : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "gui-workflow",
            name: item.name,
            description: item.description,
            modifiedAt: item.modified_at || item.updated_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      // Process checks (from Tauri invoke, not HTTP)
      if (checksRes.status === "fulfilled") {
        const result = checksRes.value;
        const data = result.success ? result.data : [];
        data?.forEach((item: any) => {
          allItems.push({
            id: item.id,
            type: "check",
            name: item.name,
            description: item.description || `${item.check_type} - ${item.tool}`,
            tags: item.tags ? JSON.parse(item.tags) : undefined,
            modifiedAt: item.updated_at || item.created_at,
            createdAt: item.created_at,
          });
        });
      }

      setItems(allItems);
    } catch (error) {
      console.error("Failed to fetch library items:", error);
      onLog?.("error", `Failed to fetch library items: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchAllItems();
  }, [fetchAllItems]);

  // Filter and sort items
  const filteredItems = useMemo(() => {
    let result = [...items];

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

    // Sort by modified date (most recent first)
    result.sort((a, b) => {
      const dateA = new Date(a.modifiedAt || a.createdAt).getTime();
      const dateB = new Date(b.modifiedAt || b.createdAt).getTime();
      return dateB - dateA;
    });

    return result;
  }, [items, typeFilter, searchQuery]);

  // Group items by type for the type filter counts
  const typeCounts = useMemo(() => {
    const counts: Record<ItemType | "all", number> = {
      all: items.length,
      task: 0,
      scriptlet: 0,
      context: 0,
      verification: 0,
      "api-request": 0,
      script: 0,
      workflow: 0,
      "unified-workflow": 0,
      "gui-workflow": 0,
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
    onNavigateToBuilder(config.builderTab, item.id, item.type);
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
    <div className="h-full flex flex-col bg-neutral-900">
      {/* Header */}
      <div className="p-4 border-b border-neutral-700">
        <div className="flex items-center justify-between mb-4">
          <div>
            <h2 className="text-xl font-semibold flex items-center gap-2">
              <Clock className="w-5 h-5 text-neutral-400" />
              Library Dashboard
            </h2>
            <p className="text-sm text-neutral-400 mt-1">
              Recently modified items across all categories
            </p>
          </div>
          <div className="text-sm text-neutral-500">{items.length} total items</div>
        </div>

        {/* Search */}
        <div className="relative mb-4">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
          <input
            type="text"
            placeholder="Search across all categories..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-10 pr-4 py-2.5 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
          />
        </div>

        {/* Type filters */}
        <div className="flex items-center gap-2 overflow-x-auto pb-1">
          <Filter className="w-4 h-4 text-neutral-500 flex-shrink-0" />
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
                    ? "bg-neutral-700 text-white"
                    : "bg-neutral-800 text-neutral-400 hover:bg-neutral-750 hover:text-neutral-300"
                }`}
              >
                {Icon && <Icon className="w-3.5 h-3.5" />}
                {isAll ? "All" : config?.label}
                <span className="text-neutral-500">({typeCounts[type]})</span>
              </button>
            );
          })}
        </div>
      </div>

      {/* Item list */}
      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Loader2 className="w-8 h-8 animate-spin text-neutral-400" />
          </div>
        ) : filteredItems.length === 0 ? (
          <div className="text-center py-12 text-neutral-400">
            <Clock className="w-12 h-12 mx-auto mb-3 opacity-30" />
            <p className="text-lg mb-1">No items found</p>
            <p className="text-sm">
              {searchQuery
                ? "Try adjusting your search or filters"
                : "Create items using the Builders menu"}
            </p>
          </div>
        ) : (
          <div className="divide-y divide-neutral-800">
            {filteredItems.map((item) => {
              const config = CATEGORY_CONFIG[item.type];
              const Icon = config.icon;
              const accentColors = getAccentColors(config.accentColor as any);

              return (
                <button
                  key={`${item.type}-${item.id}`}
                  onClick={() => handleItemClick(item)}
                  className="w-full text-left px-4 py-3 hover:bg-neutral-800/50 transition-colors group"
                >
                  <div className="flex items-start gap-3">
                    {/* Icon */}
                    <div
                      className="w-8 h-8 rounded-lg flex items-center justify-center flex-shrink-0 mt-0.5"
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
                        <p className="text-xs text-neutral-400 truncate mt-0.5">
                          {item.description}
                        </p>
                      )}
                      <div className="flex items-center gap-3 mt-1.5">
                        {item.category && (
                          <span className="text-xs text-neutral-500">{item.category}</span>
                        )}
                        <span className="text-xs text-neutral-500">
                          {formatRelativeTime(item.modifiedAt || item.createdAt)}
                        </span>
                      </div>
                    </div>

                    {/* Arrow */}
                    <ChevronRight className="w-4 h-4 text-neutral-600 group-hover:text-neutral-400 transition-colors flex-shrink-0 mt-1" />
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
