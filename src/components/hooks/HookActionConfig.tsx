/**
 * HookActionConfig
 *
 * Form component for configuring hook action settings based on action type.
 */

import { useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import type {
  HookAction,
  HookActionType,
  CommandAction,
  WebhookAction,
  LogAction,
  NotificationAction,
} from "../../types/hooks";
import {
  HOOK_ACTION_TYPE_DISPLAY_NAMES,
  HOOK_ACTION_TYPE_DESCRIPTIONS,
  createDefaultAction,
} from "../../types/hooks";

interface HookActionConfigProps {
  actionType: HookActionType;
  actionConfig: HookAction;
  onChange: (actionType: HookActionType, config: HookAction) => void;
}

const HTTP_METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE"];
const LOG_LEVELS = ["debug", "info", "warn", "error"];

// Command Action Config Component
interface CommandConfigProps {
  config: CommandAction;
  actionType: HookActionType;
  onChange: (actionType: HookActionType, config: HookAction) => void;
}

function CommandConfig({ config, actionType, onChange }: CommandConfigProps) {
  const [newEnvKey, setNewEnvKey] = useState("");
  const [newEnvValue, setNewEnvValue] = useState("");

  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Command *</label>
        <textarea
          aria-label="Command"
          value={config.command}
          onChange={(e) => onChange(actionType, { ...config, command: e.target.value })}
          placeholder="echo 'Hello World'"
          rows={3}
          className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm font-mono resize-none focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
        <p className="mt-1 text-xs text-muted-foreground">
          Supports variable substitution: {"{{task_name}}"}, {"{{iteration}}"}, {"{{status}}"}, etc.
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">
          Working Directory
        </label>
        <input
          type="text"
          aria-label="Working Directory"
          value={config.working_dir || ""}
          onChange={(e) =>
            onChange(actionType, {
              ...config,
              working_dir: e.target.value || undefined,
            })
          }
          placeholder="C:\path\to\directory"
          className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">
          Timeout (seconds)
        </label>
        <input
          type="number"
          aria-label="Timeout (seconds)"
          value={config.timeout_seconds}
          onChange={(e) =>
            onChange(actionType, {
              ...config,
              timeout_seconds: parseInt(e.target.value) || 30,
            })
          }
          min={1}
          max={600}
          className="w-32 px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">
          Environment Variables
        </label>
        <div className="space-y-2">
          {Object.entries(config.env || {}).map(([key, value]) => (
            <div key={key} className="flex items-center gap-2">
              <input
                type="text"
                value={key}
                disabled
                className="w-1/3 px-2 py-1 bg-muted border border-border rounded text-sm font-mono"
              />
              <input
                type="text"
                value={value}
                onChange={(e) => {
                  const newEnv = { ...config.env };
                  newEnv[key] = e.target.value;
                  onChange(actionType, { ...config, env: newEnv });
                }}
                className="flex-1 px-2 py-1 bg-background border border-border rounded text-sm font-mono focus:outline-hidden focus:ring-2 focus:ring-primary/50"
              />
              <button
                onClick={() => {
                  const newEnv = { ...config.env };
                  delete newEnv[key];
                  onChange(actionType, { ...config, env: newEnv });
                }}
                className="p-1 text-destructive hover:bg-destructive/10 rounded"
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          ))}
          <div className="flex items-center gap-2">
            <input
              type="text"
              aria-label="Environment variable key"
              value={newEnvKey}
              onChange={(e) => setNewEnvKey(e.target.value)}
              placeholder="KEY"
              className="w-1/3 px-2 py-1 bg-background border border-border rounded text-sm font-mono focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            />
            <input
              type="text"
              aria-label="Environment variable value"
              value={newEnvValue}
              onChange={(e) => setNewEnvValue(e.target.value)}
              placeholder="value"
              className="flex-1 px-2 py-1 bg-background border border-border rounded text-sm font-mono focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            />
            <button
              onClick={() => {
                if (newEnvKey.trim()) {
                  const newEnv = { ...config.env };
                  newEnv[newEnvKey.trim()] = newEnvValue;
                  onChange(actionType, { ...config, env: newEnv });
                  setNewEnvKey("");
                  setNewEnvValue("");
                }
              }}
              disabled={!newEnvKey.trim()}
              className="p-1 text-primary hover:bg-primary/10 rounded disabled:opacity-50"
            >
              <Plus className="w-4 h-4" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// Webhook Action Config Component
interface WebhookConfigProps {
  config: WebhookAction;
  actionType: HookActionType;
  onChange: (actionType: HookActionType, config: HookAction) => void;
}

function WebhookConfig({ config, actionType, onChange }: WebhookConfigProps) {
  const [newHeaderKey, setNewHeaderKey] = useState("");
  const [newHeaderValue, setNewHeaderValue] = useState("");

  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <div className="w-32">
          <label className="block text-sm font-medium text-muted-foreground mb-1">Method</label>
          <select
            aria-label="Method"
            value={config.method}
            onChange={(e) => onChange(actionType, { ...config, method: e.target.value })}
            className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
          >
            {HTTP_METHODS.map((method) => (
              <option key={method} value={method}>
                {method}
              </option>
            ))}
          </select>
        </div>
        <div className="flex-1">
          <label className="block text-sm font-medium text-muted-foreground mb-1">URL *</label>
          <input
            type="url"
            aria-label="URL"
            value={config.url}
            onChange={(e) => onChange(actionType, { ...config, url: e.target.value })}
            placeholder="https://api.example.com/webhook"
            className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
          />
        </div>
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Headers</label>
        <div className="space-y-2">
          {Object.entries(config.headers || {}).map(([key, value]) => (
            <div key={key} className="flex items-center gap-2">
              <input
                type="text"
                value={key}
                disabled
                className="w-1/3 px-2 py-1 bg-muted border border-border rounded text-sm"
              />
              <input
                type="text"
                value={value}
                onChange={(e) => {
                  const newHeaders = { ...config.headers };
                  newHeaders[key] = e.target.value;
                  onChange(actionType, { ...config, headers: newHeaders });
                }}
                className="flex-1 px-2 py-1 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
              />
              <button
                onClick={() => {
                  const newHeaders = { ...config.headers };
                  delete newHeaders[key];
                  onChange(actionType, { ...config, headers: newHeaders });
                }}
                className="p-1 text-destructive hover:bg-destructive/10 rounded"
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          ))}
          <div className="flex items-center gap-2">
            <input
              type="text"
              aria-label="Header name"
              value={newHeaderKey}
              onChange={(e) => setNewHeaderKey(e.target.value)}
              placeholder="Header-Name"
              className="w-1/3 px-2 py-1 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            />
            <input
              type="text"
              aria-label="Header value"
              value={newHeaderValue}
              onChange={(e) => setNewHeaderValue(e.target.value)}
              placeholder="value"
              className="flex-1 px-2 py-1 bg-background border border-border rounded text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
            />
            <button
              onClick={() => {
                if (newHeaderKey.trim()) {
                  const newHeaders = { ...config.headers };
                  newHeaders[newHeaderKey.trim()] = newHeaderValue;
                  onChange(actionType, { ...config, headers: newHeaders });
                  setNewHeaderKey("");
                  setNewHeaderValue("");
                }
              }}
              disabled={!newHeaderKey.trim()}
              className="p-1 text-primary hover:bg-primary/10 rounded disabled:opacity-50"
            >
              <Plus className="w-4 h-4" />
            </button>
          </div>
        </div>
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Request Body</label>
        <textarea
          aria-label="Request Body"
          value={config.body || ""}
          onChange={(e) =>
            onChange(actionType, {
              ...config,
              body: e.target.value || undefined,
            })
          }
          placeholder='{"key": "{{task_name}}"}'
          rows={4}
          className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm font-mono resize-none focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
        <p className="mt-1 text-xs text-muted-foreground">
          Supports variable substitution in the body.
        </p>
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">
          Timeout (seconds)
        </label>
        <input
          type="number"
          aria-label="Timeout (seconds)"
          value={config.timeout_seconds}
          onChange={(e) =>
            onChange(actionType, {
              ...config,
              timeout_seconds: parseInt(e.target.value) || 30,
            })
          }
          min={1}
          max={300}
          className="w-32 px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
      </div>
    </div>
  );
}

// Log Action Config - no hooks needed, can remain as inline render
function LogConfig({
  config,
  actionType,
  onChange,
}: {
  config: LogAction;
  actionType: HookActionType;
  onChange: (actionType: HookActionType, config: HookAction) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Log Level</label>
        <select
          aria-label="Log Level"
          value={config.level}
          onChange={(e) => onChange(actionType, { ...config, level: e.target.value })}
          className="w-40 px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        >
          {LOG_LEVELS.map((level) => (
            <option key={level} value={level}>
              {level.toUpperCase()}
            </option>
          ))}
        </select>
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Message *</label>
        <textarea
          aria-label="Message"
          value={config.message}
          onChange={(e) => onChange(actionType, { ...config, message: e.target.value })}
          placeholder="Task {{task_name}} completed with status: {{status}}"
          rows={3}
          className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
        <p className="mt-1 text-xs text-muted-foreground">
          Available variables: {"{{task_run_id}}"}, {"{{task_name}}"}, {"{{iteration}}"},{" "}
          {"{{status}}"}, {"{{error}}"}
        </p>
      </div>
    </div>
  );
}

// Notification Action Config - no hooks needed
function NotificationConfig({
  config,
  actionType,
  onChange,
}: {
  config: NotificationAction;
  actionType: HookActionType;
  onChange: (actionType: HookActionType, config: HookAction) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Title *</label>
        <input
          type="text"
          aria-label="Title"
          value={config.title}
          onChange={(e) => onChange(actionType, { ...config, title: e.target.value })}
          placeholder="Task {{task_name}} Complete"
          className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
      </div>

      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-1">Body *</label>
        <textarea
          aria-label="Body"
          value={config.body}
          onChange={(e) => onChange(actionType, { ...config, body: e.target.value })}
          placeholder="Completed after {{iteration}} iterations with status: {{status}}"
          rows={3}
          className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-hidden focus:ring-2 focus:ring-primary/50"
        />
        <p className="mt-1 text-xs text-muted-foreground">Supports variable substitution.</p>
      </div>
    </div>
  );
}

export function HookActionConfig({ actionType, actionConfig, onChange }: HookActionConfigProps) {
  // Handle action type change
  const handleActionTypeChange = (newType: HookActionType) => {
    onChange(newType, createDefaultAction(newType));
  };

  return (
    <div className="space-y-4">
      {/* Action type selector */}
      <div>
        <label className="block text-sm font-medium text-muted-foreground mb-2">Action Type</label>
        <div className="grid grid-cols-2 gap-2">
          {(Object.keys(HOOK_ACTION_TYPE_DISPLAY_NAMES) as HookActionType[]).map((type) => (
            <button
              key={type}
              onClick={() => handleActionTypeChange(type)}
              className={`p-3 rounded-lg border text-left transition-all ${
                actionType === type
                  ? "border-primary bg-primary/5"
                  : "border-border hover:border-primary/50"
              }`}
            >
              <div className="font-medium text-sm">{HOOK_ACTION_TYPE_DISPLAY_NAMES[type]}</div>
              <div className="text-xs text-muted-foreground mt-1">
                {HOOK_ACTION_TYPE_DESCRIPTIONS[type]}
              </div>
            </button>
          ))}
        </div>
      </div>

      {/* Type-specific config */}
      <div className="pt-4 border-t border-border">
        {actionType === "command" && (
          <CommandConfig
            config={actionConfig as CommandAction}
            actionType={actionType}
            onChange={onChange}
          />
        )}
        {actionType === "webhook" && (
          <WebhookConfig
            config={actionConfig as WebhookAction}
            actionType={actionType}
            onChange={onChange}
          />
        )}
        {actionType === "log" && (
          <LogConfig
            config={actionConfig as LogAction}
            actionType={actionType}
            onChange={onChange}
          />
        )}
        {actionType === "notification" && (
          <NotificationConfig
            config={actionConfig as NotificationAction}
            actionType={actionType}
            onChange={onChange}
          />
        )}
      </div>
    </div>
  );
}
