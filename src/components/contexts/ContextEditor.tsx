/**
 * ContextEditor.tsx
 *
 * Dialog component for creating and editing contexts.
 * Includes fields for name, category, tags, content, and auto-include rules.
 */

import { useState, useEffect, useMemo } from "react";
import {
  X,
  Save,
  Loader2,
  BookOpen,
  Tag,
  FolderOpen,
  Zap,
  FileText,
  Plus,
  AlertTriangle,
  Eye,
  HelpCircle,
} from "lucide-react";
import { getAccentColors, getStatusColors } from "@/design-system";
import type {
  ContextWithMetadata,
  ContextAutoInclude,
  CreateContextRequest,
  UpdateContextRequest,
} from "../../types/context";

interface ContextEditorProps {
  isOpen: boolean;
  onClose: () => void;
  context: ContextWithMetadata | null;
  mode: "create" | "edit" | "view";
  createScope?: "project" | "user";
  categories: string[];
  tags: string[];
  onSave: (data: CreateContextRequest | UpdateContextRequest) => Promise<boolean>;
  isSaving?: boolean;
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
  "EXTRACT",
  "NAVIGATE",
  "RUN_SCRIPT",
];

export function ContextEditor({
  isOpen,
  onClose,
  context,
  mode,
  createScope = "user",
  categories,
  tags: existingTags,
  onSave,
  isSaving = false,
}: ContextEditorProps) {
  // Form state
  const [name, setName] = useState("");
  const [category, setCategory] = useState("");
  const [newCategory, setNewCategory] = useState("");
  const [tagInput, setTagInput] = useState("");
  const [selectedTags, setSelectedTags] = useState<string[]>([]);
  const [content, setContent] = useState("");

  // Auto-include state
  const [taskMentions, setTaskMentions] = useState<string[]>([]);
  const [taskMentionInput, setTaskMentionInput] = useState("");
  const [actionTypes, setActionTypes] = useState<string[]>([]);
  const [errorPatterns, setErrorPatterns] = useState<string[]>([]);
  const [errorPatternInput, setErrorPatternInput] = useState("");
  const [filePatterns, setFilePatterns] = useState<string[]>([]);
  const [filePatternInput, setFilePatternInput] = useState("");

  // UI state
  const [showPreview, setShowPreview] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});

  // Initialize form when context changes
  useEffect(() => {
    if (context) {
      setName(context.name);
      setCategory(context.category || "");
      setSelectedTags(context.tags || []);
      setContent(context.content);
      setTaskMentions(context.autoInclude?.taskMentions || []);
      setActionTypes(context.autoInclude?.actionTypes || []);
      setErrorPatterns(context.autoInclude?.errorPatterns || []);
      setFilePatterns(context.autoInclude?.filePatterns || []);
    } else {
      resetForm();
    }
  }, [context]);

  const resetForm = () => {
    setName("");
    setCategory("");
    setNewCategory("");
    setTagInput("");
    setSelectedTags([]);
    setContent("");
    setTaskMentions([]);
    setTaskMentionInput("");
    setActionTypes([]);
    setErrorPatterns([]);
    setErrorPatternInput("");
    setFilePatterns([]);
    setFilePatternInput("");
    setErrors({});
  };

  const validate = (): boolean => {
    const newErrors: Record<string, string> = {};

    if (!name.trim()) {
      newErrors.name = "Name is required";
    }

    if (!content.trim()) {
      newErrors.content = "Content is required";
    }

    // Validate regex patterns
    for (const pattern of errorPatterns) {
      try {
        new RegExp(pattern);
      } catch {
        newErrors.errorPatterns = `Invalid regex pattern: ${pattern}`;
        break;
      }
    }

    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };

  const handleSave = async () => {
    if (!validate()) return;

    const autoInclude: ContextAutoInclude = {
      taskMentions: taskMentions.length > 0 ? taskMentions : null,
      actionTypes: actionTypes.length > 0 ? actionTypes : null,
      errorPatterns: errorPatterns.length > 0 ? errorPatterns : null,
      filePatterns: filePatterns.length > 0 ? filePatterns : null,
    };

    const hasAutoInclude =
      autoInclude.taskMentions ||
      autoInclude.actionTypes ||
      autoInclude.errorPatterns ||
      autoInclude.filePatterns;

    const data: CreateContextRequest | UpdateContextRequest = {
      name: name.trim(),
      content: content.trim(),
      category: (newCategory || category).trim() || null,
      tags: selectedTags,
      autoInclude: hasAutoInclude ? autoInclude : null,
    };

    const success = await onSave(data);
    if (success) {
      onClose();
    }
  };

  const addTag = (tag: string) => {
    const trimmed = tag.trim();
    if (trimmed && !selectedTags.includes(trimmed)) {
      setSelectedTags([...selectedTags, trimmed]);
    }
    setTagInput("");
  };

  const removeTag = (tag: string) => {
    setSelectedTags(selectedTags.filter((t) => t !== tag));
  };

  const addTaskMention = () => {
    const trimmed = taskMentionInput.trim().toLowerCase();
    if (trimmed && !taskMentions.includes(trimmed)) {
      setTaskMentions([...taskMentions, trimmed]);
    }
    setTaskMentionInput("");
  };

  const addErrorPattern = () => {
    const trimmed = errorPatternInput.trim();
    if (trimmed && !errorPatterns.includes(trimmed)) {
      // Validate regex
      try {
        new RegExp(trimmed);
        setErrorPatterns([...errorPatterns, trimmed]);
        setErrorPatternInput("");
        setErrors((prev) => ({ ...prev, errorPatterns: "" }));
      } catch {
        setErrors((prev) => ({ ...prev, errorPatterns: "Invalid regex pattern" }));
      }
    }
  };

  const addFilePattern = () => {
    const trimmed = filePatternInput.trim();
    if (trimmed && !filePatterns.includes(trimmed)) {
      setFilePatterns([...filePatterns, trimmed]);
    }
    setFilePatternInput("");
  };

  const isReadOnly = mode === "view";
  const title =
    mode === "create" ? "Create New Context" : mode === "edit" ? "Edit Context" : "View Context";

  // Suggestions for tags (from existing tags that aren't already selected)
  const tagSuggestions = useMemo(
    () => existingTags.filter((t) => !selectedTags.includes(t)),
    [existingTags, selectedTags],
  );

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-3xl mx-4 max-h-[90vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <div className="flex items-center gap-2">
            <BookOpen className="w-5 h-5 text-primary" />
            <h3 className="text-lg font-semibold">{title}</h3>
            {mode === "create" && (
              <span
                className={`px-2 py-0.5 text-xs rounded ${
                  createScope === "project"
                    ? `${getAccentColors("blue").bg} ${getAccentColors("blue").text}`
                    : `${getAccentColors("purple").bg} ${getAccentColors("purple").text}`
                }`}
                title={
                  createScope === "project"
                    ? "Will be saved with the loaded config file"
                    : "Will be saved locally, available across all projects"
                }
              >
                {createScope === "project" ? "Project Scope" : "User Scope"}
              </span>
            )}
          </div>
          <button
            onClick={onClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content - Scrollable */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {/* Name */}
          <div>
            <label className="block text-sm font-medium mb-1">
              Name <span className={getStatusColors("error").text}>*</span>
            </label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              disabled={isReadOnly}
              placeholder="Context name"
              className={`w-full px-3 py-2 bg-background border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary ${
                errors.name ? getStatusColors("error").border : "border-border"
              }`}
            />
            {errors.name && (
              <p className={`text-xs ${getStatusColors("error").text} mt-1`}>{errors.name}</p>
            )}
          </div>

          {/* Category */}
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium mb-1 flex items-center gap-1">
                <FolderOpen className="w-4 h-4" />
                Category
              </label>
              <select
                value={category}
                onChange={(e) => {
                  setCategory(e.target.value);
                  if (e.target.value) setNewCategory("");
                }}
                disabled={isReadOnly}
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              >
                <option value="">Select or create new</option>
                {categories.map((cat) => (
                  <option key={cat} value={cat}>
                    {cat}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="block text-sm font-medium mb-1">Or create new category</label>
              <input
                type="text"
                value={newCategory}
                onChange={(e) => {
                  setNewCategory(e.target.value);
                  if (e.target.value) setCategory("");
                }}
                disabled={isReadOnly}
                placeholder="New category name"
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              />
            </div>
          </div>

          {/* Tags */}
          <div>
            <label className="block text-sm font-medium mb-1 flex items-center gap-1">
              <Tag className="w-4 h-4" />
              Tags
            </label>
            <div className="flex flex-wrap gap-1 mb-2">
              {selectedTags.map((tag) => (
                <span
                  key={tag}
                  className="flex items-center gap-1 px-2 py-0.5 text-xs bg-primary/20 text-primary rounded"
                >
                  {tag}
                  {!isReadOnly && (
                    <button
                      onClick={() => removeTag(tag)}
                      className={`hover:${getStatusColors("error").text}`}
                    >
                      <X className="w-3 h-3" />
                    </button>
                  )}
                </span>
              ))}
            </div>
            {!isReadOnly && (
              <div className="flex gap-2">
                <input
                  type="text"
                  value={tagInput}
                  onChange={(e) => setTagInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      addTag(tagInput);
                    }
                  }}
                  placeholder="Add tag and press Enter"
                  list="tag-suggestions"
                  className="flex-1 px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                />
                <datalist id="tag-suggestions">
                  {tagSuggestions.map((tag) => (
                    <option key={tag} value={tag} />
                  ))}
                </datalist>
                <button
                  onClick={() => addTag(tagInput)}
                  disabled={!tagInput.trim()}
                  className="px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm hover:bg-primary/90 disabled:opacity-50"
                >
                  <Plus className="w-4 h-4" />
                </button>
              </div>
            )}
          </div>

          {/* Content */}
          <div>
            <div className="flex items-center justify-between mb-1">
              <label className="text-sm font-medium flex items-center gap-1">
                <FileText className="w-4 h-4" />
                Content <span className={getStatusColors("error").text}>*</span>
              </label>
              <button
                onClick={() => setShowPreview(!showPreview)}
                className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
              >
                <Eye className="w-3 h-3" />
                {showPreview ? "Hide Preview" : "Show Preview"}
              </button>
            </div>
            {showPreview ? (
              <div className="w-full min-h-[200px] max-h-[300px] overflow-y-auto px-3 py-2 bg-muted/30 border border-border rounded-md text-sm whitespace-pre-wrap font-mono">
                {content || <span className="text-muted-foreground">No content</span>}
              </div>
            ) : (
              <textarea
                value={content}
                onChange={(e) => setContent(e.target.value)}
                disabled={isReadOnly}
                placeholder="Enter markdown content that will be injected into AI prompts..."
                rows={10}
                className={`w-full px-3 py-2 bg-background border rounded-md text-sm font-mono resize-y focus:outline-none focus:ring-2 focus:ring-primary ${
                  errors.content ? getStatusColors("error").border : "border-border"
                }`}
              />
            )}
            {errors.content && (
              <p className={`text-xs ${getStatusColors("error").text} mt-1`}>{errors.content}</p>
            )}
          </div>

          {/* Auto-Include Rules */}
          <div className="border border-border rounded-lg p-4 space-y-4">
            <div className="flex items-center justify-between">
              <h4 className="text-sm font-medium flex items-center gap-2">
                <Zap className={`w-4 h-4 ${getAccentColors("amber").text}`} />
                Auto-Include Rules
              </h4>
              <button
                className="text-xs text-muted-foreground hover:text-foreground flex items-center gap-1"
                title="When any of these rules match, this context is automatically included in AI tasks"
              >
                <HelpCircle className="w-3 h-3" />
                How it works
              </button>
            </div>

            <p className="text-xs text-muted-foreground">
              When any of these rules match, this context will be automatically included in AI
              tasks.
            </p>

            {/* Task Mentions */}
            <div>
              <label className="block text-xs font-medium mb-1 text-muted-foreground">
                Task Mentions (keywords in task prompt)
              </label>
              <div className="flex flex-wrap gap-1 mb-2">
                {taskMentions.map((mention) => (
                  <span
                    key={mention}
                    className={`flex items-center gap-1 px-2 py-0.5 text-xs ${getAccentColors("blue").bg} ${getAccentColors("blue").text} rounded`}
                  >
                    {mention}
                    {!isReadOnly && (
                      <button
                        onClick={() => setTaskMentions(taskMentions.filter((m) => m !== mention))}
                        className={`hover:${getStatusColors("error").text}`}
                      >
                        <X className="w-3 h-3" />
                      </button>
                    )}
                  </span>
                ))}
              </div>
              {!isReadOnly && (
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={taskMentionInput}
                    onChange={(e) => setTaskMentionInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        addTaskMention();
                      }
                    }}
                    placeholder="e.g., debug, typescript, api"
                    className="flex-1 px-2 py-1 bg-background border border-border rounded text-xs focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                  <button
                    onClick={addTaskMention}
                    disabled={!taskMentionInput.trim()}
                    className="px-2 py-1 bg-muted text-foreground rounded text-xs hover:bg-muted/80 disabled:opacity-50"
                  >
                    Add
                  </button>
                </div>
              )}
            </div>

            {/* Action Types */}
            <div>
              <label className="block text-xs font-medium mb-1 text-muted-foreground">
                Action Types (in loaded configuration)
              </label>
              <div className="flex flex-wrap gap-1 mb-2">
                {KNOWN_ACTION_TYPES.map((type) => (
                  <button
                    key={type}
                    onClick={() => {
                      if (actionTypes.includes(type)) {
                        setActionTypes(actionTypes.filter((t) => t !== type));
                      } else {
                        setActionTypes([...actionTypes, type]);
                      }
                    }}
                    disabled={isReadOnly}
                    className={`px-2 py-0.5 text-xs rounded border transition-colors ${
                      actionTypes.includes(type)
                        ? `${getStatusColors("success").bg} ${getStatusColors("success").text} ${getStatusColors("success").border}`
                        : "bg-muted text-muted-foreground border-border hover:bg-muted/80"
                    }`}
                  >
                    {type}
                  </button>
                ))}
              </div>
            </div>

            {/* Error Patterns */}
            <div>
              <label className="block text-xs font-medium mb-1 text-muted-foreground">
                Error Patterns (regex patterns to match in logs)
              </label>
              <div className="flex flex-wrap gap-1 mb-2">
                {errorPatterns.map((pattern) => (
                  <span
                    key={pattern}
                    className={`flex items-center gap-1 px-2 py-0.5 text-xs ${getAccentColors("red").bg} ${getAccentColors("red").text} rounded font-mono`}
                  >
                    {pattern}
                    {!isReadOnly && (
                      <button
                        onClick={() => setErrorPatterns(errorPatterns.filter((p) => p !== pattern))}
                        className={`hover:${getStatusColors("error").text}`}
                      >
                        <X className="w-3 h-3" />
                      </button>
                    )}
                  </span>
                ))}
              </div>
              {!isReadOnly && (
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={errorPatternInput}
                    onChange={(e) => setErrorPatternInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        addErrorPattern();
                      }
                    }}
                    placeholder="e.g., TypeError|ReferenceError"
                    className={`flex-1 px-2 py-1 bg-background border rounded text-xs font-mono focus:outline-none focus:ring-2 focus:ring-primary ${
                      errors.errorPatterns ? getStatusColors("error").border : "border-border"
                    }`}
                  />
                  <button
                    onClick={addErrorPattern}
                    disabled={!errorPatternInput.trim()}
                    className="px-2 py-1 bg-muted text-foreground rounded text-xs hover:bg-muted/80 disabled:opacity-50"
                  >
                    Add
                  </button>
                </div>
              )}
              {errors.errorPatterns && (
                <p className={`text-xs ${getStatusColors("error").text} mt-1`}>
                  {errors.errorPatterns}
                </p>
              )}
            </div>

            {/* File Patterns */}
            <div>
              <label className="block text-xs font-medium mb-1 text-muted-foreground">
                File Patterns (glob patterns for files being worked on)
              </label>
              <div className="flex flex-wrap gap-1 mb-2">
                {filePatterns.map((pattern) => (
                  <span
                    key={pattern}
                    className={`flex items-center gap-1 px-2 py-0.5 text-xs ${getAccentColors("purple").bg} ${getAccentColors("purple").text} rounded font-mono`}
                  >
                    {pattern}
                    {!isReadOnly && (
                      <button
                        onClick={() => setFilePatterns(filePatterns.filter((p) => p !== pattern))}
                        className={`hover:${getStatusColors("error").text}`}
                      >
                        <X className="w-3 h-3" />
                      </button>
                    )}
                  </span>
                ))}
              </div>
              {!isReadOnly && (
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={filePatternInput}
                    onChange={(e) => setFilePatternInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        addFilePattern();
                      }
                    }}
                    placeholder="e.g., *.rs, src/api/**, **/*.test.ts"
                    className="flex-1 px-2 py-1 bg-background border border-border rounded text-xs font-mono focus:outline-none focus:ring-2 focus:ring-primary"
                  />
                  <button
                    onClick={addFilePattern}
                    disabled={!filePatternInput.trim()}
                    className="px-2 py-1 bg-muted text-foreground rounded text-xs hover:bg-muted/80 disabled:opacity-50"
                  >
                    Add
                  </button>
                </div>
              )}
            </div>
          </div>

          {/* Built-in Warning */}
          {context?.scope === "builtin" && (
            <div
              className={`flex items-center gap-2 text-sm ${getAccentColors("amber").text} ${getAccentColors("amber").bg} p-3 rounded-lg`}
            >
              <AlertTriangle className="w-5 h-5 flex-shrink-0" />
              <span>
                Built-in contexts are read-only. Use the duplicate feature to create an editable
                copy.
              </span>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 p-4 border-t border-border">
          <button
            onClick={onClose}
            className="px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            {isReadOnly ? "Close" : "Cancel"}
          </button>
          {!isReadOnly && (
            <button
              onClick={handleSave}
              disabled={isSaving}
              className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md font-medium hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {isSaving ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Saving...
                </>
              ) : (
                <>
                  <Save className="w-4 h-4" />
                  {mode === "create" ? "Create Context" : "Save Changes"}
                </>
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
