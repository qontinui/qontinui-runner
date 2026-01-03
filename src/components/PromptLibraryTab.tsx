import { useState, useEffect, useCallback } from "react";
import { logManager } from "../managers";
import {
  BookOpen,
  Plus,
  Search,
  Play,
  Pencil,
  Trash2,
  Copy,
  Tag,
  FolderOpen,
  Save,
  X,
  Loader2,
  ChevronDown,
  ChevronUp,
  Upload,
  Download,
  Sparkles,
  RefreshCw,
  Square,
  CheckCircle,
  AlertCircle,
  Clock,
} from "lucide-react";
import { ContextSelector, useContexts } from "./contexts";
import type { ContextSelection } from "../types/context";

// Types for the prompt library
// Simplified model: every task runs until [TASK_COMPLETE] marker is found
interface SavedPrompt {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
  /** Optional limit on number of sessions (null = unlimited). Sessions continue until [TASK_COMPLETE] is found. */
  max_sessions: number | null;
  /** Optional AI provider override (e.g., "gemini_api", "claude_cli") */
  provider?: string | null;
  /** Optional AI model override (e.g., "gemini-3-flash") */
  model?: string | null;
  created_at: string;
  modified_at: string;
}

// Provider options for the UI
const PROVIDER_OPTIONS = [
  { value: "", label: "Use Default (from Settings)" },
  { value: "claude_cli", label: "Claude CLI" },
  { value: "claude_api", label: "Claude API" },
  { value: "gemini_cli", label: "Gemini CLI" },
  { value: "gemini_api", label: "Gemini API" },
];

// Model options per provider
const MODELS_BY_PROVIDER: Record<string, { value: string; label: string }[]> = {
  claude_cli: [
    { value: "", label: "Default" },
    { value: "claude-sonnet-4", label: "Claude Sonnet 4" },
    { value: "claude-opus-4", label: "Claude Opus 4" },
  ],
  claude_api: [
    { value: "", label: "Default" },
    { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
    { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  ],
  gemini_cli: [
    { value: "", label: "Default" },
    { value: "gemini-3-flash", label: "Gemini 3 Flash (Fast/Cheap)" },
    { value: "gemini-3-pro", label: "Gemini 3 Pro" },
    { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
    { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  ],
  gemini_api: [
    { value: "", label: "Default" },
    { value: "gemini-3-flash", label: "Gemini 3 Flash (Fast/Cheap)" },
    { value: "gemini-3-pro", label: "Gemini 3 Pro" },
    { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
    { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  ],
};

// Types for unified sessions
interface SessionCheckpoint {
  session_id: string;
  completed: boolean;
  status: string;
  started_at: string;
  last_activity: string;
  sessions_count: number; // Number of sessions spawned
  restart_permitted: boolean;
  error_message: string | null;
  custom_data: Record<string, unknown>;
  activity_log: string[];
}

interface SessionConfig {
  prompt: string;
  uses_gui: boolean;
  timeout_seconds: number;
  name: string;
  description: string;
  max_sessions?: number; // Optional limit on sessions
  custom_config: Record<string, unknown>;
}

interface Session {
  id: string;
  config: SessionConfig;
  status:
    | "starting"
    | "running"
    | "completed"
    | "failed"
    | "stopped"
    | "waiting_for_continuation"
    | "stalled";
  checkpoint: SessionCheckpoint;
  active_subprocess_id: string | null;
  event_log: { timestamp: number; event_type: string; message: string }[];
}

// Task run status for UI display
interface TaskRun {
  id: string;
  task_id: string;
  task_name: string;
  status: "running" | "complete" | "failed" | "stopped";
  active_session_id: string | null;
  started_at: number;
  last_activity: number;
  sessions_count: number; // Number of sessions used
  max_sessions?: number; // Optional limit
  error_message: string | null;
  event_log: { timestamp: number; event_type: string; message: string }[];
}

// Convert Session to TaskRun for UI display
function sessionToTaskRun(session: Session, taskId?: string): TaskRun {
  const statusMap: Record<string, TaskRun["status"]> = {
    starting: "running",
    running: "running",
    completed: "complete",
    failed: "failed",
    stopped: "stopped",
    waiting_for_continuation: "running",
    stalled: "running",
  };

  return {
    id: session.id,
    task_id: taskId || session.id,
    task_name: session.config.name,
    status: statusMap[session.status] || "running",
    active_session_id: session.active_subprocess_id,
    started_at: new Date(session.checkpoint.started_at).getTime() / 1000,
    last_activity: new Date(session.checkpoint.last_activity).getTime() / 1000,
    sessions_count: session.checkpoint.sessions_count,
    max_sessions: session.config.max_sessions,
    error_message: session.checkpoint.error_message,
    event_log: session.event_log,
  };
}

interface _SpawnResponse {
  session_id: string;
  state_file: string;
  log_file: string;
  pid: number | null;
}

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface PromptLibraryTabProps {
  onLog: (level: LogLevel, message: string) => void;
}

const API_BASE = "http://localhost:9876";

export function PromptLibraryTab({ onLog }: PromptLibraryTabProps) {
  // State
  const [prompts, setPrompts] = useState<SavedPrompt[]>([]);
  const [loading, setLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedCategory, setSelectedCategory] = useState<string | null>(null);
  const [categories, setCategories] = useState<string[]>([]);

  // Storage key for persisting create form state
  const CREATE_FORM_STORAGE_KEY = "qontinui-prompt-create-form";

  // Load persisted create form state
  const loadPersistedCreateForm = useCallback(() => {
    try {
      const saved = localStorage.getItem(CREATE_FORM_STORAGE_KEY);
      if (saved) {
        return JSON.parse(saved);
      }
    } catch {
      // Ignore parse errors
    }
    return null;
  }, []);

  // Edit modal state
  const [editingPrompt, setEditingPrompt] = useState<SavedPrompt | null>(null);
  const [isCreating, setIsCreating] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.isCreating ?? false;
  });

  // Form state for create/edit - restore from localStorage if available
  const [formName, setFormName] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formName ?? "";
  });
  const [formDescription, setFormDescription] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formDescription ?? "";
  });
  const [formContent, setFormContent] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formContent ?? "";
  });
  const [formCategory, setFormCategory] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formCategory ?? "";
  });
  const [formTags, setFormTags] = useState(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formTags ?? "";
  });

  // Optional max sessions limit (null = unlimited)
  const [formMaxSessions, setFormMaxSessions] = useState<number | undefined>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formMaxSessions;
  });

  // Optional provider override
  const [formProvider, setFormProvider] = useState<string>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formProvider ?? "";
  });

  // Optional model override
  const [formModel, setFormModel] = useState<string>(() => {
    const saved = loadPersistedCreateForm();
    return saved?.formModel ?? "";
  });

  // Running state
  const [runningPromptId, setRunningPromptId] = useState<string | null>(null);

  // Active task runs state
  const [taskRuns, setTaskRuns] = useState<TaskRun[]>([]);
  const [showTaskRuns, setShowTaskRuns] = useState(false);

  // Context selection state for running tasks
  const [contextSelection, setContextSelection] = useState<ContextSelection>({
    selectedIds: [],
    autoDetect: true,
  });

  // Context hook for getting selected context content
  const { getSelectedContextsContent, recordUsage } = useContexts();

  // Load prompts on mount
  useEffect(() => {
    loadPrompts();
    loadCategories();
    loadTaskRuns();
  }, []);

  // Periodically refresh task runs
  useEffect(() => {
    if (showTaskRuns) {
      const interval = setInterval(loadTaskRuns, 5000);
      return () => clearInterval(interval);
    }
  }, [showTaskRuns]);

  // Persist create form state when in creating mode
  useEffect(() => {
    if (isCreating) {
      const formState = {
        isCreating,
        formName,
        formDescription,
        formContent,
        formCategory,
        formTags,
        formMaxSessions,
        formProvider,
        formModel,
      };
      localStorage.setItem(CREATE_FORM_STORAGE_KEY, JSON.stringify(formState));
    } else {
      // Clear persisted state when not creating
      localStorage.removeItem(CREATE_FORM_STORAGE_KEY);
    }
  }, [
    isCreating,
    formName,
    formDescription,
    formContent,
    formCategory,
    formTags,
    formMaxSessions,
    formProvider,
    formModel,
  ]);

  const loadPrompts = useCallback(async () => {
    setLoading(true);
    try {
      const response = await fetch(`${API_BASE}/prompts`);
      const result = await response.json();
      if (result.success) {
        setPrompts(result.data || []);
      } else {
        onLog("error", `Failed to load prompts: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to load prompts: ${error}`);
    } finally {
      setLoading(false);
    }
  }, [onLog]);

  const loadCategories = async () => {
    try {
      const response = await fetch(`${API_BASE}/prompts/categories`);
      const result = await response.json();
      if (result.success) {
        setCategories(result.data || []);
      }
    } catch (error) {
      console.error("Failed to load categories:", error);
    }
  };

  const loadTaskRuns = async () => {
    try {
      // Use unified sessions API
      const response = await fetch(`${API_BASE}/sessions`);
      const result = await response.json();
      if (result.success) {
        // Get all running/active sessions and convert to TaskRun format
        const sessions: Session[] = result.data || [];
        const activeTasks = sessions
          .filter((s: Session) =>
            ["running", "starting", "waiting_for_continuation"].includes(s.status),
          )
          .map((s: Session) => sessionToTaskRun(s));
        setTaskRuns(activeTasks);
      }
    } catch (error) {
      console.error("Failed to load task runs:", error);
    }
  };

  // Create a new prompt/task
  const createPrompt = async () => {
    try {
      const response = await fetch(`${API_BASE}/prompts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formName,
          description: formDescription,
          content: formContent,
          category: formCategory,
          tags: formTags
            .split(",")
            .map((t) => t.trim())
            .filter((t) => t),
          max_sessions: formMaxSessions ?? null,
          provider: formProvider || null,
          model: formModel || null,
        }),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Created task: ${formName}`);
        resetForm();
        setIsCreating(false);
        loadPrompts();
        loadCategories();
      } else {
        onLog("error", `Failed to create task: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to create task: ${error}`);
    }
  };

  // Update an existing prompt/task
  const updatePrompt = async () => {
    if (!editingPrompt) return;

    try {
      const response = await fetch(`${API_BASE}/prompts/${editingPrompt.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: formName,
          description: formDescription,
          content: formContent,
          category: formCategory,
          tags: formTags
            .split(",")
            .map((t) => t.trim())
            .filter((t) => t),
          max_sessions: formMaxSessions ?? null,
          provider: formProvider || null,
          model: formModel || null,
        }),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Updated task: ${formName}`);
        resetForm();
        setEditingPrompt(null);
        loadPrompts();
        loadCategories();
      } else {
        onLog("error", `Failed to update task: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to update task: ${error}`);
    }
  };

  // Delete a prompt
  const deletePrompt = async (id: string, name: string) => {
    if (!confirm(`Are you sure you want to delete "${name}"?`)) return;

    try {
      const response = await fetch(`${API_BASE}/prompts/${id}`, {
        method: "DELETE",
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Deleted prompt: ${name}`);
        loadPrompts();
      } else {
        onLog("error", `Failed to delete prompt: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to delete prompt: ${error}`);
    }
  };

  // Duplicate a prompt
  const duplicatePrompt = async (id: string) => {
    try {
      const response = await fetch(`${API_BASE}/prompts/${id}/duplicate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Duplicated prompt: ${result.data.name}`);
        loadPrompts();
      } else {
        onLog("error", `Failed to duplicate prompt: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to duplicate prompt: ${error}`);
    }
  };

  // Run a task
  const runPrompt = async (prompt: SavedPrompt) => {
    setRunningPromptId(prompt.id);
    onLog("info", `Running task: ${prompt.name}`);

    try {
      // Build the full prompt with context content
      let fullPrompt = prompt.content;

      // Include selected contexts if any
      if (contextSelection.selectedIds.length > 0) {
        const contextContent = getSelectedContextsContent(contextSelection);
        if (contextContent) {
          fullPrompt = `# Context\n\n${contextContent}\n\n---\n\n# Task\n\n${prompt.content}`;
          // Record context usage
          recordUsage(contextSelection.selectedIds);
        }
      }

      // Use unified sessions API - every task runs until [TASK_COMPLETE]
      const response = await fetch(`${API_BASE}/sessions/start`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: prompt.name,
          prompt: fullPrompt,
          uses_gui: false,
          timeout_seconds: 1800,
        }),
      });
      const result = await response.json();
      if (result.success) {
        const session = result.data?.session as Session;
        onLog("success", `Started task: ${session.id}`);
        setShowTaskRuns(true);
        loadTaskRuns();
      } else {
        onLog("error", `Failed to run task: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to run task: ${error}`);
    } finally {
      setRunningPromptId(null);
    }
  };

  // Stop a running task
  const stopTask = async (runId: string) => {
    try {
      const response = await fetch(`${API_BASE}/sessions/${runId}/stop`, {
        method: "POST",
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Stopped task: ${runId}`);
        loadTaskRuns();
      } else {
        onLog("error", `Failed to stop task: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to stop task: ${error}`);
    }
  };

  // Delete a task run record
  const deleteTaskRun = async (runId: string) => {
    try {
      const response = await fetch(`${API_BASE}/sessions/${runId}`, {
        method: "DELETE",
      });
      const result = await response.json();
      if (result.success) {
        loadTaskRuns();
      } else {
        onLog("error", `Failed to delete task: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to delete task: ${error}`);
    }
  };

  // Delete all task runs
  const deleteAllTaskRuns = async () => {
    try {
      const listResponse = await fetch(`${API_BASE}/sessions`);
      const listResult = await listResponse.json();
      if (!listResult.success) {
        onLog("error", `Failed to list sessions: ${listResult.error}`);
        return;
      }

      const sessions: Session[] = listResult.data || [];
      let deletedCount = 0;
      for (const session of sessions) {
        const response = await fetch(`${API_BASE}/sessions/${session.id}`, {
          method: "DELETE",
        });
        const result = await response.json();
        if (result.success) {
          deletedCount++;
        }
      }

      onLog("success", `Deleted ${deletedCount} task(s)`);
      loadTaskRuns();
    } catch (error) {
      onLog("error", `Failed to delete tasks: ${error}`);
    }
  };

  // Export prompts
  const exportPrompts = async () => {
    try {
      const response = await fetch(`${API_BASE}/prompts/export`);
      const result = await response.json();
      if (result.success) {
        // Create and download file
        const blob = new Blob([result.data], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `prompts-${new Date().toISOString().slice(0, 10)}.json`;
        a.click();
        URL.revokeObjectURL(url);
        onLog("success", "Exported prompts to file");
      } else {
        onLog("error", `Failed to export prompts: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to export prompts: ${error}`);
    }
  };

  // Import prompts
  const importPrompts = async () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.onchange = async (e) => {
      const file = (e.target as HTMLInputElement).files?.[0];
      if (!file) return;

      try {
        const text = await file.text();
        const response = await fetch(`${API_BASE}/prompts/import`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ prompts_json: text }),
        });
        const result = await response.json();
        if (result.success) {
          onLog("success", `Imported ${result.data.length} prompts`);
          loadPrompts();
          loadCategories();
        } else {
          onLog("error", `Failed to import prompts: ${result.error}`);
        }
      } catch (error) {
        onLog("error", `Failed to import prompts: ${error}`);
      }
    };
    input.click();
  };

  // Reset form
  const resetForm = () => {
    setFormName("");
    setFormDescription("");
    setFormContent("");
    setFormCategory("");
    setFormTags("");
    setFormMaxSessions(undefined);
    setFormProvider("");
    setFormModel("");
  };

  // Start editing a prompt/task
  const startEditing = (prompt: SavedPrompt) => {
    setEditingPrompt(prompt);
    setFormName(prompt.name);
    setFormDescription(prompt.description);
    setFormContent(prompt.content);
    setFormCategory(prompt.category);
    setFormTags(prompt.tags.join(", "));
    setFormMaxSessions(prompt.max_sessions ?? undefined);
    setFormProvider(prompt.provider ?? "");
    setFormModel(prompt.model ?? "");
    setIsCreating(false);
  };

  // Start creating a new prompt
  const startCreating = () => {
    resetForm();
    setEditingPrompt(null);
    setIsCreating(true);
  };

  // Filter prompts
  const filteredPrompts = prompts.filter((p) => {
    const matchesSearch =
      !searchQuery ||
      p.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      p.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
      p.tags.some((t) => t.toLowerCase().includes(searchQuery.toLowerCase()));

    const matchesCategory = !selectedCategory || p.category === selectedCategory;

    return matchesSearch && matchesCategory;
  });

  // Format date
  const formatDate = (dateStr: string) => {
    try {
      return new Date(dateStr).toLocaleDateString();
    } catch {
      return dateStr;
    }
  };

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <BookOpen className="w-6 h-6 text-primary" />
          <h2 className="text-xl font-semibold">Task Library</h2>
          <span className="text-sm text-muted-foreground">({prompts.length} tasks)</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={importPrompts}
            className="btn-secondary flex items-center gap-2 px-3 py-2 text-sm"
          >
            <Upload className="w-4 h-4" />
            Import
          </button>
          <button
            onClick={exportPrompts}
            className="btn-secondary flex items-center gap-2 px-3 py-2 text-sm"
          >
            <Download className="w-4 h-4" />
            Export
          </button>
          <button
            onClick={startCreating}
            className="btn-primary flex items-center gap-2 px-3 py-2 text-sm"
          >
            <Plus className="w-4 h-4" />
            New Task
          </button>
        </div>
      </div>

      {/* Search and Filter */}
      <div className="flex items-center gap-4">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 transform -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search tasks..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-10 pr-4 py-2 bg-card border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
          />
        </div>
        <select
          value={selectedCategory || ""}
          onChange={(e) => setSelectedCategory(e.target.value || null)}
          className="px-4 py-2 bg-card border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
        >
          <option value="">All Categories</option>
          {categories.map((cat) => (
            <option key={cat} value={cat}>
              {cat}
            </option>
          ))}
        </select>
      </div>

      {/* Context Selection - compact mode for selecting contexts to include when running tasks */}
      <div className="card p-4 border border-border">
        <ContextSelector
          selection={contextSelection}
          onSelectionChange={setContextSelection}
          compact={true}
        />
      </div>

      {/* Create/Edit Form */}
      {(isCreating || editingPrompt) && (
        <div className="card p-6 space-y-4 border-2 border-primary/30">
          <div className="flex items-center justify-between">
            <h3 className="text-lg font-semibold flex items-center gap-2">
              <Sparkles className="w-5 h-5 text-primary" />
              {isCreating ? "Create New Task" : "Edit Task"}
            </h3>
            <button
              onClick={() => {
                setIsCreating(false);
                setEditingPrompt(null);
                resetForm();
              }}
              className="p-1 hover:bg-card rounded"
            >
              <X className="w-5 h-5" />
            </button>
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium mb-1">Name *</label>
              <input
                type="text"
                value={formName}
                onChange={(e) => setFormName(e.target.value)}
                placeholder="My Task"
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>
            <div>
              <label className="block text-sm font-medium mb-1">Category</label>
              <input
                type="text"
                value={formCategory}
                onChange={(e) => setFormCategory(e.target.value)}
                placeholder="Development, Testing, etc."
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                list="category-suggestions"
              />
              <datalist id="category-suggestions">
                {categories.map((cat) => (
                  <option key={cat} value={cat} />
                ))}
              </datalist>
            </div>
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">Description</label>
            <input
              type="text"
              value={formDescription}
              onChange={(e) => setFormDescription(e.target.value)}
              placeholder="What does this task do?"
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">Task Content *</label>
            <textarea
              value={formContent}
              onChange={(e) => setFormContent(e.target.value)}
              placeholder="Enter the task instructions..."
              rows={10}
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 font-mono text-sm"
            />
            <p className="text-xs text-muted-foreground mt-1">
              Task runs until [TASK_COMPLETE] marker is found in the output.
            </p>
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium mb-1">Tags (comma-separated)</label>
              <input
                type="text"
                value={formTags}
                onChange={(e) => setFormTags(e.target.value)}
                placeholder="improvement, linting, refactoring"
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>
          </div>

          {/* Optional Max Sessions Limit */}
          <div className="border border-border rounded-lg p-4 space-y-3">
            <div className="flex items-center justify-between">
              <label className="text-sm font-medium">Max Sessions (optional)</label>
              <input
                type="number"
                value={formMaxSessions || ""}
                onChange={(e) =>
                  setFormMaxSessions(e.target.value ? parseInt(e.target.value) : undefined)
                }
                min={1}
                placeholder="No limit"
                className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 text-sm"
              />
            </div>
            <p className="text-xs text-muted-foreground">
              Limit the number of AI sessions that can be spawned for this task. Leave empty for no
              limit (task continues until [TASK_COMPLETE]).
            </p>
          </div>

          {/* AI Provider Override (Optional) */}
          <div className="border border-border rounded-lg p-4 space-y-4">
            <div className="flex items-center gap-2">
              <Sparkles className="w-4 h-4 text-primary" />
              <label className="text-sm font-medium">AI Provider Override (optional)</label>
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div>
                <label className="block text-xs text-muted-foreground mb-1">Provider</label>
                <select
                  value={formProvider}
                  onChange={(e) => {
                    setFormProvider(e.target.value);
                    setFormModel(""); // Reset model when provider changes
                  }}
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 text-sm"
                >
                  {PROVIDER_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>
              {formProvider && MODELS_BY_PROVIDER[formProvider] && (
                <div>
                  <label className="block text-xs text-muted-foreground mb-1">Model</label>
                  <select
                    value={formModel}
                    onChange={(e) => setFormModel(e.target.value)}
                    className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 text-sm"
                  >
                    {MODELS_BY_PROVIDER[formProvider].map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                </div>
              )}
            </div>
            <p className="text-xs text-muted-foreground">
              Override the default AI provider for this task. Use Gemini Flash for fast,
              cost-effective tasks like linting. Leave empty to use the provider configured in
              Settings.
            </p>
          </div>

          <div className="flex justify-end gap-2">
            <button
              onClick={() => {
                setIsCreating(false);
                setEditingPrompt(null);
                resetForm();
              }}
              className="btn-secondary px-4 py-2"
            >
              Cancel
            </button>
            <button
              onClick={isCreating ? createPrompt : updatePrompt}
              disabled={!formName || !formContent}
              className="btn-primary flex items-center gap-2 px-4 py-2 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Save className="w-4 h-4" />
              {isCreating ? "Create" : "Save"}
            </button>
          </div>
        </div>
      )}

      {/* Active Task Runs */}
      {taskRuns.length > 0 && (
        <div className="card p-4 space-y-3 border-2 border-blue-500/30">
          <div className="flex items-center justify-between">
            <h3 className="text-lg font-semibold flex items-center gap-2">
              <Sparkles className="w-5 h-5 text-blue-500" />
              Active Tasks
              <span className="text-sm text-muted-foreground">({taskRuns.length})</span>
            </h3>
            <div className="flex items-center gap-1">
              <button onClick={loadTaskRuns} className="btn-secondary p-2" title="Refresh">
                <RefreshCw className="w-4 h-4" />
              </button>
              <button onClick={deleteAllTaskRuns} className="btn-danger p-2" title="Delete All">
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
          <div className="space-y-2">
            {taskRuns.map((run) => (
              <TaskRunCard
                key={run.id}
                run={run}
                onStop={() => stopTask(run.id)}
                onDelete={() => deleteTaskRun(run.id)}
              />
            ))}
          </div>
        </div>
      )}

      {/* Tasks List */}
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="w-8 h-8 animate-spin text-primary" />
        </div>
      ) : filteredPrompts.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          {prompts.length === 0 ? (
            <div className="space-y-2">
              <BookOpen className="w-12 h-12 mx-auto opacity-50" />
              <p>No tasks yet. Create your first task to get started!</p>
            </div>
          ) : (
            <p>No tasks match your search.</p>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          {filteredPrompts.map((prompt) => (
            <TaskCard
              key={prompt.id}
              task={prompt}
              isRunning={runningPromptId === prompt.id}
              onRun={() => runPrompt(prompt)}
              onEdit={() => startEditing(prompt)}
              onDelete={() => deletePrompt(prompt.id, prompt.name)}
              onDuplicate={() => duplicatePrompt(prompt.id)}
              formatDate={formatDate}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// Task Card Component
interface TaskCardProps {
  task: SavedPrompt;
  isRunning: boolean;
  onRun: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onDuplicate: () => void;
  formatDate: (date: string) => string;
}

function TaskCard({
  task,
  isRunning,
  onRun,
  onEdit,
  onDelete,
  onDuplicate,
  formatDate,
}: TaskCardProps) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="card p-4 hover:border-primary/30 transition-colors">
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="font-semibold truncate">{task.name}</h3>
            {task.category && (
              <span className="px-2 py-0.5 text-xs bg-primary/10 text-primary rounded-full flex items-center gap-1">
                <FolderOpen className="w-3 h-3" />
                {task.category}
              </span>
            )}
            {task.max_sessions && (
              <span className="px-2 py-0.5 text-xs bg-blue-500/10 text-blue-500 rounded-full">
                Max {task.max_sessions} sessions
              </span>
            )}
            {task.provider && (
              <span className="px-2 py-0.5 text-xs bg-purple-500/10 text-purple-500 rounded-full flex items-center gap-1">
                <Sparkles className="w-3 h-3" />
                {task.provider.replace("_", " ")}
                {task.model && ` / ${task.model}`}
              </span>
            )}
          </div>
          {task.description && (
            <p className="text-sm text-muted-foreground mt-1 line-clamp-1">{task.description}</p>
          )}
          <div className="flex items-center gap-4 mt-2 text-xs text-muted-foreground">
            <span>Modified: {formatDate(task.modified_at)}</span>
          </div>
          {task.tags.length > 0 && (
            <div className="flex items-center gap-1 mt-2 flex-wrap">
              <Tag className="w-3 h-3 text-muted-foreground" />
              {task.tags.map((tag) => (
                <span
                  key={tag}
                  className="px-2 py-0.5 text-xs bg-card border border-border rounded"
                >
                  {tag}
                </span>
              ))}
            </div>
          )}
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={onRun}
            disabled={isRunning}
            className="btn-success p-2 disabled:opacity-50"
            title="Run task"
          >
            {isRunning ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Play className="w-4 h-4" />
            )}
          </button>
          <button
            onClick={() => setExpanded(!expanded)}
            className="btn-secondary p-2"
            title={expanded ? "Collapse" : "Expand"}
          >
            {expanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
          </button>
          <button onClick={onEdit} className="btn-secondary p-2" title="Edit task">
            <Pencil className="w-4 h-4" />
          </button>
          <button onClick={onDuplicate} className="btn-secondary p-2" title="Duplicate task">
            <Copy className="w-4 h-4" />
          </button>
          <button onClick={onDelete} className="btn-danger p-2" title="Delete task">
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {expanded && (
        <div className="mt-4 pt-4 border-t border-border">
          <pre className="p-3 bg-background rounded-lg text-sm font-mono whitespace-pre-wrap overflow-x-auto max-h-96">
            {task.content}
          </pre>
        </div>
      )}
    </div>
  );
}

// Task Run Card Component - shows running tasks with status
interface TaskRunCardProps {
  run: TaskRun;
  onStop: () => void;
  onDelete: () => void;
}

function TaskRunCard({ run, onStop, onDelete }: TaskRunCardProps) {
  const [expanded, setExpanded] = useState(false);
  const [aiOutputLogs, setAiOutputLogs] = useState<
    { line: string; source: string; timestamp: number }[]
  >([]);

  // Subscribe to log updates and filter AI output for this task
  useEffect(() => {
    const updateLogs = () => {
      const allLogs = logManager.getAiOutputLogs();
      const taskPrefix = `task-${run.id.slice(0, 8)}`;
      const filteredLogs = allLogs.filter(
        (log) => log.actionId?.startsWith(taskPrefix) && log.source === "claude",
      );
      setAiOutputLogs(
        filteredLogs.map((log) => ({
          line: log.line,
          source: log.source,
          timestamp: log.timestamp,
        })),
      );
    };

    updateLogs();
    const unsubscribe = logManager.subscribe(updateLogs);
    return () => unsubscribe();
  }, [run.id]);

  const getStatusIcon = () => {
    switch (run.status) {
      case "running":
        return <Loader2 className="w-4 h-4 animate-spin text-green-500" />;
      case "complete":
        return <CheckCircle className="w-4 h-4 text-green-500" />;
      case "failed":
        return <AlertCircle className="w-4 h-4 text-red-500" />;
      case "stopped":
        return <Square className="w-4 h-4 text-orange-500" />;
      default:
        return <Clock className="w-4 h-4 text-muted-foreground" />;
    }
  };

  const getStatusColor = () => {
    switch (run.status) {
      case "running":
        return "text-green-500";
      case "complete":
        return "text-green-500";
      case "failed":
        return "text-red-500";
      case "stopped":
        return "text-orange-500";
      default:
        return "text-muted-foreground";
    }
  };

  const formatTimestamp = (ts: number) => {
    return new Date(ts * 1000).toLocaleTimeString();
  };

  return (
    <div className="p-3 bg-background border border-border rounded-lg">
      <div className="flex items-center justify-between gap-4">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            {getStatusIcon()}
            <h4 className="font-medium truncate">{run.task_name}</h4>
            <span className={`text-sm ${getStatusColor()}`}>
              {run.status === "running"
                ? `Running (session ${run.sessions_count})`
                : run.status.charAt(0).toUpperCase() + run.status.slice(1)}
            </span>
          </div>
          <div className="flex items-center gap-4 mt-1 text-xs text-muted-foreground">
            <span>
              Sessions: {run.sessions_count}
              {run.max_sessions && ` / ${run.max_sessions}`}
            </span>
            <span>Started: {formatTimestamp(run.started_at)}</span>
          </div>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setExpanded(!expanded)}
            className="btn-secondary p-2"
            title={expanded ? "Hide log" : "Show log"}
          >
            {expanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
          </button>
          {run.status === "running" && (
            <button onClick={onStop} className="btn-danger p-2" title="Stop task">
              <Square className="w-4 h-4" />
            </button>
          )}
          <button
            onClick={onDelete}
            className="btn-secondary p-2 hover:text-red-500"
            title="Delete record"
          >
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {expanded && (
        <div className="mt-3 pt-3 border-t border-border space-y-3">
          {aiOutputLogs.length > 0 && (
            <div>
              <h5 className="text-xs font-semibold text-muted-foreground mb-2">AI Output</h5>
              <div className="max-h-60 overflow-y-auto space-y-1 bg-muted/30 p-2 rounded">
                {aiOutputLogs.map((log, idx) => (
                  <div key={idx} className="text-sm font-mono">
                    <span className="text-foreground">{log.line}</span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {run.event_log.length > 0 && (
            <div>
              <h5 className="text-xs font-semibold text-muted-foreground mb-2">Task Events</h5>
              <div className="max-h-40 overflow-y-auto space-y-1">
                {run.event_log.slice(-10).map((event, idx) => (
                  <div key={idx} className="text-xs font-mono flex gap-2">
                    <span className="text-muted-foreground">
                      {formatTimestamp(event.timestamp)}
                    </span>
                    <span className="text-blue-500">[{event.event_type}]</span>
                    <span>{event.message}</span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {run.error_message && (
        <div className="mt-2 p-2 bg-red-500/10 text-red-500 text-sm rounded">
          {run.error_message}
        </div>
      )}
    </div>
  );
}

export default PromptLibraryTab;
