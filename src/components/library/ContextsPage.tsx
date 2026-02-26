/**
 * ContextsPage
 *
 * Library builder page for managing context items.
 * Contexts provide additional information and instructions
 * that can be included in AI sessions.
 */

import { useState, type KeyboardEvent } from "react";
import { BookText, Zap, X, Plus, Globe, GitBranch, Target } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { LibraryBuilderLayout } from "./LibraryBuilderLayout";
import { useLibraryBuilder } from "@/hooks/useLibraryBuilder";

// =============================================================================
// Data Model
// =============================================================================

interface ContextItem {
  id: string;
  name: string;
  content: string;
  scope: "global" | "workflow" | "task";
  auto_include?: boolean;
  priority?: number;
  tags?: string[];
  created_at?: string;
  updated_at?: string;
}

const defaultFormState: Partial<ContextItem> = {
  name: "",
  content: "",
  scope: "global",
  auto_include: false,
  priority: 50,
  tags: [],
};

// =============================================================================
// Scope Metadata
// =============================================================================

const SCOPE_CONFIG = {
  global: {
    label: "Global",
    description: "Available to all AI sessions",
    variant: "info" as const,
    icon: Globe,
  },
  workflow: {
    label: "Workflow",
    description: "Available within workflow executions",
    variant: "purple" as const,
    icon: GitBranch,
  },
  task: {
    label: "Task",
    description: "Available for specific task types",
    variant: "success" as const,
    icon: Target,
  },
};

// =============================================================================
// Main Component
// =============================================================================

export function ContextsPage() {
  const builder = useLibraryBuilder<ContextItem>({
    resourcePath: "/contexts",
    defaultFormState,
    toFormState: (item) => ({
      name: item.name,
      content: item.content,
      scope: item.scope,
      auto_include: item.auto_include ?? false,
      priority: item.priority ?? 50,
      tags: item.tags ?? [],
    }),
    toRequest: (form) => ({
      name: form.name,
      content: form.content,
      scope: form.scope,
      auto_include: form.auto_include,
      priority: form.priority,
      tags: form.tags,
    }),
  });

  return (
    <LibraryBuilderLayout<ContextItem>
      title="Contexts"
      icon={BookText}
      builder={builder}
      renderListItem={(item, isSelected) => <ContextListItem item={item} isSelected={isSelected} />}
      renderEditor={() => (
        <ContextEditor
          formState={builder.formState}
          setFormState={builder.setFormState}
          isCreating={builder.isCreating}
        />
      )}
    />
  );
}

// =============================================================================
// List Item
// =============================================================================

function ContextListItem({ item, isSelected }: { item: ContextItem; isSelected: boolean }) {
  const scopeCfg = SCOPE_CONFIG[item.scope] || SCOPE_CONFIG.global;
  const contentPreview = item.content
    ? item.content.slice(0, 50) + (item.content.length > 50 ? "..." : "")
    : "";

  return (
    <div className="space-y-1">
      <div className="flex items-center gap-2">
        <BookText className="w-3.5 h-3.5 text-blue-400 shrink-0" />
        <span
          className={cn(
            "text-sm font-medium truncate",
            isSelected ? "text-foreground" : "text-muted-foreground",
          )}
        >
          {item.name || "Untitled"}
        </span>
        {item.auto_include && (
          <span title="Auto-include enabled">
            <Zap className="w-3 h-3 text-yellow-500 shrink-0" />
          </span>
        )}
      </div>
      <div className="flex items-center gap-1.5 pl-5">
        <Badge variant={scopeCfg.variant} size="sm">
          {scopeCfg.label}
        </Badge>
        {contentPreview && (
          <span className="text-[10px] text-muted-foreground truncate">{contentPreview}</span>
        )}
      </div>
    </div>
  );
}

// =============================================================================
// Editor
// =============================================================================

function ContextEditor({
  formState,
  setFormState,
  isCreating,
}: {
  formState: Partial<ContextItem>;
  setFormState: React.Dispatch<React.SetStateAction<Partial<ContextItem>>>;
  isCreating: boolean;
}) {
  const [tagInput, setTagInput] = useState("");

  const updateField = <K extends keyof ContextItem>(field: K, value: ContextItem[K]) => {
    setFormState((prev) => ({ ...prev, [field]: value }));
  };

  const currentScope = (formState.scope ?? "global") as keyof typeof SCOPE_CONFIG;
  const scopeCfg = SCOPE_CONFIG[currentScope];

  // --- Tag helpers ---

  const addTag = () => {
    const tag = tagInput.trim();
    if (!tag) return;
    const current = formState.tags ?? [];
    if (!current.includes(tag)) {
      updateField("tags", [...current, tag] as ContextItem["tags"]);
    }
    setTagInput("");
  };

  const removeTag = (tagToRemove: string) => {
    const current = formState.tags ?? [];
    updateField("tags", current.filter((t) => t !== tagToRemove) as ContextItem["tags"]);
  };

  const handleTagKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      addTag();
    }
  };

  return (
    <div className="space-y-6">
      {/* Section: Basic Info */}
      <section className="space-y-3">
        <h3 className="text-sm font-semibold text-foreground">Basic Info</h3>
        <div className="space-y-1.5">
          <label className="block text-xs font-medium text-muted-foreground">Name</label>
          <input
            type="text"
            value={formState.name ?? ""}
            onChange={(e) => updateField("name", e.target.value)}
            placeholder="Context name..."
            className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
              placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary
              text-foreground"
          />
        </div>
      </section>

      {/* Section: Scope */}
      <section className="space-y-3">
        <h3 className="text-sm font-semibold text-foreground">Scope</h3>
        <div className="space-y-1.5">
          <label className="block text-xs font-medium text-muted-foreground">Context scope</label>
          <select
            value={formState.scope ?? "global"}
            onChange={(e) => updateField("scope", e.target.value as ContextItem["scope"])}
            className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
              focus:outline-none focus:ring-1 focus:ring-primary text-foreground"
          >
            <option value="global">Global</option>
            <option value="workflow">Workflow</option>
            <option value="task">Task</option>
          </select>
          <div className="flex items-center gap-2 mt-1.5 px-3 py-2 bg-muted/30 rounded-md border border-border">
            {scopeCfg.icon && (
              <scopeCfg.icon className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            )}
            <span className="text-xs text-muted-foreground">{scopeCfg.description}</span>
          </div>
        </div>
      </section>

      {/* Section: Content */}
      <section className="space-y-3">
        <h3 className="text-sm font-semibold text-foreground">Content</h3>
        <div className="space-y-1.5">
          <label className="block text-xs font-medium text-muted-foreground">Context content</label>
          <textarea
            value={formState.content ?? ""}
            onChange={(e) => updateField("content", e.target.value)}
            placeholder="Enter the context content that will be provided to AI sessions..."
            rows={12}
            className="w-full px-3 py-2 text-sm bg-muted/50 border border-border rounded-md
              placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary
              text-foreground resize-y font-mono leading-relaxed"
          />
        </div>
      </section>

      {/* Section: Options */}
      <section className="space-y-3">
        <h3 className="text-sm font-semibold text-foreground">Options</h3>
        <div className="space-y-3">
          {/* Auto-include checkbox */}
          <label className="flex items-center justify-between px-3 py-2.5 bg-muted/50 rounded-md border border-border cursor-pointer">
            <div className="flex items-center gap-2">
              <Zap className="w-3.5 h-3.5 text-yellow-500" />
              <span className="text-sm text-foreground">Auto-include</span>
              <span className="text-xs text-muted-foreground">
                Automatically include in matching sessions
              </span>
            </div>
            <input
              type="checkbox"
              checked={formState.auto_include ?? false}
              onChange={(e) => updateField("auto_include", e.target.checked)}
              className="w-4 h-4 rounded border-border"
            />
          </label>

          {/* Priority */}
          <div className="space-y-1.5">
            <label className="block text-xs font-medium text-muted-foreground">
              Priority (0-100)
            </label>
            <input
              type="number"
              min={0}
              max={100}
              value={formState.priority ?? 50}
              onChange={(e) => {
                const val = parseInt(e.target.value, 10);
                if (!isNaN(val)) {
                  updateField("priority", Math.min(100, Math.max(0, val)));
                }
              }}
              className="w-32 px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                focus:outline-none focus:ring-1 focus:ring-primary text-foreground"
            />
            <p className="text-[10px] text-muted-foreground">
              Higher priority contexts are included first when there are token limits.
            </p>
          </div>
        </div>
      </section>

      {/* Section: Tags */}
      <section className="space-y-3">
        <h3 className="text-sm font-semibold text-foreground">Tags</h3>
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <input
              type="text"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={handleTagKeyDown}
              placeholder="Add a tag..."
              className="flex-1 px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary
                text-foreground"
            />
            <button
              type="button"
              onClick={addTag}
              disabled={!tagInput.trim()}
              className={cn(
                "inline-flex items-center gap-1 px-3 py-1.5 text-sm rounded-md border transition-colors",
                tagInput.trim()
                  ? "bg-primary/10 border-primary/30 text-primary hover:bg-primary/20"
                  : "bg-muted/30 border-border text-muted-foreground cursor-not-allowed",
              )}
            >
              <Plus className="w-3.5 h-3.5" />
              Add
            </button>
          </div>

          {/* Tag list */}
          {(formState.tags ?? []).length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {(formState.tags ?? []).map((tag) => (
                <span
                  key={tag}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-xs bg-muted/50
                    border border-border rounded-md text-foreground"
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

          {(formState.tags ?? []).length === 0 && (
            <p className="text-xs text-muted-foreground">
              No tags added. Tags help organize and filter contexts.
            </p>
          )}
        </div>
      </section>
    </div>
  );
}
