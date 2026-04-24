/**
 * TasksPage
 *
 * Library builder page for managing task/prompt items.
 * The API uses "prompts" as the resource name but the UI displays "Tasks".
 */

import { useState, useCallback, type KeyboardEvent } from "react";
import { FileText, X, Tag } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { LibraryBuilderLayout } from "./LibraryBuilderLayout";
import { useLibraryBuilder } from "@/hooks/useLibraryBuilder";

// =============================================================================
// Types
// =============================================================================

interface TaskItem {
  id: string;
  name: string;
  content: string;
  category?: string;
  tags?: string[];
  created_at?: string;
  updated_at?: string;
}

// =============================================================================
// Form helpers
// =============================================================================

const defaultFormState: Partial<TaskItem> = {
  name: "",
  content: "",
  category: "",
  tags: [],
};

function toFormState(item: TaskItem): Partial<TaskItem> {
  return {
    name: item.name,
    content: item.content,
    category: item.category ?? "",
    tags: item.tags ?? [],
  };
}

function toRequest(form: Partial<TaskItem>): unknown {
  return {
    name: form.name,
    content: form.content,
    category: form.category || undefined,
    tags: form.tags && form.tags.length > 0 ? form.tags : undefined,
  };
}

// =============================================================================
// TasksPage
// =============================================================================

export function TasksPage() {
  const builder = useLibraryBuilder<TaskItem>({
    resourcePath: "/prompts",
    defaultFormState,
    toFormState,
    toRequest,
  });

  return (
    <LibraryBuilderLayout<TaskItem>
      title="Tasks"
      icon={FileText}
      builder={builder}
      renderListItem={(item, isSelected) => <TaskListItem item={item} isSelected={isSelected} />}
      renderEditor={() => (
        <TaskEditor formState={builder.formState} setFormState={builder.setFormState} />
      )}
    />
  );
}

// =============================================================================
// List Item
// =============================================================================

function TaskListItem({ item, isSelected }: { item: TaskItem; isSelected: boolean }) {
  const contentPreview = item.content.slice(0, 60);
  const truncated = item.content.length > 60;
  const tagCount = item.tags?.length ?? 0;

  return (
    <div className="space-y-1">
      {/* Task name */}
      <div className="flex items-center gap-2">
        <FileText
          className={`w-4 h-4 shrink-0 ${isSelected ? "text-primary" : "text-muted-foreground"}`}
        />
        <span className="text-sm font-medium truncate">{item.name}</span>
      </div>

      {/* Category badge */}
      {item.category && (
        <div className="pl-6">
          <Badge variant="info" size="sm">
            {item.category}
          </Badge>
        </div>
      )}

      {/* Content preview */}
      <p className="text-xs text-muted-foreground font-mono truncate pl-6">
        {contentPreview}
        {truncated && "..."}
      </p>

      {/* Tag count */}
      {tagCount > 0 && (
        <div className="flex items-center gap-1 pl-6">
          <Tag className="w-3 h-3 text-muted-foreground" />
          <span className="text-[10px] text-muted-foreground">
            {tagCount} tag{tagCount !== 1 ? "s" : ""}
          </span>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Editor
// =============================================================================

function TaskEditor({
  formState,
  setFormState,
}: {
  formState: Partial<TaskItem>;
  setFormState: React.Dispatch<React.SetStateAction<Partial<TaskItem>>>;
}) {
  const [tagInput, setTagInput] = useState("");

  const updateField = useCallback(
    <K extends keyof TaskItem>(field: K, value: TaskItem[K]) => {
      setFormState((prev) => ({ ...prev, [field]: value }));
    },
    [setFormState],
  );

  const handleAddTag = useCallback(() => {
    const tag = tagInput.trim();
    if (!tag) return;

    const currentTags = formState.tags ?? [];
    if (currentTags.includes(tag)) {
      setTagInput("");
      return;
    }

    setFormState((prev) => ({
      ...prev,
      tags: [...(prev.tags ?? []), tag],
    }));
    setTagInput("");
  }, [tagInput, formState.tags, setFormState]);

  const handleRemoveTag = useCallback(
    (tagToRemove: string) => {
      setFormState((prev) => ({
        ...prev,
        tags: (prev.tags ?? []).filter((t) => t !== tagToRemove),
      }));
    },
    [setFormState],
  );

  const handleTagKeyDown = useCallback(
    (e: KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAddTag();
      }
    },
    [handleAddTag],
  );

  return (
    <div className="space-y-6">
      {/* Basic Info */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <FileText className="w-4 h-4 text-muted-foreground" />
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
              placeholder="Task name..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Category</label>
            <input
              type="text"
              aria-label="Category"
              value={formState.category ?? ""}
              onChange={(e) => updateField("category", e.target.value)}
              placeholder="e.g., code-review, bug-fix, feature"
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>
        </div>
      </section>

      {/* Content */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <FileText className="w-4 h-4 text-muted-foreground" />
          Content
        </h3>

        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Task Prompt</label>
          <textarea
            aria-label="Task Prompt"
            value={formState.content ?? ""}
            onChange={(e) => updateField("content", e.target.value)}
            placeholder="Enter the task/prompt content..."
            rows={10}
            className="w-full px-3 py-2 text-sm font-mono bg-muted/50 border border-border rounded-md
              placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
              text-foreground resize-y min-h-[200px]"
          />
          <p className="text-[11px] text-muted-foreground">
            The main prompt content that defines this task.
          </p>
        </div>
      </section>

      {/* Tags */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Tag className="w-4 h-4 text-muted-foreground" />
          Tags
        </h3>

        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <input
              type="text"
              aria-label="Add a tag"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={handleTagKeyDown}
              placeholder="Add a tag..."
              className="flex-1 px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
            <button
              type="button"
              onClick={handleAddTag}
              disabled={!tagInput.trim()}
              className={cn(
                "px-3 py-1.5 text-sm rounded-md border transition-colors",
                tagInput.trim()
                  ? "bg-primary/10 text-primary border-primary/30 hover:bg-primary/20"
                  : "bg-muted/30 text-muted-foreground border-border cursor-not-allowed",
              )}
            >
              Add
            </button>
          </div>
          {formState.tags && formState.tags.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {formState.tags.map((tag) => (
                <span
                  key={tag}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-md
                    bg-muted/50 text-foreground border border-border"
                >
                  {tag}
                  <button
                    type="button"
                    onClick={() => handleRemoveTag(tag)}
                    className="text-muted-foreground hover:text-foreground transition-colors"
                    aria-label={`Remove tag "${tag}"`}
                  >
                    <X className="w-3 h-3" />
                  </button>
                </span>
              ))}
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              No tags added yet. Tags help organize and filter tasks.
            </p>
          )}
        </div>
      </section>
    </div>
  );
}
