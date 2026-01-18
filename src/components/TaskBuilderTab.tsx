/**
 * TaskBuilderTab.tsx
 *
 * Builder for creating and editing single-step AI tasks (prompts).
 * These are stored as SavedPrompts and can be used in workflows.
 */

import { useState, useEffect, useCallback } from "react";
import {
  FileText,
  Plus,
  Save,
  Trash2,
  Loader2,
  Search,
  Tag,
  FolderOpen,
  X,
  Copy,
  Clock,
} from "lucide-react";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

interface SavedTask {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
  max_sessions: number | null;
  created_at: string;
  modified_at: string;
}

interface TaskBuilderTabProps {
  onLog?: (level: string, message: string) => void;
  editTaskId?: string | null;
  onNavigateToLibrary?: () => void;
}

export function TaskBuilderTab({ onLog, editTaskId, onNavigateToLibrary }: TaskBuilderTabProps) {
  // State
  const [tasks, setTasks] = useState<SavedTask[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedTask, setSelectedTask] = useState<SavedTask | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Form state
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");
  const [formMaxSessions, setFormMaxSessions] = useState<number | null>(null);

  const accentColors = getAccentColors("amber");

  // Fetch tasks
  const fetchTasks = useCallback(async () => {
    try {
      setLoading(true);
      const response = await fetch(`${API_BASE}/prompts`);
      if (response.ok) {
        const result = await response.json();
        if (result.success && result.data) {
          setTasks(result.data);
        } else {
          setTasks([]);
        }
      }
    } catch (error) {
      console.error("Failed to fetch tasks:", error);
      onLog?.("error", `Failed to fetch tasks: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  // Load task for editing
  useEffect(() => {
    if (editTaskId && tasks.length > 0) {
      const task = tasks.find((t) => t.id === editTaskId);
      if (task) {
        selectTask(task);
      }
    }
  }, [editTaskId, tasks]);

  // Select a task for editing
  const selectTask = (task: SavedTask) => {
    setSelectedTask(task);
    setIsCreating(false);
    setFormName(task.name);
    setFormDescription(task.description);
    setFormContent(task.content);
    setFormCategory(task.category);
    setFormTags(task.tags.join(", "));
    setFormMaxSessions(task.max_sessions);
  };

  // Start creating a new task
  const startCreate = () => {
    setSelectedTask(null);
    setIsCreating(true);
    setFormName("");
    setFormDescription("");
    setFormContent("");
    setFormCategory("");
    setFormTags("");
    setFormMaxSessions(null);
  };

  // Save task
  const handleSave = async () => {
    if (!formName.trim() || !formContent.trim()) {
      onLog?.("warning", "Name and content are required");
      return;
    }

    setSaving(true);
    try {
      const payload = {
        name: formName.trim(),
        description: formDescription.trim(),
        content: formContent.trim(),
        category: formCategory.trim() || "general",
        tags: formTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
        max_sessions: formMaxSessions,
      };

      let response;
      if (selectedTask) {
        // Update existing
        response = await fetch(`${API_BASE}/prompts/${selectedTask.id}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      } else {
        // Create new
        response = await fetch(`${API_BASE}/prompts`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      }

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Task "${formName}" saved successfully`);
        await fetchTasks();
        selectTask(result.data);
      } else {
        onLog?.("error", `Failed to save task: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to save task: ${error}`);
    } finally {
      setSaving(false);
    }
  };

  // Delete task
  const handleDelete = async () => {
    if (!selectedTask) return;

    if (!confirm(`Delete task "${selectedTask.name}"?`)) return;

    try {
      const response = await fetch(`${API_BASE}/prompts/${selectedTask.id}`, {
        method: "DELETE",
      });

      const result = await response.json();
      if (result.success) {
        onLog?.("success", `Task "${selectedTask.name}" deleted`);
        setSelectedTask(null);
        setIsCreating(false);
        await fetchTasks();
      } else {
        onLog?.("error", `Failed to delete task: ${result.error || "Unknown error"}`);
      }
    } catch (error) {
      onLog?.("error", `Failed to delete task: ${error}`);
    }
  };

  // Duplicate task
  const handleDuplicate = async () => {
    if (!selectedTask) return;

    try {
      const payload = {
        name: `${selectedTask.name} (Copy)`,
        description: selectedTask.description,
        content: selectedTask.content,
        category: selectedTask.category,
        tags: selectedTask.tags,
        max_sessions: selectedTask.max_sessions,
      };

      const response = await fetch(`${API_BASE}/prompts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      const result = await response.json();
      if (result.success && result.data) {
        onLog?.("success", `Task duplicated as "${result.data.name}"`);
        await fetchTasks();
        selectTask(result.data);
      }
    } catch (error) {
      onLog?.("error", `Failed to duplicate task: ${error}`);
    }
  };

  // Filter tasks
  const filteredTasks = tasks.filter((task) => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      task.name.toLowerCase().includes(query) ||
      task.description.toLowerCase().includes(query) ||
      task.category.toLowerCase().includes(query) ||
      task.tags.some((t) => t.toLowerCase().includes(query))
    );
  });

  return (
    <div className="h-full flex">
      {/* Left Panel - Task List */}
      <div className="w-80 border-r border-neutral-700 flex flex-col bg-neutral-900">
        {/* Header */}
        <div className="p-4 border-b border-neutral-700">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-lg font-semibold flex items-center gap-2">
              <FileText className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
              Tasks
            </h2>
            <button
              onClick={startCreate}
              className="p-2 rounded-lg hover:bg-neutral-800 transition-colors"
              title="New Task"
            >
              <Plus className="w-4 h-4" />
            </button>
          </div>

          {/* Search */}
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-neutral-400" />
            <input
              type="text"
              placeholder="Search tasks..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg text-sm focus:outline-none focus:border-neutral-600"
            />
          </div>
        </div>

        {/* Task List */}
        <div className="flex-1 overflow-y-auto p-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-neutral-400" />
            </div>
          ) : filteredTasks.length === 0 ? (
            <div className="text-center py-8 text-neutral-400">
              <FileText className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No tasks found</p>
            </div>
          ) : (
            <div className="space-y-1">
              {filteredTasks.map((task) => (
                <button
                  key={task.id}
                  onClick={() => selectTask(task)}
                  className={`w-full text-left p-3 rounded-lg transition-colors ${
                    selectedTask?.id === task.id ? "bg-neutral-700" : "hover:bg-neutral-800"
                  }`}
                >
                  <div className="font-medium text-sm truncate">{task.name}</div>
                  {task.description && (
                    <div className="text-xs text-neutral-400 truncate mt-0.5">
                      {task.description}
                    </div>
                  )}
                  <div className="flex items-center gap-2 mt-1.5">
                    {task.category && (
                      <span className="text-xs px-1.5 py-0.5 rounded bg-neutral-800 text-neutral-400">
                        {task.category}
                      </span>
                    )}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Right Panel - Editor */}
      <div className="flex-1 flex flex-col bg-neutral-900/50">
        {!selectedTask && !isCreating ? (
          <div className="flex-1 flex items-center justify-center">
            <div className="text-center text-neutral-400">
              <FileText className="w-12 h-12 mx-auto mb-3 opacity-30" />
              <p className="text-lg mb-2">No task selected</p>
              <p className="text-sm mb-4">Select a task to edit or create a new one</p>
              <button
                onClick={startCreate}
                className={`inline-flex items-center gap-2 px-4 py-2 rounded-lg transition-colors ${accentColors.bgSolid} text-black hover:opacity-90`}
              >
                <Plus className="w-4 h-4" />
                Create New Task
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* Toolbar */}
            <div className="flex items-center justify-between p-4 border-b border-neutral-700">
              <h3 className="font-medium">
                {isCreating ? "New Task" : `Editing: ${selectedTask?.name}`}
              </h3>
              <div className="flex items-center gap-2">
                {selectedTask && (
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
                  className={`flex items-center gap-2 px-4 py-2 rounded-lg disabled:opacity-50 transition-colors ${accentColors.bgSolid} text-black hover:opacity-90`}
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
                    placeholder="Task name"
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                  />
                </div>

                {/* Description */}
                <div>
                  <label className="block text-sm font-medium mb-1.5">Description</label>
                  <input
                    type="text"
                    value={formDescription}
                    onChange={(e) => setFormDescription(e.target.value)}
                    placeholder="Brief description"
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                  />
                </div>

                {/* Content */}
                <div>
                  <label className="block text-sm font-medium mb-1.5">Prompt Content *</label>
                  <textarea
                    value={formContent}
                    onChange={(e) => setFormContent(e.target.value)}
                    placeholder="Enter the prompt content that will be sent to the AI..."
                    rows={12}
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600 font-mono text-sm resize-y"
                  />
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
                      placeholder="e.g., automation, testing"
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

                {/* Max Sessions */}
                <div>
                  <label className="block text-sm font-medium mb-1.5">
                    <Clock className="w-4 h-4 inline mr-1" />
                    Max Sessions (optional)
                  </label>
                  <input
                    type="number"
                    value={formMaxSessions ?? ""}
                    onChange={(e) =>
                      setFormMaxSessions(e.target.value ? parseInt(e.target.value) : null)
                    }
                    placeholder="Leave empty for unlimited"
                    min={1}
                    className="w-full px-3 py-2 bg-neutral-800 border border-neutral-700 rounded-lg focus:outline-none focus:border-neutral-600"
                  />
                  <p className="text-xs text-neutral-400 mt-1">
                    Maximum number of AI sessions allowed for this task
                  </p>
                </div>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
