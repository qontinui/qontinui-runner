/**
 * PlaywrightTestsPage
 *
 * Library builder page for managing Playwright test items.
 * Uses useLibraryBuilder for state management and LibraryBuilderLayout
 * for the split-panel UI pattern.
 */

import { useState, useCallback } from "react";
import { TestTube, X, Plus, Tag, Globe, Code2, CheckCircle, XCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { LibraryBuilderLayout } from "./LibraryBuilderLayout";
import { useLibraryBuilder, type UseLibraryBuilderReturn } from "@/hooks/useLibraryBuilder";

// =============================================================================
// Types
// =============================================================================

interface PlaywrightTestItem {
  id: string;
  name: string;
  code: string;
  target_url?: string;
  category?: string;
  tags?: string[];
  created_at?: string;
  updated_at?: string;
}

interface RunResult {
  success: boolean;
  message?: string;
  error?: string;
  duration_ms?: number;
}

// =============================================================================
// Builder Configuration
// =============================================================================

const DEFAULT_TEST_CODE = `import { test, expect } from '@playwright/test';

test('example test', async ({ page }) => {
  await page.goto('https://example.com');
  await expect(page).toHaveTitle(/Example/);
});
`;

const defaultFormState: Partial<PlaywrightTestItem> = {
  name: "",
  code: "",
  target_url: "",
  category: "",
  tags: [],
};

function toFormState(item: PlaywrightTestItem): Partial<PlaywrightTestItem> {
  return {
    name: item.name,
    code: item.code ?? "",
    target_url: item.target_url ?? "",
    category: item.category ?? "",
    tags: item.tags ?? [],
  };
}

function toRequest(form: Partial<PlaywrightTestItem>): Record<string, unknown> {
  return {
    name: form.name,
    code: form.code || DEFAULT_TEST_CODE,
    target_url: form.target_url || undefined,
    category: form.category || undefined,
    tags: form.tags && form.tags.length > 0 ? form.tags : undefined,
  };
}

// =============================================================================
// List Item
// =============================================================================

interface PlaywrightTestListItemProps {
  item: PlaywrightTestItem;
  isSelected: boolean;
}

function PlaywrightTestListItem({ item, isSelected }: PlaywrightTestListItemProps) {
  const firstLine = (item.code ?? "").split("\n").find((line) => line.trim()) ?? "";
  const codePreview = firstLine.length > 50 ? firstLine.slice(0, 47) + "..." : firstLine;

  return (
    <div className="space-y-1">
      {/* Test name */}
      <span
        className={cn(
          "text-sm font-medium truncate block",
          isSelected ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {item.name || "Untitled Test"}
      </span>

      {/* Category badge */}
      {item.category && (
        <div>
          <Badge variant="info" size="sm">
            {item.category}
          </Badge>
        </div>
      )}

      {/* Target URL */}
      {item.target_url && (
        <p className="text-[11px] text-muted-foreground truncate">{item.target_url}</p>
      )}

      {/* Code preview */}
      {codePreview && (
        <p className="text-xs text-muted-foreground font-mono truncate">{codePreview}</p>
      )}
    </div>
  );
}

// =============================================================================
// Editor
// =============================================================================

interface PlaywrightTestEditorProps {
  builder: UseLibraryBuilderReturn<PlaywrightTestItem>;
  runResult: RunResult | null;
}

function PlaywrightTestEditor({ builder, runResult }: PlaywrightTestEditorProps) {
  const { formState, setFormState } = builder;
  const [tagInput, setTagInput] = useState("");

  const updateField = useCallback(
    <K extends keyof PlaywrightTestItem>(field: K, value: PlaywrightTestItem[K]) => {
      setFormState((prev) => ({ ...prev, [field]: value }));
    },
    [setFormState],
  );

  // --- Tag handlers ---

  const handleAddTag = useCallback(() => {
    const tag = tagInput.trim().toLowerCase();
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
    (e: React.KeyboardEvent<HTMLInputElement>) => {
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
          <TestTube className="w-4 h-4 text-muted-foreground" />
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
              placeholder="Test name..."
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
              placeholder="e.g., smoke, regression, e2e..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>
        </div>
      </section>

      {/* Target */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Globe className="w-4 h-4 text-muted-foreground" />
          Target
        </h3>

        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Target URL</label>
          <input
            type="text"
            aria-label="Target URL"
            value={formState.target_url ?? ""}
            onChange={(e) => updateField("target_url", e.target.value)}
            placeholder="https://localhost:3001"
            className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
              placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
              text-foreground"
          />
          <p className="text-[11px] text-muted-foreground">
            The base URL the test will run against.
          </p>
        </div>
      </section>

      {/* Code */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Code2 className="w-4 h-4 text-muted-foreground" />
          Test Code
        </h3>

        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">
            Playwright Test (TypeScript)
          </label>
          <textarea
            aria-label="Playwright Test (TypeScript)"
            value={formState.code ?? ""}
            onChange={(e) => updateField("code", e.target.value)}
            placeholder={DEFAULT_TEST_CODE}
            rows={15}
            spellCheck={false}
            className="w-full px-3 py-2 text-sm font-mono bg-muted/50 border border-border rounded-md
              placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
              text-foreground resize-y min-h-[300px] leading-relaxed"
          />
          <p className="text-[11px] text-muted-foreground">
            Write Playwright test code using TypeScript. The code will be executed as a Playwright
            test file.
          </p>
        </div>
      </section>

      {/* Tags */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Tag className="w-4 h-4 text-muted-foreground" />
          Tags
        </h3>

        {/* Tag input row */}
        <div className="flex items-center gap-2">
          <input
            type="text"
            aria-label="Add a tag"
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={handleTagKeyDown}
            placeholder="Add a tag..."
            className="flex-1 px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
              text-foreground placeholder:text-muted-foreground
              focus:outline-hidden focus:ring-1 focus:ring-primary"
          />
          <button
            type="button"
            onClick={handleAddTag}
            disabled={!tagInput.trim()}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded-md
              bg-primary text-primary-foreground hover:bg-primary/90
              disabled:opacity-50 disabled:cursor-not-allowed
              transition-colors"
          >
            <Plus className="w-3.5 h-3.5" />
            Add
          </button>
        </div>

        {/* Tag list */}
        {formState.tags && formState.tags.length > 0 ? (
          <div className="flex flex-wrap gap-2">
            {formState.tags.map((tag) => (
              <Badge key={tag} variant="outline" size="sm">
                {tag}
                <button
                  type="button"
                  onClick={() => handleRemoveTag(tag)}
                  className="ml-1 hover:text-foreground transition-colors"
                  aria-label={`Remove tag "${tag}"`}
                >
                  <X className="w-3 h-3" />
                </button>
              </Badge>
            ))}
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            No tags added yet. Tags help organize and filter tests.
          </p>
        )}
      </section>

      {/* Run Result */}
      {runResult && (
        <section className="space-y-4">
          <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
            {runResult.success ? (
              <CheckCircle className="w-4 h-4 text-green-500" />
            ) : (
              <XCircle className="w-4 h-4 text-red-500" />
            )}
            Last Run Result
          </h3>

          <div
            className={cn(
              "rounded-md border p-3 text-sm",
              runResult.success
                ? "border-green-500/30 bg-green-500/5 text-green-400"
                : "border-red-500/30 bg-red-500/5 text-red-400",
            )}
          >
            <div className="flex items-center justify-between">
              <span className="font-medium">{runResult.success ? "Passed" : "Failed"}</span>
              {runResult.duration_ms != null && (
                <span className="text-xs text-muted-foreground">{runResult.duration_ms}ms</span>
              )}
            </div>
            {runResult.message && <p className="mt-1 text-xs opacity-80">{runResult.message}</p>}
            {runResult.error && (
              <pre className="mt-2 text-xs font-mono whitespace-pre-wrap opacity-80 max-h-[200px] overflow-y-auto">
                {runResult.error}
              </pre>
            )}
          </div>
        </section>
      )}
    </div>
  );
}

// =============================================================================
// Main Page Component
// =============================================================================

export function PlaywrightTestsPage() {
  const builder = useLibraryBuilder<PlaywrightTestItem>({
    resourcePath: "/playwright/tests",
    defaultFormState,
    toFormState,
    toRequest,
  });

  const [runResult, setRunResult] = useState<RunResult | null>(null);

  // Wrap runSelected to capture the run result
  const handleRun = useCallback(async () => {
    setRunResult(null);
    const result = await builder.runSelected("run");
    if (result != null) {
      setRunResult(result as RunResult);
    }
  }, [builder]);

  // Clear run result when selection changes
  const handleSelectItem = useCallback(
    (item: PlaywrightTestItem | null) => {
      setRunResult(null);
      builder.selectItem(item);
    },
    [builder],
  );

  // Build a builder proxy that uses our custom selectItem and exposes handleRun
  const builderWithRunCapture: UseLibraryBuilderReturn<PlaywrightTestItem> = {
    ...builder,
    selectItem: handleSelectItem,
    runSelected: handleRun,
  };

  return (
    <LibraryBuilderLayout<PlaywrightTestItem>
      title="Playwright Tests"
      icon={TestTube}
      builder={builderWithRunCapture}
      renderListItem={(item, isSelected) => (
        <PlaywrightTestListItem item={item} isSelected={isSelected} />
      )}
      renderEditor={() => (
        <PlaywrightTestEditor builder={builderWithRunCapture} runResult={runResult} />
      )}
    />
  );
}
