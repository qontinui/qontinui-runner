/**
 * PromptSnippetBuilderTab.tsx
 *
 * Builder for creating and editing prompt snippets (reusable text snippets).
 * Prompt snippets capture learnings from AI debugging sessions and can be
 * inserted into Playwright test descriptions.
 */

import { useState, useEffect, useCallback } from "react";
import {
  Puzzle,
  Plus,
  Save,
  Trash2,
  Loader2,
  Search,
  Tag,
  FolderOpen,
  Copy,
  Code,
  Check,
  X,
} from "lucide-react";
import type { PromptSnippet } from "../types";
import { getAccentColors } from "@/design-system";
import { BuilderToolbar, toolbarActions } from "./ui/BuilderToolbar";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";
import { getApiBase } from "@/lib/runner-api";

interface PromptSnippetBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editPromptSnippetId?: string | null;
  onNavigateToLibrary?: () => void;
}

export function PromptSnippetBuilderTab({
  onLog,
  editPromptSnippetId,
  onNavigateToLibrary: _onNavigateToLibrary,
}: PromptSnippetBuilderTabProps) {
  // State
  const [promptSnippets, setPromptSnippets] = useState<PromptSnippet[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedSnippet, setSelectedSnippet] = useState<PromptSnippet | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Form state
  const [formName, setFormName] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");

  // Bulk deletion state
  const [isSelectionMode, setIsSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showBatchDeleteDialog, setShowBatchDeleteDialog] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);

  const accentColors = getAccentColors("cyan");

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

  // Delete selected prompt snippets
  const deleteSelectedPromptSnippets = useCallback(async () => {
    setIsDeleting(true);
    try {
      for (const id of selectedIds) {
        await fetch(`${getApiBase()}/prompt-snippets/${id}`, { method: "DELETE" });
      }
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
      await fetchPromptSnippets();
      onLog?.("success", `Deleted ${selectedIds.size} prompt snippet(s)`);
    } catch (error) {
      onLog?.("error", `Failed to delete prompt snippets: ${error}`);
    } finally {
      setIsDeleting(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedIds, exitSelectionMode, onLog]);

  // Get names of selected prompt snippets for dialog
  const getSelectedNames = useCallback(() => {
    return promptSnippets.filter((s) => selectedIds.has(s.id)).map((s) => s.name);
  }, [promptSnippets, selectedIds]);

  // Fetch prompt snippets
  const fetchPromptSnippets = useCallback(async () => {
    try {
      setLoading(true);
      const response = await fetch(`${getApiBase()}/prompt-snippets`);
      if (response.ok) {
        const result = await response.json();
        if (result.success && result.data) {
          setPromptSnippets(result.data);
        } else if (Array.isArray(result)) {
          // Some endpoints return array directly
          setPromptSnippets(result);
        } else {
          setPromptSnippets([]);
        }
      }
    } catch (error) {
      console.error("Failed to fetch prompt snippets:", error);
      onLog?.("error", `Failed to fetch prompt snippets: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchPromptSnippets();
  }, [fetchPromptSnippets]);

  // Load prompt snippet for editing
  useEffect(() => {
    if (editPromptSnippetId && promptSnippets.length > 0) {
      const snippet = promptSnippets.find((s) => s.id === editPromptSnippetId);
      if (snippet) {
        selectSnippet(snippet);
      }
    }
  }, [editPromptSnippetId, promptSnippets]);

  // Select a prompt snippet for editing
  const selectSnippet = (snippet: PromptSnippet) => {
    setSelectedSnippet(snippet);
    setIsCreating(false);
    setFormName(snippet.name);
    setFormContent(snippet.content);
    setFormCategory(snippet.category);
    setFormTags(snippet.tags.join(", "));
  };

  // Start creating a new prompt snippet
  const startCreate = () => {
    setSelectedSnippet(null);
    setIsCreating(true);
    setFormName("");
    setFormContent("");
    setFormCategory("");
    setFormTags("");
  };

  // Save prompt snippet
  const handleSave = async () => {
    if (!formName.trim() || !formContent.trim()) {
      onLog?.("warning", "Name and content are required");
      return;
    }

    setSaving(true);
    try {
      const payload = {
        name: formName.trim(),
        content: formContent.trim(),
        category: formCategory.trim() || "general",
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
      };

      let response;
      if (selectedSnippet) {
        // Update existing
        response = await fetch(`${getApiBase()}/prompt-snippets/${selectedSnippet.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      } else {
        // Create new
        response = await fetch(`${getApiBase()}/prompt-snippets`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      }

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Prompt snippet "${formName}" saved successfully`);
        await fetchPromptSnippets();
        selectSnippet(result.data);
      } else if (response.ok && result.id) {
        // Direct object response
        onLog?.("success", `Prompt snippet "${formName}" saved successfully`);
        await fetchPromptSnippets();
        selectSnippet(result);
      } else {
        onLog?.("error", `Failed to save prompt snippet: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to save prompt snippet: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete prompt snippet
  const handleDelete = async () => {
    if (!selectedSnippet) return;

    if (!confirm(`Delete prompt snippet "${selectedSnippet.name}"?`)) return;

    try {
      const response = await fetch(`${getApiBase()}/prompt-snippets/${selectedSnippet.id}`, {
        method: "DELETE",
      });

      const result = await response.json();
      if (result.success || response.ok) {
        onLog?.("success", `Prompt snippet "${selectedSnippet.name}" deleted`);
        setSelectedSnippet(null);
        setIsCreating(false);
        await fetchPromptSnippets();
      } else {
        onLog?.("error", `Failed to delete prompt snippet: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to delete prompt snippet: ${error}`);
    }
  };

  // Duplicate prompt snippet
  const handleDuplicate = async () => {
    if (!selectedSnippet) return;

    try {
      const payload = {
        name: `${selectedSnippet.name} (Copy)`,
        content: selectedSnippet.content,
        category: selectedSnippet.category,
        tags: selectedSnippet.tags,
      };

      const response = await fetch(`${getApiBase()}/prompt-snippets`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Prompt snippet duplicated as "${result.data.name}"`);
        await fetchPromptSnippets();
        selectSnippet(result.data);
      } else if (response.ok && result.id) {
        onLog?.("success", `Prompt snippet duplicated as "${result.name}"`);
        await fetchPromptSnippets();
        selectSnippet(result);
      }
    } catch (error) {
      onLog?.("error", `Failed to duplicate prompt snippet: ${error}`);
    }
  };

  // Filter prompt snippets
  const filteredSnippets = promptSnippets.filter((snippet) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      snippet.name.toLowerCase().includes(query) ||
      snippet.content.toLowerCase().includes(query) ||
      snippet.category.toLowerCase().includes(query) ||
      snippet.tags.some((t) => t.toLowerCase().includes(query))
    );
  });

  return (
    <div className="h-full flex">
      {/* Left Panel - Prompt Snippet List */}
      <div className="w-80 border-r border-border flex flex-col bg-card">
        {/* Header */}
        <div className="p-4 border-b border-border">
          <div className="flex items-center justify-between mb-2">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <Puzzle className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Prompt Snippets
            </h2>
          </div>

          {/* Action buttons */}
          <BuilderToolbar
            className="mb-3"
            actions={[
              toolbarActions.aiPlaceholder(),
              toolbarActions.editInWeb("/build/prompt-snippets"),
              toolbarActions.delete(() => setIsSelectionMode(!isSelectionMode), isSelectionMode),
              toolbarActions.new(startCreate, "New"),
            ]}
          />

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search prompt snippets..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-muted border border-border rounded-lg text-sm focus:outline-none focus:border-primary"
            />
          </div>
        </div>

        {/* Selection Mode Header */}
        {isSelectionMode && (
          <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
            <span className="text-sm text-red-400">{selectedIds.size} selected</span>
            <div className="flex items-center gap-2">
              <button
                onClick={() => setSelectedIds(new Set(filteredSnippets.map((s) => s.id)))}
                className="px-2 py-1 text-xs bg-muted/80 hover:bg-muted text-foreground rounded"
              >
                Select All
              </button>
              <button
                onClick={() => setSelectedIds(new Set())}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-muted/80 hover:bg-muted text-foreground rounded disabled:opacity-50"
              >
                Clear
              </button>
              <button
                onClick={() => setShowBatchDeleteDialog(true)}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed"
              >
                Delete Selected
              </button>
              <button
                onClick={exitSelectionMode}
                className="p-1 text-muted-foreground hover:text-foreground"
                title="Cancel selection"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          </div>
        )}

        {/* Prompt Snippet List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
            </div>
          ) : filteredSnippets.length === 0 ? (
            <div className="text-center py-8 text-muted-foreground">
              <Puzzle className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No prompt snippets found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredSnippets.map((snippet) => (
                <button
                  key={snippet.id}
                  onClick={() =>
                    isSelectionMode ? toggleSelection(snippet.id) : selectSnippet(snippet)
                  }
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    selectedSnippet?.id === snippet.id ? "bg-muted/80" : "hover:bg-muted"
                  } ${isSelectionMode && selectedIds.has(snippet.id) ? "bg-red-500/10" : ""}`}
                >
                  <div className="flex items-center gap-2">
                    {isSelectionMode && (
                      <div
                        className={`w-4 h-4 rounded border flex-shrink-0 flex items-center justify-center ${
                          selectedIds.has(snippet.id)
                            ? "bg-red-500 border-red-500"
                            : "border-muted-foreground hover:border-red-400"
                        }`}
                      >
                        {selectedIds.has(snippet.id) && <Check className="w-3 h-3 text-white" />}
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="font-medium text-sm truncate">{snippet.name}</div>
                      <div className="text-xs text-muted-foreground truncate mt-0.5 font-mono">
                        {snippet.content.slice(0, 50)}...
                      </div>
                      <div className="flex items-center gap-2 mt-1.5">
                        {snippet.category && (
                          <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
                            {snippet.category}
                          </span>
                        )}
                      </div>
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-card/50">
        {!selectedSnippet && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-muted-foreground">
              <Puzzle className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No prompt snippet selected</p>
              <p className="text-sm mb-4">Select a prompt snippet to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
              >
                <Plus className="w-4 h-4" />
                Create New Prompt Snippet
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-border">
              <h3 className="font-medium">
                {isCreating ? "New Prompt Snippet" : `Editing: ${selectedSnippet?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedSnippet && (
                  <>
                    <button
                      onClick={handleDuplicate}
                      className="p-2 rounded-lg hover:bg-muted transition-colors"
                      title="Duplicate"
                    >
                      <Copy className="w-4 h-4" />
                    </button>
                    <button
                      onClick={handleDelete}
                      className="p-2 rounded-lg hover:bg-muted text-red-400 transition-colors"
                      title="Delete"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </>
                )}
                <button
                  onClick={handleSave}
                  disabled={saving || !formName.trim() || !formContent.trim()}
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
                    placeholder="Prompt snippet name"
                    className="w-full px-3 py-2 bg-muted border border-border rounded-lg focus:outline-none focus:border-primary"
                  />
                </div>

                {/* Content */}
                <div>
                  <label className="block text-sm font-medium mb-1.5">
                    <Code className="w-4 h-4 inline mr-1" />
                    Content *
                  </label>
                  <textarea
                    value={formContent}
                    onChange={(e) => setFormContent(e.target.value)}
                    placeholder="Enter the reusable text snippet content..."
                    rows={15}
                    className="w-full px-3 py-2 bg-muted border border-border rounded-lg focus:outline-none focus:border-primary font-mono text-sm resize-y"
                  />
                  <p className="text-xs text-muted-foreground mt-1">
                    This content can be inserted into Playwright test descriptions using @mention
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
                      placeholder="e.g., Login, Navigation, Forms"
                      className="w-full px-3 py-2 bg-muted border border-border rounded-lg focus:outline-none focus:border-primary"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1.5">
                      <Tag className="w-4 h-4 inline mr-1" />
                      Tags
                    </label>
                    <input
                      type="text"
                      value={formTags}
                      onChange={(e) => setFormTags(e.target.value)}
                      placeholder="comma, separated, tags"
                      className="w-full px-3 py-2 bg-muted border border-border rounded-lg focus:outline-none focus:border-primary"
                    />
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
        title="Delete Prompt Snippets"
        itemType="prompt snippet"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedPromptSnippets}
      />
    </div>
  );
}
