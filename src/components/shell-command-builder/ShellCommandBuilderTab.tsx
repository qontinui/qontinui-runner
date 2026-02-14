/**
 * Shell Command Builder Tab
 *
 * Main component for the shell command builder.
 * Provides a 2-panel layout for creating and managing shell commands:
 * - Left: Command library panel (list of commands by category)
 * - Right: Command editor panel (command configuration and execution)
 */

import { useState, useCallback, useEffect } from "react";
import {
  Terminal,
  GitBranch,
  Package,
  FileCode,
  Container,
  Hammer,
  Play,
  FolderOpen,
  CheckCircle,
  XCircle,
  Clock,
  Search,
  Plus,
  MoreVertical,
  Trash2,
  Save,
  Tag,
  FileText,
  Power,
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Loader2,
  X,
  Sparkles,
  Check,
  ExternalLink,
} from "lucide-react";
import { BatchDeleteDialog } from "../ui/BatchDeleteDialog";
import { ShellCommandBuilderProvider, useShellCommandBuilder } from "./ShellCommandBuilderContext";
import { AiShellCommandGenerator } from "./AiShellCommandGenerator";
import type { ShellCommand, ExecuteShellCommandResult, CreateShellCommandInput } from "./types";
import { SHELL_COMMAND_CATEGORIES, getCategoryInfo, getCategoryColorClass } from "./types";

interface ShellCommandBuilderTabProps {
  onLog?: (level: string, message: string) => void;
}

// Icon mapping for categories
const categoryIcons: Record<string, React.ElementType> = {
  git: GitBranch,
  npm: Package,
  poetry: FileCode,
  docker: Container,
  build: Hammer,
  general: Terminal,
};

// Category icon component
function CategoryIcon({
  category,
  className = "w-4 h-4",
}: {
  category: string;
  className?: string;
}) {
  const Icon = categoryIcons[category] || Terminal;
  return <Icon className={className} />;
}

// Command list item component
interface CommandListItemProps {
  command: ShellCommand;
  isSelected: boolean;
  isSelectionMode: boolean;
  isChecked: boolean;
  onSelect: () => void;
  onDelete: () => void;
  onToggleSelection: () => void;
}

function CommandListItem({
  command,
  isSelected,
  isSelectionMode,
  isChecked,
  onSelect,
  onDelete,
  onToggleSelection,
}: CommandListItemProps) {
  const [showMenu, setShowMenu] = useState(false);

  return (
    <div
      className={`
        group flex items-center gap-2 px-3 py-2 rounded-md cursor-pointer
        transition-colors
        ${
          isSelected
            ? "bg-blue-500/15 text-blue-400 border-l-2 border-blue-400 ml-[-1px]"
            : "hover:bg-muted/30"
        }
        ${!command.enabled ? "opacity-50" : ""}
        ${isSelectionMode && isChecked ? "bg-red-500/10" : ""}
      `}
      onClick={isSelectionMode ? onToggleSelection : onSelect}
    >
      {isSelectionMode && (
        <div
          className={`w-4 h-4 rounded border flex-shrink-0 flex items-center justify-center ${
            isChecked ? "bg-red-500 border-red-500" : "border-neutral-500 hover:border-red-400"
          }`}
        >
          {isChecked && <Check className="w-3 h-3 text-white" />}
        </div>
      )}
      <CategoryIcon
        category={command.category}
        className={`w-4 h-4 flex-shrink-0 ${getCategoryColorClass(command.category)}`}
      />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate">{command.name}</div>
        <div className="text-xs text-muted-foreground truncate font-mono">
          {command.command.length > 40 ? command.command.slice(0, 40) + "..." : command.command}
        </div>
      </div>
      {!isSelectionMode && (
        <div className="relative">
          <button
            className="p-1 opacity-0 group-hover:opacity-100 hover:bg-muted rounded transition-opacity"
            onClick={(e) => {
              e.stopPropagation();
              setShowMenu(!showMenu);
            }}
          >
            <MoreVertical className="w-4 h-4" />
          </button>
          {showMenu && (
            <>
              <div className="fixed inset-0 z-10" onClick={() => setShowMenu(false)} />
              <div className="absolute right-0 top-full mt-1 z-20 bg-popover border border-border rounded-md shadow-lg py-1 min-w-[120px]">
                <button
                  className="w-full px-3 py-1.5 text-sm text-left hover:bg-muted flex items-center gap-2 text-destructive"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete();
                    setShowMenu(false);
                  }}
                >
                  <Trash2 className="w-4 h-4" />
                  Delete
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

// Library Panel
function CommandLibraryPanel({ onOpenAiGenerator }: { onOpenAiGenerator: () => void }) {
  const {
    shellCommands,
    selectedCommandId,
    isLoading,
    selectCommand,
    deleteCommand,
    createCommand,
  } = useShellCommandBuilder();

  const [searchQuery, setSearchQuery] = useState("");
  const [filterCategory, setFilterCategory] = useState<string | "all">("all");
  const [showNewMenu, setShowNewMenu] = useState(false);
  const [expandedCategories, setExpandedCategories] = useState<Set<string>>(
    new Set(SHELL_COMMAND_CATEGORIES.map((c) => c.id)),
  );

  // Bulk deletion state
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  // Toggle selection for bulk delete
  const toggleSelection = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Exit selection mode
  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  // Delete selected commands
  const deleteSelectedCommands = useCallback(async () => {
    setIsDeleting(true);
    try {
      for (const id of selectedIds) {
        await deleteCommand(id);
      }
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
    } catch (error) {
      console.error("Failed to delete commands:", error);
    } finally {
      setIsDeleting(false);
    }
  }, [selectedIds, deleteCommand, exitSelectionMode]);

  // Get names of selected commands for dialog
  const getSelectedNames = useCallback(() => {
    return shellCommands.filter((c) => selectedIds.has(c.id)).map((c) => c.name);
  }, [shellCommands, selectedIds]);

  const filteredCommands = shellCommands.filter((command) => {
    const matchesSearch =
      command.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      command.command.toLowerCase().includes(searchQuery.toLowerCase());
    const matchesCategory = filterCategory === "all" || command.category === filterCategory;
    return matchesSearch && matchesCategory;
  });

  // Group commands by category when showing all
  const groupedCommands = filteredCommands.reduce(
    (acc, command) => {
      const cat = command.category || "general";
      if (!acc[cat]) {
        acc[cat] = [];
      }
      acc[cat].push(command);
      return acc;
    },
    {} as Record<string, ShellCommand[]>,
  );

  const toggleCategory = (categoryId: string) => {
    setExpandedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(categoryId)) {
        next.delete(categoryId);
      } else {
        next.add(categoryId);
      }
      return next;
    });
  };

  const handleCreateCommand = async (category: string) => {
    setShowNewMenu(false);
    const categoryInfo = getCategoryInfo(category);

    const input: CreateShellCommandInput = {
      name: `New ${categoryInfo?.name || "Shell"} Command`,
      command: "",
      category,
      timeout_seconds: 60,
      fail_on_error: true,
    };

    await createCommand(input);
  };

  const handleDelete = async (id: string) => {
    if (window.confirm("Are you sure you want to delete this command?")) {
      await deleteCommand(id);
    }
  };

  return (
    <div className="h-full flex flex-col bg-card border-r border-border/50 w-72 flex-shrink-0">
      {/* Header */}
      <div className="p-4 border-b border-border/50">
        <div className="flex items-center justify-between mb-2">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Terminal className="w-4 h-4" />
            Shell Commands
          </h2>
        </div>

        {/* Action buttons */}
        <div className="flex items-center gap-1 mb-3">
          <button
            onClick={onOpenAiGenerator}
            className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-blue-400 hover:text-blue-300 text-xs"
            title="Generate with AI"
            data-ui-id="shell-builder-ai-btn"
          >
            <Sparkles className="w-3.5 h-3.5" />
            <span>AI</span>
          </button>
          <button
            onClick={() => window.open("http://localhost:3001/build/shell-commands", "_blank")}
            className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-indigo-400 hover:text-indigo-300 text-xs"
            title="Edit in Web (opens browser)"
          >
            <ExternalLink className="w-3.5 h-3.5" />
            <span>Web</span>
          </button>
          <button
            onClick={() => setIsSelectionMode(!isSelectionMode)}
            className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg transition-colors text-xs ${
              isSelectionMode
                ? "bg-red-500/20 text-red-400"
                : "hover:bg-muted text-muted-foreground hover:text-foreground"
            }`}
            title={isSelectionMode ? "Exit selection mode" : "Select commands to delete"}
            data-ui-id="shell-builder-delete-mode-btn"
          >
            <Trash2 className="w-3.5 h-3.5" />
            <span>Delete</span>
          </button>
          <div className="relative flex-1">
            <button
              className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-xs"
              onClick={() => setShowNewMenu(!showNewMenu)}
              title="Create new command"
              data-ui-id="shell-builder-new-btn"
            >
              <Plus className="w-3.5 h-3.5" />
              <span>New</span>
            </button>
            {showNewMenu && (
              <>
                <div className="fixed inset-0 z-10" onClick={() => setShowNewMenu(false)} />
                <div className="absolute right-0 top-full mt-1 z-20 bg-popover border border-border rounded-md shadow-lg py-1 min-w-[180px]">
                  {/* Category options */}
                  {SHELL_COMMAND_CATEGORIES.map((category) => {
                    const Icon = categoryIcons[category.id] || Terminal;
                    return (
                      <button
                        key={category.id}
                        className="w-full px-3 py-2 text-sm text-left hover:bg-muted flex items-center gap-2"
                        onClick={() => handleCreateCommand(category.id)}
                        data-ui-id={`shell-builder-new-${category.id}-btn`}
                      >
                        <Icon className={`w-4 h-4 ${getCategoryColorClass(category.id)}`} />
                        <span>{category.name}</span>
                      </button>
                    );
                  })}
                </div>
              </>
            )}
          </div>
        </div>

        {/* Search */}
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search commands..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-8 pr-3 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="shell-builder-search-input"
          />
        </div>

        {/* Filter */}
        <div className="mt-2">
          <select
            value={filterCategory}
            onChange={(e) => setFilterCategory(e.target.value)}
            className="w-full px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="shell-builder-filter-select"
          >
            <option value="all">All Categories</option>
            {SHELL_COMMAND_CATEGORIES.map((category) => (
              <option key={category.id} value={category.id}>
                {category.name}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Selection Mode Header */}
      {isSelectionMode && (
        <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
          <span className="text-sm text-red-400">{selectedIds.size} selected</span>
          <div className="flex items-center gap-2">
            <button
              onClick={() => setSelectedIds(new Set(filteredCommands.map((c) => c.id)))}
              className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded"
              data-ui-id="shell-builder-select-all-btn"
            >
              Select All
            </button>
            <button
              onClick={() => setSelectedIds(new Set())}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded disabled:opacity-50"
              data-ui-id="shell-builder-clear-selection-btn"
            >
              Clear
            </button>
            <button
              onClick={() => setShowBatchDeleteDialog(true)}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="shell-builder-delete-selected-btn"
            >
              Delete Selected
            </button>
            <button
              onClick={exitSelectionMode}
              className="p-1 text-neutral-400 hover:text-neutral-200"
              title="Cancel selection"
              data-ui-id="shell-builder-cancel-selection-btn"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>
      )}

      {/* Command List */}
      <div className="flex-1 overflow-y-auto p-2 space-y-2">
        {isLoading ? (
          <div className="text-center py-8 text-muted-foreground text-sm">Loading commands...</div>
        ) : filteredCommands.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground text-sm">
            {searchQuery || filterCategory !== "all"
              ? "No commands match your search"
              : "No commands yet. Click + to create one."}
          </div>
        ) : filterCategory === "all" ? (
          SHELL_COMMAND_CATEGORIES.map((category) => {
            const categoryCommands = groupedCommands[category.id];
            if (!categoryCommands || categoryCommands.length === 0) return null;

            const isExpanded = expandedCategories.has(category.id);
            const Icon = categoryIcons[category.id] || Terminal;

            return (
              <div key={category.id}>
                <button
                  className="w-full flex items-center gap-2 px-2 py-1.5 text-xs font-semibold text-muted-foreground/70 uppercase hover:bg-muted/20 rounded"
                  onClick={() => toggleCategory(category.id)}
                >
                  {isExpanded ? (
                    <ChevronDown className="w-3 h-3" />
                  ) : (
                    <ChevronRight className="w-3 h-3" />
                  )}
                  <Icon className={`w-3 h-3 ${getCategoryColorClass(category.id)}`} />
                  {category.name}
                  <span className="text-muted-foreground/50">({categoryCommands.length})</span>
                </button>
                {isExpanded && (
                  <div className="space-y-0.5 mt-1">
                    {categoryCommands.map((command) => (
                      <CommandListItem
                        key={command.id}
                        command={command}
                        isSelected={selectedCommandId === command.id}
                        isSelectionMode={isSelectionMode}
                        isChecked={selectedIds.has(command.id)}
                        onSelect={() => selectCommand(command.id)}
                        onDelete={() => handleDelete(command.id)}
                        onToggleSelection={() => toggleSelection(command.id)}
                      />
                    ))}
                  </div>
                )}
              </div>
            );
          })
        ) : (
          <div className="space-y-0.5">
            {filteredCommands.map((command) => (
              <CommandListItem
                key={command.id}
                command={command}
                isSelected={selectedCommandId === command.id}
                isSelectionMode={isSelectionMode}
                isChecked={selectedIds.has(command.id)}
                onSelect={() => selectCommand(command.id)}
                onDelete={() => handleDelete(command.id)}
                onToggleSelection={() => toggleSelection(command.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="p-3 border-t border-border/50">
        <span className="text-xs text-muted-foreground">
          {shellCommands.length} command{shellCommands.length !== 1 ? "s" : ""} total
        </span>
      </div>

      {/* Batch Delete Dialog */}
      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete Commands"
        itemType="command"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedCommands}
      />
    </div>
  );
}

// Editor Panel
function CommandEditorPanel() {
  const {
    selectedCommand,
    updateCommand,
    setDirty,
    executeCommand,
    lastExecutionResult,
    isSaving,
    isExecuting,
    clearExecutionResult,
  } = useShellCommandBuilder();

  // Form state
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [command, setCommand] = useState("");
  const [workingDirectory, setWorkingDirectory] = useState("");
  const [timeoutSeconds, setTimeoutSeconds] = useState(60);
  const [failOnError, setFailOnError] = useState(true);
  const [category, setCategory] = useState("general");
  const [tags, setTags] = useState<string[]>([]);
  const [tagInput, setTagInput] = useState("");
  const [enabled, setEnabled] = useState(true);

  // Reset form when selected command changes
  useEffect(() => {
    if (selectedCommand) {
      setName(selectedCommand.name);
      setDescription(selectedCommand.description || "");
      setCommand(selectedCommand.command);
      setWorkingDirectory(selectedCommand.working_directory || "");
      setTimeoutSeconds(selectedCommand.timeout_seconds);
      setFailOnError(selectedCommand.fail_on_error);
      setCategory(selectedCommand.category || "general");
      setTags(selectedCommand.tags || []);
      setEnabled(selectedCommand.enabled);
    } else {
      setName("");
      setDescription("");
      setCommand("");
      setWorkingDirectory("");
      setTimeoutSeconds(60);
      setFailOnError(true);
      setCategory("general");
      setTags([]);
      setEnabled(true);
    }
  }, [selectedCommand]);

  const handleAddTag = () => {
    if (tagInput.trim() && !tags.includes(tagInput.trim())) {
      setTags([...tags, tagInput.trim()]);
      setTagInput("");
      setDirty(true);
    }
  };

  const handleRemoveTag = (tag: string) => {
    setTags(tags.filter((t) => t !== tag));
    setDirty(true);
  };

  const handleSave = async () => {
    if (!selectedCommand) return;
    await updateCommand(selectedCommand.id, {
      name,
      description: description || undefined,
      command,
      working_directory: workingDirectory || undefined,
      timeout_seconds: timeoutSeconds,
      fail_on_error: failOnError,
      category,
      tags,
      enabled,
    });
  };

  const handleExecute = async () => {
    if (!selectedCommand) return;
    clearExecutionResult();
    await executeCommand({
      ...selectedCommand,
      name,
      command,
      working_directory: workingDirectory || selectedCommand.working_directory,
      timeout_seconds: timeoutSeconds,
      fail_on_error: failOnError,
    });
  };

  if (!selectedCommand) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground bg-neutral-900/50">
        <Terminal className="w-16 h-16 mb-4 opacity-20" />
        <p className="text-sm">Select a command to configure</p>
        <p className="text-xs text-muted-foreground/70 mt-2">
          or create a new one from the library
        </p>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 bg-neutral-800 border-b border-neutral-700">
        <div className="flex items-center gap-3">
          <CategoryIcon
            category={selectedCommand.category}
            className={`w-5 h-5 ${getCategoryColorClass(selectedCommand.category)}`}
          />
          <div>
            <h3 className="text-sm font-medium">{selectedCommand.name}</h3>
            <p className="text-xs text-muted-foreground">
              {getCategoryInfo(selectedCommand.category)?.name || "General"} Command
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleSave}
            disabled={isSaving}
            className="px-3 py-1.5 text-sm bg-neutral-700 hover:bg-neutral-600 rounded-md flex items-center gap-1.5 transition-colors disabled:opacity-50"
            title="Save command configuration"
            data-ui-id="shell-builder-save-btn"
          >
            <Save className="w-3.5 h-3.5" />
            {isSaving ? "Saving..." : "Save"}
          </button>
          <button
            onClick={handleExecute}
            disabled={isExecuting || !command.trim()}
            className="px-3 py-1.5 text-sm bg-blue-600 hover:bg-blue-700 text-white rounded-md flex items-center gap-1.5 transition-colors disabled:opacity-50"
            title="Execute this command"
            data-ui-id="shell-builder-execute-btn"
          >
            {isExecuting ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Play className="w-3.5 h-3.5" />
            )}
            {isExecuting ? "Running..." : "Execute"}
          </button>
        </div>
      </div>

      {/* Form */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {/* Name */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Name</label>
          <input
            type="text"
            value={name}
            onChange={(e) => {
              setName(e.target.value);
              setDirty(true);
            }}
            placeholder="Command name"
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="shell-builder-name-input"
          />
        </div>

        {/* Description */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <FileText className="w-3 h-3" />
            Description
          </label>
          <textarea
            value={description}
            onChange={(e) => {
              setDescription(e.target.value);
              setDirty(true);
            }}
            rows={2}
            placeholder="What does this command do?"
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50 resize-none"
            data-ui-id="shell-builder-description-input"
          />
        </div>

        {/* Category */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Category</label>
          <select
            value={category}
            onChange={(e) => {
              setCategory(e.target.value);
              setDirty(true);
            }}
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="shell-builder-category-select"
          >
            {SHELL_COMMAND_CATEGORIES.map((cat) => (
              <option key={cat.id} value={cat.id}>
                {cat.name}
              </option>
            ))}
          </select>
        </div>

        {/* Command */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <Terminal className="w-3 h-3" />
            Command
          </label>
          <textarea
            value={command}
            onChange={(e) => {
              setCommand(e.target.value);
              setDirty(true);
            }}
            rows={4}
            placeholder="Enter shell command..."
            className="w-full px-3 py-2 text-sm font-mono bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50 resize-none"
            data-ui-id="shell-builder-command-input"
          />
          <p className="text-xs text-muted-foreground">
            Multi-line commands supported. Use && for chaining commands.
          </p>
        </div>

        {/* Working Directory */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <FolderOpen className="w-3 h-3" />
            Working Directory
          </label>
          <input
            type="text"
            value={workingDirectory}
            onChange={(e) => {
              setWorkingDirectory(e.target.value);
              setDirty(true);
            }}
            placeholder="Current directory (default)"
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="shell-builder-working-dir-input"
          />
        </div>

        {/* Timeout */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <Clock className="w-3 h-3" />
            Timeout (seconds)
          </label>
          <input
            type="number"
            value={timeoutSeconds}
            onChange={(e) => {
              setTimeoutSeconds(parseInt(e.target.value) || 60);
              setDirty(true);
            }}
            min={1}
            max={3600}
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-blue-500/50"
            data-ui-id="shell-builder-timeout-input"
          />
        </div>

        {/* Toggles */}
        <div className="space-y-3 pt-2">
          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-sm flex items-center gap-2">
              <AlertTriangle className="w-4 h-4 text-amber-500" />
              Fail on Error
            </span>
            <input
              type="checkbox"
              checked={failOnError}
              onChange={(e) => {
                setFailOnError(e.target.checked);
                setDirty(true);
              }}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="shell-builder-fail-on-error-checkbox"
            />
          </label>
          <p className="text-xs text-muted-foreground ml-6">
            Stop workflow if this command returns non-zero exit code
          </p>

          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-sm flex items-center gap-2">
              <Power className="w-4 h-4" />
              Enabled
            </span>
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => {
                setEnabled(e.target.checked);
                setDirty(true);
              }}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-blue-500/50"
              data-ui-id="shell-builder-enabled-checkbox"
            />
          </label>
        </div>

        {/* Tags */}
        <div className="space-y-1.5 pt-2">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <Tag className="w-3 h-3" />
            Tags
          </label>
          <div className="flex gap-2">
            <input
              type="text"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  handleAddTag();
                }
              }}
              className="flex-1 px-3 py-1.5 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-blue-500/50"
              placeholder="Add tag..."
              data-ui-id="shell-builder-tag-input"
            />
            <button
              onClick={handleAddTag}
              className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md"
              data-ui-id="shell-builder-add-tag-btn"
            >
              Add
            </button>
          </div>
          {tags.length > 0 && (
            <div className="flex flex-wrap gap-1 mt-2">
              {tags.map((tag) => (
                <span
                  key={tag}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-xs bg-muted rounded-full"
                >
                  {tag}
                  <button onClick={() => handleRemoveTag(tag)} className="hover:text-destructive">
                    <X className="w-3 h-3" />
                  </button>
                </span>
              ))}
            </div>
          )}
        </div>

        {/* Metadata */}
        <div className="pt-4 border-t border-neutral-700 space-y-2 text-xs text-muted-foreground">
          <div>
            <span className="font-medium">Created:</span>{" "}
            {new Date(selectedCommand.created_at).toLocaleString()}
          </div>
          <div>
            <span className="font-medium">Updated:</span>{" "}
            {new Date(selectedCommand.updated_at).toLocaleString()}
          </div>
        </div>
      </div>

      {/* Execution Results */}
      {(lastExecutionResult || isExecuting) && (
        <div className="border-t border-neutral-700">
          <ExecutionResultPanel result={lastExecutionResult} isExecuting={isExecuting} />
        </div>
      )}
    </div>
  );
}

// Execution Result Panel
function ExecutionResultPanel({
  result,
  isExecuting,
}: {
  result: ExecuteShellCommandResult | null;
  isExecuting: boolean;
}) {
  const [expanded, setExpanded] = useState(true);

  if (isExecuting) {
    return (
      <div className="bg-neutral-900 px-4 py-3 flex items-center gap-3">
        <Loader2 className="w-4 h-4 animate-spin text-blue-400" />
        <span className="text-sm text-blue-400">Executing command...</span>
      </div>
    );
  }

  if (!result) return null;

  const StatusIcon = result.success ? CheckCircle : XCircle;
  const statusColorClass = result.success ? "text-green-400" : "text-red-400";

  return (
    <div className="bg-neutral-900">
      {/* Header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full px-4 py-2 flex items-center justify-between hover:bg-neutral-800/50"
      >
        <div className="flex items-center gap-3">
          <StatusIcon className={`w-4 h-4 ${statusColorClass}`} />
          <span className="text-sm font-medium">
            {result.success ? "Success" : "Failed"}
            {result.exit_code !== undefined && (
              <span className="text-muted-foreground ml-2">(exit code: {result.exit_code})</span>
            )}
          </span>
          <span className="text-xs text-muted-foreground">{result.duration_ms}ms</span>
        </div>
        <ChevronDown className={`w-4 h-4 transition-transform ${expanded ? "" : "-rotate-90"}`} />
      </button>

      {/* Output */}
      {expanded && (
        <div className="px-4 pb-4 space-y-3">
          {result.stdout && (
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1">stdout</div>
              <pre className="p-3 bg-black/30 rounded-md text-xs text-neutral-300 overflow-x-auto max-h-48 overflow-y-auto font-mono whitespace-pre-wrap">
                {result.stdout}
              </pre>
            </div>
          )}
          {result.stderr && (
            <div>
              <div className="text-xs font-medium text-red-400 mb-1">stderr</div>
              <pre className="p-3 bg-red-900/20 border border-red-700/30 rounded-md text-xs text-red-300 overflow-x-auto max-h-48 overflow-y-auto font-mono whitespace-pre-wrap">
                {result.stderr}
              </pre>
            </div>
          )}
          {!result.stdout && !result.stderr && (
            <div className="text-xs text-muted-foreground italic">No output</div>
          )}
        </div>
      )}
    </div>
  );
}

// Main content component
function ShellCommandBuilderContent({ onLog: _onLog }: ShellCommandBuilderTabProps) {
  const { createCommand, selectCommand } = useShellCommandBuilder();
  const [showAiGenerator, setShowAiGenerator] = useState(false);

  const handleAiCommandGenerated = useCallback(
    async (command: string, description: string) => {
      // Create a new shell command with the AI-generated content
      const input: CreateShellCommandInput = {
        name: description.length > 50 ? description.slice(0, 50) + "..." : description,
        description: description,
        command: command,
        category: "general",
        timeout_seconds: 60,
        fail_on_error: true,
      };

      const newCommand = await createCommand(input);
      if (newCommand) {
        selectCommand(newCommand.id);
      }
      setShowAiGenerator(false);
    },
    [createCommand, selectCommand],
  );

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left: Command Library */}
        <CommandLibraryPanel onOpenAiGenerator={() => setShowAiGenerator(true)} />

        {/* Right: Command Editor or AI Generator */}
        {showAiGenerator ? (
          <div className="flex-1 p-4">
            <AiShellCommandGenerator
              onCommandGenerated={handleAiCommandGenerated}
              onCancel={() => setShowAiGenerator(false)}
            />
          </div>
        ) : (
          <CommandEditorPanel />
        )}
      </div>
    </div>
  );
}

// Exported component with provider
export function ShellCommandBuilderTab({ onLog }: ShellCommandBuilderTabProps) {
  return (
    <ShellCommandBuilderProvider onLog={onLog}>
      <ShellCommandBuilderContent onLog={onLog} />
    </ShellCommandBuilderProvider>
  );
}
