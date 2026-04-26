/**
 * SchedulerTaskForm
 *
 * Form for creating and editing scheduled tasks.
 */

import { useState, useEffect, useMemo } from "react";
import {
  X,
  Save,
  Loader2,
  Trash2,
  Plus,
  FolderOpen,
  Search,
  Star,
  Terminal,
  ChevronDown,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { getAccentColors } from "@/design-system";
import type {
  ScheduledTask,
  ScheduleExpression,
  ScheduledTaskType,
  CreateScheduledTaskRequest,
  ScheduleConditions,
  RepositoryWatch,
  CatchUpPolicy,
  McpConnectionRef,
} from "../../types/scheduler";
import type { UnifiedWorkflow } from "../../types/unified-workflow";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface PromptOption {
  id: string;
  name: string;
  description?: string;
}

interface RepositoryWatchWithId extends RepositoryWatch {
  _formId: string;
}

interface McpConnectionRefWithId extends McpConnectionRef {
  _formId: string;
}

const REMOTE_AGENT_DEFAULT_TOOLS = ["Bash", "Read", "Write", "Edit", "Glob", "Grep"];

const REMOTE_AGENT_AVAILABLE_TOOLS = [
  "Bash",
  "Read",
  "Write",
  "Edit",
  "Glob",
  "Grep",
  "WebFetch",
  "WebSearch",
  "Task",
];

const REMOTE_AGENT_MODEL_PRESETS = [
  { value: "claude-sonnet-4-6", label: "Claude Sonnet 4.6 (default)" },
  { value: "claude-opus-4-7", label: "Claude Opus 4.7" },
  { value: "claude-haiku-4-5", label: "Claude Haiku 4.5" },
];

const REMOTE_AGENT_MODEL_CUSTOM_SENTINEL = "__custom__";

const CATCH_UP_POLICY_OPTIONS: ReadonlyArray<{
  value: CatchUpPolicy;
  label: string;
  hint: string;
}> = [
  {
    value: "run_once",
    label: "Run once after returning",
    hint: "Collapse missed slots into a single catch-up run",
  },
  {
    value: "run",
    label: "Run all missed slots",
    hint: "Execute one run for every slot that was missed",
  },
  {
    value: "skip",
    label: "Skip and log",
    hint: "Record missed slots without running them",
  },
];

interface SchedulerTaskFormProps {
  task?: ScheduledTask;
  /** Pre-fill form fields from an AI-generated or external request (for new tasks only) */
  prefill?: CreateScheduledTaskRequest;
  onSubmit: (request: CreateScheduledTaskRequest) => void;
  onCancel: () => void;
  loading: boolean;
}

type ScheduleType = "once" | "cron" | "interval" | "condition";
type TaskType = "workflow" | "prompt" | "autofix" | "remote_agent";

const CRON_PRESETS = [
  { label: "Every minute (testing)", value: "* * * * *" },
  { label: "Every hour", value: "0 * * * *" },
  { label: "Every day at midnight", value: "0 0 * * *" },
  { label: "Every day at 9 AM", value: "0 9 * * *" },
  { label: "Weekdays at 9 AM", value: "0 9 * * 1-5" },
  { label: "Every Sunday at midnight", value: "0 0 * * 0" },
  { label: "First of month at midnight", value: "0 0 1 * *" },
];

export function SchedulerTaskForm({
  task,
  prefill,
  onSubmit,
  onCancel,
  loading,
}: SchedulerTaskFormProps) {
  const isEditing = !!task;
  const src = task ?? prefill;

  // Form state
  const [name, setName] = useState(src?.name || "");
  const [description, setDescription] = useState(src?.description || "");
  const [skipIfCompleted, setSkipIfCompleted] = useState(
    (task ? task.skipIfCompleted : prefill?.skipIfCompleted) || false,
  );
  const [autoFixOnFailure, setAutoFixOnFailure] = useState(
    (task ? task.autoFixOnFailure : prefill?.autoFixOnFailure) || false,
  );
  const [successCriteria, setSuccessCriteria] = useState(
    (task ? task.successCriteria : prefill?.successCriteria) || "",
  );

  // Schedule state
  const initSchedule = task?.schedule ?? prefill?.schedule;
  const [scheduleType, setScheduleType] = useState<ScheduleType>(() => {
    if (!initSchedule) return "cron";
    switch (initSchedule.type) {
      case "Once":
        return "once";
      case "Cron":
        return "cron";
      case "Interval":
        return "interval";
      case "Condition":
        return "condition";
      default:
        return "cron";
    }
  });
  const [onceDateTime, setOnceDateTime] = useState(() => {
    if (initSchedule?.type === "Once") {
      const date = new Date(initSchedule.value);
      return date.toISOString().slice(0, 16);
    }
    const date = new Date(Date.now() + 60 * 60 * 1000);
    return date.toISOString().slice(0, 16);
  });
  const [cronExpression, setCronExpression] = useState(
    initSchedule?.type === "Cron" ? initSchedule.value : "0 9 * * *",
  );
  const [intervalSeconds, setIntervalSeconds] = useState(
    initSchedule?.type === "Interval" ? initSchedule.value : 3600,
  );
  const [rearmDelay, setRearmDelay] = useState(
    initSchedule?.type === "Condition" ? (initSchedule.value?.rearmDelayMinutes ?? 60) : 60,
  );

  // Task type state
  const initTask = task?.task ?? prefill?.task;
  const [taskType, setTaskType] = useState<TaskType>(() => {
    if (!initTask) return "workflow";
    switch (initTask.task_type) {
      case "Workflow":
        return "workflow";
      case "Prompt":
        return "prompt";
      case "AutoFix":
        return "autofix";
      case "RemoteAgent":
        return "remote_agent";
      default:
        return "workflow";
    }
  });
  const [workflowId, setWorkflowId] = useState(
    initTask?.task_type === "Workflow" ? initTask.workflow_id || "" : "",
  );
  const [workflowName, setWorkflowName] = useState(
    initTask?.task_type === "Workflow" ? initTask.workflow_name : "",
  );
  const [taskConfigPath, _setTaskConfigPath] = useState(
    initTask?.task_type === "Workflow" ? initTask.config_path || "" : "",
  );
  const [monitorIndex, setMonitorIndex] = useState<string>(
    initTask?.task_type === "Workflow" && initTask.monitor_index !== undefined
      ? String(initTask.monitor_index)
      : "",
  );
  const [promptId, setPromptId] = useState(
    initTask?.task_type === "Prompt" ? initTask.prompt_id : "",
  );
  const [maxSessions, setMaxSessions] = useState<string>(
    initTask?.task_type === "Prompt" && initTask.max_sessions !== undefined
      ? String(initTask.max_sessions)
      : "",
  );
  const [checkFindings, setCheckFindings] = useState(
    initTask?.task_type === "AutoFix" ? initTask.check_findings : true,
  );
  const [forceRun, setForceRun] = useState(
    initTask?.task_type === "AutoFix" ? initTask.force_run : false,
  );

  // RemoteAgent state
  const initRemoteAgent = initTask?.task_type === "RemoteAgent" ? initTask : undefined;
  const [agentPrompt, setAgentPrompt] = useState(initRemoteAgent?.prompt ?? "");
  const [agentWorkingDirectory, setAgentWorkingDirectory] = useState(
    initRemoteAgent?.working_directory ?? "",
  );
  const initialModel = initRemoteAgent?.model ?? "";
  const initialModelIsPreset =
    initialModel === "" || REMOTE_AGENT_MODEL_PRESETS.some((p) => p.value === initialModel);
  const [agentModelSelect, setAgentModelSelect] = useState<string>(() => {
    if (!initialModel) return "claude-sonnet-4-6";
    return initialModelIsPreset ? initialModel : REMOTE_AGENT_MODEL_CUSTOM_SENTINEL;
  });
  const [agentModelCustom, setAgentModelCustom] = useState<string>(
    initialModelIsPreset ? "" : initialModel,
  );
  const [agentAllowedTools, setAgentAllowedTools] = useState<string[]>(() => {
    const fromInit = initRemoteAgent?.allowed_tools;
    if (fromInit && fromInit.length > 0) return [...fromInit];
    return [...REMOTE_AGENT_DEFAULT_TOOLS];
  });
  const [agentMaxTurns, setAgentMaxTurns] = useState<string>(
    initRemoteAgent?.max_turns !== undefined && initRemoteAgent?.max_turns !== null
      ? String(initRemoteAgent.max_turns)
      : "50",
  );
  const [agentTimeoutSeconds, setAgentTimeoutSeconds] = useState<string>(
    initRemoteAgent?.timeout_seconds !== undefined && initRemoteAgent?.timeout_seconds !== null
      ? String(initRemoteAgent.timeout_seconds)
      : "600",
  );
  const [agentMcpConnections, setAgentMcpConnections] = useState<McpConnectionRefWithId[]>(() => {
    const fromInit = initRemoteAgent?.mcp_connections;
    if (!fromInit || fromInit.length === 0) return [];
    return fromInit.map((c) => ({
      name: c.name,
      url: c.url ?? null,
      _formId: crypto.randomUUID(),
    }));
  });
  const [agentAdvancedOpen, setAgentAdvancedOpen] = useState(false);

  // Catch-up policy state
  const initCatchUpPolicy: CatchUpPolicy =
    task?.catchUpPolicy ?? prefill?.catchUpPolicy ?? "run_once";
  const [catchUpPolicy, setCatchUpPolicy] = useState<CatchUpPolicy>(initCatchUpPolicy);
  const initCatchUpGrace = task?.catchUpGraceSeconds ?? prefill?.catchUpGraceSeconds ?? 300;
  const [catchUpGraceSeconds, setCatchUpGraceSeconds] = useState<number>(initCatchUpGrace);

  // Conditions state
  const initConditions = task?.conditions ?? prefill?.conditions;
  const [requireIdle, setRequireIdle] = useState(initConditions?.requireIdle?.enabled ?? false);
  const [requireRepoInactive, setRequireRepoInactive] = useState(
    initConditions?.requireRepoInactive?.enabled ?? false,
  );
  const [repositories, setRepositories] = useState<RepositoryWatchWithId[]>(() =>
    (initConditions?.requireRepoInactive?.repositories ?? []).map((r) => ({
      ...r,
      _formId: crypto.randomUUID(),
    })),
  );
  const [timeoutMinutes, setTimeoutMinutes] = useState<number | undefined>(
    initConditions?.timeoutMinutes ?? undefined,
  );

  // Auto-enable conditions when Condition schedule type is selected.
  // Detect the transition during render so the setState is inline (allowed
  // pattern) instead of via a post-render effect (set-state-in-effect).
  const [prevScheduleType, setPrevScheduleType] = useState(scheduleType);
  if (prevScheduleType !== scheduleType) {
    setPrevScheduleType(scheduleType);
    if (scheduleType === "condition" && !requireIdle && !requireRepoInactive) {
      setRequireIdle(true);
    }
  }

  // Unified workflows from API
  const [unifiedWorkflows, setUnifiedWorkflows] = useState<UnifiedWorkflow[]>([]);
  const [loadingWorkflows, setLoadingWorkflows] = useState(false);
  const [workflowSearch, setWorkflowSearch] = useState("");
  const [workflowFilter, setWorkflowFilter] = useState<"all" | "favorites" | "commands">("all");
  const [workflowPickerOpen, setWorkflowPickerOpen] = useState(false);

  // Fetch unified workflows from API
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    const fetchWorkflows = async () => {
      setLoadingWorkflows(true);
      try {
        const response = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled && result.success && result.data) {
          setUnifiedWorkflows(result.data);
        }
      } catch (error) {
        if (!cancelled) console.error("Failed to fetch workflows:", error);
      } finally {
        if (!cancelled) setLoadingWorkflows(false);
      }
    };
    fetchWorkflows();
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Filtered unified workflows
  const filteredWorkflows = useMemo(() => {
    let list = unifiedWorkflows;
    if (workflowFilter === "favorites") {
      list = list.filter((w) => w.isFavorite);
    } else if (workflowFilter === "commands") {
      list = list.filter((w) => w.category === "slash-command");
    }
    if (workflowSearch) {
      const q = workflowSearch.toLowerCase();
      list = list.filter(
        (w) => w.name.toLowerCase().includes(q) || w.description?.toLowerCase().includes(q),
      );
    }
    return list;
  }, [unifiedWorkflows, workflowFilter, workflowSearch]);

  // Selected workflow display name
  const selectedWorkflow = unifiedWorkflows.find((w) => w.id === workflowId);

  // Available prompts
  const [prompts, setPrompts] = useState<PromptOption[]>([]);
  const [loadingPrompts, setLoadingPrompts] = useState(false);

  // Fetch prompts from library
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    const fetchPrompts = async () => {
      setLoadingPrompts(true);
      try {
        const response = await tracedFetch(`${getApiBase()}/prompts`, {
          signal: controller.signal,
        });
        const result = await response.json();
        if (!cancelled && result.success && result.data) {
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
        if (!cancelled) {
          console.error("Failed to fetch prompts:", error);
        }
      } finally {
        if (!cancelled) setLoadingPrompts(false);
      }
    };
    fetchPrompts();
    return () => {
      cancelled = true;
      controller.abort();
    };
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
      case "condition":
        return {
          type: "Condition",
          value: { rearmDelayMinutes: rearmDelay ?? 0 },
        };
    }
  };

  // Build the task type
  const buildTaskType = (): ScheduledTaskType => {
    switch (taskType) {
      case "workflow":
        return {
          task_type: "Workflow",
          workflow_name: workflowId ? selectedWorkflow?.name || workflowName : workflowName,
          config_path: taskConfigPath || undefined,
          monitor_index: monitorIndex ? parseInt(monitorIndex, 10) : undefined,
          workflow_id: workflowId || undefined,
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
      case "remote_agent": {
        const resolvedModel =
          agentModelSelect === REMOTE_AGENT_MODEL_CUSTOM_SENTINEL
            ? agentModelCustom.trim() || undefined
            : agentModelSelect || undefined;
        const parsedMaxTurns = agentMaxTurns ? parseInt(agentMaxTurns, 10) : NaN;
        const parsedTimeout = agentTimeoutSeconds ? parseInt(agentTimeoutSeconds, 10) : NaN;
        const cleanedMcp: McpConnectionRef[] = agentMcpConnections
          .filter((c) => c.name.trim() !== "")
          .map(({ _formId: _, url, name }) => ({
            name: name.trim(),
            url: url && url.trim() !== "" ? url.trim() : null,
          }));
        return {
          task_type: "RemoteAgent",
          prompt: agentPrompt,
          working_directory: agentWorkingDirectory.trim() || null,
          allowed_tools: agentAllowedTools,
          model: resolvedModel ?? null,
          mcp_connections: cleanedMcp,
          max_turns: Number.isFinite(parsedMaxTurns) ? parsedMaxTurns : null,
          timeout_seconds: Number.isFinite(parsedTimeout) ? parsedTimeout : null,
        };
      }
    }
  };

  // Build the conditions configuration
  const buildConditions = (): ScheduleConditions | undefined => {
    // Only include conditions if at least one is enabled
    if (!requireIdle && !requireRepoInactive) {
      return undefined;
    }

    const conditions: ScheduleConditions = {};

    if (requireIdle) {
      conditions.requireIdle = { enabled: true };
    }

    if (requireRepoInactive && repositories.length > 0) {
      conditions.requireRepoInactive = {
        enabled: true,
        repositories: repositories
          .filter((r) => r.path.trim() !== "")
          .map(({ _formId: _, ...rest }) => rest),
      };
    }

    if (timeoutMinutes !== undefined && timeoutMinutes > 0) {
      conditions.timeoutMinutes = timeoutMinutes;
    }

    return conditions;
  };

  // Repository management helpers
  const addRepository = () => {
    setRepositories([
      ...repositories,
      { path: "", inactiveMinutes: 10, _formId: crypto.randomUUID() },
    ]);
  };

  const removeRepository = (index: number) => {
    setRepositories(repositories.filter((_, i) => i !== index));
  };

  const updateRepository = (
    index: number,
    field: keyof RepositoryWatch,
    value: string | number,
  ) => {
    setRepositories(
      repositories.map((repo, i) => (i === index ? { ...repo, [field]: value } : repo)),
    );
  };

  const browseForRepository = async (index: number) => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Select Repository Directory",
      });
      if (selected && typeof selected === "string") {
        updateRepository(index, "path", selected);
      }
    } catch (error) {
      console.error("Failed to open directory picker:", error);
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit({
      name,
      description: description || undefined,
      schedule: buildSchedule(),
      task: buildTaskType(),
      conditions: buildConditions(),
      skipIfCompleted: skipIfCompleted,
      autoFixOnFailure: autoFixOnFailure,
      successCriteria: successCriteria || undefined,
      catchUpPolicy: catchUpPolicy,
      catchUpGraceSeconds: catchUpGraceSeconds,
    });
  };

  const isValid = () => {
    if (!name.trim()) return false;

    // Schedule-type validation
    if (scheduleType === "condition" && !requireIdle && !requireRepoInactive) return false;

    switch (taskType) {
      case "workflow":
        return workflowId.trim() !== "" || workflowName.trim() !== "";
      case "prompt":
        return promptId.trim() !== "";
      case "autofix":
        return true;
      case "remote_agent":
        if (!agentPrompt.trim()) return false;
        if (agentModelSelect === REMOTE_AGENT_MODEL_CUSTOM_SENTINEL && !agentModelCustom.trim()) {
          return false;
        }
        return true;
    }
  };

  const toggleAllowedTool = (tool: string) => {
    setAgentAllowedTools((prev) =>
      prev.includes(tool) ? prev.filter((t) => t !== tool) : [...prev, tool],
    );
  };

  const addMcpConnection = () => {
    setAgentMcpConnections((prev) => [
      ...prev,
      { name: "", url: null, _formId: crypto.randomUUID() },
    ]);
  };

  const removeMcpConnection = (index: number) => {
    setAgentMcpConnections((prev) => prev.filter((_, i) => i !== index));
  };

  const updateMcpConnection = (index: number, field: "name" | "url", value: string) => {
    setAgentMcpConnections((prev) =>
      prev.map((c, i) =>
        i === index ? { ...c, [field]: field === "url" ? value || null : value } : c,
      ),
    );
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
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
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
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 resize-none"
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
          <label className="flex items-center gap-2">
            <input
              type="radio"
              name="scheduleType"
              checked={scheduleType === "condition"}
              onChange={() => setScheduleType("condition")}
              className="text-primary"
            />
            <span className="text-sm">Condition</span>
          </label>
        </div>

        {scheduleType === "cron" && (
          <div className="space-y-2">
            <select
              value={CRON_PRESETS.find((p) => p.value === cronExpression) ? cronExpression : ""}
              onChange={(e) => e.target.value && setCronExpression(e.target.value)}
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
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
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 font-mono text-sm"
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
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
          />
        )}

        {scheduleType === "interval" && (
          <div className="flex items-center gap-2">
            <input
              type="number"
              value={intervalSeconds}
              onChange={(e) => setIntervalSeconds(parseInt(e.target.value, 10) || 60)}
              min={60}
              className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            />
            <span className="text-sm text-muted-foreground">seconds</span>
          </div>
        )}

        {scheduleType === "condition" && (
          <div className="space-y-4">
            <p className="text-xs text-muted-foreground">
              Runs when the conditions below are met. No time schedule required.
            </p>
            <div className="space-y-2">
              <label className="block text-sm font-medium mb-1">Rearm Delay (minutes)</label>
              <input
                type="number"
                value={rearmDelay}
                onChange={(e) => setRearmDelay(Number(e.target.value) || 60)}
                placeholder="60"
                min={1}
                className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
              />
              <p className="text-xs text-muted-foreground">
                After executing, wait this long before re-evaluating conditions
              </p>
            </div>
          </div>
        )}

        {/* Catch-up policy: applies to all time-based schedule types. Hidden for
            condition-based schedules where the concept doesn't apply. */}
        {scheduleType !== "condition" && (
          <div className="space-y-2">
            <label htmlFor="catch-up-policy" className="block text-sm font-medium mb-1">
              If the runner was down
            </label>
            <select
              id="catch-up-policy"
              value={catchUpPolicy}
              onChange={(e) => setCatchUpPolicy(e.target.value as CatchUpPolicy)}
              className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            >
              {CATCH_UP_POLICY_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            <p className="text-xs text-muted-foreground">
              {CATCH_UP_POLICY_OPTIONS.find((o) => o.value === catchUpPolicy)?.hint ?? ""}
            </p>
            <div className="flex items-center gap-2 pt-1">
              <label htmlFor="catch-up-grace" className="text-xs text-muted-foreground">
                Grace window
              </label>
              <input
                id="catch-up-grace"
                type="number"
                value={catchUpGraceSeconds}
                onChange={(e) =>
                  setCatchUpGraceSeconds(Math.max(0, parseInt(e.target.value, 10) || 0))
                }
                min={0}
                className="w-24 px-2 py-1 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
              />
              <span className="text-xs text-muted-foreground">
                seconds (slots within this window are not treated as missed)
              </span>
            </div>
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
          <label className="flex items-center gap-2" title="Run a remote AI agent prompt">
            <input
              type="radio"
              name="taskType"
              checked={taskType === "remote_agent"}
              onChange={() => setTaskType("remote_agent")}
              className="text-primary"
            />
            <span className="text-sm">Remote agent</span>
          </label>
        </div>

        {taskType === "workflow" && (
          <div className="space-y-3">
            <div>
              <label className="block text-sm font-medium mb-1">Workflow *</label>

              {/* Selected workflow display / trigger */}
              <button
                type="button"
                onClick={() => setWorkflowPickerOpen(!workflowPickerOpen)}
                className="w-full flex items-center justify-between px-3 py-2 bg-background border border-border rounded-lg hover:border-primary/50 transition-colors text-left"
              >
                {selectedWorkflow ? (
                  <span className="flex items-center gap-2 truncate">
                    {selectedWorkflow.category === "slash-command" && (
                      <Terminal className="w-3.5 h-3.5 text-cyan-400 shrink-0" />
                    )}
                    {selectedWorkflow.isFavorite && (
                      <Star className="w-3.5 h-3.5 text-amber-400 fill-amber-400 shrink-0" />
                    )}
                    <span className="truncate">{selectedWorkflow.name}</span>
                  </span>
                ) : (
                  <span className="text-muted-foreground">Select a workflow...</span>
                )}
                <ChevronDown
                  className={`w-4 h-4 text-muted-foreground shrink-0 transition-transform ${workflowPickerOpen ? "rotate-180" : ""}`}
                />
              </button>

              {/* Workflow picker dropdown */}
              {workflowPickerOpen && (
                <>
                  <div
                    className="fixed inset-0 z-10"
                    onClick={() => setWorkflowPickerOpen(false)}
                  />
                  <div className="relative z-20 mt-1 border border-border rounded-lg bg-card shadow-xl overflow-hidden">
                    {/* Search + filters */}
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

                    {/* Workflow list */}
                    <div className="max-h-52 overflow-y-auto">
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
                            onClick={() => {
                              setWorkflowId(wf.id);
                              setWorkflowName(wf.name);
                              setWorkflowPickerOpen(false);
                              setWorkflowSearch("");
                            }}
                            className={`w-full text-left px-3 py-2 text-sm hover:bg-white/5 transition-colors flex items-center gap-2 ${
                              wf.id === workflowId ? "bg-cyan-500/10 text-cyan-400" : ""
                            }`}
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
                            {wf.tags?.includes("requires-arguments") && (
                              <span className="shrink-0 px-1 py-0.5 rounded text-[9px] font-medium bg-amber-500/20 text-amber-400">
                                Args
                              </span>
                            )}
                          </button>
                        ))
                      )}
                    </div>
                  </div>
                </>
              )}
            </div>
            <div>
              <label className="block text-sm font-medium mb-1">Monitor Index</label>
              <input
                type="number"
                value={monitorIndex}
                onChange={(e) => setMonitorIndex(e.target.value)}
                placeholder="Monitor index (optional)"
                min={0}
                className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
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
                  className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
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
                    className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
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
                className="w-32 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
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

        {taskType === "remote_agent" && (
          <div className="space-y-3">
            <div>
              <label htmlFor="agent-prompt" className="block text-sm font-medium mb-1">
                Prompt *
              </label>
              <textarea
                id="agent-prompt"
                value={agentPrompt}
                onChange={(e) => setAgentPrompt(e.target.value)}
                placeholder="Describe what you want the remote agent to do..."
                rows={5}
                required={taskType === "remote_agent"}
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 resize-y text-sm"
              />
            </div>
            <div>
              <label htmlFor="agent-working-directory" className="block text-sm font-medium mb-1">
                Working directory
              </label>
              <input
                id="agent-working-directory"
                type="text"
                value={agentWorkingDirectory}
                onChange={(e) => setAgentWorkingDirectory(e.target.value)}
                placeholder="Leave blank for the runner's project root"
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 text-sm"
              />
            </div>
            <div>
              <label htmlFor="agent-model" className="block text-sm font-medium mb-1">
                Model
              </label>
              <select
                id="agent-model"
                value={agentModelSelect}
                onChange={(e) => setAgentModelSelect(e.target.value)}
                className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50"
              >
                {REMOTE_AGENT_MODEL_PRESETS.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label}
                  </option>
                ))}
                <option value={REMOTE_AGENT_MODEL_CUSTOM_SENTINEL}>(custom)</option>
              </select>
              {agentModelSelect === REMOTE_AGENT_MODEL_CUSTOM_SENTINEL && (
                <input
                  type="text"
                  value={agentModelCustom}
                  onChange={(e) => setAgentModelCustom(e.target.value)}
                  placeholder="Enter a model identifier"
                  className="mt-2 w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 text-sm"
                />
              )}
            </div>
            <div>
              <span className="block text-sm font-medium mb-1">Allowed tools</span>
              <div className="flex flex-wrap gap-2">
                {REMOTE_AGENT_AVAILABLE_TOOLS.map((tool) => {
                  const checked = agentAllowedTools.includes(tool);
                  return (
                    <label
                      key={tool}
                      className={`flex items-center gap-1.5 px-2 py-1 rounded border text-xs cursor-pointer transition-colors ${
                        checked
                          ? "bg-cyan-500/10 text-cyan-400 border-cyan-500/40"
                          : "bg-background text-muted-foreground border-border hover:border-primary/50"
                      }`}
                    >
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={() => toggleAllowedTool(tool)}
                        className="sr-only"
                      />
                      <span>{tool}</span>
                    </label>
                  );
                })}
              </div>
              <p className="text-xs text-muted-foreground mt-1">
                Defaults: {REMOTE_AGENT_DEFAULT_TOOLS.join(", ")}
              </p>
            </div>

            <div className="border border-border rounded-lg">
              <button
                type="button"
                onClick={() => setAgentAdvancedOpen(!agentAdvancedOpen)}
                className="w-full flex items-center justify-between px-3 py-2 text-sm hover:bg-muted/40 transition-colors"
                aria-expanded={agentAdvancedOpen}
              >
                <span>Advanced</span>
                <ChevronDown
                  className={`w-4 h-4 text-muted-foreground transition-transform ${
                    agentAdvancedOpen ? "rotate-180" : ""
                  }`}
                />
              </button>
              {agentAdvancedOpen && (
                <div className="px-3 pb-3 pt-1 space-y-3 border-t border-border">
                  <div className="flex items-center gap-3 flex-wrap">
                    <label htmlFor="agent-max-turns" className="text-sm">
                      Max turns
                    </label>
                    <input
                      id="agent-max-turns"
                      type="number"
                      value={agentMaxTurns}
                      onChange={(e) => setAgentMaxTurns(e.target.value)}
                      min={1}
                      className="w-24 px-2 py-1 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
                    />
                    <label htmlFor="agent-timeout" className="text-sm">
                      Timeout (seconds)
                    </label>
                    <input
                      id="agent-timeout"
                      type="number"
                      value={agentTimeoutSeconds}
                      onChange={(e) => setAgentTimeoutSeconds(e.target.value)}
                      min={1}
                      className="w-28 px-2 py-1 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
                    />
                  </div>
                  <div className="space-y-2">
                    <span className="block text-sm font-medium">MCP connections</span>
                    {agentMcpConnections.length === 0 ? (
                      <p className="text-xs text-muted-foreground">
                        Inherit the runner's existing MCP config when empty.
                      </p>
                    ) : (
                      <div className="space-y-2">
                        {agentMcpConnections.map((conn, idx) => (
                          <div key={conn._formId} className="flex items-center gap-2">
                            <input
                              type="text"
                              value={conn.name}
                              onChange={(e) => updateMcpConnection(idx, "name", e.target.value)}
                              placeholder="MCP server name"
                              className="flex-1 px-3 py-1.5 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
                            />
                            <input
                              type="text"
                              value={conn.url ?? ""}
                              onChange={(e) => updateMcpConnection(idx, "url", e.target.value)}
                              placeholder="URL (optional)"
                              className="flex-1 px-3 py-1.5 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
                            />
                            <button
                              type="button"
                              onClick={() => removeMcpConnection(idx)}
                              className={`p-2 rounded ${getAccentColors("red").text} hover:${getAccentColors("red").bg} transition-colors`}
                              title="Remove MCP connection"
                            >
                              <Trash2 className="w-4 h-4" />
                            </button>
                          </div>
                        ))}
                      </div>
                    )}
                    <button
                      type="button"
                      onClick={addMcpConnection}
                      className="flex items-center gap-1 text-sm text-primary hover:text-primary/80 transition-colors"
                    >
                      <Plus className="w-4 h-4" />
                      Add MCP connection
                    </button>
                  </div>
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Execution Conditions */}
      <div className="space-y-4">
        <div>
          <h4 className="text-sm font-medium">
            Execution Conditions{scheduleType === "condition" ? " *" : ""}
          </h4>
          <p className="text-xs text-muted-foreground mt-1">
            {scheduleType === "condition"
              ? "At least one condition is required for condition-based scheduling"
              : "Optional conditions that must be met before the task executes"}
          </p>
        </div>

        {/* Idle Condition */}
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={requireIdle}
            onChange={(e) => setRequireIdle(e.target.checked)}
            className="text-primary"
          />
          <span className="text-sm">Wait for runner to be idle</span>
        </label>

        {/* Repository Inactivity Condition */}
        <div className="space-y-2">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={requireRepoInactive}
              onChange={(e) => {
                setRequireRepoInactive(e.target.checked);
                if (e.target.checked && repositories.length === 0) {
                  addRepository();
                }
              }}
              className="text-primary"
            />
            <span className="text-sm">Wait for repository inactivity</span>
          </label>

          {requireRepoInactive && (
            <div className="ml-6 space-y-3">
              <p className="text-xs text-muted-foreground">
                All repositories must be inactive (no file changes) for the specified time
              </p>
              {repositories.map((repo, idx) => (
                <div key={repo._formId} className="flex items-center gap-2">
                  <input
                    type="text"
                    value={repo.path}
                    onChange={(e) => updateRepository(idx, "path", e.target.value)}
                    placeholder="Repository path"
                    className="flex-1 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 text-sm"
                  />
                  <button
                    type="button"
                    onClick={() => browseForRepository(idx)}
                    className="p-2 rounded hover:bg-muted transition-colors"
                    title="Browse for directory"
                  >
                    <FolderOpen className="w-4 h-4" />
                  </button>
                  <input
                    type="number"
                    value={repo.inactiveMinutes}
                    onChange={(e) =>
                      updateRepository(idx, "inactiveMinutes", parseInt(e.target.value, 10) || 1)
                    }
                    min={1}
                    className="w-20 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 text-sm"
                  />
                  <span className="text-xs text-muted-foreground">min</span>
                  {repositories.length > 1 && (
                    <button
                      type="button"
                      onClick={() => removeRepository(idx)}
                      className={`p-2 rounded ${getAccentColors("red").text} hover:${getAccentColors("red").bg} transition-colors`}
                      title="Remove repository"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  )}
                </div>
              ))}
              <button
                type="button"
                onClick={addRepository}
                className="flex items-center gap-1 text-sm text-primary hover:text-primary/80 transition-colors"
              >
                <Plus className="w-4 h-4" />
                Add repository
              </button>
            </div>
          )}
        </div>

        {/* Timeout */}
        {(requireIdle || requireRepoInactive) && (
          <div className="flex items-center gap-2">
            <span className="text-sm">Timeout after</span>
            <input
              type="number"
              value={timeoutMinutes ?? ""}
              onChange={(e) =>
                setTimeoutMinutes(e.target.value ? parseInt(e.target.value, 10) : undefined)
              }
              placeholder="No limit"
              min={1}
              className="w-24 px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 text-sm"
            />
            <span className="text-sm text-muted-foreground">minutes (empty = no limit)</span>
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
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-hidden focus:ring-2 focus:ring-primary/50 resize-none"
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
