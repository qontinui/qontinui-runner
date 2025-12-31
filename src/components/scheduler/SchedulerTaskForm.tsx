/**
 * SchedulerTaskForm
 *
 * Form for creating and editing scheduled tasks.
 */

import { useState, useEffect } from "react";
import { X, Save, Loader2 } from "lucide-react";
import type {
  ScheduledTask,
  ScheduleExpression,
  ScheduledTaskType,
  CreateScheduledTaskRequest,
} from "../../types/scheduler";

interface WorkflowOption {
  id: string;
  name: string;
  configPath?: string;
}

interface PromptOption {
  id: string;
  name: string;
  description?: string;
}

interface SchedulerTaskFormProps {
  task?: ScheduledTask;
  onSubmit: (request: CreateScheduledTaskRequest) => void;
  onCancel: () => void;
  loading: boolean;
}

type ScheduleType = "once" | "cron" | "interval";
type TaskType = "workflow" | "prompt" | "autofix";

const CRON_PRESETS = [
  { label: "Every minute (testing)", value: "* * * * *" },
  { label: "Every hour", value: "0 * * * *" },
  { label: "Every day at midnight", value: "0 0 * * *" },
  { label: "Every day at 9 AM", value: "0 9 * * *" },
  { label: "Weekdays at 9 AM", value: "0 9 * * 1-5" },
  { label: "Every Sunday at midnight", value: "0 0 * * 0" },
  { label: "First of month at midnight", value: "0 0 1 * *" },
];

export function SchedulerTaskForm({ task, onSubmit, onCancel, loading }: SchedulerTaskFormProps) {
  const isEditing = !!task;

  // Form state
  const [name, setName] = useState(task?.name || "");
  const [description, setDescription] = useState(task?.description || "");
  const [skipIfCompleted, setSkipIfCompleted] = useState(task?.skip_if_completed || false);
  const [autoFixOnFailure, setAutoFixOnFailure] = useState(task?.auto_fix_on_failure || false);
  const [successCriteria, setSuccessCriteria] = useState(task?.success_criteria || "");

  // Schedule state
  const [scheduleType, setScheduleType] = useState<ScheduleType>(() => {
    if (!task) return "cron";
    switch (task.schedule.type) {
      case "Once":
        return "once";
      case "Cron":
        return "cron";
      case "Interval":
        return "interval";
    }
  });
  const [onceDateTime, setOnceDateTime] = useState(() => {
    if (task?.schedule.type === "Once") {
      // Convert to local datetime-local format
      const date = new Date(task.schedule.value);
      return date.toISOString().slice(0, 16);
    }
    // Default to 1 hour from now
    const date = new Date(Date.now() + 60 * 60 * 1000);
    return date.toISOString().slice(0, 16);
  });
  const [cronExpression, setCronExpression] = useState(
    task?.schedule.type === "Cron" ? task.schedule.value : "0 9 * * *",
  );
  const [intervalSeconds, setIntervalSeconds] = useState(
    task?.schedule.type === "Interval" ? task.schedule.value : 3600,
  );

  // Task type state
  const [taskType, setTaskType] = useState<TaskType>(() => {
    if (!task) return "workflow";
    switch (task.task.task_type) {
      case "Workflow":
        return "workflow";
      case "Prompt":
        return "prompt";
      case "AutoFix":
        return "autofix";
    }
  });
  const [workflowName, setWorkflowName] = useState(
    task?.task.task_type === "Workflow" ? task.task.workflow_name : "",
  );
  const [configPath, setConfigPath] = useState(
    task?.task.task_type === "Workflow" ? task.task.config_path || "" : "",
  );
  const [monitorIndex, setMonitorIndex] = useState<string>(
    task?.task.task_type === "Workflow" && task.task.monitor_index !== undefined
      ? String(task.task.monitor_index)
      : "",
  );
  const [promptId, setPromptId] = useState(
    task?.task.task_type === "Prompt" ? task.task.prompt_id : "",
  );
  const [maxSessions, setMaxSessions] = useState<string>(
    task?.task.task_type === "Prompt" && task.task.max_sessions !== undefined
      ? String(task.task.max_sessions)
      : "",
  );
  const [checkFindings, setCheckFindings] = useState(
    task?.task.task_type === "AutoFix" ? task.task.check_findings : true,
  );
  const [forceRun, setForceRun] = useState(
    task?.task.task_type === "AutoFix" ? task.task.force_run : false,
  );

  // Available workflows and prompts
  const [workflows, setWorkflows] = useState<WorkflowOption[]>([]);
  const [prompts, setPrompts] = useState<PromptOption[]>([]);
  const [loadingWorkflows, setLoadingWorkflows] = useState(false);
  const [loadingPrompts, setLoadingPrompts] = useState(false);

  // Fetch workflows from config
  useEffect(() => {
    const fetchWorkflows = async () => {
      setLoadingWorkflows(true);
      try {
        // Get the current loaded config which contains workflows
        const response = await fetch("http://localhost:9876/config");
        const result = await response.json();
        if (result.success && result.data?.workflows) {
          const workflowOptions: WorkflowOption[] = result.data.workflows.map(
            (w: { id: string; name: string }) => ({
              id: w.id,
              name: w.name,
              configPath: result.data.config_path,
            }),
          );
          setWorkflows(workflowOptions);
        }
      } catch (error) {
        console.error("Failed to fetch workflows:", error);
      } finally {
        setLoadingWorkflows(false);
      }
    };
    fetchWorkflows();
  }, []);

  // Fetch prompts from library
  useEffect(() => {
    const fetchPrompts = async () => {
      setLoadingPrompts(true);
      try {
        const response = await fetch("http://localhost:9876/prompts");
        const result = await response.json();
        if (result.success && result.data) {
          const promptOptions: PromptOption[] = result.data.map(
            (p: { id: string; name: string; description?: string }) => ({
              id: p.id,
              name: p.name,
              description: p.description,
            }),
          );
          setPrompts(promptOptions);
        }
      } catch (error) {
        console.error("Failed to fetch prompts:", error);
      } finally {
        setLoadingPrompts(false);
      }
    };
    fetchPrompts();
  }, []);

  // Build the schedule expression
  const buildSchedule = (): ScheduleExpression => {
    switch (scheduleType) {
      case "once":
        return { type: "Once", value: new Date(onceDateTime).toISOString() };
      case "cron":
        return { type: "Cron", value: cronExpression };
      case "interval":
        return { type: "Interval", value: intervalSeconds };
    }
  };

  // Build the task type
  const buildTaskType = (): ScheduledTaskType => {
    switch (taskType) {
      case "workflow":
        return {
          task_type: "Workflow",
          workflow_name: workflowName,
          config_path: configPath || undefined,
          monitor_index: monitorIndex ? parseInt(monitorIndex, 10) : undefined,
        };
      case "prompt":
        return {
          task_type: "Prompt",
          prompt_id: promptId,
          max_sessions: maxSessions ? parseInt(maxSessions, 10) : undefined,
        };
      case "autofix":
        return {
          task_type: "AutoFix",
          check_findings: checkFindings,
          force_run: forceRun,
        };
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit({
      name,
      description: description || undefined,
      schedule: buildSchedule(),
      task: buildTaskType(),
      skip_if_completed: skipIfCompleted,
      auto_fix_on_failure: autoFixOnFailure,
      success_criteria: successCriteria || undefined,
    });
  };

  const isValid = () => {
    if (!name.trim()) return false;

    switch (taskType) {
      case "workflow":
        return workflowName.trim() !== "";
      case "prompt":
        return promptId.trim() !== "";
      case "autofix":
        return true;
    }
  };

  return (
    <form onSubmit={handleSubmit} className="space-y-6 max-w-2xl">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-medium">
          {isEditing ? "Edit Scheduled Task" : "New Scheduled Task"}
        </h3>
        <button
          type="button"
          onClick={onCancel}
          className="p-1.5 rounded hover:bg-muted transition-colors"
        >
          <X className="w-5 h-5" />
        </button>
      </div>

      {/* Basic Info */}
      <div className="space-y-4">
        <div>
          <label className="block text-sm font-medium mb-1">Name *</label>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Task name"
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            required
          />
        </div>
        <div>
          <label className="block text-sm font-medium mb-1">Description</label>
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Optional description"
            rows={2}
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 resize-none"
          />
        </div>
      </div>

      {/* Schedule */}
      <div className="space-y-4">
        <h4 className="text-sm font-medium">Schedule</h4>
        <div className="flex items-center gap-4">
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="scheduleType"
              checked={scheduleType === "cron"}
              onChange={() => setScheduleType("cron")}
              className="text-primary"
            />
            <span className="text-sm">Cron</span>
          </label>
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="scheduleType"
              checked={scheduleType === "once"}
              onChange={() => setScheduleType("once")}
              className="text-primary"
            />
            <span className="text-sm">Once</span>
          </label>
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="scheduleType"
              checked={scheduleType === "interval"}
              onChange={() => setScheduleType("interval")}
              className="text-primary"
            />
            <span className="text-sm">Interval</span>
          </label>
        </div>

        {scheduleType === "cron" && (
          <div className="space-y-2">
            <select
              value={CRON_PRESETS.find((p) => p.value === cronExpression) ? cronExpression : ""}
              onChange={(e) => e.target.value && setCronExpression(e.target.value)}
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            >
              <option value="">Custom expression</option>
              {CRON_PRESETS.map((preset) => (
                <option key={preset.value} value={preset.value}>
                  {preset.label}
                </option>
              ))}
            </select>
            <input
              type="text"
              value={cronExpression}
              onChange={(e) => setCronExpression(e.target.value)}
              placeholder="* * * * *"
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 font-mono text-sm"
            />
            <p className="text-xs text-muted-foreground">
              Format: minute hour day-of-month month day-of-week
            </p>
          </div>
        )}

        {scheduleType === "once" && (
          <input
            type="datetime-local"
            value={onceDateTime}
            onChange={(e) => setOnceDateTime(e.target.value)}
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
          />
        )}

        {scheduleType === "interval" && (
          <div className="flex items-center gap-2">
            <input
              type="number"
              value={intervalSeconds}
              onChange={(e) => setIntervalSeconds(parseInt(e.target.value, 10) || 60)}
              min={60}
              className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
            />
            <span className="text-sm text-muted-foreground">seconds</span>
          </div>
        )}
      </div>

      {/* Task Type */}
      <div className="space-y-4">
        <h4 className="text-sm font-medium">Task Type</h4>
        <div className="flex items-center gap-4">
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="taskType"
              checked={taskType === "workflow"}
              onChange={() => setTaskType("workflow")}
              className="text-primary"
            />
            <span className="text-sm">Workflow</span>
          </label>
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="taskType"
              checked={taskType === "prompt"}
              onChange={() => setTaskType("prompt")}
              className="text-primary"
            />
            <span className="text-sm">Prompt</span>
          </label>
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="taskType"
              checked={taskType === "autofix"}
              onChange={() => setTaskType("autofix")}
              className="text-primary"
            />
            <span className="text-sm">Auto-Fix</span>
          </label>
        </div>

        {taskType === "workflow" && (
          <div className="space-y-3">
            <div>
              <label className="block text-sm font-medium mb-1">Workflow *</label>
              {loadingWorkflows ? (
                <div className="flex items-center gap-2 text-sm text-muted-foreground py-2">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Loading workflows...
                </div>
              ) : workflows.length > 0 ? (
                <select
                  value={workflowName}
                  onChange={(e) => {
                    const selected = workflows.find((w) => w.name === e.target.value);
                    setWorkflowName(e.target.value);
                    if (selected?.configPath && !configPath) {
                      setConfigPath(selected.configPath);
                    }
                  }}
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                  required={taskType === "workflow"}
                >
                  <option value="">Select a workflow...</option>
                  {workflows.map((workflow) => (
                    <option key={workflow.id} value={workflow.name}>
                      {workflow.name}
                    </option>
                  ))}
                </select>
              ) : (
                <div className="space-y-2">
                  <p className="text-sm text-muted-foreground">
                    No workflows found. Load a config file first, or enter manually:
                  </p>
                  <input
                    type="text"
                    value={workflowName}
                    onChange={(e) => setWorkflowName(e.target.value)}
                    placeholder="Enter workflow name"
                    className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                    required={taskType === "workflow"}
                  />
                </div>
              )}
            </div>
            <div>
              <label className="block text-sm font-medium mb-1">Config Path</label>
              <input
                type="text"
                value={configPath}
                onChange={(e) => setConfigPath(e.target.value)}
                placeholder="Path to config file (optional, uses current if empty)"
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>
            <div>
              <label className="block text-sm font-medium mb-1">Monitor Index</label>
              <input
                type="number"
                value={monitorIndex}
                onChange={(e) => setMonitorIndex(e.target.value)}
                placeholder="Monitor index (optional)"
                min={0}
                className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
            </div>
          </div>
        )}

        {taskType === "prompt" && (
          <div className="space-y-3">
            <div>
              <label className="block text-sm font-medium mb-1">Prompt *</label>
              {loadingPrompts ? (
                <div className="flex items-center gap-2 text-sm text-muted-foreground py-2">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Loading prompts...
                </div>
              ) : prompts.length > 0 ? (
                <select
                  value={promptId}
                  onChange={(e) => setPromptId(e.target.value)}
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                  required={taskType === "prompt"}
                >
                  <option value="">Select a prompt...</option>
                  {prompts.map((prompt) => (
                    <option key={prompt.id} value={prompt.id}>
                      {prompt.name}
                      {prompt.description ? ` - ${prompt.description.slice(0, 40)}...` : ""}
                    </option>
                  ))}
                </select>
              ) : (
                <div className="space-y-2">
                  <p className="text-sm text-muted-foreground">
                    No prompts found. Create prompts in the Library first, or enter ID manually:
                  </p>
                  <input
                    type="text"
                    value={promptId}
                    onChange={(e) => setPromptId(e.target.value)}
                    placeholder="Enter prompt ID"
                    className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
                    required={taskType === "prompt"}
                  />
                </div>
              )}
              {prompts.length > 0 && promptId && (
                <p className="text-xs text-muted-foreground mt-1">
                  Selected: {prompts.find((p) => p.id === promptId)?.name || promptId}
                </p>
              )}
            </div>
            <div>
              <label className="block text-sm font-medium mb-1">Max Sessions (optional)</label>
              <input
                type="number"
                value={maxSessions}
                onChange={(e) => setMaxSessions(e.target.value)}
                placeholder="No limit"
                min={1}
                className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
              />
              <p className="text-xs text-muted-foreground mt-1">
                Leave empty for no limit (runs until [TASK_COMPLETE])
              </p>
            </div>
          </div>
        )}

        {taskType === "autofix" && (
          <div className="space-y-3">
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={checkFindings}
                onChange={(e) => setCheckFindings(e.target.checked)}
                className="text-primary"
              />
              <span className="text-sm">Check findings before fixing</span>
            </label>
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={forceRun}
                onChange={(e) => setForceRun(e.target.checked)}
                className="text-primary"
              />
              <span className="text-sm">Force run even if no findings</span>
            </label>
          </div>
        )}
      </div>

      {/* Options */}
      <div className="space-y-4">
        <h4 className="text-sm font-medium">Options</h4>
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={skipIfCompleted}
            onChange={(e) => setSkipIfCompleted(e.target.checked)}
            className="text-primary"
          />
          <span className="text-sm">Skip if already completed successfully</span>
        </label>
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={autoFixOnFailure}
            onChange={(e) => setAutoFixOnFailure(e.target.checked)}
            className="text-primary"
          />
          <span className="text-sm">Trigger auto-fix on failure</span>
        </label>
        <div>
          <label className="block text-sm font-medium mb-1">Success Criteria</label>
          <textarea
            value={successCriteria}
            onChange={(e) => setSuccessCriteria(e.target.value)}
            placeholder="Optional description of what constitutes success"
            rows={2}
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 resize-none"
          />
        </div>
      </div>

      {/* Actions */}
      <div className="flex items-center justify-end gap-3 pt-4 border-t border-border">
        <button
          type="button"
          onClick={onCancel}
          className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={loading || !isValid()}
          className="flex items-center gap-2 px-4 py-2 text-sm bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Save className="w-4 h-4" />}
          {isEditing ? "Update" : "Create"}
        </button>
      </div>
    </form>
  );
}
