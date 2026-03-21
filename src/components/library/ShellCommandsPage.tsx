/**
 * ShellCommandsPage
 *
 * Library builder page for managing shell command items.
 * Uses useLibraryBuilder for state management and LibraryBuilderLayout
 * for the split-panel UI pattern.
 */

import { useState, useCallback, type KeyboardEvent } from "react";
import { Terminal, X, FolderOpen, Clock, Monitor, Tag } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { LibraryBuilderLayout } from "./LibraryBuilderLayout";
import { useLibraryBuilder, type UseLibraryBuilderReturn } from "@/hooks/useLibraryBuilder";

// =============================================================================
// Types
// =============================================================================

interface ShellCommandItem {
  id: string;
  name: string;
  command: string;
  working_directory?: string;
  platform?: "any" | "windows" | "linux" | "macos";
  timeout?: number;
  category?: string;
  tags?: string[];
  created_at?: string;
  updated_at?: string;
}

// =============================================================================
// Builder Configuration
// =============================================================================

const defaultFormState: Partial<ShellCommandItem> = {
  name: "",
  command: "",
  working_directory: "",
  platform: "any",
  timeout: 30,
  category: "",
  tags: [],
};

function toFormState(item: ShellCommandItem): Partial<ShellCommandItem> {
  return {
    name: item.name,
    command: item.command,
    working_directory: item.working_directory ?? "",
    platform: item.platform ?? "any",
    timeout: item.timeout ?? 30,
    category: item.category ?? "",
    tags: item.tags ?? [],
  };
}

function toRequest(form: Partial<ShellCommandItem>): Record<string, unknown> {
  return {
    name: form.name,
    command: form.command,
    working_directory: form.working_directory || undefined,
    platform: form.platform || "any",
    timeout: form.timeout ?? 30,
    category: form.category || undefined,
    tags: form.tags ?? [],
  };
}

// =============================================================================
// Platform display helpers
// =============================================================================

const PLATFORM_OPTIONS = [
  { value: "any", label: "Any Platform" },
  { value: "windows", label: "Windows" },
  { value: "linux", label: "Linux" },
  { value: "macos", label: "macOS" },
] as const;

function platformLabel(platform: string): string {
  return PLATFORM_OPTIONS.find((o) => o.value === platform)?.label ?? platform;
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
// List Item
// =============================================================================

interface ShellCommandListItemProps {
  item: ShellCommandItem;
  isSelected: boolean;
}

function ShellCommandListItem({ item, isSelected }: ShellCommandListItemProps) {
  const firstLine = item.command.split("\n")[0];
  const truncatedCommand = firstLine.length > 60 ? firstLine.slice(0, 57) + "..." : firstLine;

  return (
    <div className="space-y-1">
      <span
        className={cn(
          "text-sm font-medium truncate block",
          isSelected ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {item.name || "Untitled Command"}
      </span>
      <div className="flex items-center gap-1.5">
        {item.platform && item.platform !== "any" && (
          <Badge variant="info" size="sm">
            {platformLabel(item.platform)}
          </Badge>
        )}
        {item.category && (
          <Badge variant="muted" size="sm">
            {item.category}
          </Badge>
        )}
      </div>
      <p className="text-xs text-muted-foreground font-mono truncate">{truncatedCommand}</p>
    </div>
  );
}

// =============================================================================
// Editor
// =============================================================================

interface ShellCommandEditorProps {
  builder: UseLibraryBuilderReturn<ShellCommandItem>;
}

function ShellCommandEditor({ builder }: ShellCommandEditorProps) {
  const { formState, setFormState } = builder;

  const updateField = useCallback(
    <K extends keyof ShellCommandItem>(field: K, value: ShellCommandItem[K]) => {
      setFormState((prev) => ({ ...prev, [field]: value }));
    },
    [setFormState],
  );

  return (
    <div className="space-y-6">
      {/* Basic Info */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Terminal className="w-4 h-4 text-muted-foreground" />
          Basic Info
        </h3>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Name</label>
            <input
              type="text"
              value={formState.name ?? ""}
              onChange={(e) => updateField("name", e.target.value)}
              placeholder="Command name..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Category</label>
            <input
              type="text"
              value={formState.category ?? ""}
              onChange={(e) => updateField("category", e.target.value)}
              placeholder="e.g., build, deploy, test..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>
        </div>
      </section>

      {/* Command */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Terminal className="w-4 h-4 text-muted-foreground" />
          Command
        </h3>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Shell Command</label>
            <textarea
              value={formState.command ?? ""}
              onChange={(e) => updateField("command", e.target.value)}
              placeholder="Enter shell command..."
              rows={6}
              className="w-full px-3 py-2 text-sm font-mono bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground resize-y"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground flex items-center gap-1.5">
              <FolderOpen className="w-3.5 h-3.5" />
              Working Directory
            </label>
            <input
              type="text"
              value={formState.working_directory ?? ""}
              onChange={(e) => updateField("working_directory", e.target.value)}
              placeholder="/path/to/directory (optional)"
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>
        </div>
      </section>

      {/* Execution */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Monitor className="w-4 h-4 text-muted-foreground" />
          Execution
        </h3>

        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Platform</label>
            <select
              value={formState.platform ?? "any"}
              onChange={(e) =>
                updateField("platform", e.target.value as ShellCommandItem["platform"])
              }
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary"
            >
              {PLATFORM_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground flex items-center gap-1.5">
              <Clock className="w-3.5 h-3.5" />
              Timeout (seconds)
            </label>
            <input
              type="number"
              value={formState.timeout ?? 30}
              onChange={(e) => updateField("timeout", parseInt(e.target.value, 10) || 30)}
              min={1}
              max={3600}
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary"
            />
          </div>
        </div>
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

export function ShellCommandsPage() {
  const builder = useLibraryBuilder<ShellCommandItem>({
    resourcePath: "/shell-commands",
    defaultFormState,
    toFormState,
    toRequest,
  });

  return (
    <LibraryBuilderLayout<ShellCommandItem>
      title="Shell Commands"
      icon={Terminal}
      builder={builder}
      renderListItem={(item, isSelected) => (
        <ShellCommandListItem item={item} isSelected={isSelected} />
      )}
      renderEditor={() => <ShellCommandEditor builder={builder} />}
    />
  );
}
