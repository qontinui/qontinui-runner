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
  Workflow,
} from "lucide-react";

// Types for workflow configuration
interface WorkflowConfig {
  enabled: boolean;
  checkpoint_path: string;
  phase_field: string;
  completion_value: number;
  stall_threshold_secs: number;
  continuation_prompt: string;
  auto_continue: boolean; // Default: true - auto-continue on runner restart
}

// Types for the prompt library
interface SavedPrompt {
  id: string;
  name: string;
  description: string;
  content: string;
  category: string;
  tags: string[];
  max_iterations: number;
  workflow: WorkflowConfig;
  created_at: string;
  modified_at: string;
}

// Types for unified sessions (replaces WorkflowRun)
interface SessionCheckpoint {
  session_id: string;
  session_type: string;
  current_phase: number;
  total_phases: number;
  completed: boolean;
  status: string;
  started_at: string;
  last_activity: string;
  sessions_spawned: number;
  restart_permitted: boolean;
  error_message: string | null;
  custom_data: Record<string, unknown>;
  activity_log: string[];
}

interface SessionConfig {
  session_type: string;
  prompt: string;
  continuation_prompt: string | null;
  total_phases: number;
  uses_gui: boolean;
  timeout_seconds: number;
  stall_threshold_seconds: number;
  name: string;
  description: string;
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

// Legacy type alias for backward compatibility in UI
interface WorkflowRun {
  id: string;
  prompt_id: string;
  prompt_name: string;
  status: "running" | "idle" | "stalled" | "completed" | "failed";
  current_phase: number;
  completion_value: number;
  checkpoint_path: string;
  active_session_id: string | null;
  started_at: number;
  last_checkpoint_update: number | null;
  last_activity: number;
  sessions_spawned: number;
  error_message: string | null;
  event_log: { timestamp: number; event_type: string; message: string }[];
}

// Convert Session to WorkflowRun for UI compatibility
function sessionToWorkflowRun(session: Session, promptId?: string): WorkflowRun {
  const statusMap: Record<string, WorkflowRun["status"]> = {
    starting: "running",
    running: "running",
    completed: "completed",
    failed: "failed",
    stopped: "failed",
    waiting_for_continuation: "idle",
    stalled: "stalled",
  };

  return {
    id: session.id,
    prompt_id: promptId || session.id,
    prompt_name: session.config.name,
    status: statusMap[session.status] || "running",
    current_phase: session.checkpoint.current_phase,
    completion_value: session.config.total_phases,
    checkpoint_path: "",
    active_session_id: session.active_subprocess_id,
    started_at: new Date(session.checkpoint.started_at).getTime() / 1000,
    last_checkpoint_update: null,
    last_activity: new Date(session.checkpoint.last_activity).getTime() / 1000,
    sessions_spawned: session.checkpoint.sessions_spawned,
    error_message: session.checkpoint.error_message,
    event_log: session.event_log,
  };
}

interface SpawnResponse {
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
  const [_allTags, _setAllTags] = useState<string[]>([]);

  // Edit modal state
  const [editingPrompt, setEditingPrompt] = useState<SavedPrompt | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Form state for create/edit
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formTags, setFormTags] = useState("");
  const [formMaxIterations, setFormMaxIterations] = useState(10);

  // Workflow form state
  const [formWorkflowEnabled, setFormWorkflowEnabled] = useState(false);
  const [formCheckpointPath, setFormCheckpointPath] = useState("");
  const [formPhaseField, setFormPhaseField] = useState("current_phase");
  const [formCompletionValue, setFormCompletionValue] = useState(12);
  const [formStallThreshold, setFormStallThreshold] = useState(300);
  const [formContinuationPrompt, setFormContinuationPrompt] = useState("");

  // Running state
  const [runningPromptId, setRunningPromptId] = useState<string | null>(null);

  // Workflow runs state
  const [workflowRuns, setWorkflowRuns] = useState<WorkflowRun[]>([]);
  const [showWorkflowRuns, setShowWorkflowRuns] = useState(false);

  // Load prompts on mount
  useEffect(() => {
    loadPrompts();
    loadCategories();
    loadTags();
    loadWorkflowRuns();
  }, []);

  // Periodically refresh workflow runs
  useEffect(() => {
    if (showWorkflowRuns) {
      const interval = setInterval(loadWorkflowRuns, 5000);
      return () => clearInterval(interval);
    }
  }, [showWorkflowRuns]);

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

  const loadTags = async () => {
    try {
      const response = await fetch(`${API_BASE}/prompts/tags`);
      const result = await response.json();
      if (result.success) {
        _setAllTags(result.data || []);
      }
    } catch (error) {
      console.error("Failed to load tags:", error);
    }
  };

  const loadWorkflowRuns = async () => {
    try {
      // Use new unified sessions API
      const response = await fetch(`${API_BASE}/sessions`);
      const result = await response.json();
      if (result.success) {
        // Filter to only prompt_workflow sessions and convert to WorkflowRun format
        const sessions: Session[] = result.data || [];
        const promptWorkflows = sessions
          .filter((s: Session) => s.config.session_type === "prompt_workflow")
          .map((s: Session) => sessionToWorkflowRun(s));
        setWorkflowRuns(promptWorkflows);
      }
    } catch (error) {
      console.error("Failed to load workflow runs:", error);
    }
  };

  // Create a new prompt
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
          max_iterations: formMaxIterations,
          workflow: formWorkflowEnabled
            ? {
                enabled: true,
                checkpoint_path: formCheckpointPath,
                phase_field: formPhaseField,
                completion_value: formCompletionValue,
                stall_threshold_secs: formStallThreshold,
                continuation_prompt: formContinuationPrompt,
              }
            : undefined,
        }),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Created prompt: ${formName}`);
        resetForm();
        setIsCreating(false);
        loadPrompts();
        loadCategories();
        loadTags();
      } else {
        onLog("error", `Failed to create prompt: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to create prompt: ${error}`);
    }
  };

  // Update an existing prompt
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
          max_iterations: formMaxIterations,
          workflow: {
            enabled: formWorkflowEnabled,
            checkpoint_path: formCheckpointPath,
            phase_field: formPhaseField,
            completion_value: formCompletionValue,
            stall_threshold_secs: formStallThreshold,
            continuation_prompt: formContinuationPrompt,
          },
        }),
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Updated prompt: ${formName}`);
        resetForm();
        setEditingPrompt(null);
        loadPrompts();
        loadCategories();
        loadTags();
      } else {
        onLog("error", `Failed to update prompt: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to update prompt: ${error}`);
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

  // Run a prompt
  const runPrompt = async (prompt: SavedPrompt) => {
    setRunningPromptId(prompt.id);
    onLog("info", `Running prompt: ${prompt.name}`);

    try {
      const response = await fetch(`${API_BASE}/prompts/${prompt.id}/run`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      const result = await response.json();
      if (result.success) {
        const data = result.data as SpawnResponse;
        onLog("success", `Started AI session: ${data.session_id}`);
        onLog("info", `Log file: ${data.log_file}`);
      } else {
        onLog("error", `Failed to run prompt: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to run prompt: ${error}`);
    } finally {
      setRunningPromptId(null);
    }
  };

  // Run a prompt as a workflow (multi-session with automatic continuation)
  const runWorkflow = async (prompt: SavedPrompt) => {
    if (!prompt.workflow?.enabled) {
      onLog("error", "Workflow mode is not enabled for this prompt");
      return;
    }

    setRunningPromptId(prompt.id);
    onLog("info", `Starting workflow: ${prompt.name}`);

    try {
      // Use new unified sessions API
      const response = await fetch(`${API_BASE}/sessions/start`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          session_type: "prompt_workflow",
          name: prompt.name,
          prompt: prompt.content,
          continuation_prompt: prompt.workflow.continuation_prompt || null,
          total_phases: prompt.workflow.completion_value,
          uses_gui: false,
          timeout_seconds: 1800,
          // Multi-session workflow config (runner handles continuation)
          checkpoint_path: prompt.workflow.enabled ? prompt.workflow.checkpoint_path : null,
          phase_field: prompt.workflow.phase_field || "current_phase",
          completion_value: prompt.workflow.enabled ? prompt.workflow.completion_value : null,
        }),
      });
      const result = await response.json();
      if (result.success) {
        const session = result.data?.session as Session;
        onLog("success", `Started workflow: ${session.id}`);
        setShowWorkflowRuns(true);
        loadWorkflowRuns();
      } else {
        onLog("error", `Failed to start workflow: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to start workflow: ${error}`);
    } finally {
      setRunningPromptId(null);
    }
  };

  // Stop a running workflow
  const stopWorkflow = async (runId: string) => {
    try {
      // Use new unified sessions API
      const response = await fetch(`${API_BASE}/sessions/${runId}/stop`, {
        method: "POST",
      });
      const result = await response.json();
      if (result.success) {
        onLog("success", `Stopped workflow: ${runId}`);
        loadWorkflowRuns();
      } else {
        onLog("error", `Failed to stop workflow: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to stop workflow: ${error}`);
    }
  };

  // Delete a workflow run record
  const deleteWorkflow = async (runId: string) => {
    try {
      // Use new unified sessions API
      const response = await fetch(`${API_BASE}/sessions/${runId}`, {
        method: "DELETE",
      });
      const result = await response.json();
      if (result.success) {
        loadWorkflowRuns();
      } else {
        onLog("error", `Failed to delete workflow: ${result.error}`);
      }
    } catch (error) {
      onLog("error", `Failed to delete workflow: ${error}`);
    }
  };

  // Delete all workflow run records
  const deleteAllWorkflows = async () => {
    try {
      // Get all sessions and delete prompt_workflow ones
      const listResponse = await fetch(`${API_BASE}/sessions`);
      const listResult = await listResponse.json();
      if (!listResult.success) {
        onLog("error", `Failed to list sessions: ${listResult.error}`);
        return;
      }

      const sessions: Session[] = listResult.data || [];
      const promptWorkflows = sessions.filter(
        (s: Session) => s.config.session_type === "prompt_workflow",
      );

      let deletedCount = 0;
      for (const session of promptWorkflows) {
        const response = await fetch(`${API_BASE}/sessions/${session.id}`, {
          method: "DELETE",
        });
        const result = await response.json();
        if (result.success) {
          deletedCount++;
        }
      }

      onLog("success", `Deleted ${deletedCount} workflow(s)`);
      loadWorkflowRuns();
    } catch (error) {
      onLog("error", `Failed to delete workflows: ${error}`);
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
          loadTags();
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
    setFormMaxIterations(10);
    // Reset workflow fields
    setFormWorkflowEnabled(false);
    setFormCheckpointPath("");
    setFormPhaseField("current_phase");
    setFormCompletionValue(12);
    setFormStallThreshold(300);
    setFormContinuationPrompt("");
  };

  // Start editing a prompt
  const startEditing = (prompt: SavedPrompt) => {
    setEditingPrompt(prompt);
    setFormName(prompt.name);
    setFormDescription(prompt.description);
    setFormContent(prompt.content);
    setFormCategory(prompt.category);
    setFormTags(prompt.tags.join(", "));
    setFormMaxIterations(prompt.max_iterations);
    // Load workflow fields
    setFormWorkflowEnabled(prompt.workflow?.enabled || false);
    setFormCheckpointPath(prompt.workflow?.checkpoint_path || "");
    setFormPhaseField(prompt.workflow?.phase_field || "current_phase");
    setFormCompletionValue(prompt.workflow?.completion_value || 12);
    setFormStallThreshold(prompt.workflow?.stall_threshold_secs || 300);
    setFormContinuationPrompt(prompt.workflow?.continuation_prompt || "");
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
          <h2 className="text-xl font-semibold">Prompt Library</h2>
          <span className="text-sm text-muted-foreground">({prompts.length} prompts)</span>
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
            New Prompt
          </button>
        </div>
      </div>

      {/* Search and Filter */}
      <div className="flex items-center gap-4">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 transform -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search prompts..."
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

      {/* Create/Edit Form */}
      {(isCreating || editingPrompt) && (
        <div className="card p-6 space-y-4 border-2 border-primary/30">
          <div className="flex items-center justify-between">
            <h3 className="text-lg font-semibold flex items-center gap-2">
              <Sparkles className="w-5 h-5 text-primary" />
              {isCreating ? "Create New Prompt" : "Edit Prompt"}
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
                placeholder="My Prompt"
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
              placeholder="What does this prompt do?"
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-1">Prompt Content *</label>
            <textarea
              value={formContent}
              onChange={(e) => setFormContent(e.target.value)}
              placeholder="Enter the prompt content..."
              rows={10}
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 font-mono text-sm"
            />
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
            <div>
              <label className="block text-sm font-medium mb-1">Max Iterations</label>
              <input
                type="number"
                value={formMaxIterations}
                onChange={(e) => setFormMaxIterations(parseInt(e.target.value) || 10)}
                min={1}
                max={50}
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>
          </div>

          {/* Workflow Settings */}
          <div className="border border-border rounded-lg p-4 space-y-4">
            <div className="flex items-center gap-3">
              <input
                type="checkbox"
                id="workflowEnabled"
                checked={formWorkflowEnabled}
                onChange={(e) => setFormWorkflowEnabled(e.target.checked)}
                className="w-4 h-4 rounded border-border"
              />
              <label
                htmlFor="workflowEnabled"
                className="flex items-center gap-2 text-sm font-medium cursor-pointer"
              >
                <Workflow className="w-4 h-4 text-primary" />
                Enable Multi-Session Workflow
              </label>
            </div>
            <p className="text-xs text-muted-foreground">
              When enabled, the runner will automatically spawn new AI sessions to continue the
              workflow based on checkpoint progress. This allows long-running tasks to complete
              across multiple sessions.
            </p>

            {formWorkflowEnabled && (
              <div className="space-y-4 pt-2">
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Checkpoint Path *</label>
                    <input
                      type="text"
                      value={formCheckpointPath}
                      onChange={(e) => setFormCheckpointPath(e.target.value)}
                      placeholder="C:/path/to/checkpoint.json"
                      className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 font-mono text-sm"
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Phase Field</label>
                    <input
                      type="text"
                      value={formPhaseField}
                      onChange={(e) => setFormPhaseField(e.target.value)}
                      placeholder="current_phase"
                      className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                    />
                  </div>
                </div>
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Completion Value</label>
                    <input
                      type="number"
                      value={formCompletionValue}
                      onChange={(e) => setFormCompletionValue(parseInt(e.target.value) || 12)}
                      min={1}
                      className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                    />
                    <p className="text-xs text-muted-foreground mt-1">
                      Workflow is complete when phase reaches this value
                    </p>
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">
                      Stall Threshold (seconds)
                    </label>
                    <input
                      type="number"
                      value={formStallThreshold}
                      onChange={(e) => setFormStallThreshold(parseInt(e.target.value) || 300)}
                      min={60}
                      className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                    />
                    <p className="text-xs text-muted-foreground mt-1">
                      Spawn new session if no progress for this long
                    </p>
                  </div>
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">
                    Continuation Prompt (optional)
                  </label>
                  <textarea
                    value={formContinuationPrompt}
                    onChange={(e) => setFormContinuationPrompt(e.target.value)}
                    placeholder="Custom prompt for continuation sessions. Leave empty to use a default prompt that references the checkpoint."
                    rows={3}
                    className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 font-mono text-sm"
                  />
                </div>
              </div>
            )}
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

      {/* Active Workflow Runs */}
      {workflowRuns.length > 0 && (
        <div className="card p-4 space-y-3 border-2 border-blue-500/30">
          <div className="flex items-center justify-between">
            <h3 className="text-lg font-semibold flex items-center gap-2">
              <Workflow className="w-5 h-5 text-blue-500" />
              Active Workflows
              <span className="text-sm text-muted-foreground">({workflowRuns.length})</span>
            </h3>
            <div className="flex items-center gap-1">
              <button onClick={loadWorkflowRuns} className="btn-secondary p-2" title="Refresh">
                <RefreshCw className="w-4 h-4" />
              </button>
              <button onClick={deleteAllWorkflows} className="btn-danger p-2" title="Delete All">
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          </div>
          <div className="space-y-2">
            {workflowRuns.map((run) => (
              <WorkflowRunCard
                key={run.id}
                run={run}
                onStop={() => stopWorkflow(run.id)}
                onDelete={() => deleteWorkflow(run.id)}
              />
            ))}
          </div>
        </div>
      )}

      {/* Prompts List */}
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="w-8 h-8 animate-spin text-primary" />
        </div>
      ) : filteredPrompts.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          {prompts.length === 0 ? (
            <div className="space-y-2">
              <BookOpen className="w-12 h-12 mx-auto opacity-50" />
              <p>No prompts yet. Create your first prompt to get started!</p>
            </div>
          ) : (
            <p>No prompts match your search.</p>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          {filteredPrompts.map((prompt) => (
            <PromptCard
              key={prompt.id}
              prompt={prompt}
              isRunning={runningPromptId === prompt.id}
              onRun={() => runPrompt(prompt)}
              onRunWorkflow={() => runWorkflow(prompt)}
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

// Prompt Card Component
interface PromptCardProps {
  prompt: SavedPrompt;
  isRunning: boolean;
  onRun: () => void;
  onRunWorkflow: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onDuplicate: () => void;
  formatDate: (date: string) => string;
}

function PromptCard({
  prompt,
  isRunning,
  onRun,
  onRunWorkflow,
  onEdit,
  onDelete,
  onDuplicate,
  formatDate,
}: PromptCardProps) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div
      className={`card p-4 hover:border-primary/30 transition-colors ${prompt.workflow?.enabled ? "border-l-4 border-l-blue-500" : ""}`}
    >
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="font-semibold truncate">{prompt.name}</h3>
            {prompt.workflow?.enabled && (
              <span className="px-2 py-0.5 text-xs bg-blue-500/10 text-blue-500 rounded-full flex items-center gap-1">
                <Workflow className="w-3 h-3" />
                Workflow
              </span>
            )}
            {prompt.category && (
              <span className="px-2 py-0.5 text-xs bg-primary/10 text-primary rounded-full flex items-center gap-1">
                <FolderOpen className="w-3 h-3" />
                {prompt.category}
              </span>
            )}
          </div>
          {prompt.description && (
            <p className="text-sm text-muted-foreground mt-1 line-clamp-1">{prompt.description}</p>
          )}
          <div className="flex items-center gap-4 mt-2 text-xs text-muted-foreground">
            <span>Max iterations: {prompt.max_iterations}</span>
            <span>Modified: {formatDate(prompt.modified_at)}</span>
          </div>
          {prompt.tags.length > 0 && (
            <div className="flex items-center gap-1 mt-2 flex-wrap">
              <Tag className="w-3 h-3 text-muted-foreground" />
              {prompt.tags.map((tag) => (
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
          {/* Regular Run button - always available */}
          <button
            onClick={onRun}
            disabled={isRunning}
            className="btn-success p-2 disabled:opacity-50"
            title="Run prompt once"
          >
            {isRunning ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Play className="w-4 h-4" />
            )}
          </button>
          {/* Workflow button - only for workflow-enabled prompts */}
          {prompt.workflow?.enabled && (
            <button
              onClick={onRunWorkflow}
              disabled={isRunning}
              className="btn-primary p-2 disabled:opacity-50 flex items-center gap-1"
              title="Run as workflow (multi-session with auto-continuation)"
            >
              <Workflow className="w-4 h-4" />
            </button>
          )}
          <button
            onClick={() => setExpanded(!expanded)}
            className="btn-secondary p-2"
            title={expanded ? "Collapse" : "Expand"}
          >
            {expanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
          </button>
          <button onClick={onEdit} className="btn-secondary p-2" title="Edit prompt">
            <Pencil className="w-4 h-4" />
          </button>
          <button onClick={onDuplicate} className="btn-secondary p-2" title="Duplicate prompt">
            <Copy className="w-4 h-4" />
          </button>
          <button onClick={onDelete} className="btn-danger p-2" title="Delete prompt">
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {expanded && (
        <div className="mt-4 pt-4 border-t border-border">
          <pre className="p-3 bg-background rounded-lg text-sm font-mono whitespace-pre-wrap overflow-x-auto max-h-96">
            {prompt.content}
          </pre>
        </div>
      )}
    </div>
  );
}

// Workflow Run Card Component
interface WorkflowRunCardProps {
  run: WorkflowRun;
  onStop: () => void;
  onDelete: () => void;
}

function WorkflowRunCard({ run, onStop, onDelete }: WorkflowRunCardProps) {
  const [expanded, setExpanded] = useState(false);
  const [aiOutputLogs, setAiOutputLogs] = useState<
    { line: string; source: string; timestamp: number }[]
  >([]);

  // Subscribe to log updates and filter AI output for this workflow
  useEffect(() => {
    const updateLogs = () => {
      const allLogs = logManager.getAiOutputLogs();
      // Filter logs that match this workflow's action IDs (workflow-{id.slice(0,8)})
      const workflowPrefix = `workflow-${run.id.slice(0, 8)}`;
      const filteredLogs = allLogs.filter(
        (log) => log.actionId?.startsWith(workflowPrefix) && log.source === "claude",
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
      case "idle":
        return <Clock className="w-4 h-4 text-yellow-500" />;
      case "stalled":
        return <AlertCircle className="w-4 h-4 text-orange-500" />;
      case "completed":
        return <CheckCircle className="w-4 h-4 text-green-500" />;
      case "failed":
        return <AlertCircle className="w-4 h-4 text-red-500" />;
      default:
        return <Clock className="w-4 h-4 text-muted-foreground" />;
    }
  };

  const getStatusColor = () => {
    switch (run.status) {
      case "running":
        return "text-green-500";
      case "idle":
        return "text-yellow-500";
      case "stalled":
        return "text-orange-500";
      case "completed":
        return "text-green-500";
      case "failed":
        return "text-red-500";
      default:
        return "text-muted-foreground";
    }
  };

  const formatTimestamp = (ts: number) => {
    return new Date(ts * 1000).toLocaleTimeString();
  };

  const progress =
    run.completion_value > 0 ? Math.round((run.current_phase / run.completion_value) * 100) : 0;

  return (
    <div className="p-3 bg-background border border-border rounded-lg">
      <div className="flex items-center justify-between gap-4">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            {getStatusIcon()}
            <h4 className="font-medium truncate">{run.prompt_name}</h4>
            <span className={`text-sm ${getStatusColor()}`}>
              {run.status.charAt(0).toUpperCase() + run.status.slice(1)}
            </span>
          </div>
          <div className="flex items-center gap-4 mt-1 text-xs text-muted-foreground">
            <span>
              Phase: {run.current_phase}/{run.completion_value}
            </span>
            <span>Sessions: {run.sessions_spawned}</span>
            <span>Started: {formatTimestamp(run.started_at)}</span>
          </div>
          {/* Progress bar */}
          <div className="mt-2 w-full bg-border rounded-full h-1.5">
            <div
              className="bg-blue-500 h-1.5 rounded-full transition-all"
              style={{ width: `${progress}%` }}
            />
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
          {(run.status === "running" || run.status === "idle" || run.status === "stalled") && (
            <button onClick={onStop} className="btn-danger p-2" title="Stop workflow">
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
          {/* AI Output Section */}
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

          {/* Workflow Events Section */}
          {run.event_log.length > 0 && (
            <div>
              <h5 className="text-xs font-semibold text-muted-foreground mb-2">Workflow Events</h5>
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
