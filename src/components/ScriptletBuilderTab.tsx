/**
 * ScriptletBuilderTab.tsx
 *
 * Builder for creating and editing scriptlets (reusable code snippets).
 * Scriptlets capture learnings from AI debugging sessions and can be
 * inserted into Playwright script descriptions.
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
import type { Scriptlet } from "../types";
import { getAccentColors } from "@/design-system";
import { BuilderToolbar, toolbarActions } from "./ui/BuilderToolbar";
import { BatchDeleteDialog } from "./ui/BatchDeleteDialog";

const API_BASE = "http://localhost:9876";

interface ScriptletBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editScriptletId?: string | null;
  onNavigateToLibrary?: () => void;
}

export function ScriptletBuilderTab({
  onLog,
  editScriptletId,
  onNavigateToLibrary: _onNavigateToLibrary,
}: ScriptletBuilderTabProps) {
  // State
  const [scriptlets, setScriptlets] = useState<Scriptlet[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedScriptlet, setSelectedScriptlet] = useState<Scriptlet | null>(null);
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

  // Delete selected scriptlets
  const deleteSelectedScriptlets = useCallback(async () => {
    setIsDeleting(true);
    try {
      for (const id of selectedIds) {
        await fetch(`${API_BASE}/scriptlets/${id}`, { method: "DELETE" });
      }
      exitSelectionMode();
      setShowBatchDeleteDialog(false);
      await fetchScriptlets();
      onLog?.("success", `Deleted ${selectedIds.size} scriptlet(s)`);
    } catch (error) {
      onLog?.("error", `Failed to delete scriptlets: ${error}`);
    } finally {
      setIsDeleting(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedIds, exitSelectionMode, onLog]);

  // Get names of selected scriptlets for dialog
  const getSelectedNames = useCallback(() => {
    return scriptlets.filter((s) => selectedIds.has(s.id)).map((s) => s.name);
  }, [scriptlets, selectedIds]);

  // Fetch scriptlets
  const fetchScriptlets = useCallback(async () => {
    try {
      setLoading(true);
      const response = await fetch(`${API_BASE}/scriptlets`);
      if (response.ok) {
        const result = await response.json();
        if (result.success && result.data) {
          setScriptlets(result.data);
        } else if (Array.isArray(result)) {
          // Some endpoints return array directly
          setScriptlets(result);
        } else {
          setScriptlets([]);
        }
      }
    } catch (error) {
      console.error("Failed to fetch scriptlets:", error);
      onLog?.("error", `Failed to fetch scriptlets: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchScriptlets();
  }, [fetchScriptlets]);

  // Load scriptlet for editing
  useEffect(() => {
    if (editScriptletId && scriptlets.length > 0) {
      const scriptlet = scriptlets.find((s) => s.id === editScriptletId);
      if (scriptlet) {
        selectScriptlet(scriptlet);
      }
    }
  }, [editScriptletId, scriptlets]);

  // Select a scriptlet for editing
  const selectScriptlet = (scriptlet: Scriptlet) => {
    setSelectedScriptlet(scriptlet);
    setIsCreating(false);
    setFormName(scriptlet.name);
    setFormContent(scriptlet.content);
    setFormCategory(scriptlet.category);
    setFormTags(scriptlet.tags.join(", "));
  };

  // Start creating a new scriptlet
  const startCreate = () => {
    setSelectedScriptlet(null);
    setIsCreating(true);
    setFormName("");
    setFormContent("");
    setFormCategory("");
    setFormTags("");
  };

  // Save scriptlet
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
      if (selectedScriptlet) {
        // Update existing
        response = await fetch(`${API_BASE}/scriptlets/${selectedScriptlet.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      } else {
        // Create new
        response = await fetch(`${API_BASE}/scriptlets`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      }

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Scriptlet "${formName}" saved successfully`);
        await fetchScriptlets();
        selectScriptlet(result.data);
      } else if (response.ok && result.id) {
        // Direct object response
        onLog?.("success", `Scriptlet "${formName}" saved successfully`);
        await fetchScriptlets();
        selectScriptlet(result);
      } else {
        onLog?.("error", `Failed to save scriptlet: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to save scriptlet: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete scriptlet
  const handleDelete = async () => {
    if (!selectedScriptlet) return;

    if (!confirm(`Delete scriptlet "${selectedScriptlet.name}"?`)) return;

    try {
      const response = await fetch(`${API_BASE}/scriptlets/${selectedScriptlet.id}`, {
        method: "DELETE",
      });

      const result = await response.json();
      if (result.success || response.ok) {
        onLog?.("success", `Scriptlet "${selectedScriptlet.name}" deleted`);
        setSelectedScriptlet(null);
        setIsCreating(false);
        await fetchScriptlets();
      } else {
        onLog?.("error", `Failed to delete scriptlet: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to delete scriptlet: ${error}`);
    }
  };

  // Duplicate scriptlet
  const handleDuplicate = async () => {
    if (!selectedScriptlet) return;

    try {
      const payload = {
        name: `${selectedScriptlet.name} (Copy)`,
        content: selectedScriptlet.content,
        category: selectedScriptlet.category,
        tags: selectedScriptlet.tags,
      };

      const response = await fetch(`${API_BASE}/scriptlets`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Scriptlet duplicated as "${result.data.name}"`);
        await fetchScriptlets();
        selectScriptlet(result.data);
      } else if (response.ok && result.id) {
        onLog?.("success", `Scriptlet duplicated as "${result.name}"`);
        await fetchScriptlets();
        selectScriptlet(result);
      }
    } catch (error) {
      onLog?.("error", `Failed to duplicate scriptlet: ${error}`);
    }
  };

  // Filter scriptlets
  const filteredScriptlets = scriptlets.filter((scriptlet) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      scriptlet.name.toLowerCase().includes(query) ||
      scriptlet.content.toLowerCase().includes(query) ||
      scriptlet.category.toLowerCase().includes(query) ||
      scriptlet.tags.some((t) => t.toLowerCase().includes(query))
    );
  });

  return (
    <div className="h-full flex">
      {/* Left Panel - Scriptlet List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-2">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <Puzzle className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Scriptlets
            </h2>
          </div>

          {/* Action buttons */}
          <BuilderToolbar
            className="mb-3"
            actions={[
              toolbarActions.aiPlaceholder(),
              toolbarActions.delete(() => setIsSelectionMode(!isSelectionMode), isSelectionMode),
              toolbarActions.new(startCreate, "New"),
            ]}
          />

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search scriptlets..."
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
              <button
                onClick={() => setSelectedIds(new Set(filteredScriptlets.map((s) => s.id)))}
                className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded"
              >
                Select All
              </button>
              <button
                onClick={() => setSelectedIds(new Set())}
                disabled={selectedIds.size === 0}
                className="px-2 py-1 text-xs bg-neutral-700 hover:bg-neutral-600 text-neutral-200 rounded disabled:opacity-50"
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
                className="p-1 text-neutral-400 hover:text-neutral-200"
                title="Cancel selection"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          </div>
        )}

        {/* Scriptlet List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredScriptlets.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <Puzzle className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No scriptlets found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredScriptlets.map((scriptlet) => (
                <button
                  key={scriptlet.id}
                  onClick={() =>
                    isSelectionMode ? toggleSelection(scriptlet.id) : selectScriptlet(scriptlet)
                  }
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    selectedScriptlet?.id === scriptlet.id
                      ? "bg-neutral-700"
                      : "hover:bg-neutral-800"
                  } ${isSelectionMode && selectedIds.has(scriptlet.id) ? "bg-red-500/10" : ""}`}
                >
                  <div className="flex items-center gap-2">
                    {isSelectionMode && (
                      <div
                        className={`w-4 h-4 rounded border flex-shrink-0 flex items-center justify-center ${
                          selectedIds.has(scriptlet.id)
                            ? "bg-red-500 border-red-500"
                            : "border-neutral-500 hover:border-red-400"
                        }`}
                      >
                        {selectedIds.has(scriptlet.id) && <Check className="w-3 h-3 text-white" />}
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="font-medium text-sm truncate">{scriptlet.name}</div>
                      <div className="text-xs text-neutral-400 truncate mt-0.5 font-mono">
                        {scriptlet.content.slice(0, 50)}...
                      </div>
                      <div className="flex items-center gap-2 mt-1.5">
                        {scriptlet.category && (
                          <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-400">
                            {scriptlet.category}
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
      <div className="flex-1 flex flex-col bg-neutral-900/50">
        {!selectedScriptlet && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-neutral-400">
              <Puzzle className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No scriptlet selected</p>
              <p className="text-sm mb-4">Select a scriptlet to edit or create a new one</p>
              <button
                onClick={startCreate}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors"
                style={{ backgroundColor: accentColors.bgSolid, color: "#000" }}
              >
                <Plus className="w-4 h-4" />
                Create New Scriptlet
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-neutral-700">
              <h3 className="font-medium">
                {isCreating ? "New Scriptlet" : `Editing: ${selectedScriptlet?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedScriptlet && (
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
                    placeholder="Scriptlet name"
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
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
                    placeholder="Enter the reusable code snippet or text content..."
                    rows={15}
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 font-mono text-sm resize-y"
                  />
                  <p className="text-xs text-neutral-400 mt-1">
                    This content can be inserted into Playwright script descriptions using @mention
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
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
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
                      className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
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
        title="Delete Scriptlets"
        itemType="scriptlet"
        itemNames={getSelectedNames()}
        isDeleting={isDeleting}
        onClose={() => setShowBatchDeleteDialog(false)}
        onConfirm={deleteSelectedScriptlets}
      />
    </div>
  );
}
