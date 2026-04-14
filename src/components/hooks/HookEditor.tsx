/**
 * HookEditor
 *
 * Dialog/modal for creating or editing a lifecycle hook.
 */

import { useState, useEffect } from "react";
import { X, Save, AlertTriangle } from "lucide-react";
import type {
  Hook,
  HookTrigger,
  HookActionType,
  HookAction,
  HookCondition,
  CreateHookRequest,
  UpdateHookRequest,
} from "../../types/hooks";
import {
  HOOK_TRIGGER_DISPLAY_NAMES,
  HOOK_TRIGGER_DESCRIPTIONS,
  createDefaultHook,
  createDefaultAction,
} from "../../types/hooks";
import { HookActionConfig } from "./HookActionConfig";
import { HookConditionBuilder } from "./HookConditionBuilder";

interface HookEditorProps {
  hook: Hook | null; // null = create new
  onSave: (data: CreateHookRequest | UpdateHookRequest) => Promise<void>;
  onCancel: () => void;
  loading: boolean;
}

export function HookEditor({ hook, onSave, onCancel, loading }: HookEditorProps) {
  const isEditing = hook !== null;

  // Form state
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [trigger, setTrigger] = useState<HookTrigger>("pre_execution");
  const [actionType, setActionType] = useState<HookActionType>("log");
  const [actionConfig, setActionConfig] = useState<HookAction>(() => createDefaultAction("log"));
  const [enabled, setEnabled] = useState(true);
  const [executionOrder, setExecutionOrder] = useState(0);
  const [continueOnFailure, setContinueOnFailure] = useState(true);
  const [conditions, setConditions] = useState<HookCondition[]>([]);

  // Validation errors
  const [errors, setErrors] = useState<Record<string, string>>({});

  // Initialize form when hook changes
  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- sync form state from prop */
    if (hook) {
      setName(hook.name);
      setDescription(hook.description || "");
      setTrigger(hook.trigger);
      setActionType(hook.action_type);
      setActionConfig(hook.action_config);
      setEnabled(hook.enabled);
      setExecutionOrder(hook.execution_order);
      setContinueOnFailure(hook.continue_on_failure);
      setConditions(hook.conditions || []);
    } else {
      const defaults = createDefaultHook();
      setName("");
      setDescription("");
      setTrigger(defaults.trigger);
      setActionType(defaults.action_type);
      setActionConfig(defaults.action_config);
      setEnabled(defaults.enabled);
      setExecutionOrder(defaults.execution_order);
      setContinueOnFailure(defaults.continue_on_failure);
      setConditions(defaults.conditions);
    }
    setErrors({});
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [hook]);

  // Validate form
  const validate = (): boolean => {
    const newErrors: Record<string, string> = {};

    if (!name.trim()) {
      newErrors.name = "Name is required";
    }

    // Validate action config based on type
    if (actionType === "command") {
      const config = actionConfig as { command: string };
      if (!config.command?.trim()) {
        newErrors.action = "Command is required";
      }
    } else if (actionType === "webhook") {
      const config = actionConfig as { url: string };
      if (!config.url?.trim()) {
        newErrors.action = "URL is required";
      }
    } else if (actionType === "log") {
      const config = actionConfig as { message: string };
      if (!config.message?.trim()) {
        newErrors.action = "Message is required";
      }
    } else if (actionType === "notification") {
      const config = actionConfig as { title: string; body: string };
      if (!config.title?.trim()) {
        newErrors.action = "Title is required";
      }
      if (!config.body?.trim()) {
        newErrors.action = "Body is required";
      }
    }

    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  };

  // Handle save
  const handleSave = async () => {
    if (!validate()) return;

    if (isEditing && hook) {
      // Update existing hook
      const update: UpdateHookRequest = {
        id: hook.id,
        name: name !== hook.name ? name : undefined,
        description:
          description !== (hook.description || "") ? description || undefined : undefined,
        trigger: trigger !== hook.trigger ? trigger : undefined,
        action_type: actionType !== hook.action_type ? actionType : undefined,
        action_config:
          JSON.stringify(actionConfig) !== JSON.stringify(hook.action_config)
            ? actionConfig
            : undefined,
        enabled: enabled !== hook.enabled ? enabled : undefined,
        execution_order: executionOrder !== hook.execution_order ? executionOrder : undefined,
        continue_on_failure:
          continueOnFailure !== hook.continue_on_failure ? continueOnFailure : undefined,
        conditions:
          JSON.stringify(conditions) !== JSON.stringify(hook.conditions) ? conditions : undefined,
      };
      await onSave(update);
    } else {
      // Create new hook
      const create: CreateHookRequest = {
        name: name.trim(),
        description: description.trim() || undefined,
        trigger,
        action_type: actionType,
        action_config: actionConfig,
        enabled,
        execution_order: executionOrder,
        continue_on_failure: continueOnFailure,
        conditions,
      };
      await onSave(create);
    }
  };

  // Handle action config change
  const handleActionChange = (newType: HookActionType, config: HookAction) => {
    setActionType(newType);
    setActionConfig(config);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl max-h-[90vh] overflow-hidden flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h2 className="text-lg font-semibold">{isEditing ? "Edit Hook" : "Create Hook"}</h2>
          <button onClick={onCancel} className="p-1 hover:bg-muted rounded transition-colors">
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-6">
          {/* Basic info */}
          <div className="space-y-4">
            <div>
              <label className="block text-sm font-medium text-muted-foreground mb-1">Name *</label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="My Hook"
                className={`w-full px-3 py-2 bg-background border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50 ${
                  errors.name ? "border-destructive" : "border-border"
                }`}
              />
              {errors.name && <p className="mt-1 text-xs text-destructive">{errors.name}</p>}
            </div>

            <div>
              <label className="block text-sm font-medium text-muted-foreground mb-1">
                Description
              </label>
              <textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What this hook does..."
                rows={2}
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-hidden focus:ring-2 focus:ring-primary/50"
              />
            </div>
          </div>

          {/* Trigger selection */}
          <div>
            <label className="block text-sm font-medium text-muted-foreground mb-2">Trigger</label>
            <div className="grid grid-cols-2 gap-2">
              {(Object.keys(HOOK_TRIGGER_DISPLAY_NAMES) as HookTrigger[]).map((t) => (
                <button
                  key={t}
                  onClick={() => setTrigger(t)}
                  className={`p-3 rounded-lg border text-left transition-all ${
                    trigger === t
                      ? "border-primary bg-primary/5"
                      : "border-border hover:border-primary/50"
                  }`}
                >
                  <div className="font-medium text-sm">{HOOK_TRIGGER_DISPLAY_NAMES[t]}</div>
                  <div className="text-xs text-muted-foreground mt-1">
                    {HOOK_TRIGGER_DESCRIPTIONS[t]}
                  </div>
                </button>
              ))}
            </div>
          </div>

          {/* Action configuration */}
          <div>
            <h3 className="text-sm font-medium mb-3">Action</h3>
            <HookActionConfig
              actionType={actionType}
              actionConfig={actionConfig}
              onChange={handleActionChange}
            />
            {errors.action && (
              <p className="mt-2 text-xs text-destructive flex items-center gap-1">
                <AlertTriangle className="w-3 h-3" />
                {errors.action}
              </p>
            )}
          </div>

          {/* Conditions */}
          <HookConditionBuilder conditions={conditions} onChange={setConditions} />

          {/* Execution settings */}
          <div className="space-y-4">
            <h3 className="text-sm font-medium">Execution Settings</h3>

            <div className="flex items-center gap-4">
              <div>
                <label className="block text-sm font-medium text-muted-foreground mb-1">
                  Execution Order
                </label>
                <input
                  type="number"
                  value={executionOrder}
                  onChange={(e) => setExecutionOrder(parseInt(e.target.value) || 0)}
                  className="w-24 px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-hidden focus:ring-2 focus:ring-primary/50"
                />
                <p className="text-xs text-muted-foreground mt-1">Lower values execute first</p>
              </div>
            </div>

            <div className="flex flex-col gap-3">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={enabled}
                  onChange={(e) => setEnabled(e.target.checked)}
                  className="w-4 h-4 rounded border-border text-primary focus:ring-primary/50"
                />
                <span className="text-sm">Enabled</span>
              </label>

              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={continueOnFailure}
                  onChange={(e) => setContinueOnFailure(e.target.checked)}
                  className="w-4 h-4 rounded border-border text-primary focus:ring-primary/50"
                />
                <span className="text-sm">Continue on failure</span>
                <span className="text-xs text-muted-foreground">
                  (uncheck to stop execution if this hook fails)
                </span>
              </label>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 p-4 border-t border-border bg-muted/30">
          <button
            onClick={onCancel}
            disabled={loading}
            className="px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={loading}
            className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-md text-sm hover:bg-primary/90 transition-colors disabled:opacity-50"
          >
            <Save className="w-4 h-4" />
            {loading ? "Saving..." : isEditing ? "Save Changes" : "Create Hook"}
          </button>
        </div>
      </div>
    </div>
  );
}
