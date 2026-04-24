/**
 * CheckGroupsPage
 *
 * Library builder page for managing check groups. A check group bundles
 * multiple check items together for sequential or parallel execution.
 * Uses useLibraryBuilder for state management and LibraryBuilderLayout
 * for the split-panel UI pattern.
 */

import { useState, useCallback, type KeyboardEvent } from "react";
import { Layers, X, Settings, Tag, CheckCircle2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { LibraryBuilderLayout } from "./LibraryBuilderLayout";
import { useLibraryBuilder, type UseLibraryBuilderReturn } from "@/hooks/useLibraryBuilder";
import { useRunnerCrud } from "@/hooks/useRunnerCrud";

// =============================================================================
// Types
// =============================================================================

interface CheckGroupItem {
  id: string;
  name: string;
  description?: string;
  check_ids: string[];
  stop_on_failure: boolean;
  run_in_parallel: boolean;
  tags?: string[];
  created_at?: string;
  updated_at?: string;
}

/** Minimal check reference used for the check selector. */
interface CheckReference {
  id: string;
  name: string;
  check_type: string;
  tool?: string;
  enabled: boolean;
}

// =============================================================================
// Constants
// =============================================================================

const RUN_MODE_OPTIONS = [
  { value: "sequential", label: "Sequential" },
  { value: "parallel", label: "Parallel" },
] as const;

const CHECK_TYPE_COLORS: Record<string, string> = {
  linter: "text-amber-400",
  formatter: "text-blue-400",
  type_checker: "text-purple-400",
  test_runner: "text-green-400",
  security: "text-red-400",
  build: "text-cyan-400",
  custom: "text-gray-400",
};

// =============================================================================
// Builder Configuration
// =============================================================================

const defaultFormState: Partial<CheckGroupItem> = {
  name: "",
  description: "",
  check_ids: [],
  stop_on_failure: true,
  run_in_parallel: false,
  tags: [],
};

function toFormState(item: CheckGroupItem): Partial<CheckGroupItem> {
  return {
    name: item.name,
    description: item.description ?? "",
    check_ids: item.check_ids ?? [],
    stop_on_failure: item.stop_on_failure,
    run_in_parallel: item.run_in_parallel,
    tags: item.tags ?? [],
  };
}

function toRequest(form: Partial<CheckGroupItem>): Record<string, unknown> {
  return {
    name: form.name,
    description: form.description || undefined,
    check_ids: form.check_ids ?? [],
    stop_on_failure: form.stop_on_failure ?? true,
    run_in_parallel: form.run_in_parallel ?? false,
    tags: form.tags ?? [],
  };
}

// =============================================================================
// Tag Input Component
// =============================================================================

interface TagInputProps {
  tags: string[];
  onChange: (tags: string[]) => void;
}

function TagInput({ tags, onChange }: TagInputProps) {
  const [inputValue, setInputValue] = useState("");

  const addTag = useCallback(() => {
    const trimmed = inputValue.trim().toLowerCase();
    if (trimmed && !tags.includes(trimmed)) {
      onChange([...tags, trimmed]);
    }
    setInputValue("");
  }, [inputValue, tags, onChange]);

  const removeTag = useCallback(
    (tagToRemove: string) => {
      onChange(tags.filter((t) => t !== tagToRemove));
    },
    [tags, onChange],
  );

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        e.preventDefault();
        addTag();
      }
    },
    [addTag],
  );

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <input
          type="text"
          aria-label="Add a tag"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Add a tag..."
          className="flex-1 px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
            placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
            text-foreground"
        />
        <button
          type="button"
          onClick={addTag}
          disabled={!inputValue.trim()}
          className={cn(
            "px-3 py-1.5 text-sm rounded-md border transition-colors",
            inputValue.trim()
              ? "bg-primary/10 text-primary border-primary/30 hover:bg-primary/20"
              : "bg-muted/30 text-muted-foreground border-border cursor-not-allowed",
          )}
        >
          Add
        </button>
      </div>
      {tags.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {tags.map((tag) => (
            <span
              key={tag}
              className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-md
                bg-muted/50 text-foreground border border-border"
            >
              {tag}
              <button
                type="button"
                onClick={() => removeTag(tag)}
                className="text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-3 h-3" />
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Check Selector
// =============================================================================

interface CheckSelectorProps {
  selectedIds: string[];
  onChange: (ids: string[]) => void;
}

function CheckSelector({ selectedIds, onChange }: CheckSelectorProps) {
  const checksCrud = useRunnerCrud<CheckReference>({ resourcePath: "/checks" });

  const toggleCheck = useCallback(
    (checkId: string) => {
      if (selectedIds.includes(checkId)) {
        onChange(selectedIds.filter((id) => id !== checkId));
      } else {
        onChange([...selectedIds, checkId]);
      }
    },
    [selectedIds, onChange],
  );

  const selectAll = useCallback(() => {
    onChange(checksCrud.items.map((c) => c.id));
  }, [checksCrud.items, onChange]);

  const deselectAll = useCallback(() => {
    onChange([]);
  }, [onChange]);

  if (checksCrud.loading) {
    return <div className="text-sm text-muted-foreground py-4 text-center">Loading checks...</div>;
  }

  if (checksCrud.items.length === 0) {
    return (
      <div className="text-sm text-muted-foreground py-4 text-center border border-dashed border-border rounded-md">
        No checks available. Create checks in the Checks page first.
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <span className="text-xs text-muted-foreground">
          {selectedIds.length} of {checksCrud.items.length} selected
        </span>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={selectAll}
            className="text-xs text-primary hover:text-primary/80 transition-colors"
          >
            Select all
          </button>
          <span className="text-xs text-muted-foreground">|</span>
          <button
            type="button"
            onClick={deselectAll}
            className="text-xs text-primary hover:text-primary/80 transition-colors"
          >
            Clear
          </button>
        </div>
      </div>

      <div className="border border-border rounded-md divide-y divide-border max-h-64 overflow-y-auto">
        {checksCrud.items.map((check) => {
          const isSelected = selectedIds.includes(check.id);
          const colorClass = CHECK_TYPE_COLORS[check.check_type] ?? "text-gray-400";

          return (
            <label
              key={check.id}
              className={cn(
                "flex items-center gap-3 px-3 py-2 cursor-pointer transition-colors",
                isSelected ? "bg-primary/5" : "hover:bg-muted/30",
              )}
            >
              <input
                type="checkbox"
                checked={isSelected}
                onChange={() => toggleCheck(check.id)}
                className="w-4 h-4 rounded border-border bg-muted/50 text-primary
                  focus:ring-1 focus:ring-primary accent-primary"
              />
              <CheckCircle2 className={cn("w-3.5 h-3.5 shrink-0", colorClass)} />
              <div className="flex-1 min-w-0">
                <span
                  className={cn(
                    "text-sm truncate block",
                    isSelected ? "text-foreground font-medium" : "text-muted-foreground",
                  )}
                >
                  {check.name}
                </span>
                <span className="text-[10px] text-muted-foreground">
                  {check.check_type.replace(/_/g, " ")}
                  {check.tool && check.tool !== "custom" ? ` \u00B7 ${check.tool}` : ""}
                </span>
              </div>
              {!check.enabled && (
                <Badge variant="warning" size="sm">
                  disabled
                </Badge>
              )}
            </label>
          );
        })}
      </div>
    </div>
  );
}

// =============================================================================
// List Item
// =============================================================================

interface CheckGroupListItemProps {
  item: CheckGroupItem;
  isSelected: boolean;
}

function CheckGroupListItem({ item, isSelected }: CheckGroupListItemProps) {
  const checkCount = item.check_ids?.length ?? 0;

  return (
    <div className="space-y-1">
      <span
        className={cn(
          "text-sm font-medium truncate block",
          isSelected ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {item.name || "Untitled Group"}
      </span>
      <div className="flex items-center gap-1.5">
        <Badge variant="muted" size="sm">
          {checkCount} check{checkCount !== 1 ? "s" : ""}
        </Badge>
        <Badge variant={item.run_in_parallel ? "info" : "muted"} size="sm">
          {item.run_in_parallel ? "parallel" : "sequential"}
        </Badge>
      </div>
      {item.tags && item.tags.length > 0 && (
        <div className="flex items-center gap-1 flex-wrap">
          {item.tags.slice(0, 3).map((tag) => (
            <span
              key={tag}
              className="text-[10px] px-1 py-0.5 rounded bg-muted/30 text-muted-foreground"
            >
              {tag}
            </span>
          ))}
          {item.tags.length > 3 && (
            <span className="text-[10px] text-muted-foreground">+{item.tags.length - 3}</span>
          )}
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Editor
// =============================================================================

interface CheckGroupEditorProps {
  builder: UseLibraryBuilderReturn<CheckGroupItem>;
}

function CheckGroupEditor({ builder }: CheckGroupEditorProps) {
  const { formState, setFormState } = builder;

  const updateField = useCallback(
    <K extends keyof CheckGroupItem>(field: K, value: CheckGroupItem[K]) => {
      setFormState((prev) => ({ ...prev, [field]: value }));
    },
    [setFormState],
  );

  return (
    <div className="space-y-6">
      {/* Basic Info */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Layers className="w-4 h-4 text-muted-foreground" />
          Basic Info
        </h3>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Name</label>
            <input
              type="text"
              aria-label="Name"
              value={formState.name ?? ""}
              onChange={(e) => updateField("name", e.target.value)}
              placeholder="Group name..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Description</label>
            <textarea
              aria-label="Description"
              value={formState.description ?? ""}
              onChange={(e) => updateField("description", e.target.value)}
              placeholder="What does this group of checks verify?"
              rows={3}
              className="w-full px-3 py-2 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground resize-y"
            />
          </div>
        </div>
      </section>

      {/* Configuration */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Settings className="w-4 h-4 text-muted-foreground" />
          Configuration
        </h3>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Run Mode</label>
            <select
              aria-label="Run Mode"
              value={formState.run_in_parallel ? "parallel" : "sequential"}
              onChange={(e) => updateField("run_in_parallel", e.target.value === "parallel")}
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary"
            >
              {RUN_MODE_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            <p className="text-xs text-muted-foreground">
              {formState.run_in_parallel
                ? "All checks run at the same time for faster execution."
                : "Checks run one after another in order."}
            </p>
          </div>

          <label className="flex items-center gap-3 cursor-pointer group">
            <input
              type="checkbox"
              checked={formState.stop_on_failure ?? true}
              onChange={(e) => updateField("stop_on_failure", e.target.checked)}
              className="w-4 h-4 rounded border-border bg-muted/50 text-primary
                focus:ring-1 focus:ring-primary accent-primary"
            />
            <div>
              <span className="text-sm text-foreground group-hover:text-foreground">
                Stop on failure
              </span>
              <p className="text-xs text-muted-foreground">
                Halt remaining checks when one fails
                {formState.run_in_parallel
                  ? " (applies when any parallel check finishes with failure)"
                  : ""}
              </p>
            </div>
          </label>
        </div>
      </section>

      {/* Checks */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <CheckCircle2 className="w-4 h-4 text-muted-foreground" />
          Checks
        </h3>

        <CheckSelector
          selectedIds={formState.check_ids ?? []}
          onChange={(ids) => updateField("check_ids", ids)}
        />
      </section>

      {/* Tags */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Tag className="w-4 h-4 text-muted-foreground" />
          Tags
        </h3>

        <TagInput tags={formState.tags ?? []} onChange={(tags) => updateField("tags", tags)} />
      </section>
    </div>
  );
}

// =============================================================================
// Main Page Component
// =============================================================================

export function CheckGroupsPage() {
  const builder = useLibraryBuilder<CheckGroupItem>({
    resourcePath: "/check-groups",
    defaultFormState,
    toFormState,
    toRequest,
  });

  return (
    <LibraryBuilderLayout<CheckGroupItem>
      title="Check Groups"
      icon={Layers}
      builder={builder}
      renderListItem={(item, isSelected) => (
        <CheckGroupListItem item={item} isSelected={isSelected} />
      )}
      renderEditor={() => <CheckGroupEditor builder={builder} />}
    />
  );
}
