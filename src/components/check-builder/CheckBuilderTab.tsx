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
} from "lucide-react";
import { CheckBuilderProvider, useCheckBuilder } from "./CheckBuilderContext";
import { PageTutorialMenu } from "../tutorial";
import type { Check, CheckType, CheckTool, CheckExecutionResult, CreateCheckInput } from "./types";
import { CHECK_TOOLS, CHECK_TYPE_INFO, getToolInfo, getStatusInfo } from "./types";

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
  onSelect: () => void;
  onDelete: () => void;
}

function CheckListItem({ check, isSelected, onSelect, onDelete }: CheckListItemProps) {
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
      `}
      onClick={onSelect}
    >
      <TypeIcon className="w-4 h-4 flex-shrink-0 text-muted-foreground" />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate">{check.name}</div>
        <div className="text-xs text-muted-foreground truncate">
          {toolInfo?.name || check.tool}
          {check.auto_fix && <span className="ml-1 text-emerald-500">auto-fix</span>}
        </div>
      </div>
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
    </div>
  );
}

// Library Panel
function CheckLibraryPanel() {
  const { checks, selectedCheckId, isLoading, selectCheck, deleteCheck, createCheck } =
    useCheckBuilder();

  const [searchQuery, setSearchQuery] = useState("");
  const [filterType, setFilterType] = useState<CheckType | "all">("all");
  const [showNewMenu, setShowNewMenu] = useState(false);

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
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Shield className="w-4 h-4" />
            Checks
          </h2>
          <div className="flex items-center gap-1">
            <PageTutorialMenu page="check-builder" variant="compact" />
            <div className="relative">
              <button
                className="p-1.5 hover:bg-muted rounded-md transition-colors"
                onClick={() => setShowNewMenu(!showNewMenu)}
                data-tutorial-id="new-check-button"
                title="Create new check"
              >
                <Plus className="w-4 h-4" />
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
          />
        </div>

        {/* Filter */}
        <div className="mt-2">
          <select
            value={filterType}
            onChange={(e) => setFilterType(e.target.value as CheckType | "all")}
            className="w-full px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-cyan-500/50"
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
                      onSelect={() => selectCheck(check.id)}
                      onDelete={() => handleDelete(check.id)}
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
                onSelect={() => selectCheck(check.id)}
                onDelete={() => handleDelete(check.id)}
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
    </div>
  );
}

// Editor Panel (center) - Check configuration
function CheckEditorPanel() {
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
            onClick={() => {
              // TODO: Implement AI project detection
              console.log("AI project detection not yet implemented");
            }}
            data-tutorial-id="detect-project-button"
            title="Scan project and suggest appropriate checks"
          >
            <Sparkles className="w-4 h-4 text-purple-400" />
            <span className="text-sm text-purple-300">Detect Project & Suggest Checks</span>
          </button>
        </div>
      </div>

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
          />
        </div>

        {/* Toggles */}
        <div className="space-y-3">
          <label
            className="flex items-center justify-between cursor-pointer"
            title="Mark as critical to fail the entire workflow if this check fails"
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
            />
          </label>
          <p className="text-xs text-muted-foreground ml-6">
            Critical check failure will fail the entire workflow
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
            />
          </label>

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
            />
          </label>
        </div>

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
            />
            <button
              onClick={handleAddTag}
              className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md"
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

// Main content component
function CheckBuilderContent({ onLog }: CheckBuilderTabProps) {
  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Left: Check Library */}
        <CheckLibraryPanel />

        {/* Center: Check Editor */}
        <CheckEditorPanel />

        {/* Right: Properties */}
        <CheckPropertiesPanel />
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
