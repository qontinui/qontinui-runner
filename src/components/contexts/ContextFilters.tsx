/**
 * ContextFilters.tsx
 *
 * Filter bar component for the context list.
 * Provides scope, category, search, and sort controls.
 */

import { Search, SlidersHorizontal, X } from "lucide-react";
import type { ContextFilterOptions, ContextScope } from "../../types/context";

interface ContextFiltersProps {
  filters: ContextFilterOptions;
  onFiltersChange: (filters: ContextFilterOptions) => void;
  categories: string[];
  resultCount: number;
  totalCount: number;
}

const scopeOptions: { value: ContextScope | "all"; label: string }[] = [
  { value: "all", label: "All Scopes" },
  { value: "project", label: "Project" },
  { value: "user", label: "User" },
  { value: "builtin", label: "Built-in" },
];

const sortOptions: { value: ContextFilterOptions["sortBy"]; label: string }[] = [
  { value: "modifiedAt", label: "Last Modified" },
  { value: "createdAt", label: "Date Created" },
  { value: "name", label: "Name" },
  { value: "useCount", label: "Usage Count" },
  { value: "lastUsedAt", label: "Last Used" },
];

export function ContextFilters({
  filters,
  onFiltersChange,
  categories,
  resultCount,
  totalCount,
}: ContextFiltersProps) {
  const updateFilter = <K extends keyof ContextFilterOptions>(
    key: K,
    value: ContextFilterOptions[K],
  ) => {
    onFiltersChange({ ...filters, [key]: value });
  };

  const clearFilters = () => {
    onFiltersChange({
      scope: "all",
      category: "all",
      searchQuery: "",
      sortBy: "modifiedAt",
      sortDirection: "desc",
      enabledOnly: false,
    });
  };

  const hasActiveFilters =
    filters.scope !== "all" ||
    filters.category !== "all" ||
    filters.searchQuery ||
    filters.enabledOnly;

  return (
    <div className="space-y-3">
      {/* Search and Quick Filters Row */}
      <div className="flex items-center gap-3">
        {/* Search Input */}
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 transform -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search contexts by name, content, or tags..."
            value={filters.searchQuery || ""}
            onChange={(e) => updateFilter("searchQuery", e.target.value)}
            className="w-full pl-10 pr-4 py-2 bg-card border border-border rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
          />
          {filters.searchQuery && (
            <button
              onClick={() => updateFilter("searchQuery", "")}
              className="absolute right-3 top-1/2 transform -translate-y-1/2 p-0.5 text-muted-foreground hover:text-foreground"
            >
              <X className="w-4 h-4" />
            </button>
          )}
        </div>

        {/* Scope Filter */}
        <select
          value={filters.scope || "all"}
          onChange={(e) => updateFilter("scope", e.target.value as ContextScope | "all")}
          className="px-3 py-2 bg-card border border-border rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        >
          {scopeOptions.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>

        {/* Category Filter */}
        <select
          value={filters.category || "all"}
          onChange={(e) => updateFilter("category", e.target.value)}
          className="px-3 py-2 bg-card border border-border rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        >
          <option value="all">All Categories</option>
          {categories.map((cat) => (
            <option key={cat} value={cat}>
              {cat}
            </option>
          ))}
        </select>
      </div>

      {/* Sort and Additional Filters Row */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          {/* Sort */}
          <div className="flex items-center gap-2">
            <SlidersHorizontal className="w-4 h-4 text-muted-foreground" />
            <select
              value={filters.sortBy || "modifiedAt"}
              onChange={(e) =>
                updateFilter("sortBy", e.target.value as ContextFilterOptions["sortBy"])
              }
              className="px-2 py-1 bg-card border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            >
              {sortOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
            <button
              onClick={() =>
                updateFilter("sortDirection", filters.sortDirection === "asc" ? "desc" : "asc")
              }
              className="px-2 py-1 bg-card border border-border rounded text-sm hover:bg-muted/50 transition-colors"
              title={filters.sortDirection === "asc" ? "Sort ascending" : "Sort descending"}
            >
              {filters.sortDirection === "asc" ? "Asc" : "Desc"}
            </button>
          </div>

          {/* Enabled Only Toggle */}
          <label className="flex items-center gap-2 text-sm cursor-pointer">
            <input
              type="checkbox"
              checked={filters.enabledOnly || false}
              onChange={(e) => updateFilter("enabledOnly", e.target.checked)}
              className="w-4 h-4 rounded border-border text-primary focus:ring-primary"
            />
            <span className="text-muted-foreground">Enabled only</span>
          </label>

          {/* Clear Filters */}
          {hasActiveFilters && (
            <button
              onClick={clearFilters}
              className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
            >
              <X className="w-3 h-3" />
              Clear filters
            </button>
          )}
        </div>

        {/* Result Count */}
        <span className="text-sm text-muted-foreground">
          {resultCount === totalCount
            ? `${totalCount} context${totalCount !== 1 ? "s" : ""}`
            : `${resultCount} of ${totalCount} contexts`}
        </span>
      </div>
    </div>
  );
}
