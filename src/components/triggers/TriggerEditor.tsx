import { useState } from "react";
import type {
  WorkflowTrigger,
  TriggerConfig,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from "../../types/triggers";

interface TriggerEditorProps {
  trigger: WorkflowTrigger | null;
  onSave: (data: CreateTriggerRequest | UpdateTriggerRequest) => Promise<void>;
  onCancel: () => void;
}

type TriggerType = "webhook" | "file_watch" | "workflow_chain" | "git_event" | "health_check";

const TRIGGER_TYPES: { value: TriggerType; label: string }[] = [
  { value: "webhook", label: "Webhook" },
  { value: "file_watch", label: "File Watch" },
  { value: "workflow_chain", label: "Workflow Chain" },
  { value: "git_event", label: "Git Event" },
  { value: "health_check", label: "Health Check" },
];

function getDefaultConfig(type: TriggerType): TriggerConfig {
  switch (type) {
    case "webhook":
      return { type: "webhook", variable_mapping: {} };
    case "file_watch":
      return {
        type: "file_watch",
        paths: [],
        patterns: ["*"],
        ignore_patterns: [],
        recursive: true,
      };
    case "workflow_chain":
      return {
        type: "workflow_chain",
        source_workflow_id: "",
        on_status: "completed",
        pass_context: true,
      };
    case "git_event":
      return {
        type: "git_event",
        repo_path: ".",
        events: ["commit"],
      };
    case "health_check":
      return {
        type: "health_check",
        urls: [{ url: "", expected_status: 200, timeout_seconds: 10 }],
        check_interval_seconds: 60,
        consecutive_failures: 3,
      };
  }
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
  const [saving, setSaving] = useState(false);

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
    }
  };

  const handleSubmit = async () => {
    if (!name.trim() || !workflowId.trim()) return;
    setSaving(true);
    try {
      const config = buildConfig();
      if (isEditing) {
        const data: UpdateTriggerRequest = {
          name,
          description: description || undefined,
          trigger_config: config,
          workflow_id: workflowId,
          debounce_ms: debouncMs,
          cooldown_seconds: cooldownSeconds,
          max_concurrent: maxConcurrent,
        };
        await onSave(data);
      } else {
        const data: CreateTriggerRequest = {
          name,
          trigger_config: config,
          workflow_id: workflowId,
          description: description || undefined,
          debounce_ms: debouncMs,
          cooldown_seconds: cooldownSeconds,
          max_concurrent: maxConcurrent,
        };
        await onSave(data);
      }
    } finally {
      setSaving(false);
    }
  };

  const inputClass =
    "w-full px-3 py-1.5 text-sm bg-gray-800 border border-gray-600 rounded text-white placeholder-gray-500 focus:border-blue-500 focus:outline-none";
  const labelClass = "block text-xs font-medium text-gray-400 mb-1";

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
            disabled={saving || !name.trim() || !workflowId.trim()}
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
              className={inputClass}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="My trigger"
            />
          </div>
          <div>
            <label className={labelClass}>Workflow ID *</label>
            <input
              className={inputClass}
              value={workflowId}
              onChange={(e) => setWorkflowId(e.target.value)}
              placeholder="workflow-uuid"
            />
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
                <label className={labelClass}>Payload Filter (JSON path expression)</label>
                <input
                  className={inputClass}
                  value={webhookPayloadFilter}
                  onChange={(e) => setWebhookPayloadFilter(e.target.value)}
                  placeholder='e.g. $.action == "opened"'
                />
              </div>
              <div>
                <label className={labelClass}>Variable Mapping (JSON)</label>
                <textarea
                  className={`${inputClass} h-20 font-mono`}
                  value={webhookVariableMapping}
                  onChange={(e) => setWebhookVariableMapping(e.target.value)}
                  placeholder='{"branch": "$.ref", "author": "$.sender.login"}'
                />
              </div>
            </>
          )}

          {triggerType === "file_watch" && (
            <>
              <div>
                <label className={labelClass}>Watch Paths (one per line)</label>
                <textarea
                  className={`${inputClass} h-20 font-mono`}
                  value={filePaths}
                  onChange={(e) => setFilePaths(e.target.value)}
                  placeholder="/path/to/watch"
                />
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
                <label className={labelClass}>Source Workflow ID</label>
                <input
                  className={inputClass}
                  value={chainSourceWorkflowId}
                  onChange={(e) => setChainSourceWorkflowId(e.target.value)}
                  placeholder="UUID of the workflow to chain from"
                />
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
                  className={inputClass}
                  value={gitBranchFilter}
                  onChange={(e) => setGitBranchFilter(e.target.value)}
                  placeholder="^main$|^develop$"
                />
              </div>
            </>
          )}

          {triggerType === "health_check" && (
            <>
              <div>
                <label className={labelClass}>
                  URLs (one per line: url|expected_status|timeout_seconds)
                </label>
                <textarea
                  className={`${inputClass} h-20 font-mono`}
                  value={healthUrls}
                  onChange={(e) => setHealthUrls(e.target.value)}
                  placeholder="https://example.com|200|10"
                />
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
            </div>
            <div>
              <label className={labelClass}>Cooldown (seconds)</label>
              <input
                className={inputClass}
                type="number"
                value={cooldownSeconds}
                onChange={(e) => setCooldownSeconds(parseInt(e.target.value, 10) || 0)}
              />
            </div>
            <div>
              <label className={labelClass}>Max Concurrent</label>
              <input
                className={inputClass}
                type="number"
                value={maxConcurrent}
                onChange={(e) => setMaxConcurrent(parseInt(e.target.value, 10) || 1)}
              />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
