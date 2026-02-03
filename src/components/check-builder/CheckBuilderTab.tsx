/**
 * Check Builder Tab
 *
 * Main component for the code quality check builder.
 * Provides a 3-panel layout for creating and managing checks:
 * - Left: Check library panel (list of checks)
 * - Center: Check editor panel (command configuration)
 * - Right: Properties panel (metadata and settings)
 * - Bottom: Execution panel (run checks and view results)
 */

import { useState, useCallback, useEffect } from "react";
import {
  Shield,
  Sparkles,
  Play,
  FolderSearch,
  CheckCircle,
  XCircle,
  Wrench,
  AlertTriangle,
  Terminal,
  Clock,
  Search,
  Plus,
  MoreVertical,
  Trash2,
  Save,
  Settings,
  Tag,
  FileText,
  Power,
  AlignLeft,
  FileType,
  FolderOpen,
  Check as CheckIcon,
  X,
} from "lucide-react";
import { BatchDeleteDialog } from "../ui/BatchDeleteDialog";
import { CheckBuilderProvider, useCheckBuilder } from "./CheckBuilderContext";
import { AiConfigureChecksModal } from "./AiConfigureChecksModal";
import { CheckGroupsPanel } from "./CheckGroupsPanel";
import { PageTutorialMenu } from "../tutorial";
import type { Check, CheckType, CheckTool, CheckExecutionResult, CreateCheckInput } from "./types";
import { CHECK_TOOLS, CHECK_TYPE_INFO, getToolInfo, getStatusInfo } from "./types";

type TabView = "checks" | "groups";

interface CheckBuilderTabProps {
  onLog?: (level: string, message: string) => void;
}

// Icon mapping for check types
const checkTypeIcons: Record<CheckType, React.ElementType> = {
  lint: AlertTriangle,
  format: AlignLeft,
  typecheck: FileType,
  security: Shield,
  analyze: Search,
  custom_command: Terminal,
};

// Tool icon component
function ToolIcon({ tool }: { tool: CheckTool }) {
  const toolInfo = getToolInfo(tool);
  if (!toolInfo) return <Terminal className="w-4 h-4" />;

  const typeIcon = checkTypeIcons[toolInfo.check_type];
  const Icon = typeIcon || Terminal;
  return <Icon className="w-4 h-4" />;
}

// Check list item component
interface CheckListItemProps {
  check: Check;
  isSelected: boolean;
  isSelectionMode: boolean;
  isChecked: boolean;
  onSelect: () => void;
  onDelete: () => void;
  onToggleSelection: () => void;
}

function CheckListItem({
  check,
  isSelected,
  isSelectionMode,
  isChecked,
  onSelect,
  onDelete,
  onToggleSelection,
}: CheckListItemProps) {
  const [showMenu, setShowMenu] = useState(false);
  const toolInfo = getToolInfo(check.tool);
  const TypeIcon = checkTypeIcons[check.check_type] || Terminal;

  return (
    <div
      className={`
        group flex items-center gap-2 px-3 py-2 rounded-md cursor-pointer
        transition-colors
        ${
          isSelected
            ? "bg-cyan-500/15 text-cyan-400 border-l-2 border-cyan-400 ml-[-1px]"
            : "hover:bg-muted/30"
        }
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
          {isChecked && <CheckIcon className="w-3 h-3 text-white" />}
        </div>
      )}
      <TypeIcon className="w-4 h-4 flex-shrink-0 text-muted-foreground" />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate">{check.name}</div>
        <div className="text-xs text-muted-foreground truncate">
          {toolInfo?.name || check.tool}
          {check.auto_fix && <span className="ml-1 text-emerald-500">auto-fix</span>}
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
interface CheckLibraryPanelProps {
  onOpenAiModal?: () => void;
}

function CheckLibraryPanel({ onOpenAiModal }: CheckLibraryPanelProps) {
  const { checks, selectedCheckId, isLoading, selectCheck, deleteCheck, createCheck } =
    useCheckBuilder();

  const [searchQuery, setSearchQuery] = useState("");
  const [filterType, setFilterType] = useState<CheckType | "all">("all");
  const [showNewMenu, setShowNewMenu] = useState(false);

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

  // Delete selected checks
  const deleteSelectedChecks = useCallback(async () => {
    setIsDeleting(true);
    try {
      for (const id of selectedIds) {
        await deleteCheck(id);
      }
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
    } catch (error) {
      console.error("Failed to delete checks:", error);
    } finally {
      setIsDeleting(false);
    }
  }, [selectedIds, deleteCheck, exitSelectionMode]);

  // Get names of selected checks for dialog
  const getSelectedNames = useCallback(() => {
    return checks.filter((c) => selectedIds.has(c.id)).map((c) => c.name);
  }, [checks, selectedIds]);

  const filteredChecks = checks.filter((check) => {
    const matchesSearch = check.name.toLowerCase().includes(searchQuery.toLowerCase());
    const matchesType = filterType === "all" || check.check_type === filterType;
    return matchesSearch && matchesType;
  });

  // Group checks by type when showing all
  const groupedChecks = filteredChecks.reduce(
    (acc, check) => {
      if (!acc[check.check_type]) {
        acc[check.check_type] = [];
      }
      acc[check.check_type].push(check);
      return acc;
    },
    {} as Record<CheckType, Check[]>,
  );

  const handleCreateCheck = async (tool: CheckTool) => {
    setShowNewMenu(false);
    const toolInfo = getToolInfo(tool);
    if (!toolInfo) return;

    const input: CreateCheckInput = {
      name: `New ${toolInfo.name} Check`,
      check_type: toolInfo.check_type,
      tool,
      command: toolInfo.default_command,
      auto_fix: toolInfo.check_type === "format", // Default formatters to auto-fix
      timeout_seconds: 60,
      is_critical: false,
      enabled: true,
    };

    await createCheck(input);
  };

  const handleDelete = async (id: string) => {
    if (window.confirm("Are you sure you want to delete this check?")) {
      await deleteCheck(id);
    }
  };

  return (
    <div
      className="h-full flex flex-col bg-card border-r border-border/50 w-64 flex-shrink-0"
      data-tutorial-id="check-library-panel"
    >
      {/* Header */}
      <div className="p-4 border-b border-border/50">
        <div className="flex items-center justify-between mb-2">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Shield className="w-4 h-4" />
            Checks
          </h2>
          <PageTutorialMenu page="check-builder" variant="compact" />
        </div>

        {/* Action buttons */}
        <div className="flex items-center gap-1 mb-3">
          {onOpenAiModal ? (
            <button
              onClick={onOpenAiModal}
              className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-blue-400 hover:text-blue-300 text-xs"
              title="Detect project and suggest checks"
            >
              <Sparkles className="w-3.5 h-3.5" />
              <span>AI</span>
            </button>
          ) : (
            <button
              disabled
              className="flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg text-xs text-muted-foreground cursor-not-allowed"
              title="AI detection coming soon"
            >
              <Sparkles className="w-3.5 h-3.5" />
              <span>AI</span>
            </button>
          )}
          <button
            onClick={() => setIsSelectionMode(!isSelectionMode)}
            className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg transition-colors text-xs ${
              isSelectionMode
                ? "bg-red-500/20 text-red-400"
                : "hover:bg-muted text-muted-foreground hover:text-foreground"
            }`}
            title={isSelectionMode ? "Exit selection mode" : "Select checks to delete"}
            data-ui-id="check-builder-delete-mode-btn"
          >
            <Trash2 className="w-3.5 h-3.5" />
            <span>Delete</span>
          </button>
          <div className="relative flex-1">
            <button
              className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg hover:bg-muted transition-colors text-xs"
              onClick={() => setShowNewMenu(!showNewMenu)}
              data-tutorial-id="new-check-button"
              title="Create new check"
            >
              <Plus className="w-3.5 h-3.5" />
              <span>New</span>
            </button>
            {showNewMenu && (
              <>
                <div className="fixed inset-0 z-10" onClick={() => setShowNewMenu(false)} />
                <div className="absolute right-0 top-full mt-1 z-20 bg-popover border border-border rounded-md shadow-lg py-1 min-w-[200px] max-h-80 overflow-y-auto">
                  {CHECK_TYPE_INFO.map((typeInfo) => (
                    <div key={typeInfo.type}>
                      <div className="px-3 py-1.5 text-xs font-semibold text-muted-foreground/70 uppercase bg-muted/20">
                        {typeInfo.name}
                      </div>
                      {CHECK_TOOLS.filter((t) => t.check_type === typeInfo.type).map((tool) => {
                        const Icon = checkTypeIcons[tool.check_type] || Terminal;
                        return (
                          <button
                            key={tool.tool}
                            className="w-full px-3 py-2 text-sm text-left hover:bg-muted flex items-center gap-2"
                            onClick={() => handleCreateCheck(tool.tool)}
                          >
                            <Icon className="w-4 h-4" />
                            <div>
                              <div>{tool.name}</div>
                              <div className="text-xs text-muted-foreground">{tool.language}</div>
                            </div>
                          </button>
                        );
                      })}
                    </div>
                  ))}
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
            placeholder="Search checks..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-8 pr-3 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            data-ui-id="check-builder-search-input"
          />
        </div>

        {/* Filter */}
        <div className="mt-2">
          <select
            value={filterType}
            onChange={(e) => setFilterType(e.target.value as CheckType | "all")}
            className="w-full px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            data-ui-id="check-builder-filter-type-select"
          >
            <option value="all">All Types</option>
            {CHECK_TYPE_INFO.map((type) => (
              <option key={type.type} value={type.type}>
                {type.name}
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
              onClick={() => setSelectedIds(new Set(filteredChecks.map((c) => c.id)))}
              className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded"
              data-ui-id="check-builder-select-all-btn"
            >
              Select All
            </button>
            <button
              onClick={() => setSelectedIds(new Set())}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded disabled:opacity-50"
              data-ui-id="check-builder-clear-selection-btn"
            >
              Clear
            </button>
            <button
              onClick={() => setShowBatchDeleteDialog(true)}
              disabled={selectedIds.size === 0}
              className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
              data-ui-id="check-builder-delete-selected-btn"
            >
              Delete Selected
            </button>
            <button
              onClick={exitSelectionMode}
              className="p-1 text-neutral-400 hover:text-neutral-200"
              title="Cancel selection"
              data-ui-id="check-builder-cancel-selection-btn"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>
      )}

      {/* Check List */}
      <div className="flex-1 overflow-y-auto p-2 space-y-4">
        {isLoading ? (
          <div className="text-center py-8 text-muted-foreground text-sm">Loading checks...</div>
        ) : filteredChecks.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground text-sm">
            {searchQuery || filterType !== "all"
              ? "No checks match your search"
              : "No checks yet. Click + to create one."}
          </div>
        ) : filterType === "all" ? (
          Object.entries(groupedChecks).map(([type, typeChecks]) => {
            const TypeIcon = checkTypeIcons[type as CheckType] || Terminal;
            const typeInfo = CHECK_TYPE_INFO.find((t) => t.type === type);
            return (
              <div key={type}>
                <div className="flex items-center gap-2 px-2 py-1 text-xs font-semibold text-muted-foreground/70 uppercase">
                  <TypeIcon className="w-3 h-3" />
                  {typeInfo?.name || type}
                  <span className="text-muted-foreground/50">({typeChecks.length})</span>
                </div>
                <div className="space-y-0.5">
                  {typeChecks.map((check) => (
                    <CheckListItem
                      key={check.id}
                      check={check}
                      isSelected={selectedCheckId === check.id}
                      isSelectionMode={isSelectionMode}
                      isChecked={selectedIds.has(check.id)}
                      onSelect={() => selectCheck(check.id)}
                      onDelete={() => handleDelete(check.id)}
                      onToggleSelection={() => toggleSelection(check.id)}
                    />
                  ))}
                </div>
              </div>
            );
          })
        ) : (
          <div className="space-y-0.5">
            {filteredChecks.map((check) => (
              <CheckListItem
                key={check.id}
                check={check}
                isSelected={selectedCheckId === check.id}
                isSelectionMode={isSelectionMode}
                isChecked={selectedIds.has(check.id)}
                onSelect={() => selectCheck(check.id)}
                onDelete={() => handleDelete(check.id)}
                onToggleSelection={() => toggleSelection(check.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="p-3 border-t border-border/50">
        <span className="text-xs text-muted-foreground">
          {checks.length} check{checks.length !== 1 ? "s" : ""} total
        </span>
      </div>

      {/* Batch Delete Dialog */}
      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete Checks"
        itemType="check"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedChecks}
      />
    </div>
  );
}

// Editor Panel (center) - Check configuration
interface CheckEditorPanelProps {
  showAiConfigureModal: boolean;
  onOpenAiModal: () => void;
  onCloseAiModal: () => void;
}

function CheckEditorPanel({
  showAiConfigureModal,
  onOpenAiModal,
  onCloseAiModal,
}: CheckEditorPanelProps) {
  const { selectedCheck, updateCheck, setDirty, executeCheck, lastExecutionResult, isSaving } =
    useCheckBuilder();

  const [command, setCommand] = useState("");
  const [workingDirectory, setWorkingDirectory] = useState("");
  const [configPath, setConfigPath] = useState("");
  const [autoFix, setAutoFix] = useState(false);
  const [isExecuting, setIsExecuting] = useState(false);

  // Reset form when selected check changes
  useEffect(() => {
    if (selectedCheck) {
      setCommand(selectedCheck.command || "");
      setWorkingDirectory(selectedCheck.working_directory || "");
      setConfigPath(selectedCheck.config_path || "");
      setAutoFix(selectedCheck.auto_fix);
    } else {
      setCommand("");
      setWorkingDirectory("");
      setConfigPath("");
      setAutoFix(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentionally sync only on ID change
  }, [selectedCheck?.id]);

  const handleCommandChange = (value: string) => {
    setCommand(value);
    setDirty(true);
  };

  const handleSave = async () => {
    if (!selectedCheck) return;
    await updateCheck(selectedCheck.id, {
      command,
      working_directory: workingDirectory || undefined,
      config_path: configPath || undefined,
      auto_fix: autoFix,
    });
  };

  const handleExecute = async () => {
    if (!selectedCheck) return;
    setIsExecuting(true);
    try {
      await executeCheck({
        ...selectedCheck,
        command,
        working_directory: workingDirectory || selectedCheck.working_directory,
        config_path: configPath || selectedCheck.config_path,
        auto_fix: autoFix,
      });
    } finally {
      setIsExecuting(false);
    }
  };

  if (!selectedCheck) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center text-muted-foreground bg-neutral-900/50">
        <Shield className="w-16 h-16 mb-4 opacity-20" />
        <p className="text-sm">Select a check to configure</p>
        <p className="text-xs text-muted-foreground/70 mt-2">
          or create a new one from the library
        </p>

        {/* AI Detection Button - also available when no check selected */}
        <div className="mt-6">
          <button
            className="px-4 py-3 bg-purple-900/30 hover:bg-purple-900/50 border border-purple-700/30 rounded-lg flex items-center justify-center gap-2 transition-colors"
            onClick={() => onOpenAiModal()}
            data-tutorial-id="detect-project-button-empty"
            title="Scan project and suggest appropriate checks"
          >
            <Sparkles className="w-4 h-4 text-purple-400" />
            <span className="text-sm text-purple-300">Detect Project & Suggest Checks</span>
          </button>
        </div>

        {/* AI Configure Checks Modal */}
        <AiConfigureChecksModal isOpen={showAiConfigureModal} onClose={() => onCloseAiModal()} />
      </div>
    );
  }

  const toolInfo = getToolInfo(selectedCheck.tool);

  return (
    <div
      className="flex-1 flex flex-col min-h-0 overflow-hidden"
      data-tutorial-id="check-editor-panel"
    >
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 bg-neutral-800 border-b border-neutral-700">
        <div className="flex items-center gap-3">
          <ToolIcon tool={selectedCheck.tool} />
          <div>
            <h3 className="text-sm font-medium">{selectedCheck.name}</h3>
            <p className="text-xs text-muted-foreground">
              {toolInfo?.name || selectedCheck.tool} - {toolInfo?.description}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleSave}
            disabled={isSaving}
            className="px-3 py-1.5 text-sm bg-neutral-700 hover:bg-neutral-600 rounded-md flex items-center gap-1.5 transition-colors disabled:opacity-50"
            data-tutorial-id="save-check-button"
            title="Save check configuration"
          >
            <Save className="w-3.5 h-3.5" />
            {isSaving ? "Saving..." : "Save"}
          </button>
          <button
            onClick={handleExecute}
            disabled={isExecuting}
            className="px-3 py-1.5 text-sm bg-cyan-600 hover:bg-cyan-700 text-white rounded-md flex items-center gap-1.5 transition-colors disabled:opacity-50"
            data-tutorial-id="run-check-button"
            title="Run this check now"
          >
            <Play className="w-3.5 h-3.5" />
            {isExecuting ? "Running..." : "Run Check"}
          </button>
        </div>
      </div>

      {/* Configuration Form */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {/* Command */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <Terminal className="w-3 h-3" />
            Command
          </label>
          <div className="flex gap-2">
            <input
              type="text"
              value={command}
              onChange={(e) => handleCommandChange(e.target.value)}
              placeholder={toolInfo?.default_command || "Enter command..."}
              className="flex-1 px-3 py-2 text-sm font-mono bg-neutral-800 border border-neutral-700 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              data-tutorial-id="check-command-input"
              data-ui-id="check-builder-command-input"
              title="The command to execute for this check"
            />
          </div>
          {toolInfo?.default_command && command !== toolInfo.default_command && (
            <button
              onClick={() => {
                setCommand(toolInfo.default_command);
                setDirty(true);
              }}
              className="text-xs text-cyan-500 hover:text-cyan-400"
            >
              Reset to default: {toolInfo.default_command}
            </button>
          )}
        </div>

        {/* Working Directory */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <FolderSearch className="w-3 h-3" />
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
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            data-ui-id="check-builder-working-directory-input"
          />
        </div>

        {/* Config Path */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <FileText className="w-3 h-3" />
            Config File
          </label>
          <input
            type="text"
            value={configPath}
            onChange={(e) => {
              setConfigPath(e.target.value);
              setDirty(true);
            }}
            placeholder={toolInfo?.config_files?.join(", ") || "Optional config file path"}
            className="w-full px-3 py-2 text-sm bg-neutral-800 border border-neutral-700 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            data-ui-id="check-builder-config-path-input"
          />
          {toolInfo?.config_files && toolInfo.config_files.length > 0 && (
            <p className="text-xs text-muted-foreground">
              Common: {toolInfo.config_files.join(", ")}
            </p>
          )}
        </div>

        {/* Auto-fix Toggle */}
        {toolInfo?.supports_auto_fix && (
          <div
            className="p-3 bg-emerald-900/20 border border-emerald-700/30 rounded-lg"
            data-tutorial-id="check-autofix-toggle"
          >
            <label className="flex items-center justify-between cursor-pointer">
              <span className="text-sm flex items-center gap-2">
                <Wrench className="w-4 h-4 text-emerald-500" />
                Auto-fix Mode
              </span>
              <input
                type="checkbox"
                checked={autoFix}
                onChange={(e) => {
                  setAutoFix(e.target.checked);
                  setDirty(true);
                }}
                className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-emerald-500/50"
                title="Automatically fix issues when possible"
                data-ui-id="check-builder-auto-fix-checkbox"
              />
            </label>
            <p className="text-xs text-emerald-400/80 mt-1.5">
              {autoFix
                ? "Will automatically fix issues when possible"
                : "Will only report issues without fixing"}
            </p>
          </div>
        )}

        {/* AI Detection Button */}
        <div className="pt-4 border-t border-neutral-700">
          <button
            className="w-full px-4 py-3 bg-purple-900/30 hover:bg-purple-900/50 border border-purple-700/30 rounded-lg flex items-center justify-center gap-2 transition-colors"
            onClick={() => onOpenAiModal()}
            data-tutorial-id="detect-project-button"
            title="Scan project and suggest appropriate checks"
          >
            <Sparkles className="w-4 h-4 text-purple-400" />
            <span className="text-sm text-purple-300">Detect Project & Suggest Checks</span>
          </button>
        </div>
      </div>

      {/* AI Configure Checks Modal */}
      <AiConfigureChecksModal isOpen={showAiConfigureModal} onClose={() => onCloseAiModal()} />

      {/* Execution Results */}
      {lastExecutionResult && (
        <div className="border-t border-neutral-700">
          <ExecutionResultPanel result={lastExecutionResult} />
        </div>
      )}
    </div>
  );
}

// Execution Result Panel
function ExecutionResultPanel({ result }: { result: CheckExecutionResult }) {
  const [expanded, setExpanded] = useState(true);
  const statusInfo = getStatusInfo(result.status);

  const StatusIcon = {
    pending: Clock,
    running: Clock,
    passed: CheckCircle,
    failed: XCircle,
    fixed: Wrench,
    error: AlertTriangle,
    timeout: Clock,
  }[result.status];

  const statusColorClass = {
    pending: "text-gray-400",
    running: "text-blue-400",
    passed: "text-green-400",
    failed: "text-red-400",
    fixed: "text-emerald-400",
    error: "text-red-400",
    timeout: "text-amber-400",
  }[result.status];

  return (
    <div className="bg-neutral-900">
      {/* Header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full px-4 py-2 flex items-center justify-between hover:bg-neutral-800/50"
      >
        <div className="flex items-center gap-3">
          <StatusIcon className={`w-4 h-4 ${statusColorClass}`} />
          <span className="text-sm font-medium">{statusInfo.label}</span>
          <span className="text-xs text-muted-foreground">{result.duration_ms}ms</span>
        </div>
        <div className="flex items-center gap-4 text-xs text-muted-foreground">
          {result.issues_found > 0 && (
            <span className="text-amber-400">{result.issues_found} issues</span>
          )}
          {result.issues_fixed > 0 && (
            <span className="text-emerald-400">{result.issues_fixed} fixed</span>
          )}
          {result.files_checked > 0 && <span>{result.files_checked} files</span>}
        </div>
      </button>

      {/* Output */}
      {expanded && result.output && (
        <div className="px-4 pb-4">
          <pre className="p-3 bg-black/30 rounded-md text-xs text-neutral-300 overflow-x-auto max-h-64 overflow-y-auto font-mono">
            {result.output}
          </pre>
          {result.error && (
            <div className="mt-2 p-2 bg-red-900/20 border border-red-700/30 rounded text-xs text-red-400">
              {result.error}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// Properties Panel (right)
function CheckPropertiesPanel() {
  const { selectedCheck, updateCheck, setDirty, isSaving } = useCheckBuilder();

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [timeoutSeconds, setTimeoutSeconds] = useState(60);
  const [isCritical, setIsCritical] = useState(false);
  const [failOnWarning, setFailOnWarning] = useState(false);
  const [enabled, setEnabled] = useState(true);
  const [tags, setTags] = useState<string[]>([]);
  const [tagInput, setTagInput] = useState("");

  // Sync with selected check
  useEffect(() => {
    if (selectedCheck) {
      setName(selectedCheck.name);
      setDescription(selectedCheck.description || "");
      setTimeoutSeconds(selectedCheck.timeout_seconds);
      setIsCritical(selectedCheck.is_critical);
      setFailOnWarning(selectedCheck.fail_on_warning);
      setEnabled(selectedCheck.enabled);
      setTags(selectedCheck.tags || []);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentionally sync only on ID change
  }, [selectedCheck?.id]);

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
    if (!selectedCheck) return;
    await updateCheck(selectedCheck.id, {
      name,
      description: description || undefined,
      timeout_seconds: timeoutSeconds,
      is_critical: isCritical,
      fail_on_warning: failOnWarning,
      enabled,
      tags,
    });
  };

  if (!selectedCheck) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground bg-card border-l border-border/50 w-72">
        <Settings className="w-12 h-12 mb-4 opacity-20" />
        <p className="text-sm">Select a check to edit properties</p>
      </div>
    );
  }

  return (
    <div
      className="h-full flex flex-col bg-card border-l border-border/50 w-72 flex-shrink-0"
      data-tutorial-id="check-properties-panel"
    >
      {/* Header */}
      <div className="p-4 border-b border-border/50 flex items-center justify-between">
        <h3 className="text-sm font-semibold flex items-center gap-2">
          <Settings className="w-4 h-4" />
          Properties
        </h3>
        <button
          className={`
            px-3 py-1.5 text-sm font-medium rounded-md flex items-center gap-1.5
            transition-colors bg-cyan-600 text-white hover:bg-cyan-700
            ${isSaving ? "opacity-50" : ""}
          `}
          onClick={handleSave}
          disabled={isSaving}
        >
          <Save className="w-3.5 h-3.5" />
          {isSaving ? "Saving..." : "Save"}
        </button>
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
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            title="A descriptive name for this check"
            placeholder="e.g., Lint Python Code"
            data-ui-id="check-builder-name-input"
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
            rows={3}
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50 resize-none"
            placeholder="What does this check verify?"
            title="Describe what this check validates and why it's important"
            data-ui-id="check-builder-description-input"
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
            max={600}
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
            title="Maximum time in seconds before the check is considered timed out. Type checks may need longer timeouts."
            data-ui-id="check-builder-timeout-input"
          />
        </div>

        {/* Enabled Toggle - always visible */}
        <div className="space-y-3">
          <label
            className="flex items-center justify-between cursor-pointer"
            title="Enable or disable this check without deleting it"
          >
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
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
              data-ui-id="check-builder-enabled-checkbox"
            />
          </label>
        </div>

        {/* Advanced Options - Collapsible */}
        <details className="space-y-3">
          <summary className="cursor-pointer text-sm font-medium text-muted-foreground hover:text-foreground flex items-center gap-2">
            <Settings className="w-3 h-3" />
            Advanced Options
          </summary>
          <div className="mt-3 pl-5 space-y-3 border-l-2 border-border/30">
            <label
              className="flex items-center justify-between cursor-pointer"
              title="Mark as critical to STOP the entire workflow if this check fails. Use sparingly - most checks should not be critical."
            >
              <span className="text-sm flex items-center gap-2">
                <AlertTriangle className="w-4 h-4 text-amber-500" />
                Critical Check
              </span>
              <input
                type="checkbox"
                checked={isCritical}
                onChange={(e) => {
                  setIsCritical(e.target.checked);
                  setDirty(true);
                }}
                className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
                data-ui-id="check-builder-critical-checkbox"
              />
            </label>
            <p className="text-xs text-amber-500/80">
              ⚠️ Critical check failure will STOP the entire workflow immediately. Other checks will
              be skipped and the AI will not attempt to fix issues. Only enable for checks that must
              pass before any work can proceed.
            </p>

            <label
              className="flex items-center justify-between cursor-pointer"
              title="Treat warnings as failures (stricter checking)"
            >
              <span className="text-sm flex items-center gap-2">
                <AlertTriangle className="w-4 h-4 text-amber-500" />
                Fail on Warning
              </span>
              <input
                type="checkbox"
                checked={failOnWarning}
                onChange={(e) => {
                  setFailOnWarning(e.target.checked);
                  setDirty(true);
                }}
                className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-cyan-500/50"
                data-ui-id="check-builder-fail-on-warning-checkbox"
              />
            </label>
            <p className="text-xs text-muted-foreground">
              Treat warnings from this check as failures (stricter checking)
            </p>
          </div>
        </details>

        {/* Tags */}
        <div className="space-y-1.5">
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
              className="flex-1 px-3 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
              placeholder="Add tag..."
              data-ui-id="check-builder-tag-input"
            />
            <button
              onClick={handleAddTag}
              className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md"
              data-ui-id="check-builder-add-tag-btn"
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
                    x
                  </button>
                </span>
              ))}
            </div>
          )}
        </div>

        {/* Metadata */}
        <div className="pt-4 border-t border-border/50 space-y-2 text-xs text-muted-foreground">
          <div>
            <span className="font-medium">Created:</span>{" "}
            {new Date(selectedCheck.created_at).toLocaleString()}
          </div>
          <div>
            <span className="font-medium">Updated:</span>{" "}
            {new Date(selectedCheck.updated_at).toLocaleString()}
          </div>
          {selectedCheck.ai_generated && (
            <div className="flex items-center gap-1 text-purple-400">
              <Sparkles className="w-3 h-3" />
              AI Generated
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// Tab Switcher Component
function TabSwitcher({
  activeTab,
  onTabChange,
}: {
  activeTab: TabView;
  onTabChange: (tab: TabView) => void;
}) {
  return (
    <div className="flex items-center border-b border-border/50 bg-card px-4">
      <button
        onClick={() => onTabChange("checks")}
        className={`
          flex items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 transition-colors
          ${
            activeTab === "checks"
              ? "border-cyan-500 text-cyan-400"
              : "border-transparent text-muted-foreground hover:text-foreground"
          }
        `}
        data-ui-id="check-builder-checks-tab-btn"
      >
        <Shield className="w-4 h-4" />
        Checks
      </button>
      <button
        onClick={() => onTabChange("groups")}
        className={`
          flex items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 transition-colors
          ${
            activeTab === "groups"
              ? "border-cyan-500 text-cyan-400"
              : "border-transparent text-muted-foreground hover:text-foreground"
          }
        `}
        data-ui-id="check-builder-groups-tab-btn"
      >
        <FolderOpen className="w-4 h-4" />
        Groups
      </button>
    </div>
  );
}

// Main content component
function CheckBuilderContent({ onLog: _onLog }: CheckBuilderTabProps) {
  const [activeTab, setActiveTab] = useState<TabView>("checks");
  const [showAiConfigureModal, setShowAiConfigureModal] = useState(false);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Tab Switcher */}
      <TabSwitcher activeTab={activeTab} onTabChange={setActiveTab} />

      {/* Content based on active tab */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {activeTab === "checks" ? (
          <>
            {/* Left: Check Library */}
            <CheckLibraryPanel onOpenAiModal={() => setShowAiConfigureModal(true)} />

            {/* Center: Check Editor */}
            <CheckEditorPanel
              showAiConfigureModal={showAiConfigureModal}
              onOpenAiModal={() => setShowAiConfigureModal(true)}
              onCloseAiModal={() => setShowAiConfigureModal(false)}
            />

            {/* Right: Properties */}
            <CheckPropertiesPanel />
          </>
        ) : (
          <CheckGroupsPanel />
        )}
      </div>
    </div>
  );
}

// Exported component with provider
export function CheckBuilderTab({ onLog }: CheckBuilderTabProps) {
  return (
    <CheckBuilderProvider onLog={onLog}>
      <CheckBuilderContent onLog={onLog} />
    </CheckBuilderProvider>
  );
}
