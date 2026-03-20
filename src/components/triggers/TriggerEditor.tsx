import { useEffect, useState } from "react";
import type {
  WorkflowTrigger,
  TriggerConfig,
  TriggerCondition,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from "../../types/triggers";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface WorkflowOption {
  id: string;
  name: string;
}

interface TriggerEditorProps {
  trigger: WorkflowTrigger | null;
  onSave: (data: CreateTriggerRequest | UpdateTriggerRequest) => Promise<void>;
  onCancel: () => void;
}

type TriggerType =
  | "webhook"
  | "file_watch"
  | "workflow_chain"
  | "git_event"
  | "health_check"
  | "schedule";

const TRIGGER_TYPES: { value: TriggerType; label: string }[] = [
  { value: "webhook", label: "Webhook" },
  { value: "file_watch", label: "File Watch" },
  { value: "workflow_chain", label: "Workflow Chain" },
  { value: "git_event", label: "Git Event" },
  { value: "health_check", label: "Health Check" },
  { value: "schedule", label: "Schedule (Cron)" },
];

/** Validation error inline display */
function FieldError({ message }: { message?: string }) {
  if (!message) return null;
  return <p className="text-xs text-red-400 mt-0.5">{message}</p>;
}

/** Validate a regex pattern, return error message or undefined */
function validateRegex(pattern: string): string | undefined {
  if (!pattern) return undefined;
  try {
    new RegExp(pattern);
    return undefined;
  } catch (e) {
    return `Invalid regex: ${e instanceof Error ? e.message : "syntax error"}`;
  }
}

/** Validate JSON string, return error message or undefined */
function validateJson(text: string): string | undefined {
  if (!text.trim()) return undefined;
  try {
    JSON.parse(text);
    return undefined;
  } catch (e) {
    return `Invalid JSON: ${e instanceof Error ? e.message : "syntax error"}`;
  }
}

/** Validate cron expression (basic 5-field check) */
function validateCron(expr: string): string | undefined {
  if (!expr.trim()) return "Cron expression is required";
  const parts = expr.trim().split(/\s+/);
  if (parts.length < 5 || parts.length > 7) {
    return `Expected 5-7 fields (got ${parts.length}). Format: minute hour day-of-month month day-of-week`;
  }
  return undefined;
}

export function TriggerEditor({ trigger, onSave, onCancel }: TriggerEditorProps) {
  const isEditing = trigger !== null;

  const [name, setName] = useState(trigger?.name ?? "");
  const [description, setDescription] = useState(trigger?.description ?? "");
  const [workflowId, setWorkflowId] = useState(trigger?.workflow_id ?? "");
  const [triggerType, setTriggerType] = useState<TriggerType>(
    (trigger?.trigger_config?.type as TriggerType) ?? "webhook",
  );
  const [debouncMs, setDebounceMs] = useState(trigger?.debounce_ms ?? 1000);
  const [cooldownSeconds, setCooldownSeconds] = useState(trigger?.cooldown_seconds ?? 60);
  const [maxConcurrent, setMaxConcurrent] = useState(trigger?.max_concurrent ?? 1);
  const [retryCount, setRetryCount] = useState(trigger?.retry_count ?? 0);
  const [retryDelaySeconds, setRetryDelaySeconds] = useState(trigger?.retry_delay_seconds ?? 30);
  const [saving, setSaving] = useState(false);
  const [workflows, setWorkflows] = useState<WorkflowOption[]>([]);
  const [workflowOverrides, setWorkflowOverrides] = useState(
    trigger?.workflow_overrides ? JSON.stringify(trigger.workflow_overrides, null, 2) : "",
  );
  const [conditions, setConditions] = useState<TriggerCondition[]>(trigger?.conditions ?? []);
  const [validationErrors, setValidationErrors] = useState<Record<string, string>>({});

  // Fetch available workflows for the picker
  useEffect(() => {
    (async () => {
      try {
        const res = await tracedFetch(`${getApiBase()}/unified-workflows`, {
          headers: { "Content-Type": "application/json" },
        });
        const json = await res.json();
        const data = json.success ? json.data : json;
        if (Array.isArray(data)) {
          setWorkflows(data.map((w: { id: string; name: string }) => ({ id: w.id, name: w.name })));
        }
      } catch {
        // Silently fail — user can still type a workflow ID manually
      }
    })();
  }, []);

  // Type-specific config state
  const [webhookSecret, setWebhookSecret] = useState(
    trigger?.trigger_config?.type === "webhook" ? (trigger.trigger_config.secret ?? "") : "",
  );
  const [webhookPayloadFilter, setWebhookPayloadFilter] = useState(
    trigger?.trigger_config?.type === "webhook"
      ? (trigger.trigger_config.payload_filter ?? "")
      : "",
  );
  const [webhookVariableMapping, setWebhookVariableMapping] = useState(
    trigger?.trigger_config?.type === "webhook"
      ? JSON.stringify(trigger.trigger_config.variable_mapping, null, 2)
      : "{}",
  );

  const [filePaths, setFilePaths] = useState(
    trigger?.trigger_config?.type === "file_watch" ? trigger.trigger_config.paths.join("\n") : "",
  );
  const [filePatterns, setFilePatterns] = useState(
    trigger?.trigger_config?.type === "file_watch"
      ? trigger.trigger_config.patterns.join("\n")
      : "*",
  );
  const [fileIgnorePatterns, setFileIgnorePatterns] = useState(
    trigger?.trigger_config?.type === "file_watch"
      ? trigger.trigger_config.ignore_patterns.join("\n")
      : "",
  );
  const [fileRecursive, setFileRecursive] = useState(
    trigger?.trigger_config?.type === "file_watch" ? trigger.trigger_config.recursive : true,
  );

  const [chainSourceWorkflowId, setChainSourceWorkflowId] = useState(
    trigger?.trigger_config?.type === "workflow_chain"
      ? trigger.trigger_config.source_workflow_id
      : "",
  );
  const [chainOnStatus, setChainOnStatus] = useState(
    trigger?.trigger_config?.type === "workflow_chain"
      ? trigger.trigger_config.on_status
      : "completed",
  );
  const [chainPassContext, setChainPassContext] = useState(
    trigger?.trigger_config?.type === "workflow_chain" ? trigger.trigger_config.pass_context : true,
  );

  const [gitRepoPath, setGitRepoPath] = useState(
    trigger?.trigger_config?.type === "git_event" ? trigger.trigger_config.repo_path : ".",
  );
  const [gitEvents, setGitEvents] = useState(
    trigger?.trigger_config?.type === "git_event"
      ? trigger.trigger_config.events.join(", ")
      : "commit",
  );
  const [gitBranchFilter, setGitBranchFilter] = useState(
    trigger?.trigger_config?.type === "git_event"
      ? (trigger.trigger_config.branch_filter ?? "")
      : "",
  );

  const [healthUrls, setHealthUrls] = useState(
    trigger?.trigger_config?.type === "health_check"
      ? trigger.trigger_config.urls
          .map((u) => `${u.url}|${u.expected_status}|${u.timeout_seconds}`)
          .join("\n")
      : "",
  );
  const [healthInterval, setHealthInterval] = useState(
    trigger?.trigger_config?.type === "health_check"
      ? trigger.trigger_config.check_interval_seconds
      : 60,
  );
  const [healthConsecutiveFailures, setHealthConsecutiveFailures] = useState(
    trigger?.trigger_config?.type === "health_check"
      ? trigger.trigger_config.consecutive_failures
      : 3,
  );

  const [cronExpression, setCronExpression] = useState(
    trigger?.trigger_config?.type === "schedule"
      ? trigger.trigger_config.cron_expression
      : "0 */6 * * *",
  );
  const [cronTimezone, setCronTimezone] = useState(
    trigger?.trigger_config?.type === "schedule" ? trigger.trigger_config.timezone : "UTC",
  );

  /** Run all validations, return true if valid */
  const validate = (): boolean => {
    const errors: Record<string, string> = {};

    if (!name.trim()) errors.name = "Name is required";
    if (!workflowId.trim()) errors.workflowId = "Workflow is required";

    // Type-specific validation
    if (triggerType === "webhook") {
      const mappingErr = validateJson(webhookVariableMapping);
      if (mappingErr) errors.webhookVariableMapping = mappingErr;
    }
    if (triggerType === "git_event") {
      const filterErr = validateRegex(gitBranchFilter);
      if (filterErr) errors.gitBranchFilter = filterErr;
    }
    if (triggerType === "schedule") {
      const cronErr = validateCron(cronExpression);
      if (cronErr) errors.cronExpression = cronErr;
    }
    if (triggerType === "file_watch") {
      if (!filePaths.trim()) errors.filePaths = "At least one watch path is required";
    }
    if (triggerType === "health_check") {
      if (!healthUrls.trim()) errors.healthUrls = "At least one URL is required";
    }

    // Workflow overrides validation
    const overridesErr = validateJson(workflowOverrides);
    if (overridesErr) errors.workflowOverrides = overridesErr;

    // Condition validation
    conditions.forEach((cond, idx) => {
      if (cond.type === "branch_filter") {
        const pattern = (cond.config as Record<string, string>)?.pattern ?? "";
        const err = validateRegex(pattern);
        if (err) errors[`condition-${idx}`] = err;
      }
    });

    setValidationErrors(errors);
    return Object.keys(errors).length === 0;
  };

  const buildConfig = (): TriggerConfig => {
    switch (triggerType) {
      case "webhook": {
        let mapping: Record<string, string> = {};
        try {
          mapping = JSON.parse(webhookVariableMapping);
        } catch {
          // keep empty
        }
        return {
          type: "webhook",
          secret: webhookSecret || undefined,
          payload_filter: webhookPayloadFilter || undefined,
          variable_mapping: mapping,
        };
      }
      case "file_watch":
        return {
          type: "file_watch",
          paths: filePaths
            .split("\n")
            .map((s) => s.trim())
            .filter(Boolean),
          patterns: filePatterns
            .split("\n")
            .map((s) => s.trim())
            .filter(Boolean),
          ignore_patterns: fileIgnorePatterns
            .split("\n")
            .map((s) => s.trim())
            .filter(Boolean),
          recursive: fileRecursive,
        };
      case "workflow_chain":
        return {
          type: "workflow_chain",
          source_workflow_id: chainSourceWorkflowId,
          on_status: chainOnStatus,
          pass_context: chainPassContext,
        };
      case "git_event":
        return {
          type: "git_event",
          repo_path: gitRepoPath,
          events: gitEvents
            .split(",")
            .map((s) => s.trim())
            .filter(Boolean),
          branch_filter: gitBranchFilter || undefined,
        };
      case "health_check":
        return {
          type: "health_check",
          urls: healthUrls
            .split("\n")
            .map((line) => line.trim())
            .filter(Boolean)
            .map((line) => {
              const parts = line.split("|");
              return {
                url: parts[0] || "",
                expected_status: parseInt(parts[1] || "200", 10),
                timeout_seconds: parseInt(parts[2] || "10", 10),
              };
            }),
          check_interval_seconds: healthInterval,
          consecutive_failures: healthConsecutiveFailures,
        };
      case "schedule":
        return {
          type: "schedule",
          cron_expression: cronExpression,
          timezone: cronTimezone,
        };
    }
  };

  const parseOverrides = (): Record<string, unknown> | undefined => {
    if (!workflowOverrides.trim()) return undefined;
    try {
      return JSON.parse(workflowOverrides);
    } catch {
      return undefined;
    }
  };

  const handleSubmit = async () => {
    if (!validate()) return;
    setSaving(true);
    try {
      const config = buildConfig();
      const overrides = parseOverrides();
      const conds = conditions.length > 0 ? conditions : undefined;
      if (isEditing) {
        const data: UpdateTriggerRequest = {
          name,
          description: description || null,
          trigger_config: config,
          workflow_id: workflowId,
          workflow_overrides: overrides ?? null,
          conditions: conds,
          debounce_ms: debouncMs,
          cooldown_seconds: cooldownSeconds,
          max_concurrent: maxConcurrent,
          retry_count: retryCount,
          retry_delay_seconds: retryDelaySeconds,
        };
        await onSave(data);
      } else {
        const data: CreateTriggerRequest = {
          name,
          trigger_config: config,
          workflow_id: workflowId,
          description: description || undefined,
          workflow_overrides: overrides,
          conditions: conds,
          debounce_ms: debouncMs,
          cooldown_seconds: cooldownSeconds,
          max_concurrent: maxConcurrent,
          retry_count: retryCount,
          retry_delay_seconds: retryDelaySeconds,
        };
        await onSave(data);
      }
    } finally {
      setSaving(false);
    }
  };

  const inputClass =
    "w-full px-3 py-1.5 text-sm bg-gray-800 border border-gray-600 rounded text-white placeholder-gray-500 focus:border-blue-500 focus:outline-none";
  const inputErrorClass =
    "w-full px-3 py-1.5 text-sm bg-gray-800 border border-red-500 rounded text-white placeholder-gray-500 focus:border-red-400 focus:outline-none";
  const labelClass = "block text-xs font-medium text-gray-400 mb-1";

  const getInputClass = (field: string) => (validationErrors[field] ? inputErrorClass : inputClass);

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
        <h2 className="text-lg font-semibold text-white">
          {isEditing ? "Edit Trigger" : "New Trigger"}
        </h2>
        <div className="flex items-center gap-2">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={saving}
            className="px-3 py-1.5 text-xs text-white bg-blue-600 rounded hover:bg-blue-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {saving ? "Saving..." : isEditing ? "Update" : "Create"}
          </button>
        </div>
      </div>

      {/* Form */}
      <div className="flex-1 overflow-auto p-4 space-y-4">
        {/* Basic fields */}
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className={labelClass}>Name *</label>
            <input
              className={getInputClass("name")}
              value={name}
              onChange={(e) => {
                setName(e.target.value);
                if (validationErrors.name) {
                  setValidationErrors((prev) => {
                    const next = { ...prev };
                    delete next.name;
                    return next;
                  });
                }
              }}
              placeholder="My trigger"
            />
            <FieldError message={validationErrors.name} />
          </div>
          <div>
            <label className={labelClass}>Target Workflow *</label>
            {workflows.length > 0 ? (
              <select
                className={getInputClass("workflowId")}
                value={workflowId}
                onChange={(e) => {
                  setWorkflowId(e.target.value);
                  if (validationErrors.workflowId) {
                    setValidationErrors((prev) => {
                      const next = { ...prev };
                      delete next.workflowId;
                      return next;
                    });
                  }
                }}
              >
                <option value="">Select a workflow...</option>
                {workflows.map((w) => (
                  <option key={w.id} value={w.id}>
                    {w.name}
                  </option>
                ))}
              </select>
            ) : (
              <input
                className={getInputClass("workflowId")}
                value={workflowId}
                onChange={(e) => setWorkflowId(e.target.value)}
                placeholder="workflow-uuid"
              />
            )}
            <FieldError message={validationErrors.workflowId} />
          </div>
        </div>

        <div>
          <label className={labelClass}>Description</label>
          <input
            className={inputClass}
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Optional description"
          />
        </div>

        {/* Trigger type selector */}
        <div>
          <label className={labelClass}>Trigger Type</label>
          <select
            className={inputClass}
            value={triggerType}
            onChange={(e) => setTriggerType(e.target.value as TriggerType)}
            disabled={isEditing}
          >
            {TRIGGER_TYPES.map((t) => (
              <option key={t.value} value={t.value}>
                {t.label}
              </option>
            ))}
          </select>
        </div>

        {/* Type-specific config */}
        <div className="border border-gray-700 rounded p-3 space-y-3">
          <h3 className="text-sm font-medium text-gray-300">
            {TRIGGER_TYPES.find((t) => t.value === triggerType)?.label} Configuration
          </h3>

          {triggerType === "webhook" && (
            <>
              {/* Show webhook URL for existing triggers */}
              {isEditing && trigger && (
                <div className="bg-gray-900/50 rounded p-2">
                  <label className={labelClass}>Webhook URL</label>
                  <code className="text-xs text-blue-400 font-mono break-all">
                    {getApiBase()}/triggers/webhook/{trigger.id}
                  </code>
                </div>
              )}
              <div>
                <label className={labelClass}>Secret (HMAC-SHA256)</label>
                <input
                  className={inputClass}
                  type="password"
                  value={webhookSecret}
                  onChange={(e) => setWebhookSecret(e.target.value)}
                  placeholder="Optional webhook secret"
                />
              </div>
              <div>
                <label className={labelClass}>Payload Filter (JSONPath expression)</label>
                <input
                  className={inputClass}
                  value={webhookPayloadFilter}
                  onChange={(e) => setWebhookPayloadFilter(e.target.value)}
                  placeholder="e.g. $.action"
                />
                <p className="text-xs text-gray-500 mt-0.5">
                  Must resolve to a truthy value for the trigger to fire
                </p>
              </div>
              <div>
                <label className={labelClass}>Variable Mapping (JSON)</label>
                <textarea
                  className={`${getInputClass("webhookVariableMapping")} h-20 font-mono`}
                  value={webhookVariableMapping}
                  onChange={(e) => {
                    setWebhookVariableMapping(e.target.value);
                    if (validationErrors.webhookVariableMapping) {
                      setValidationErrors((prev) => {
                        const next = { ...prev };
                        delete next.webhookVariableMapping;
                        return next;
                      });
                    }
                  }}
                  placeholder='{"branch": "$.ref", "author": "$.sender.login"}'
                />
                <FieldError message={validationErrors.webhookVariableMapping} />
              </div>
            </>
          )}

          {triggerType === "file_watch" && (
            <>
              <div>
                <label className={labelClass}>Watch Paths (one per line) *</label>
                <textarea
                  className={`${getInputClass("filePaths")} h-20 font-mono`}
                  value={filePaths}
                  onChange={(e) => setFilePaths(e.target.value)}
                  placeholder="/path/to/watch"
                />
                <FieldError message={validationErrors.filePaths} />
              </div>
              <div>
                <label className={labelClass}>Include Patterns (one per line)</label>
                <textarea
                  className={`${inputClass} h-16 font-mono`}
                  value={filePatterns}
                  onChange={(e) => setFilePatterns(e.target.value)}
                  placeholder="*.rs&#10;*.ts"
                />
              </div>
              <div>
                <label className={labelClass}>Ignore Patterns (one per line)</label>
                <textarea
                  className={`${inputClass} h-16 font-mono`}
                  value={fileIgnorePatterns}
                  onChange={(e) => setFileIgnorePatterns(e.target.value)}
                  placeholder="node_modules/**&#10;target/**"
                />
              </div>
              <div className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={fileRecursive}
                  onChange={(e) => setFileRecursive(e.target.checked)}
                  className="rounded"
                />
                <span className="text-sm text-gray-300">Recursive</span>
              </div>
            </>
          )}

          {triggerType === "workflow_chain" && (
            <>
              <div>
                <label className={labelClass}>Source Workflow</label>
                {workflows.length > 0 ? (
                  <select
                    className={inputClass}
                    value={chainSourceWorkflowId}
                    onChange={(e) => setChainSourceWorkflowId(e.target.value)}
                  >
                    <option value="">Select source workflow...</option>
                    {workflows.map((w) => (
                      <option key={w.id} value={w.id}>
                        {w.name}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    className={inputClass}
                    value={chainSourceWorkflowId}
                    onChange={(e) => setChainSourceWorkflowId(e.target.value)}
                    placeholder="UUID of the workflow to chain from"
                  />
                )}
              </div>
              <div>
                <label className={labelClass}>Trigger On Status</label>
                <select
                  className={inputClass}
                  value={chainOnStatus}
                  onChange={(e) => setChainOnStatus(e.target.value)}
                >
                  <option value="completed">Completed</option>
                  <option value="failed">Failed</option>
                  <option value="any">Any</option>
                </select>
              </div>
              <div className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={chainPassContext}
                  onChange={(e) => setChainPassContext(e.target.checked)}
                  className="rounded"
                />
                <span className="text-sm text-gray-300">Pass execution context</span>
              </div>
            </>
          )}

          {triggerType === "git_event" && (
            <>
              <div>
                <label className={labelClass}>Repository Path</label>
                <input
                  className={inputClass}
                  value={gitRepoPath}
                  onChange={(e) => setGitRepoPath(e.target.value)}
                  placeholder="."
                />
              </div>
              <div>
                <label className={labelClass}>Events (comma-separated)</label>
                <input
                  className={inputClass}
                  value={gitEvents}
                  onChange={(e) => setGitEvents(e.target.value)}
                  placeholder="commit, branch_switch, tag"
                />
              </div>
              <div>
                <label className={labelClass}>Branch Filter (regex)</label>
                <input
                  className={getInputClass("gitBranchFilter")}
                  value={gitBranchFilter}
                  onChange={(e) => {
                    setGitBranchFilter(e.target.value);
                    if (validationErrors.gitBranchFilter) {
                      setValidationErrors((prev) => {
                        const next = { ...prev };
                        delete next.gitBranchFilter;
                        return next;
                      });
                    }
                  }}
                  placeholder="^main$|^develop$"
                />
                <FieldError message={validationErrors.gitBranchFilter} />
              </div>
            </>
          )}

          {triggerType === "health_check" && (
            <>
              <div>
                <label className={labelClass}>
                  URLs (one per line: url|expected_status|timeout_seconds) *
                </label>
                <textarea
                  className={`${getInputClass("healthUrls")} h-20 font-mono`}
                  value={healthUrls}
                  onChange={(e) => setHealthUrls(e.target.value)}
                  placeholder="https://example.com|200|10"
                />
                <FieldError message={validationErrors.healthUrls} />
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <label className={labelClass}>Check Interval (seconds)</label>
                  <input
                    className={inputClass}
                    type="number"
                    value={healthInterval}
                    onChange={(e) => setHealthInterval(parseInt(e.target.value, 10) || 60)}
                  />
                </div>
                <div>
                  <label className={labelClass}>Consecutive Failures</label>
                  <input
                    className={inputClass}
                    type="number"
                    value={healthConsecutiveFailures}
                    onChange={(e) =>
                      setHealthConsecutiveFailures(parseInt(e.target.value, 10) || 3)
                    }
                  />
                </div>
              </div>
            </>
          )}

          {triggerType === "schedule" && (
            <>
              <div>
                <label className={labelClass}>Cron Expression *</label>
                <input
                  className={`${getInputClass("cronExpression")} font-mono`}
                  value={cronExpression}
                  onChange={(e) => {
                    setCronExpression(e.target.value);
                    if (validationErrors.cronExpression) {
                      setValidationErrors((prev) => {
                        const next = { ...prev };
                        delete next.cronExpression;
                        return next;
                      });
                    }
                  }}
                  placeholder="0 */6 * * *"
                />
                <FieldError message={validationErrors.cronExpression} />
                {!validationErrors.cronExpression && (
                  <p className="text-xs text-gray-500 mt-1">
                    Format: minute hour day-of-month month day-of-week (e.g., "0 */6 * * *" = every
                    6 hours)
                  </p>
                )}
              </div>
              <div>
                <label className={labelClass}>Timezone</label>
                <select
                  className={inputClass}
                  value={cronTimezone}
                  onChange={(e) => setCronTimezone(e.target.value)}
                >
                  <option value="UTC">UTC</option>
                  <option value="Local">Local (system timezone)</option>
                  <optgroup label="Americas">
                    <option value="America/New_York">America/New_York (EST/EDT)</option>
                    <option value="America/Chicago">America/Chicago (CST/CDT)</option>
                    <option value="America/Denver">America/Denver (MST/MDT)</option>
                    <option value="America/Los_Angeles">America/Los_Angeles (PST/PDT)</option>
                    <option value="America/Anchorage">America/Anchorage (AKST)</option>
                    <option value="America/Sao_Paulo">America/Sao_Paulo (BRT)</option>
                    <option value="America/Toronto">America/Toronto (EST/EDT)</option>
                    <option value="America/Vancouver">America/Vancouver (PST/PDT)</option>
                  </optgroup>
                  <optgroup label="Europe">
                    <option value="Europe/London">Europe/London (GMT/BST)</option>
                    <option value="Europe/Paris">Europe/Paris (CET/CEST)</option>
                    <option value="Europe/Berlin">Europe/Berlin (CET/CEST)</option>
                    <option value="Europe/Amsterdam">Europe/Amsterdam (CET/CEST)</option>
                    <option value="Europe/Moscow">Europe/Moscow (MSK)</option>
                    <option value="Europe/Istanbul">Europe/Istanbul (TRT)</option>
                  </optgroup>
                  <optgroup label="Asia/Pacific">
                    <option value="Asia/Tokyo">Asia/Tokyo (JST)</option>
                    <option value="Asia/Shanghai">Asia/Shanghai (CST)</option>
                    <option value="Asia/Kolkata">Asia/Kolkata (IST)</option>
                    <option value="Asia/Dubai">Asia/Dubai (GST)</option>
                    <option value="Asia/Singapore">Asia/Singapore (SGT)</option>
                    <option value="Asia/Seoul">Asia/Seoul (KST)</option>
                    <option value="Australia/Sydney">Australia/Sydney (AEST/AEDT)</option>
                    <option value="Pacific/Auckland">Pacific/Auckland (NZST/NZDT)</option>
                  </optgroup>
                </select>
                <p className="text-xs text-gray-500 mt-1">Or type a custom IANA timezone below</p>
                <input
                  className={`${inputClass} mt-1`}
                  value={cronTimezone}
                  onChange={(e) => setCronTimezone(e.target.value)}
                  placeholder="e.g. America/New_York"
                />
              </div>
            </>
          )}
        </div>

        {/* Rate limiting */}
        <div className="border border-gray-700 rounded p-3 space-y-3">
          <h3 className="text-sm font-medium text-gray-300">Rate Limiting</h3>
          <div className="grid grid-cols-3 gap-4">
            <div>
              <label className={labelClass}>Debounce (ms)</label>
              <input
                className={inputClass}
                type="number"
                value={debouncMs}
                onChange={(e) => setDebounceMs(parseInt(e.target.value, 10) || 0)}
              />
              <p className="text-xs text-gray-500 mt-0.5">Coalesce rapid events</p>
            </div>
            <div>
              <label className={labelClass}>Cooldown (seconds)</label>
              <input
                className={inputClass}
                type="number"
                value={cooldownSeconds}
                onChange={(e) => setCooldownSeconds(parseInt(e.target.value, 10) || 0)}
              />
              <p className="text-xs text-gray-500 mt-0.5">Min time between executions</p>
            </div>
            <div>
              <label className={labelClass}>Max Concurrent</label>
              <input
                className={inputClass}
                type="number"
                value={maxConcurrent}
                onChange={(e) => setMaxConcurrent(parseInt(e.target.value, 10) || 1)}
              />
              <p className="text-xs text-gray-500 mt-0.5">Parallel workflow limit</p>
            </div>
          </div>
        </div>

        {/* Retry on failure */}
        <div className="border border-gray-700 rounded p-3 space-y-3">
          <h3 className="text-sm font-medium text-gray-300">Retry on Failure</h3>
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className={labelClass}>Retry Count</label>
              <input
                className={inputClass}
                type="number"
                min={0}
                max={10}
                value={retryCount}
                onChange={(e) => setRetryCount(parseInt(e.target.value, 10) || 0)}
              />
              <p className="text-xs text-gray-500 mt-0.5">0 = no retries, max 10</p>
            </div>
            <div>
              <label className={labelClass}>Retry Delay (seconds)</label>
              <input
                className={inputClass}
                type="number"
                min={1}
                value={retryDelaySeconds}
                onChange={(e) => setRetryDelaySeconds(parseInt(e.target.value, 10) || 30)}
              />
              <p className="text-xs text-gray-500 mt-0.5">Base delay, doubles each retry</p>
            </div>
          </div>
        </div>

        {/* Workflow Overrides */}
        <div className="border border-gray-700 rounded p-3 space-y-3">
          <h3 className="text-sm font-medium text-gray-300">Workflow Overrides</h3>
          <p className="text-xs text-gray-500">
            Override workflow settings when triggered (e.g., max_iterations, model). Leave empty to
            use workflow defaults.
          </p>
          <textarea
            className={`${getInputClass("workflowOverrides")} h-24 font-mono`}
            value={workflowOverrides}
            onChange={(e) => {
              setWorkflowOverrides(e.target.value);
              if (validationErrors.workflowOverrides) {
                setValidationErrors((prev) => {
                  const next = { ...prev };
                  delete next.workflowOverrides;
                  return next;
                });
              }
            }}
            placeholder='{"max_iterations": 3, "model": "claude-sonnet-4-5-20250514"}'
          />
          <FieldError message={validationErrors.workflowOverrides} />
        </div>

        {/* Conditions */}
        <div className="border border-gray-700 rounded p-3 space-y-3">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-medium text-gray-300">Pre-fire Conditions</h3>
            <button
              type="button"
              onClick={() => setConditions([...conditions, { type: "require_idle", config: {} }])}
              className="px-2 py-1 text-xs text-gray-300 bg-gray-700 rounded hover:bg-gray-600"
            >
              + Add Condition
            </button>
          </div>
          {conditions.length === 0 && (
            <p className="text-xs text-gray-500">
              No conditions. Trigger will fire whenever the event matches.
            </p>
          )}
          {conditions.map((condition, idx) => (
            <div
              key={`${condition.type || "cond"}-${idx}`}
              className="flex items-start gap-2 bg-gray-800/50 rounded p-2"
            >
              <div className="flex-1 space-y-2">
                <div>
                  <label className={labelClass}>Type</label>
                  <select
                    className={inputClass}
                    value={condition.type}
                    onChange={(e) => {
                      const updated = [...conditions];
                      updated[idx] = { ...updated[idx], type: e.target.value, config: {} };
                      setConditions(updated);
                    }}
                  >
                    <option value="require_idle">Require Idle (no running workflows)</option>
                    <option value="branch_filter">Branch Filter</option>
                    <option value="time_window">Time Window (business hours)</option>
                    <option value="custom">Custom (JSON)</option>
                  </select>
                </div>
                {condition.type === "branch_filter" && (
                  <div>
                    <label className={labelClass}>Branch Pattern (regex)</label>
                    <input
                      className={getInputClass(`condition-${idx}`)}
                      value={(condition.config as Record<string, string>)?.pattern ?? ""}
                      onChange={(e) => {
                        const updated = [...conditions];
                        updated[idx] = {
                          ...updated[idx],
                          config: { pattern: e.target.value },
                        };
                        setConditions(updated);
                      }}
                      placeholder="^main$|^release/.*"
                    />
                    <FieldError message={validationErrors[`condition-${idx}`]} />
                  </div>
                )}
                {condition.type === "time_window" && (
                  <div className="space-y-2">
                    <div className="grid grid-cols-2 gap-2">
                      <div>
                        <label className={labelClass}>Start Hour (0-23)</label>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          max={23}
                          value={(condition.config as Record<string, number>)?.start_hour ?? 9}
                          onChange={(e) => {
                            const updated = [...conditions];
                            updated[idx] = {
                              ...updated[idx],
                              config: {
                                ...(updated[idx].config as Record<string, unknown>),
                                start_hour: parseInt(e.target.value, 10) || 0,
                              },
                            };
                            setConditions(updated);
                          }}
                        />
                      </div>
                      <div>
                        <label className={labelClass}>End Hour (0-24)</label>
                        <input
                          className={inputClass}
                          type="number"
                          min={0}
                          max={24}
                          value={(condition.config as Record<string, number>)?.end_hour ?? 17}
                          onChange={(e) => {
                            const updated = [...conditions];
                            updated[idx] = {
                              ...updated[idx],
                              config: {
                                ...(updated[idx].config as Record<string, unknown>),
                                end_hour: parseInt(e.target.value, 10) || 24,
                              },
                            };
                            setConditions(updated);
                          }}
                        />
                      </div>
                    </div>
                    <div>
                      <label className={labelClass}>Days (0=Sun, 1=Mon, ..., 6=Sat)</label>
                      <div className="flex gap-1">
                        {["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].map((day, dayIdx) => {
                          const days = ((condition.config as Record<string, unknown>)
                            ?.days as number[]) ?? [1, 2, 3, 4, 5];
                          const isSelected = days.includes(dayIdx);
                          return (
                            <button
                              key={day}
                              type="button"
                              className={`px-2 py-1 text-xs rounded ${
                                isSelected ? "bg-blue-600 text-white" : "bg-gray-700 text-gray-400"
                              }`}
                              onClick={() => {
                                const updated = [...conditions];
                                const newDays = isSelected
                                  ? days.filter((d) => d !== dayIdx)
                                  : [...days, dayIdx].sort();
                                updated[idx] = {
                                  ...updated[idx],
                                  config: {
                                    ...(updated[idx].config as Record<string, unknown>),
                                    days: newDays,
                                  },
                                };
                                setConditions(updated);
                              }}
                            >
                              {day}
                            </button>
                          );
                        })}
                      </div>
                    </div>
                    <div>
                      <label className={labelClass}>Timezone</label>
                      <select
                        className={inputClass}
                        value={(condition.config as Record<string, string>)?.timezone ?? "Local"}
                        onChange={(e) => {
                          const updated = [...conditions];
                          updated[idx] = {
                            ...updated[idx],
                            config: {
                              ...(updated[idx].config as Record<string, unknown>),
                              timezone: e.target.value,
                            },
                          };
                          setConditions(updated);
                        }}
                      >
                        <option value="Local">Local</option>
                        <option value="UTC">UTC</option>
                      </select>
                    </div>
                  </div>
                )}
                {condition.type === "custom" && (
                  <div>
                    <label className={labelClass}>Config (JSON)</label>
                    <textarea
                      className={`${inputClass} h-16 font-mono`}
                      value={JSON.stringify(condition.config, null, 2)}
                      onChange={(e) => {
                        try {
                          const parsed = JSON.parse(e.target.value);
                          const updated = [...conditions];
                          updated[idx] = { ...updated[idx], config: parsed };
                          setConditions(updated);
                        } catch {
                          // Keep current value while user is typing
                        }
                      }}
                    />
                  </div>
                )}
              </div>
              <button
                type="button"
                onClick={() => setConditions(conditions.filter((_, i) => i !== idx))}
                className="px-2 py-1 text-xs text-red-400 bg-gray-700 rounded hover:bg-red-900/30 mt-5"
              >
                Del
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
