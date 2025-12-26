/**
 * ScriptletsTab - Manage reusable text snippets for script descriptions
 *
 * Scriptlets capture learnings from AI debugging sessions and can be inserted
 * into Playwright script descriptions to provide context and guidance.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Plus,
  Search,
  Trash2,
  Edit3,
  Save,
  X,
  ChevronDown,
  ChevronUp,
  Sparkles,
  Tag,
  FolderOpen,
  Copy,
  Check,
} from "lucide-react";
import type { Scriptlet, CreateScriptletRequest, UpdateScriptletRequest } from "../types";
import type { AiOutputLine } from "./AiOutputTab";
import { groupEntriesIntoLoops } from "../types/aiLoop";

const API_BASE = "http://localhost:9876";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface ScriptletsTabProps {
  onLog: (level: LogLevel, message: string) => void;
  aiOutputLines?: AiOutputLine[];
}

export function ScriptletsTab({ onLog, aiOutputLines = [] }: ScriptletsTabProps) {
  // Scriptlet library state
  const [scriptlets, setScriptlets] = useState<Scriptlet[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedCategory, setSelectedCategory] = useState<string | null>(null);
  const [categories, setCategories] = useState<string[]>([]);

  // Editing state
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Form state
  const [formName, setFormName] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");
  const [newCategoryInput, setNewCategoryInput] = useState("");
  const [showNewCategoryInput, setShowNewCategoryInput] = useState(false);

  // AI generation state
  const [showLogSelector, setShowLogSelector] = useState(false);
  const [selectedLogIds, setSelectedLogIds] = useState<Set<string>>(new Set());
  const [generationPrompt, setGenerationPrompt] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);

  // Copy feedback
  const [copiedId, setCopiedId] = useState<string | null>(null);

  // Load scriptlets on mount
  useEffect(() => {
    loadScriptlets();
    loadCategories();
  }, []);

  const loadScriptlets = async () => {
    try {
      setLoading(true);
      const response = await fetch(`${API_BASE}/scriptlets`);
      const result = await response.json();
      if (result.success) {
        setScriptlets(result.data || []);
      } else {
        onLog("error", `Failed to load scriptlets: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to load scriptlets: ${error}`);
    } finally {
      setLoading(false);
    }
  };

  const loadCategories = async () => {
    try {
      const response = await fetch(`${API_BASE}/scriptlets/categories`);
      const result = await response.json();
      if (result.success) {
        setCategories(result.data || []);
      }
    } catch (error) {
      console.error("Failed to load categories:", error);
    }
  };

  const createScriptlet = async () => {
    if (!formName.trim() || !formContent.trim()) {
      onLog("warning", "Name and content are required");
      return;
    }

    try {
      const request: CreateScriptletRequest = {
        name: formName.trim(),
        content: formContent.trim(),
        category: formCategory.trim() || "Uncategorized",
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
      };

      const response = await fetch(`${API_BASE}/scriptlets`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });
      const result = await response.json();

      if (result.success) {
        onLog("success", `Created scriptlet: ${formName}`);
        resetForm();
        setIsCreating(false);
        loadScriptlets();
        loadCategories();
      } else {
        onLog("error", `Failed to create scriptlet: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to create scriptlet: ${error}`);
    }
  };

  const updateScriptlet = async (id: string) => {
    try {
      const request: UpdateScriptletRequest = {
        name: formName.trim() || undefined,
        content: formContent.trim() || undefined,
        category: formCategory.trim() || undefined,
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
      };

      const response = await fetch(`${API_BASE}/scriptlets/${id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });
      const result = await response.json();

      if (result.success) {
        onLog("success", `Updated scriptlet: ${formName}`);
        setExpandedId(null);
        loadScriptlets();
        loadCategories();
      } else {
        onLog("error", `Failed to update scriptlet: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to update scriptlet: ${error}`);
    }
  };

  const deleteScriptlet = async (id: string, name: string) => {
    if (!confirm(`Delete scriptlet "${name}"?`)) return;

    try {
      const response = await fetch(`${API_BASE}/scriptlets/${id}`, {
        method: "DELETE",
      });
      const result = await response.json();

      if (result.success) {
        onLog("success", `Deleted scriptlet: ${name}`);
        loadScriptlets();
        loadCategories();
      } else {
        onLog("error", `Failed to delete scriptlet: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to delete scriptlet: ${error}`);
    }
  };

  const resetForm = () => {
    setFormName("");
    setFormContent("");
    setFormCategory("");
    setFormTags("");
    setNewCategoryInput("");
    setShowNewCategoryInput(false);
  };

  const startEditing = (scriptlet: Scriptlet) => {
    setFormName(scriptlet.name);
    setFormContent(scriptlet.content);
    setFormCategory(scriptlet.category);
    setFormTags(scriptlet.tags.join(", "));
    setExpandedId(scriptlet.id);
  };

  const copyToClipboard = async (content: string, id: string) => {
    try {
      await navigator.clipboard.writeText(content);
      setCopiedId(id);
      setTimeout(() => setCopiedId(null), 2000);
    } catch (error) {
      onLog("error", "Failed to copy to clipboard");
    }
  };

  // AI generation from logs
  const generateFromLogs = useCallback(async () => {
    if (selectedLogIds.size === 0) {
      onLog("warning", "Please select at least one AI log to analyze");
      return;
    }

    if (!generationPrompt.trim()) {
      onLog("warning", "Please enter a prompt describing what to extract");
      return;
    }

    setIsGenerating(true);

    try {
      // Get the AI loops
      const loops = groupEntriesIntoLoops(aiOutputLines);

      // Collect content from selected loops
      const selectedContent = loops
        .filter((loop) => selectedLogIds.has(loop.id))
        .map((loop) => {
          const loopContent = loop.entries.map((e) => `[${e.source}] ${e.line}`).join("\n");
          return `--- AI Session: ${loop.label} ---\n${loopContent}`;
        })
        .join("\n\n");

      const prompt = `You are analyzing AI debugging sessions to extract reusable learnings.

## User Request
${generationPrompt}

## AI Session Logs
${selectedContent}

## Task
Based on the logs above, create a concise, reusable description snippet that captures the key learnings.
This snippet will be inserted into Playwright test descriptions to help future tests avoid the same issues.

## Output Format
Provide ONLY the text content that should be saved as a scriptlet. Do not include any explanation or markdown formatting.
Keep it concise but comprehensive - typically 2-5 sentences.`;

      const response = await fetch(`${API_BASE}/trigger-ai-analysis`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          prompt,
          display_prompt: "Generating scriptlet from AI logs...",
          timeout_seconds: 60,
          wait_for_completion: true,
        }),
      });

      const result = await response.json();

      if (result.success && result.data?.output) {
        // Extract the content from the AI response
        let content = result.data.output.trim();

        // Remove any code blocks if present
        if (content.startsWith("```")) {
          content = content.replace(/^```\w*\n?/, "").replace(/\n?```$/, "");
        }

        setFormContent(content);
        setShowLogSelector(false);
        setSelectedLogIds(new Set());
        setGenerationPrompt("");
        onLog("success", "Generated scriptlet content from AI logs");
      } else {
        onLog("error", `Failed to generate: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog("error", `Failed to generate scriptlet: ${error}`);
    } finally {
      setIsGenerating(false);
    }
  }, [selectedLogIds, generationPrompt, aiOutputLines, onLog]);

  // Filter scriptlets
  const filteredScriptlets = scriptlets.filter((s) => {
    const matchesSearch =
      !searchQuery ||
      s.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.content.toLowerCase().includes(searchQuery.toLowerCase()) ||
      s.tags.some((t) => t.toLowerCase().includes(searchQuery.toLowerCase()));

    const matchesCategory = !selectedCategory || s.category === selectedCategory;

    return matchesSearch && matchesCategory;
  });

  // Get AI loops for the selector
  const aiLoops = groupEntriesIntoLoops(aiOutputLines).reverse(); // Most recent first

  if (loading) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-muted-foreground">Loading scriptlets...</div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col p-4">
      {/* Header */}
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-xl font-semibold">Scriptlets</h2>
        <button
          onClick={() => {
            resetForm();
            setIsCreating(true);
            setExpandedId(null);
          }}
          className="flex items-center gap-2 px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors"
        >
          <Plus className="w-4 h-4" />
          New Scriptlet
        </button>
      </div>

      {/* Search and Filter */}
      <div className="flex gap-3 mb-4">
        <div className="flex-1 relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search scriptlets..."
            className="w-full pl-10 pr-4 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
          />
        </div>
        <select
          value={selectedCategory || ""}
          onChange={(e) => setSelectedCategory(e.target.value || null)}
          className="px-4 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
        >
          <option value="">All Categories</option>
          {categories.map((cat) => (
            <option key={cat} value={cat}>
              {cat}
            </option>
          ))}
        </select>
      </div>

      {/* Create Form */}
      {isCreating && (
        <div className="card p-4 mb-4 border-2 border-green-500/30">
          <h3 className="text-lg font-medium mb-4">Create New Scriptlet</h3>
          <ScriptletForm
            formName={formName}
            setFormName={setFormName}
            formContent={formContent}
            setFormContent={setFormContent}
            formCategory={formCategory}
            setFormCategory={setFormCategory}
            formTags={formTags}
            setFormTags={setFormTags}
            categories={categories}
            onAddCategory={(cat) => {
              if (!categories.includes(cat)) {
                setCategories([...categories, cat]);
              }
            }}
            newCategoryInput={newCategoryInput}
            setNewCategoryInput={setNewCategoryInput}
            showNewCategoryInput={showNewCategoryInput}
            setShowNewCategoryInput={setShowNewCategoryInput}
            onShowLogSelector={() => setShowLogSelector(true)}
            onSave={createScriptlet}
            onCancel={() => {
              resetForm();
              setIsCreating(false);
            }}
            saveLabel="Create Scriptlet"
          />
        </div>
      )}

      {/* Scriptlet List */}
      <div className="flex-1 overflow-y-auto space-y-3">
        {filteredScriptlets.length === 0 ? (
          <div className="text-center text-muted-foreground py-8">
            {searchQuery || selectedCategory
              ? "No scriptlets match your search"
              : "No scriptlets yet. Create one to get started!"}
          </div>
        ) : (
          filteredScriptlets.map((scriptlet) => (
            <div key={scriptlet.id} className="card p-4">
              {/* Header */}
              <div className="flex items-start justify-between">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <h3 className="font-medium truncate">{scriptlet.name}</h3>
                    <span className="px-2 py-0.5 text-xs bg-muted rounded-full">
                      {scriptlet.category}
                    </span>
                  </div>
                  {scriptlet.tags.length > 0 && (
                    <div className="flex items-center gap-1 mt-1 flex-wrap">
                      <Tag className="w-3 h-3 text-muted-foreground" />
                      {scriptlet.tags.map((tag) => (
                        <span
                          key={tag}
                          className="text-xs text-muted-foreground px-1.5 py-0.5 bg-muted/50 rounded"
                        >
                          {tag}
                        </span>
                      ))}
                    </div>
                  )}
                  {/* Preview */}
                  {expandedId !== scriptlet.id && (
                    <p className="text-sm text-muted-foreground mt-2 line-clamp-2">
                      {scriptlet.content}
                    </p>
                  )}
                </div>

                {/* Actions */}
                <div className="flex items-center gap-1 ml-4">
                  <button
                    onClick={() => copyToClipboard(scriptlet.content, scriptlet.id)}
                    className="p-2 hover:bg-muted rounded-lg transition-colors"
                    title="Copy content"
                  >
                    {copiedId === scriptlet.id ? (
                      <Check className="w-4 h-4 text-green-500" />
                    ) : (
                      <Copy className="w-4 h-4 text-muted-foreground" />
                    )}
                  </button>
                  <button
                    onClick={() => {
                      if (expandedId === scriptlet.id) {
                        setExpandedId(null);
                      } else {
                        startEditing(scriptlet);
                      }
                    }}
                    className="p-2 hover:bg-muted rounded-lg transition-colors"
                    title={expandedId === scriptlet.id ? "Collapse" : "Edit"}
                  >
                    {expandedId === scriptlet.id ? (
                      <ChevronUp className="w-4 h-4" />
                    ) : (
                      <Edit3 className="w-4 h-4 text-muted-foreground" />
                    )}
                  </button>
                  <button
                    onClick={() => deleteScriptlet(scriptlet.id, scriptlet.name)}
                    className="p-2 hover:bg-red-500/10 rounded-lg transition-colors"
                    title="Delete"
                  >
                    <Trash2 className="w-4 h-4 text-red-500" />
                  </button>
                </div>
              </div>

              {/* Edit Form */}
              {expandedId === scriptlet.id && (
                <div className="mt-4 pt-4 border-t border-border">
                  <ScriptletForm
                    formName={formName}
                    setFormName={setFormName}
                    formContent={formContent}
                    setFormContent={setFormContent}
                    formCategory={formCategory}
                    setFormCategory={setFormCategory}
                    formTags={formTags}
                    setFormTags={setFormTags}
                    categories={categories}
                    onAddCategory={(cat) => {
                      if (!categories.includes(cat)) {
                        setCategories([...categories, cat]);
                      }
                    }}
                    newCategoryInput={newCategoryInput}
                    setNewCategoryInput={setNewCategoryInput}
                    showNewCategoryInput={showNewCategoryInput}
                    setShowNewCategoryInput={setShowNewCategoryInput}
                    onShowLogSelector={() => setShowLogSelector(true)}
                    onSave={() => updateScriptlet(scriptlet.id)}
                    onCancel={() => setExpandedId(null)}
                    saveLabel="Save Changes"
                  />
                </div>
              )}
            </div>
          ))
        )}
      </div>

      {/* AI Log Selector Modal */}
      {showLogSelector && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-card border border-border rounded-lg w-full max-w-2xl max-h-[80vh] flex flex-col">
            <div className="p-4 border-b border-border">
              <h3 className="text-lg font-medium">Generate from AI Logs</h3>
              <p className="text-sm text-muted-foreground mt-1">
                Select AI sessions and describe what learnings to extract
              </p>
            </div>

            <div className="p-4 flex-1 overflow-y-auto">
              {/* Prompt */}
              <div className="mb-4">
                <label className="block text-sm font-medium mb-1">What should be extracted?</label>
                <textarea
                  value={generationPrompt}
                  onChange={(e) => setGenerationPrompt(e.target.value)}
                  placeholder="e.g., Extract the learnings about login timing and auto-login behavior..."
                  rows={3}
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-purple-500/50"
                />
              </div>

              {/* Loop Selection */}
              <div>
                <label className="block text-sm font-medium mb-2">
                  Select AI sessions ({selectedLogIds.size} selected)
                </label>
                {aiLoops.length === 0 ? (
                  <div className="text-sm text-muted-foreground py-4 text-center">
                    No AI sessions found. Run some AI operations first.
                  </div>
                ) : (
                  <div className="space-y-2 max-h-60 overflow-y-auto">
                    {aiLoops.map((loop) => (
                      <label
                        key={loop.id}
                        className={`flex items-start gap-3 p-3 rounded-lg border cursor-pointer transition-colors ${
                          selectedLogIds.has(loop.id)
                            ? "border-purple-500 bg-purple-500/10"
                            : "border-border hover:bg-muted/50"
                        }`}
                      >
                        <input
                          type="checkbox"
                          checked={selectedLogIds.has(loop.id)}
                          onChange={(e) => {
                            const newSelected = new Set(selectedLogIds);
                            if (e.target.checked) {
                              newSelected.add(loop.id);
                            } else {
                              newSelected.delete(loop.id);
                            }
                            setSelectedLogIds(newSelected);
                          }}
                          className="mt-1"
                        />
                        <div className="flex-1 min-w-0">
                          <div className="font-medium">{loop.label}</div>
                          <div className="text-xs text-muted-foreground">
                            {new Date(loop.startTime).toLocaleString()} - {loop.entries.length}{" "}
                            messages
                          </div>
                          {loop.promptPreview && (
                            <div className="text-sm text-muted-foreground truncate mt-1">
                              {loop.promptPreview}
                            </div>
                          )}
                        </div>
                      </label>
                    ))}
                  </div>
                )}
              </div>
            </div>

            <div className="p-4 border-t border-border flex justify-end gap-3">
              <button
                onClick={() => {
                  setShowLogSelector(false);
                  setSelectedLogIds(new Set());
                  setGenerationPrompt("");
                }}
                className="px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={generateFromLogs}
                disabled={isGenerating || selectedLogIds.size === 0 || !generationPrompt.trim()}
                className="flex items-center gap-2 px-4 py-2 bg-purple-600 text-white rounded-lg hover:bg-purple-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {isGenerating ? (
                  <>
                    <Sparkles className="w-4 h-4 animate-pulse" />
                    Generating...
                  </>
                ) : (
                  <>
                    <Sparkles className="w-4 h-4" />
                    Generate Scriptlet
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// Scriptlet form component
interface ScriptletFormProps {
  formName: string;
  setFormName: (value: string) => void;
  formContent: string;
  setFormContent: (value: string) => void;
  formCategory: string;
  setFormCategory: (value: string) => void;
  formTags: string;
  setFormTags: (value: string) => void;
  categories: string[];
  onAddCategory: (category: string) => void;
  newCategoryInput: string;
  setNewCategoryInput: (value: string) => void;
  showNewCategoryInput: boolean;
  setShowNewCategoryInput: (value: boolean) => void;
  onShowLogSelector: () => void;
  onSave: () => void;
  onCancel: () => void;
  saveLabel: string;
}

function ScriptletForm({
  formName,
  setFormName,
  formContent,
  setFormContent,
  formCategory,
  setFormCategory,
  formTags,
  setFormTags,
  categories,
  onAddCategory,
  newCategoryInput,
  setNewCategoryInput,
  showNewCategoryInput,
  setShowNewCategoryInput,
  onShowLogSelector,
  onSave,
  onCancel,
  saveLabel,
}: ScriptletFormProps) {
  return (
    <div className="space-y-4">
      {/* Name */}
      <div>
        <label className="block text-sm font-medium mb-1">Name</label>
        <input
          type="text"
          value={formName}
          onChange={(e) => setFormName(e.target.value)}
          placeholder="e.g., Login Auto-Authentication Behavior"
          className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
        />
      </div>

      {/* Content */}
      <div>
        <div className="flex items-center justify-between mb-1">
          <label className="text-sm font-medium">Content</label>
          <button
            onClick={onShowLogSelector}
            className="flex items-center gap-1 text-xs text-purple-400 hover:text-purple-300"
          >
            <Sparkles className="w-3 h-3" />
            Generate from AI Logs
          </button>
        </div>
        <textarea
          value={formContent}
          onChange={(e) => setFormContent(e.target.value)}
          placeholder="The text content that will be inserted into script descriptions..."
          rows={6}
          className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50 font-mono text-sm"
        />
      </div>

      {/* Category and Tags */}
      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium mb-1">Category</label>
          {showNewCategoryInput ? (
            <div className="flex gap-2">
              <input
                type="text"
                value={newCategoryInput}
                onChange={(e) => setNewCategoryInput(e.target.value)}
                placeholder="New category name"
                className="flex-1 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
                autoFocus
              />
              <button
                onClick={() => {
                  const newCat = newCategoryInput.trim();
                  if (newCat) {
                    setFormCategory(newCat);
                    onAddCategory(newCat);
                  }
                  setShowNewCategoryInput(false);
                  setNewCategoryInput("");
                }}
                className="px-3 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700"
              >
                <Check className="w-4 h-4" />
              </button>
              <button
                onClick={() => {
                  setShowNewCategoryInput(false);
                  setNewCategoryInput("");
                }}
                className="px-3 py-2 border border-border rounded-lg hover:bg-muted"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          ) : (
            <div className="flex gap-2">
              <select
                value={formCategory}
                onChange={(e) => setFormCategory(e.target.value)}
                className="flex-1 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
              >
                <option value="">Select category...</option>
                {categories.map((cat) => (
                  <option key={cat} value={cat}>
                    {cat}
                  </option>
                ))}
              </select>
              <button
                onClick={() => setShowNewCategoryInput(true)}
                className="px-3 py-2 border border-border rounded-lg hover:bg-muted"
                title="Add new category"
              >
                <FolderOpen className="w-4 h-4" />
              </button>
            </div>
          )}
        </div>

        <div>
          <label className="block text-sm font-medium mb-1">Tags (comma-separated)</label>
          <input
            type="text"
            value={formTags}
            onChange={(e) => setFormTags(e.target.value)}
            placeholder="e.g., login, auto, timing"
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-green-500/50"
          />
        </div>
      </div>

      {/* Actions */}
      <div className="flex justify-end gap-3 pt-2">
        <button
          onClick={onCancel}
          className="px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={onSave}
          disabled={!formName.trim() || !formContent.trim()}
          className="flex items-center gap-2 px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <Save className="w-4 h-4" />
          {saveLabel}
        </button>
      </div>
    </div>
  );
}

export default ScriptletsTab;
