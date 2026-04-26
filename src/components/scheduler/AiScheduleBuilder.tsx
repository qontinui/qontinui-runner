/**
 * AiScheduleBuilder
 *
 * Natural language interface for creating scheduled tasks.
 * Users describe what they want in plain text, select/order workflows,
 * and AI generates the full schedule configuration.
 */

import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import {
  Sparkles,
  Loader2,
  X,
  GripVertical,
  Plus,
  Trash2,
  ChevronDown,
  Search,
  Star,
  Terminal,
  ArrowRight,
  Check,
  Pencil,
} from "lucide-react";
import type { CreateScheduledTaskRequest } from "../../types/scheduler";
import type { UnifiedWorkflow } from "../../types/unified-workflow";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ============================================================================
// Types
// ============================================================================

interface AiScheduleBuilderProps {
  onSubmit: (requests: CreateScheduledTaskRequest[]) => void;
  onCancel: () => void;
  onEditManually: (request: CreateScheduledTaskRequest) => void;
  loading: boolean;
}

interface SelectedWorkflow {
  id: string;
  name: string;
  description?: string;
  category?: string;
  isFavorite?: boolean | null;
}

interface AiGeneratedTask {
  request: CreateScheduledTaskRequest;
  description: string;
}

// ============================================================================
// Prompt Templates
// ============================================================================

const PROMPT_TEMPLATES = [
  {
    label: "After inactivity",
    prompt: "Run after 30 minutes of repo inactivity, only when the runner is idle",
  },
  {
    label: "Nightly",
    prompt: "Run every night at 2 AM",
  },
  {
    label: "On weekdays",
    prompt: "Run every weekday at 9 AM, wait for idle before starting",
  },
  {
    label: "Hourly check",
    prompt: "Run every hour during business hours (9-5)",
  },
  {
    label: "After deploy",
    prompt: "Run once all repos are idle for 10 minutes, then repeat every 2 hours",
  },
];

// ============================================================================
// Component
// ============================================================================

export function AiScheduleBuilder({
  onSubmit,
  onCancel,
  onEditManually,
  loading,
}: AiScheduleBuilderProps) {
  // NL input
  const [prompt, setPrompt] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);

  // Generated result
  const [generatedTasks, setGeneratedTasks] = useState<AiGeneratedTask[] | null>(null);

  // Workflow selection
  const [workflows, setWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [loadingWorkflows, setLoadingWorkflows] = useState(false);
  const [selectedWorkflows, setSelectedWorkflows] = useState<SelectedWorkflow[]>([]);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [workflowSearch, setWorkflowSearch] = useState("");
  const [workflowFilter, setWorkflowFilter] = useState<"all" | "favorites" | "commands">("all");

  // Drag state
  const dragItem = useRef<number | null>(null);
  const dragOverItem = useRef<number | null>(null);

  // Fetch workflows
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;
    const loadWorkflows = async () => {
      setLoadingWorkflows(true);
      try {
        const resp = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          signal: controller.signal,
        });
        if (!resp.ok) return;
        const result = await resp.json();
        if (!cancelled && result.success && result.data) {
          setWorkflows(result.data);
        }
      } catch {
        /* ignore — aborted or network error */
      } finally {
        if (!cancelled) setLoadingWorkflows(false);
      }
    };
    loadWorkflows();
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Filtered workflows for picker
  const filteredWorkflows = useMemo(() => {
    let list = workflows;
    if (workflowFilter === "favorites") list = list.filter((w) => w.isFavorite);
    else if (workflowFilter === "commands")
      list = list.filter((w) => w.category === "slash-command");
    if (workflowSearch) {
      const q = workflowSearch.toLowerCase();
      list = list.filter(
        (w) => w.name.toLowerCase().includes(q) || w.description?.toLowerCase().includes(q),
      );
    }
    // Exclude already selected
    const selectedIds = new Set(selectedWorkflows.map((s) => s.id));
    return list.filter((w) => !selectedIds.has(w.id));
  }, [workflows, workflowFilter, workflowSearch, selectedWorkflows]);

  // Add workflow to selection
  const addWorkflow = useCallback((wf: UnifiedWorkflow) => {
    setSelectedWorkflows((prev) => [
      ...prev,
      {
        id: wf.id,
        name: wf.name,
        description: wf.description,
        category: wf.category,
        isFavorite: wf.isFavorite,
      },
    ]);
    setPickerOpen(false);
    setWorkflowSearch("");
  }, []);

  // Remove workflow from selection
  const removeWorkflow = useCallback((index: number) => {
    setSelectedWorkflows((prev) => prev.filter((_, i) => i !== index));
  }, []);

  // Drag handlers for reordering
  const handleDragStart = useCallback((index: number) => {
    dragItem.current = index;
  }, []);

  const handleDragEnter = useCallback((index: number) => {
    dragOverItem.current = index;
  }, []);

  const handleDragEnd = useCallback(() => {
    if (dragItem.current === null || dragOverItem.current === null) return;
    const from = dragItem.current;
    const to = dragOverItem.current;
    if (from === to) return;

    setSelectedWorkflows((prev) => {
      const copy = [...prev];
      const [removed] = copy.splice(from, 1);
      copy.splice(to, 0, removed);
      return copy;
    });

    dragItem.current = null;
    dragOverItem.current = null;
  }, []);

  // Build the AI prompt with workflow context
  const buildAiPrompt = useCallback(() => {
    const workflowList =
      selectedWorkflows.length > 0
        ? selectedWorkflows.map((w, i) => `  ${i + 1}. "${w.name}" (id: ${w.id})`).join("\n")
        : "(no workflows selected — user will configure task type separately)";

    return `You are a scheduling assistant. Given a natural language description, generate one or more scheduled task configurations as JSON.

The user's request: "${prompt}"

Selected workflows (in execution order):
${workflowList}

Respond ONLY with a JSON array. Each element must have this exact shape:
{
  "description": "human-readable summary of what this task does",
  "request": {
    "name": "short task name",
    "description": "optional longer description",
    "schedule": <one of the schedule types below>,
    "task": <task type>,
    "conditions": <optional conditions>,
    "skipIfCompleted": false,
    "autoFixOnFailure": false
  }
}

Schedule types (pick the best fit):
- {"type":"Cron","value":"<cron expression>"} — for recurring time-based (use 5-field cron: min hour dom month dow)
- {"type":"Once","value":"<ISO 8601 datetime>"} — for one-time
- {"type":"Interval","value":<seconds>} — for simple intervals
- {"type":"Condition","value":{"rearmDelayMinutes":<number>}} — for condition-triggered (no time, runs when conditions met)

Task type (use Workflow if workflows are selected):
- {"task_type":"Workflow","workflow_name":"<name>","workflow_id":"<id>"}
- {"task_type":"AutoFix","check_findings":true,"force_run":false}
- {"task_type":"RemoteAgent","prompt":"<full instruction>","allowed_tools":["Bash","Read","Write","Edit","Glob","Grep"],"max_turns":50,"timeout_seconds":600,"mcp_connections":[]}

Use RemoteAgent when no workflow is selected and the user describes an ad-hoc AI task (e.g., "summarize my email", "review recent commits", "draft a report"). Set the prompt to a concrete, self-contained instruction the agent can act on. Include working_directory only if the user mentions a specific project path.

Conditions (optional, include when user mentions idle/inactivity):
- {"requireIdle":{"enabled":true}} — wait for runner to be idle
- {"requireRepoInactive":{"enabled":true,"repositories":[{"path":"<path>","inactiveMinutes":<min>}]}} — wait for repo inactivity
- {"timeoutMinutes":<number>} — max wait time

If multiple workflows are selected, create one task per workflow in order.
If the user mentions sequencing or ordering, use appropriate scheduling (stagger by delay or use conditions).
Only output the JSON array, nothing else.`;
  }, [prompt, selectedWorkflows]);

  // Generate via AI
  const handleGenerate = useCallback(async () => {
    if (!prompt.trim()) {
      setAiError("Please describe when you want tasks to run");
      return;
    }

    setIsGenerating(true);
    setAiError(null);
    setGeneratedTasks(null);

    try {
      const aiPrompt = buildAiPrompt();
      const resp = await tracedFetch(`${getApiBase()}/ai/simple-query`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ prompt: aiPrompt, timeout_seconds: 30 }),
      });

      if (!resp.ok) {
        throw new Error(`AI service returned ${resp.status}`);
      }

      const result = await resp.json();

      if (!result.success || !result.response) {
        throw new Error(result.error || "AI did not return a response");
      }

      // Parse JSON from AI response (may be wrapped in markdown code fence)
      let jsonText = result.response.trim();
      const fenceMatch = jsonText.match(/```(?:json)?\s*([\s\S]*?)```/);
      if (fenceMatch) jsonText = fenceMatch[1].trim();

      const parsed = JSON.parse(jsonText);
      const tasks: AiGeneratedTask[] = Array.isArray(parsed) ? parsed : [parsed];

      // Validate structure
      for (const t of tasks) {
        if (!t.request?.name || !t.request?.schedule || !t.request?.task) {
          throw new Error("AI returned an incomplete task configuration");
        }
      }

      setGeneratedTasks(tasks);
    } catch (err) {
      setAiError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsGenerating(false);
    }
  }, [prompt, buildAiPrompt]);

  // Accept all generated tasks
  const handleAcceptAll = useCallback(() => {
    if (!generatedTasks) return;
    onSubmit(generatedTasks.map((t) => t.request));
  }, [generatedTasks, onSubmit]);

  // Edit a single generated task in the manual form
  const handleEditTask = useCallback(
    (index: number) => {
      if (!generatedTasks) return;
      onEditManually(generatedTasks[index].request);
    },
    [generatedTasks, onEditManually],
  );

  return (
    <div className="space-y-6 max-w-2xl">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Sparkles className="w-5 h-5 text-purple-400" />
          <h3 className="text-lg font-medium">AI Schedule Builder</h3>
        </div>
        <button
          type="button"
          onClick={onCancel}
          className="p-1.5 rounded hover:bg-muted transition-colors"
        >
          <X className="w-5 h-5" />
        </button>
      </div>

      {/* Workflow Selection */}
      <div className="space-y-3">
        <div>
          <h4 className="text-sm font-medium">Workflows</h4>
          <p className="text-xs text-muted-foreground mt-1">
            Select and order workflows. Drag to reorder. Leave empty to configure task type in the
            generated result.
          </p>
        </div>

        {/* Selected workflows (reorderable) */}
        {selectedWorkflows.length > 0 && (
          <div className="space-y-1">
            {selectedWorkflows.map((wf, idx) => (
              <div
                key={wf.id}
                draggable
                onDragStart={() => handleDragStart(idx)}
                onDragEnter={() => handleDragEnter(idx)}
                onDragEnd={handleDragEnd}
                onDragOver={(e) => e.preventDefault()}
                className="flex items-center gap-2 px-3 py-2 bg-card border border-border rounded-lg group cursor-grab active:cursor-grabbing"
              >
                <GripVertical className="w-4 h-4 text-muted-foreground shrink-0" />
                <span className="text-xs text-muted-foreground font-mono w-5 shrink-0">
                  {idx + 1}.
                </span>
                {wf.category === "slash-command" && (
                  <Terminal className="w-3.5 h-3.5 text-cyan-400 shrink-0" />
                )}
                {wf.isFavorite && (
                  <Star className="w-3 h-3 text-amber-400 fill-amber-400 shrink-0" />
                )}
                <span className="text-sm truncate flex-1">{wf.name}</span>
                <button
                  type="button"
                  onClick={() => removeWorkflow(idx)}
                  className="p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-destructive/10 text-destructive transition-all"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Add workflow button + picker */}
        <div className="relative">
          <button
            type="button"
            onClick={() => setPickerOpen(!pickerOpen)}
            className="flex items-center gap-2 text-sm text-primary hover:text-primary/80 transition-colors"
          >
            <Plus className="w-4 h-4" />
            Add workflow
            <ChevronDown
              className={`w-3.5 h-3.5 transition-transform ${pickerOpen ? "rotate-180" : ""}`}
            />
          </button>

          {pickerOpen && (
            <>
              <div className="fixed inset-0 z-10" onClick={() => setPickerOpen(false)} />
              <div className="absolute z-20 mt-1 w-full border border-border rounded-lg bg-card shadow-xl overflow-hidden">
                <div className="p-2 border-b border-border space-y-2">
                  <div className="relative">
                    <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
                    <input
                      type="text"
                      value={workflowSearch}
                      onChange={(e) => setWorkflowSearch(e.target.value)}
                      placeholder="Search workflows..."
                      className="w-full pl-8 pr-3 py-1.5 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-1 focus:ring-primary/50"
                      autoFocus
                    />
                  </div>
                  <div className="flex gap-1">
                    {(["all", "favorites", "commands"] as const).map((f) => (
                      <button
                        key={f}
                        type="button"
                        onClick={() => setWorkflowFilter(f)}
                        className={`px-2 py-0.5 rounded text-xs transition-colors ${
                          workflowFilter === f
                            ? "bg-cyan-500/20 text-cyan-400 border border-cyan-500/50"
                            : "bg-white/5 text-muted-foreground border border-transparent hover:bg-white/10"
                        }`}
                      >
                        {f === "all" ? "All" : f === "favorites" ? "Favorites" : "Commands"}
                      </button>
                    ))}
                  </div>
                </div>
                <div className="max-h-48 overflow-y-auto">
                  {loadingWorkflows ? (
                    <div className="flex items-center justify-center py-4">
                      <Loader2 className="w-4 h-4 animate-spin text-muted-foreground" />
                    </div>
                  ) : filteredWorkflows.length === 0 ? (
                    <div className="text-center py-4 text-sm text-muted-foreground">
                      No workflows match
                    </div>
                  ) : (
                    filteredWorkflows.map((wf) => (
                      <button
                        key={wf.id}
                        type="button"
                        onClick={() => addWorkflow(wf)}
                        className="w-full text-left px-3 py-2 text-sm hover:bg-white/5 transition-colors flex items-center gap-2"
                      >
                        {wf.category === "slash-command" && (
                          <Terminal className="w-3.5 h-3.5 text-cyan-400 shrink-0" />
                        )}
                        {wf.isFavorite && (
                          <Star className="w-3 h-3 text-amber-400 fill-amber-400 shrink-0" />
                        )}
                        <div className="min-w-0 flex-1">
                          <div className="truncate">{wf.name}</div>
                          {wf.description && (
                            <div className="text-xs text-muted-foreground truncate">
                              {wf.description}
                            </div>
                          )}
                        </div>
                      </button>
                    ))
                  )}
                </div>
              </div>
            </>
          )}
        </div>
      </div>

      {/* Natural Language Input */}
      <div className="space-y-3">
        <div>
          <h4 className="text-sm font-medium">Describe the schedule</h4>
          <p className="text-xs text-muted-foreground mt-1">
            Tell the AI when and how you want these workflows to run
          </p>
        </div>

        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder='e.g., "Run every night at 2 AM" or "Run after 30 minutes of inactivity"'
          rows={3}
          className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-purple-500/50 resize-none text-sm"
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              handleGenerate();
            }
          }}
        />

        {/* Quick templates */}
        <div className="flex flex-wrap gap-1.5">
          {PROMPT_TEMPLATES.map((t) => (
            <button
              key={t.label}
              type="button"
              onClick={() => setPrompt(t.prompt)}
              className="px-2 py-1 text-xs rounded-md bg-purple-500/10 text-purple-300 hover:bg-purple-500/20 border border-purple-500/20 transition-colors"
            >
              {t.label}
            </button>
          ))}
        </div>

        {/* Generate button */}
        <button
          type="button"
          onClick={handleGenerate}
          disabled={isGenerating || !prompt.trim()}
          className="flex items-center gap-2 px-4 py-2 text-sm bg-purple-600 text-white rounded-lg hover:bg-purple-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {isGenerating ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <Sparkles className="w-4 h-4" />
          )}
          {isGenerating ? "Generating..." : "Generate Schedule"}
        </button>

        {aiError && (
          <div className="p-3 bg-destructive/10 text-destructive rounded-lg text-sm">{aiError}</div>
        )}
      </div>

      {/* Generated Results */}
      {generatedTasks && (
        <div className="space-y-4">
          <div className="flex items-center gap-2">
            <Check className="w-4 h-4 text-green-400" />
            <h4 className="text-sm font-medium">
              Generated {generatedTasks.length} task{generatedTasks.length > 1 ? "s" : ""}
            </h4>
          </div>

          <div className="space-y-2">
            {generatedTasks.map((task, idx) => (
              <div key={idx} className="p-3 bg-card border border-border rounded-lg space-y-2">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    {generatedTasks.length > 1 && (
                      <span className="text-xs text-muted-foreground font-mono">{idx + 1}.</span>
                    )}
                    <span className="text-sm font-medium">{task.request.name}</span>
                  </div>
                  <button
                    type="button"
                    onClick={() => handleEditTask(idx)}
                    className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
                    title="Edit in manual form"
                  >
                    <Pencil className="w-3 h-3" />
                    Edit
                  </button>
                </div>
                <p className="text-xs text-muted-foreground">{task.description}</p>
                <div className="flex flex-wrap gap-2 text-xs">
                  <span className="px-2 py-0.5 rounded bg-blue-500/10 text-blue-400 border border-blue-500/20">
                    {task.request.schedule.type}
                    {task.request.schedule.type === "Cron" && `: ${task.request.schedule.value}`}
                  </span>
                  <span className="px-2 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20">
                    {task.request.task.task_type}
                    {task.request.task.task_type === "Workflow" &&
                      `: ${task.request.task.workflow_name}`}
                    {task.request.task.task_type === "RemoteAgent" && ": ad-hoc agent prompt"}
                  </span>
                  {task.request.conditions?.requireIdle?.enabled && (
                    <span className="px-2 py-0.5 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20">
                      Wait for idle
                    </span>
                  )}
                  {task.request.conditions?.requireRepoInactive?.enabled && (
                    <span className="px-2 py-0.5 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20">
                      Wait for repo inactive
                    </span>
                  )}
                </div>
              </div>
            ))}
          </div>

          {/* Actions */}
          <div className="flex items-center justify-end gap-3 pt-2 border-t border-border">
            <button
              type="button"
              onClick={() => setGeneratedTasks(null)}
              className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
            >
              Discard
            </button>
            <button
              type="button"
              onClick={handleAcceptAll}
              disabled={loading}
              className="flex items-center gap-2 px-4 py-2 text-sm bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors disabled:opacity-50"
            >
              {loading ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <ArrowRight className="w-4 h-4" />
              )}
              Create {generatedTasks.length > 1 ? `All ${generatedTasks.length} Tasks` : "Task"}
            </button>
          </div>
        </div>
      )}

      {/* Cancel footer (when no results shown) */}
      {!generatedTasks && (
        <div className="flex items-center justify-end gap-3 pt-4 border-t border-border">
          <button
            type="button"
            onClick={onCancel}
            className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
          >
            Cancel
          </button>
        </div>
      )}
    </div>
  );
}
