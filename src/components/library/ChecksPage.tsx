/**
 * ChecksPage
 *
 * Library builder page for managing check items (linters, formatters,
 * type checkers, test runners, security scanners, build tools, custom).
 * Uses useLibraryBuilder for state management and LibraryBuilderLayout
 * for the split-panel UI pattern.
 */

import { useState, useCallback, useMemo, type KeyboardEvent } from "react";
import { CheckCircle2, X, FolderOpen, Clock, Settings, Tag, Wrench, Terminal } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui";
import { LibraryBuilderLayout } from "./LibraryBuilderLayout";
import { useLibraryBuilder, type UseLibraryBuilderReturn } from "@/hooks/useLibraryBuilder";

// =============================================================================
// Types
// =============================================================================

interface CheckItem {
  id: string;
  name: string;
  description?: string;
  check_type: string;
  tool?: string;
  command?: string;
  working_directory?: string;
  config_path?: string;
  auto_fix: boolean;
  fail_on_warning: boolean;
  is_critical: boolean;
  timeout_seconds: number;
  enabled: boolean;
  tags?: string[];
  created_at?: string;
  updated_at?: string;
}

// =============================================================================
// Constants
// =============================================================================

const CHECK_TYPES = [
  { value: "linter", label: "Linter", color: "warning" as const },
  { value: "formatter", label: "Formatter", color: "info" as const },
  { value: "type_checker", label: "Type Checker", color: "purple" as const },
  { value: "test_runner", label: "Test Runner", color: "success" as const },
  { value: "security", label: "Security", color: "danger" as const },
  { value: "build", label: "Build", color: "info" as const },
  { value: "custom", label: "Custom", color: "muted" as const },
] as const;

const CHECK_TYPE_BADGE_COLORS: Record<string, { bg: string; text: string; border: string }> = {
  linter: {
    bg: "bg-amber-500/10",
    text: "text-amber-400",
    border: "border-amber-500/30",
  },
  formatter: {
    bg: "bg-blue-500/10",
    text: "text-blue-400",
    border: "border-blue-500/30",
  },
  type_checker: {
    bg: "bg-purple-500/10",
    text: "text-purple-400",
    border: "border-purple-500/30",
  },
  test_runner: {
    bg: "bg-green-500/10",
    text: "text-green-400",
    border: "border-green-500/30",
  },
  security: {
    bg: "bg-red-500/10",
    text: "text-red-400",
    border: "border-red-500/30",
  },
  build: {
    bg: "bg-cyan-500/10",
    text: "text-cyan-400",
    border: "border-cyan-500/30",
  },
  custom: {
    bg: "bg-gray-500/10",
    text: "text-gray-400",
    border: "border-gray-500/30",
  },
};

const TOOLS_BY_CHECK_TYPE: Record<string, string[]> = {
  linter: ["eslint", "ruff", "pylint", "clippy", "custom"],
  formatter: ["prettier", "black", "rustfmt", "custom"],
  type_checker: ["tsc", "mypy", "flow", "custom"],
  test_runner: ["jest", "pytest", "cargo-test", "vitest", "custom"],
  security: ["bandit", "npm-audit", "cargo-audit", "snyk", "custom"],
  build: ["webpack", "vite", "cargo", "tsc", "custom"],
  custom: ["custom"],
};

/**
 * Sensible defaults per check_type + tool combo.
 */
const CHECK_DEFAULTS: Record<string, { name: string; command: string; auto_fix: boolean }> = {
  "linter:eslint": {
    name: "ESLint",
    command: "npx eslint . --ext .ts,.tsx,.js,.jsx",
    auto_fix: false,
  },
  "linter:ruff": {
    name: "Ruff Lint",
    command: "ruff check .",
    auto_fix: false,
  },
  "linter:pylint": {
    name: "Pylint",
    command: "pylint **/*.py",
    auto_fix: false,
  },
  "linter:clippy": {
    name: "Clippy",
    command: "cargo clippy -- -D warnings",
    auto_fix: false,
  },
  "formatter:prettier": {
    name: "Prettier",
    command: "npx prettier --check .",
    auto_fix: true,
  },
  "formatter:black": {
    name: "Black",
    command: "black --check .",
    auto_fix: true,
  },
  "formatter:rustfmt": {
    name: "Rustfmt",
    command: "cargo fmt --check",
    auto_fix: true,
  },
  "type_checker:tsc": {
    name: "TypeScript",
    command: "npx tsc --noEmit",
    auto_fix: false,
  },
  "type_checker:mypy": {
    name: "Mypy",
    command: "mypy .",
    auto_fix: false,
  },
  "type_checker:flow": {
    name: "Flow",
    command: "npx flow check",
    auto_fix: false,
  },
  "test_runner:jest": {
    name: "Jest Tests",
    command: "npx jest",
    auto_fix: false,
  },
  "test_runner:pytest": {
    name: "Pytest",
    command: "pytest",
    auto_fix: false,
  },
  "test_runner:cargo-test": {
    name: "Cargo Test",
    command: "cargo test",
    auto_fix: false,
  },
  "test_runner:vitest": {
    name: "Vitest",
    command: "npx vitest run",
    auto_fix: false,
  },
  "security:bandit": {
    name: "Bandit",
    command: "bandit -r .",
    auto_fix: false,
  },
  "security:npm-audit": {
    name: "NPM Audit",
    command: "npm audit",
    auto_fix: false,
  },
  "security:cargo-audit": {
    name: "Cargo Audit",
    command: "cargo audit",
    auto_fix: false,
  },
  "security:snyk": {
    name: "Snyk",
    command: "npx snyk test",
    auto_fix: false,
  },
  "build:webpack": {
    name: "Webpack Build",
    command: "npx webpack --mode production",
    auto_fix: false,
  },
  "build:vite": {
    name: "Vite Build",
    command: "npx vite build",
    auto_fix: false,
  },
  "build:cargo": {
    name: "Cargo Build",
    command: "cargo build --release",
    auto_fix: false,
  },
  "build:tsc": {
    name: "TSC Build",
    command: "npx tsc --build",
    auto_fix: false,
  },
};

function getToolsForCheckType(checkType: string): string[] {
  return TOOLS_BY_CHECK_TYPE[checkType] ?? ["custom"];
}

function getCheckTypeLabel(checkType: string): string {
  return (
    CHECK_TYPES.find((t) => t.value === checkType)?.label ??
    checkType.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase())
  );
}

// =============================================================================
// Builder Configuration
// =============================================================================

const defaultFormState: Partial<CheckItem> = {
  name: "",
  description: "",
  check_type: "linter",
  tool: "custom",
  command: "",
  working_directory: "",
  config_path: "",
  auto_fix: false,
  fail_on_warning: false,
  is_critical: false,
  timeout_seconds: 300,
  enabled: true,
  tags: [],
};

function toFormState(item: CheckItem): Partial<CheckItem> {
  return {
    name: item.name,
    description: item.description ?? "",
    check_type: item.check_type,
    tool: item.tool ?? "custom",
    command: item.command ?? "",
    working_directory: item.working_directory ?? "",
    config_path: item.config_path ?? "",
    auto_fix: item.auto_fix,
    fail_on_warning: item.fail_on_warning,
    is_critical: item.is_critical,
    timeout_seconds: item.timeout_seconds ?? 300,
    enabled: item.enabled,
    tags: item.tags ?? [],
  };
}

function toRequest(form: Partial<CheckItem>): Record<string, unknown> {
  return {
    name: form.name,
    description: form.description || undefined,
    check_type: form.check_type || "linter",
    tool: form.tool || undefined,
    command: form.command || undefined,
    working_directory: form.working_directory || undefined,
    config_path: form.config_path || undefined,
    auto_fix: form.auto_fix ?? false,
    fail_on_warning: form.fail_on_warning ?? false,
    is_critical: form.is_critical ?? false,
    timeout_seconds: form.timeout_seconds ?? 300,
    enabled: form.enabled ?? true,
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
// Check Type Badge
// =============================================================================

function CheckTypeBadge({ checkType }: { checkType: string }) {
  const colors = CHECK_TYPE_BADGE_COLORS[checkType] ?? CHECK_TYPE_BADGE_COLORS.custom;
  return (
    <span
      className={cn(
        "inline-flex items-center px-1.5 py-0.5 text-[10px] font-medium rounded-md border",
        colors.bg,
        colors.text,
        colors.border,
      )}
    >
      {getCheckTypeLabel(checkType)}
    </span>
  );
}

// =============================================================================
// List Item
// =============================================================================

interface CheckListItemProps {
  item: CheckItem;
  isSelected: boolean;
}

function CheckListItem({ item, isSelected }: CheckListItemProps) {
  return (
    <div className="space-y-1">
      <span
        className={cn(
          "text-sm font-medium truncate block",
          isSelected ? "text-foreground" : "text-muted-foreground",
        )}
      >
        {item.name || "Untitled Check"}
      </span>
      <div className="flex items-center gap-1.5 flex-wrap">
        <CheckTypeBadge checkType={item.check_type} />
        {item.tool && item.tool !== "custom" && (
          <Badge variant="muted" size="sm">
            {item.tool}
          </Badge>
        )}
        {!item.enabled && (
          <Badge variant="warning" size="sm">
            disabled
          </Badge>
        )}
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

interface CheckEditorProps {
  builder: UseLibraryBuilderReturn<CheckItem>;
}

function CheckEditor({ builder }: CheckEditorProps) {
  const { formState, setFormState } = builder;

  const updateField = useCallback(
    <K extends keyof CheckItem>(field: K, value: CheckItem[K]) => {
      setFormState((prev) => ({ ...prev, [field]: value }));
    },
    [setFormState],
  );

  const availableTools = useMemo(
    () => getToolsForCheckType(formState.check_type ?? "custom"),
    [formState.check_type],
  );

  const handleCheckTypeChange = useCallback(
    (newType: string) => {
      const tools = getToolsForCheckType(newType);
      const newTool = tools[0] ?? "custom";

      // Look up defaults for the new type + tool combination
      const defaultsKey = `${newType}:${newTool}`;
      const defaults = CHECK_DEFAULTS[defaultsKey];

      setFormState((prev) => ({
        ...prev,
        check_type: newType,
        tool: newTool,
        // Apply defaults if the name is empty or this is a create
        ...(defaults && (!prev.name || builder.isCreating)
          ? {
              name: defaults.name,
              command: defaults.command,
              auto_fix: defaults.auto_fix,
            }
          : {}),
      }));
    },
    [setFormState, builder.isCreating],
  );

  const handleToolChange = useCallback(
    (newTool: string) => {
      const checkType = formState.check_type ?? "custom";
      const defaultsKey = `${checkType}:${newTool}`;
      const defaults = CHECK_DEFAULTS[defaultsKey];

      setFormState((prev) => ({
        ...prev,
        tool: newTool,
        ...(defaults && (!prev.name || builder.isCreating)
          ? {
              name: defaults.name,
              command: defaults.command,
              auto_fix: defaults.auto_fix,
            }
          : {}),
      }));
    },
    [formState.check_type, setFormState, builder.isCreating],
  );

  return (
    <div className="space-y-6">
      {/* Basic Info */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <CheckCircle2 className="w-4 h-4 text-muted-foreground" />
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
              placeholder="Check name..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Description</label>
            <input
              type="text"
              aria-label="Description"
              value={formState.description ?? ""}
              onChange={(e) => updateField("description", e.target.value)}
              placeholder="Brief description (optional)..."
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">Check Type</label>
              <select
                aria-label="Check Type"
                value={formState.check_type ?? "linter"}
                onChange={(e) => handleCheckTypeChange(e.target.value)}
                className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                  text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary"
              >
                {CHECK_TYPES.map((ct) => (
                  <option key={ct.value} value={ct.value}>
                    {ct.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground flex items-center gap-1.5">
                <Wrench className="w-3.5 h-3.5" />
                Tool
              </label>
              <select
                aria-label="Tool"
                value={formState.tool ?? "custom"}
                onChange={(e) => handleToolChange(e.target.value)}
                className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                  text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary"
              >
                {availableTools.map((tool) => (
                  <option key={tool} value={tool}>
                    {tool}
                  </option>
                ))}
              </select>
            </div>
          </div>
        </div>
      </section>

      {/* Execution */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Terminal className="w-4 h-4 text-muted-foreground" />
          Execution
        </h3>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Command</label>
            <textarea
              aria-label="Command"
              value={formState.command ?? ""}
              onChange={(e) => updateField("command", e.target.value)}
              placeholder="Shell command to execute..."
              rows={4}
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
              aria-label="Working Directory"
              value={formState.working_directory ?? ""}
              onChange={(e) => updateField("working_directory", e.target.value)}
              placeholder="/path/to/project (optional)"
              className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                text-foreground"
            />
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">Config Path</label>
              <input
                type="text"
                aria-label="Config Path"
                value={formState.config_path ?? ""}
                onChange={(e) => updateField("config_path", e.target.value)}
                placeholder=".eslintrc.json (optional)"
                className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                  placeholder:text-muted-foreground focus:outline-hidden focus:ring-1 focus:ring-primary
                  text-foreground"
              />
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground flex items-center gap-1.5">
                <Clock className="w-3.5 h-3.5" />
                Timeout (seconds)
              </label>
              <input
                type="number"
                aria-label="Timeout (seconds)"
                value={formState.timeout_seconds ?? 300}
                onChange={(e) =>
                  updateField("timeout_seconds", parseInt(e.target.value, 10) || 300)
                }
                min={1}
                max={7200}
                className="w-full px-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                  text-foreground focus:outline-hidden focus:ring-1 focus:ring-primary"
              />
            </div>
          </div>
        </div>
      </section>

      {/* Behavior */}
      <section className="space-y-4">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-2">
          <Settings className="w-4 h-4 text-muted-foreground" />
          Behavior
        </h3>

        <div className="space-y-3">
          <label className="flex items-center gap-3 cursor-pointer group">
            <input
              type="checkbox"
              checked={formState.auto_fix ?? false}
              onChange={(e) => updateField("auto_fix", e.target.checked)}
              className="w-4 h-4 rounded border-border bg-muted/50 text-primary
                focus:ring-1 focus:ring-primary accent-primary"
            />
            <div>
              <span className="text-sm text-foreground group-hover:text-foreground">Auto-fix</span>
              <p className="text-xs text-muted-foreground">
                Automatically apply fixes when available
              </p>
            </div>
          </label>

          <label className="flex items-center gap-3 cursor-pointer group">
            <input
              type="checkbox"
              checked={formState.fail_on_warning ?? false}
              onChange={(e) => updateField("fail_on_warning", e.target.checked)}
              className="w-4 h-4 rounded border-border bg-muted/50 text-primary
                focus:ring-1 focus:ring-primary accent-primary"
            />
            <div>
              <span className="text-sm text-foreground group-hover:text-foreground">
                Fail on warning
              </span>
              <p className="text-xs text-muted-foreground">Treat warnings as errors</p>
            </div>
          </label>

          <label className="flex items-center gap-3 cursor-pointer group">
            <input
              type="checkbox"
              checked={formState.is_critical ?? false}
              onChange={(e) => updateField("is_critical", e.target.checked)}
              className="w-4 h-4 rounded border-border bg-muted/50 text-primary
                focus:ring-1 focus:ring-primary accent-primary"
            />
            <div>
              <span className="text-sm text-foreground group-hover:text-foreground">Critical</span>
              <p className="text-xs text-muted-foreground">
                Mark as a critical check that must pass
              </p>
            </div>
          </label>

          <label className="flex items-center gap-3 cursor-pointer group">
            <input
              type="checkbox"
              checked={formState.enabled ?? true}
              onChange={(e) => updateField("enabled", e.target.checked)}
              className="w-4 h-4 rounded border-border bg-muted/50 text-primary
                focus:ring-1 focus:ring-primary accent-primary"
            />
            <div>
              <span className="text-sm text-foreground group-hover:text-foreground">Enabled</span>
              <p className="text-xs text-muted-foreground">
                Include this check in group and workflow runs
              </p>
            </div>
          </label>
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

export function ChecksPage() {
  const builder = useLibraryBuilder<CheckItem>({
    resourcePath: "/checks",
    defaultFormState,
    toFormState,
    toRequest,
  });

  return (
    <LibraryBuilderLayout<CheckItem>
      title="Checks"
      icon={CheckCircle2}
      builder={builder}
      renderListItem={(item, isSelected) => <CheckListItem item={item} isSelected={isSelected} />}
      renderEditor={() => <CheckEditor builder={builder} />}
    />
  );
}
