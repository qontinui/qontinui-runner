/**
 * ContextBuilderTab.tsx
 *
 * Builder for creating and editing AI contexts (knowledge base entries).
 * Contexts provide domain-specific guidance to AI tasks.
 */

import { useState, useEffect, useCallback } from "react";
import {
  BookOpen,
  Plus,
  Save,
  Trash2,
  Loader2,
  Search,
  Tag,
  FolderOpen,
  Copy,
  Zap,
  FileText,
  X,
  AlertTriangle,
  Check,
} from "lucide-react";
import type {
  ContextWithMetadata,
  ContextAutoInclude,
  CreateContextRequest,
  UpdateContextRequest,
} from "../types/context";
import { useContexts } from "./contexts/hooks/useContexts";
import { getAccentColors } from "@/design-system";
import { BuilderToolbar, toolbarActions } from "./ui/BuilderToolbar";
import { AiContextGenerator } from "./contexts/AiContextGenerator";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";

interface ContextBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editContextId?: string | null;
  _onNavigateToLibrary?: () => void;
}

const KNOWN_ACTION_TYPES = [
  "CLICK",
  "DOUBLE_CLICK",
  "RIGHT_CLICK",
  "TYPE",
  "KEY",
  "SCROLL",
  "WAIT",
  "FIND",
  "ASSERT",
  "SCREENSHOT",
];

export function ContextBuilderTab({
  onLog,
  editContextId,
  _onNavigateToLibrary,
}: ContextBuilderTabProps) {
  const {
    contexts,
    categories: _categories,
    tags: _existingTags,
    loading,
    error: _error,
    createContext,
    updateContext,
    deleteContext: deleteContextFn,
    duplicateContext,
    refresh: _refresh,
  } = useContexts();

  // State
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedContext, setSelectedContext] = useState<ContextWithMetadata | null>(null);
  const [isCreating, setIsCreating] = useState(false);
  const [saving, setSaving] = useState(false);
  const [showAiGenerator, setShowAiGenerator] = useState(false);

  // Selection mode state (for batch delete)
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  // Form state
  const [formName, setFormName] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState<string[]>([]);
  const [tagInput, setTagInput] = useState("");

  // Auto-include rules
  const [taskMentions, setTaskMentions] = useState<string[]>([]);
  const [taskMentionInput, setTaskMentionInput] = useState("");
  const [actionTypes, setActionTypes] = useState<string[]>([]);
  const [errorPatterns, setErrorPatterns] = useState<string[]>([]);
  const [errorPatternInput, setErrorPatternInput] = useState("");

  const accentColors = getAccentColors("blue");

  // Load context for editing
  useEffect(() => {
    if (editContextId && contexts.length > 0) {
      const context = contexts.find((c) => c.id === editContextId);
      if (context) {
        selectContext(context);
      }
    }
  }, [editContextId, contexts]);

  // Select a context for editing
  const selectContext = (context: ContextWithMetadata) => {
    setSelectedContext(context);
    setIsCreating(false);
    setFormName(context.name);
    setFormContent(context.content);
    setFormCategory(context.category || "");
    setFormTags(context.tags || []);
    setTaskMentions(context.autoInclude?.taskMentions || []);
    setActionTypes(context.autoInclude?.actionTypes || []);
    setErrorPatterns(context.autoInclude?.errorPatterns || []);
  };

  // Start creating a new context
  const startCreate = () => {
    setSelectedContext(null);
    setIsCreating(true);
    setFormName("");
    setFormContent("");
    setFormCategory("");
    setFormTags([]);
    setTaskMentions([]);
    setActionTypes([]);
    setErrorPatterns([]);
  };

  // Build auto-include object
  const buildAutoInclude = (): ContextAutoInclude | null => {
    const hasRules = taskMentions.length > 0 || actionTypes.length > 0 || errorPatterns.length > 0;
    if (!hasRules) return null;

    return {
      taskMentions: taskMentions.length > 0 ? taskMentions : null,
      actionTypes: actionTypes.length > 0 ? actionTypes : null,
      errorPatterns: errorPatterns.length > 0 ? errorPatterns : null,
      filePatterns: null,
    };
  };

  // Save context
  const handleSave = async () => {
    if (!formName.trim() || !formContent.trim()) {
      onLog?.("warning", "Name and content are required");
      return;
    }

    setSaving(true);
    try {
      const payload: CreateContextRequest | UpdateContextRequest = {
        name: formName.trim(),
        content: formContent.trim(),
        category: formCategory.trim() || null,
        tags: formTags,
        autoInclude: buildAutoInclude(),
      };

      let result;
      if (selectedContext) {
        // Update existing
        result = await updateContext(selectedContext.scope, selectedContext.id, payload);
      } else {
        // Create new (default to user scope)
        result = await createContext("user", payload as CreateContextRequest);
      }

      if (result) {
        onLog?.("success", `Context "${formName}" saved successfully`);
        selectContext(result);
      } else {
        onLog?.("error", "Failed to save context");
      }
    } catch (error) {
      onLog?.("error", `Failed to save context: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete context
  const handleDelete = async () => {
    if (!selectedContext) return;

    if (!confirm(`Delete context "${selectedContext.name}"?`)) return;

    try {
      const success = await deleteContextFn(selectedContext.scope, selectedContext.id);
      if (success) {
        onLog?.("success", `Context "${selectedContext.name}" deleted`);
        setSelectedContext(null);
        setIsCreating(false);
      } else {
        onLog?.("error", "Failed to delete context");
      }
    } catch (error) {
      onLog?.("error", `Failed to delete context: ${error}`);
    }
  };

  // Duplicate context
  const handleDuplicate = async () => {
    if (!selectedContext) return;

    try {
      const result = await duplicateContext(selectedContext.scope, selectedContext.id, "user");
      if (result) {
        onLog?.("success", `Context duplicated as "${result.name}"`);
        selectContext(result);
      }
    } catch (error) {
      onLog?.("error", `Failed to duplicate context: ${error}`);
    }
  };

  // Add tag
  const addTag = () => {
    const tag = tagInput.trim();
    if (tag && !formTags.includes(tag)) {
      setFormTags([...formTags, tag]);
      setTagInput("");
    }
  };

  // Remove tag
  const removeTag = (tag: string) => {
    setFormTags(formTags.filter((t) => t !== tag));
  };

  // Add task mention
  const addTaskMention = () => {
    const mention = taskMentionInput.trim();
    if (mention && !taskMentions.includes(mention)) {
      setTaskMentions([...taskMentions, mention]);
      setTaskMentionInput("");
    }
  };

  // Add error pattern
  const addErrorPattern = () => {
    const pattern = errorPatternInput.trim();
    if (pattern && !errorPatterns.includes(pattern)) {
      setErrorPatterns([...errorPatterns, pattern]);
      setErrorPatternInput("");
    }
  };

  // Toggle action type
  const toggleActionType = (type: string) => {
    if (actionTypes.includes(type)) {
      setActionTypes(actionTypes.filter((t) => t !== type));
    } else {
      setActionTypes([...actionTypes, type]);
    }
  };

  // Handle AI-generated context
  const handleAiContextGenerated = async (generated: {
    name: string;
    content: string;
    category?: string;
    tags: string[];
    taskMentions: string[];
  }) => {
    // Set up form with generated values
    setFormName(generated.name);
    setFormContent(generated.content);
    setFormCategory(generated.category || "");
    setFormTags(generated.tags);
    setTaskMentions(generated.taskMentions);
    setActionTypes([]);
    setErrorPatterns([]);

    // Switch to create mode
    setSelectedContext(null);
    setIsCreating(true);
    setShowAiGenerator(false);

    onLog?.("success", "Context generated with AI - review and save");
  };

  // Filter contexts
  const filteredContexts = contexts.filter((context) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      context.name.toLowerCase().includes(query) ||
      context.content.toLowerCase().includes(query) ||
      (context.category && context.category.toLowerCase().includes(query)) ||
      context.tags?.some((t) => t.toLowerCase().includes(query))
    );
  });

  // Toggle selection for batch delete
  const toggleSelection = useCallback((contextId: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(contextId)) {
        next.delete(contextId);
      } else {
        next.add(contextId);
      }
      return next;
    });
  }, []);

  // Exit selection mode
  const exitSelectionMode = useCallback(() => {
    setIsSelectionMode(false);
    setSelectedIds(new Set());
  }, []);

  // Delete selected contexts
  const deleteSelected = useCallback(async () => {
    if (selectedIds.size === 0) return;

    setIsDeleting(true);
    try {
      // Get contexts to delete (only non-builtin can be deleted)
      const toDelete = contexts.filter((c) => selectedIds.has(c.id) && c.scope !== "builtin");

      // Delete in parallel
      const deletePromises = toDelete.map((c) => deleteContextFn(c.scope, c.id));
      await Promise.all(deletePromises);

      // If currently selected context was deleted, clear it
      if (selectedContext && selectedIds.has(selectedContext.id)) {
        setSelectedContext(null);
        setIsCreating(false);
      }

      // Exit selection mode
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
      onLog?.("success", `Deleted ${toDelete.length} context(s)`);
    } catch (error) {
      onLog?.("error", `Failed to delete contexts: ${error}`);
    } finally {
      setIsDeleting(false);
    }
  }, [selectedIds, contexts, deleteContextFn, selectedContext, exitSelectionMode, onLog]);

  // Get names of selected contexts for the delete dialog
  const getSelectedNames = useCallback((): string[] => {
    return contexts.filter((c) => selectedIds.has(c.id)).map((c) => c.name);
  }, [contexts, selectedIds]);

  return (
    <div className="h-full flex">
      {/* Left Panel - Context List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-2">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <BookOpen className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Contexts
            </h2>
          </div>

          {/* Action buttons */}
          <BuilderToolbar
            className="mb-3"
            actions={[
              toolbarActions.ai(() => setShowAiGenerator(true)),
              toolbarActions.new(startCreate, "New"),
              toolbarActions.delete(() => setIsSelectionMode(!isSelectionMode), isSelectionMode),
            ]}
          />

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search contexts..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
            />
          </div>
        </div>

        {/* Selection Mode Header */}
        {isSelectionMode && (
          <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
            <span className="text-sm text-red-400">{selectedIds.size} selected</span>
            <div className="flex items-center gap-2">
              {selectedIds.size > 0 && (
                <button
                  onClick={() => setShowBatchDeleteDialog(true)}
                  className="flex items-center gap-1 px-3 py-1 text-sm font-medium bg-red-600 hover:bg-red-500 text-white rounded-md transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  Delete
                </button>
              )}
              <button
                onClick={exitSelectionMode}
                className="px-3 py-1 text-sm text-neutral-400 hover:text-neutral-200 transition-colors"
              >
                Cancel
              </button>
            </div>
          </div>
        )}

        {/* Context List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredContexts.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <BookOpen className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No contexts found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredContexts.map((context) => (
                <button
                  key={context.id}
                  onClick={() => {
                    if (isSelectionMode) {
                      toggleSelection(context.id);
                    } else {
                      selectContext(context);
                    }
                  }}
                  className={`w-full text-left p-3 rounded-lg transition-colors flex items-start gap-3 ${
                    isSelectionMode && selectedIds.has(context.id)
                      ? "bg-red-500/20 border border-red-500/50"
                      : selectedContext?.id === context.id
                        ? "bg-neutral-700"
                        : "hover:bg-neutral-800"
                  } ${isSelectionMode ? "border" : ""} ${
                    isSelectionMode && !selectedIds.has(context.id) ? "border-transparent" : ""
                  }`}
                >
                  {/* Checkbox in selection mode */}
                  {isSelectionMode && (
                    <div
                      className={`flex-shrink-0 w-5 h-5 mt-0.5 rounded border-2 flex items-center justify-center transition-colors ${
                        selectedIds.has(context.id)
                          ? "bg-red-500 border-red-500"
                          : "border-neutral-500"
                      }`}
                    >
                      {selectedIds.has(context.id) && <Check className="w-3 h-3 text-white" />}
                    </div>
                  )}
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm truncate flex-1">{context.name}</span>
                      <span
                        className={`text-xs px-1.5 py-0.5 rounded ${
                          context.scope === "builtin"
                            ? "bg-purple-900/50 text-purple-300"
                            : context.scope === "project"
                              ? "bg-blue-900/50 text-blue-300"
                              : "bg-neutral-800 text-neutral-400"
                        }`}
                      >
                        {context.scope}
                      </span>
                    </div>
                    <div className="text-xs text-neutral-400 truncate mt-0.5">
                      {context.content.slice(0, 60)}...
                    </div>
                    {context.category && (
                      <div className="mt-1.5">
                        <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-400">
                          {context.category}
                        </span>
                      </div>
                    )}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor or AI Generator */}
      <div className="flex-1 flex flex-col bg-neutral-900/50">
        {showAiGenerator ? (
          <div className="flex-1 p-4">
            <AiContextGenerator
              onContextGenerated={handleAiContextGenerated}
              onCancel={() => setShowAiGenerator(false)}
            />
          </div>
        ) : !selectedContext && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-neutral-400">
              <BookOpen className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No context selected</p>
              <p className="text-sm mb-4">Select a context to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
              >
                <Plus className="w-4 h-4" />
                Create New Context
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-neutral-700">
              <h3 className="font-medium">
                {isCreating ? "New Context" : `Editing: ${selectedContext?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedContext && selectedContext.scope !== "builtin" && (
                  <>
                    <button
                      onClick={handleDuplicate}
                      className="p-2 rounded-lg hover:bg-neutral-800 transition-colors"
                      title="Duplicate"
                    >
                      <Copy className="w-4 h-4" />
                    </button>
                    <button
                      onClick={handleDelete}
                      className="p-2 rounded-lg hover:bg-neutral-800 text-red-400 transition-colors"
                      title="Delete"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </>
                )}
                {selectedContext?.scope === "builtin" && (
                  <span className="text-xs text-neutral-400 bg-neutral-800 px-2 py-1 rounded">
                    Read-only (built-in)
                  </span>
                )}
                <button
                  onClick={handleSave}
                  disabled={
                    saving ||
                    !formName.trim() ||
                    !formContent.trim() ||
                    selectedContext?.scope === "builtin"
                  }
                  className="flex items-center gap-2 px-4 py-2 rounded-lg disabled:opacity-50 transition-colors"
                  style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
                >
                  {saving ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <Save className="w-4 h-4" />
                  )}
                  Save
                </button>
              </div>
            </div>

            {/* Form */}
            <div className="flex-1 overflow-y-auto p-4">
              <div className="max-w-3xl space-y-4">
                {/* Name */}
                <div>
                  <label className="block text-sm font-medium mb-1.5">Name *</label>
                  <input
                    type="text"
                    value={formName}
                    onChange={(e) => setFormName(e.target.value)}
                    placeholder="Context name"
                    disabled={selectedContext?.scope === "builtin"}
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 disabled:opacity-50"
                  />
                </div>

                {/* Content */}
                <div>
                  <label className="block text-sm font-medium mb-1.5">
                    <FileText className="w-4 h-4 inline mr-1" />
                    Content (Markdown) *
                  </label>
                  <textarea
                    value={formContent}
                    onChange={(e) => setFormContent(e.target.value)}
                    placeholder="Enter the context content in Markdown format..."
                    rows={12}
                    disabled={selectedContext?.scope === "builtin"}
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 font-mono text-sm resize-y disabled:opacity-50"
                  />
                  <p className="text-xs text-neutral-400 mt-1">
                    This content will be injected into AI task prompts for context
                  </p>
                </div>

                {/* Category & Tags */}
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1.5">
                      <FolderOpen className="w-4 h-4 inline mr-1" />
                      Category
                    </label>
                    <input
                      type="text"
                      value={formCategory}
                      onChange={(e) => setFormCategory(e.target.value)}
                      placeholder="e.g., architecture, debugging"
                      disabled={selectedContext?.scope === "builtin"}
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 disabled:opacity-50"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1.5">
                      <Tag className="w-4 h-4 inline mr-1" />
                      Tags
                    </label>
                    <div className="flex gap-2">
                      <input
                        type="text"
                        value={tagInput}
                        onChange={(e) => setTagInput(e.target.value)}
                        onKeyDown={(e) => e.key === "Enter" && (e.preventDefault(), addTag())}
                        placeholder="Add tag..."
                        disabled={selectedContext?.scope === "builtin"}
                        className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 disabled:opacity-50"
                      />
                      <button
                        onClick={addTag}
                        disabled={selectedContext?.scope === "builtin"}
                        className="px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg hover:bg-neutral-700 disabled:opacity-50"
                      >
                        <Plus className="w-4 h-4" />
                      </button>
                    </div>
                    {formTags.length > 0 && (
                      <div className="flex flex-wrap gap-1.5 mt-2">
                        {formTags.map((tag) => (
                          <span
                            key={tag}
                            className="inline-flex items-center gap-1 px-2 py-0.5 bg-neutral-800 text-xs rounded"
                          >
                            {tag}
                            {selectedContext?.scope !== "builtin" && (
                              <button onClick={() => removeTag(tag)}>
                                <X className="w-3 h-3" />
                              </button>
                            )}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                </div>

                {/* Auto-Include Rules */}
                <div className="border border-neutral-700 rounded-lg p-4 space-y-4">
                  <div className="flex items-center gap-2">
                    <Zap className="w-4 h-4" style={{ color: accentColors.bgSolid }} />
                    <h4 className="font-medium">Auto-Include Rules</h4>
                  </div>
                  <p className="text-xs text-neutral-400">
                    Context will be automatically included in AI tasks when any of these conditions
                    match
                  </p>

                  {/* Task Mentions */}
                  <div>
                    <label className="block text-sm mb-1.5">Task Prompt Keywords</label>
                    <div className="flex gap-2">
                      <input
                        type="text"
                        value={taskMentionInput}
                        onChange={(e) => setTaskMentionInput(e.target.value)}
                        onKeyDown={(e) =>
                          e.key === "Enter" && (e.preventDefault(), addTaskMention())
                        }
                        placeholder="e.g., login, authentication"
                        disabled={selectedContext?.scope === "builtin"}
                        className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm disabled:opacity-50"
                      />
                      <button
                        onClick={addTaskMention}
                        disabled={selectedContext?.scope === "builtin"}
                        className="px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg hover:bg-neutral-700 disabled:opacity-50"
                      >
                        <Plus className="w-4 h-4" />
                      </button>
                    </div>
                    {taskMentions.length > 0 && (
                      <div className="flex flex-wrap gap-1.5 mt-2">
                        {taskMentions.map((mention) => (
                          <span
                            key={mention}
                            className="inline-flex items-center gap-1 px-2 py-0.5 bg-blue-900/30 text-blue-300 text-xs rounded"
                          >
                            {mention}
                            {selectedContext?.scope !== "builtin" && (
                              <button
                                onClick={() =>
                                  setTaskMentions(taskMentions.filter((m) => m !== mention))
                                }
                              >
                                <X className="w-3 h-3" />
                              </button>
                            )}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>

                  {/* Action Types */}
                  <div>
                    <label className="block text-sm mb-1.5">Action Types in Config</label>
                    <div className="flex flex-wrap gap-2">
                      {KNOWN_ACTION_TYPES.map((type) => (
                        <button
                          key={type}
                          onClick={() => toggleActionType(type)}
                          disabled={selectedContext?.scope === "builtin"}
                          className={`px-2 py-1 text-xs rounded border transition-colors disabled:opacity-50 ${
                            actionTypes.includes(type)
                              ? "bg-green-900/30 border-green-700 text-green-300"
                              : "bg-neutral-800 border-neutral-700 text-neutral-400 hover:border-neutral-600"
                          }`}
                        >
                          {actionTypes.includes(type) && <Check className="w-3 h-3 inline mr-1" />}
                          {type}
                        </button>
                      ))}
                    </div>
                  </div>

                  {/* Error Patterns */}
                  <div>
                    <label className="block text-sm mb-1.5">
                      <AlertTriangle className="w-4 h-4 inline mr-1" />
                      Error Patterns (Regex)
                    </label>
                    <div className="flex gap-2">
                      <input
                        type="text"
                        value={errorPatternInput}
                        onChange={(e) => setErrorPatternInput(e.target.value)}
                        onKeyDown={(e) =>
                          e.key === "Enter" && (e.preventDefault(), addErrorPattern())
                        }
                        placeholder="e.g., timeout|connection refused"
                        disabled={selectedContext?.scope === "builtin"}
                        className="flex-1 px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 text-sm font-mono disabled:opacity-50"
                      />
                      <button
                        onClick={addErrorPattern}
                        disabled={selectedContext?.scope === "builtin"}
                        className="px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg hover:bg-neutral-700 disabled:opacity-50"
                      >
                        <Plus className="w-4 h-4" />
                      </button>
                    </div>
                    {errorPatterns.length > 0 && (
                      <div className="flex flex-wrap gap-1.5 mt-2">
                        {errorPatterns.map((pattern) => (
                          <span
                            key={pattern}
                            className="inline-flex items-center gap-1 px-2 py-0.5 bg-amber-900/30 text-amber-300 text-xs rounded font-mono"
                          >
                            {pattern}
                            {selectedContext?.scope !== "builtin" && (
                              <button
                                onClick={() =>
                                  setErrorPatterns(errorPatterns.filter((p) => p !== pattern))
                                }
                              >
                                <X className="w-3 h-3" />
                              </button>
                            )}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                </div>
              </div>
            </div>
          </>
        )}
      </div>

      {/* Batch Delete Dialog */}
      <BatchDeleteDialog
        open={showBatchDeleteDialog}
        title="Delete Contexts"
        itemType="context"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelected}
      />
    </div>
  );
}
