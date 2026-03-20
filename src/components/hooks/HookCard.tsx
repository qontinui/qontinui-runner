/**
 * HookCard
 *
 * Displays a single lifecycle hook with status, actions, and details.
 */

import {
  Play,
  Pause,
  Pencil,
  Trash2,
  PlayCircle,
  GripVertical,
  Terminal,
  Globe,
  MessageSquare,
  Bell,
  ChevronDown,
  ChevronUp,
  AlertTriangle,
} from "lucide-react";
import { useState } from "react";
import type { Hook, HookActionType, HookTrigger } from "../../types/hooks";
import { HOOK_TRIGGER_DISPLAY_NAMES, HOOK_ACTION_TYPE_DISPLAY_NAMES } from "../../types/hooks";
import { getAccentColors } from "@/design-system";

interface HookCardProps {
  hook: Hook;
  onEdit: (hook: Hook) => void;
  onDelete: (hook: Hook) => void;
  onTest: (hook: Hook) => void;
  onToggleEnabled: (hook: Hook) => void;
  loading: boolean;
  isDragging?: boolean;
  dragHandleProps?: React.HTMLAttributes<HTMLDivElement>;
}

const ACTION_ICONS: Record<HookActionType, typeof Terminal> = {
  command: Terminal,
  webhook: Globe,
  log: MessageSquare,
  notification: Bell,
};

const TRIGGER_COLORS: Record<
  HookTrigger,
  "blue" | "green" | "red" | "orange" | "purple" | "cyan" | "amber"
> = {
  pre_execution: "blue",
  post_execution: "green",
  on_error: "red",
  on_verification_fail: "orange",
  on_complete: "emerald" as "green",
  pre_iteration: "cyan",
  post_iteration: "purple",
};

export function HookCard({
  hook,
  onEdit,
  onDelete,
  onTest,
  onToggleEnabled,
  loading,
  isDragging,
  dragHandleProps,
}: HookCardProps) {
  const [expanded, setExpanded] = useState(false);
  const ActionIcon = ACTION_ICONS[hook.action_type];
  const triggerColor = TRIGGER_COLORS[hook.trigger] || "blue";
  const colors = getAccentColors(triggerColor);

  const getActionSummary = () => {
    const config = hook.action_config;
    switch (hook.action_type) {
      case "command": {
        const cmdConfig = config as { command?: string };
        const cmd = cmdConfig.command || "";
        return cmd.slice(0, 50) + (cmd.length > 50 ? "..." : "");
      }
      case "webhook": {
        const whConfig = config as { method?: string; url?: string };
        return `${whConfig.method || "POST"} ${whConfig.url || ""}`;
      }
      case "log": {
        const logConfig = config as { level?: string; message?: string };
        return `[${logConfig.level || "info"}] ${(logConfig.message || "").slice(0, 40)}`;
      }
      case "notification": {
        const notifyConfig = config as { title?: string };
        return notifyConfig.title || "";
      }
      default:
        return "";
    }
  };

  return (
    <div
      className={`rounded-lg border transition-all ${
        isDragging
          ? "border-primary bg-primary/5 shadow-lg"
          : hook.enabled
            ? "border-border hover:border-primary/50 hover:bg-muted/30"
            : "border-border/50 bg-muted/20 opacity-60"
      }`}
    >
      <div className="p-4">
        <div className="flex items-start gap-3">
          {/* Drag handle */}
          <div
            {...dragHandleProps}
            className="flex-shrink-0 cursor-grab active:cursor-grabbing text-muted-foreground hover:text-foreground mt-1"
          >
            <GripVertical className="w-4 h-4" />
          </div>

          {/* Main content */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap">
              {/* Hook name */}
              <span className="font-medium">{hook.name}</span>

              {/* Trigger badge */}
              <span className={`px-2 py-0.5 text-xs rounded ${colors.bg} ${colors.text}`}>
                {HOOK_TRIGGER_DISPLAY_NAMES[hook.trigger]}
              </span>

              {/* Action type badge */}
              <span className="px-2 py-0.5 text-xs rounded bg-muted text-muted-foreground flex items-center gap-1">
                <ActionIcon className="w-3 h-3" />
                {HOOK_ACTION_TYPE_DISPLAY_NAMES[hook.action_type]}
              </span>

              {/* Disabled badge */}
              {!hook.enabled && (
                <span className="px-2 py-0.5 text-xs bg-muted rounded">Disabled</span>
              )}

              {/* Continue on failure badge */}
              {!hook.continue_on_failure && (
                <span
                  className={`px-2 py-0.5 text-xs rounded flex items-center gap-1 ${getAccentColors("red").bg} ${getAccentColors("red").text}`}
                  title="Execution stops if this hook fails"
                >
                  <AlertTriangle className="w-3 h-3" />
                  Critical
                </span>
              )}

              {/* Conditions badge */}
              {hook.conditions.length > 0 && (
                <span className="px-2 py-0.5 text-xs rounded bg-muted text-muted-foreground">
                  {hook.conditions.length} condition{hook.conditions.length !== 1 ? "s" : ""}
                </span>
              )}
            </div>

            {/* Description */}
            {hook.description && (
              <p className="mt-1 text-sm text-muted-foreground line-clamp-1">{hook.description}</p>
            )}

            {/* Action summary */}
            <p className="mt-1 text-xs text-muted-foreground font-mono truncate">
              {getActionSummary()}
            </p>
          </div>

          {/* Actions */}
          <div className="flex items-center gap-1 flex-shrink-0">
            <button
              onClick={() => onToggleEnabled(hook)}
              disabled={loading}
              className="p-1.5 rounded hover:bg-muted transition-colors"
              title={hook.enabled ? "Disable" : "Enable"}
            >
              {hook.enabled ? (
                <Pause className="w-4 h-4 text-muted-foreground" />
              ) : (
                <Play className={`w-4 h-4 ${getAccentColors("green").text}`} />
              )}
            </button>
            <button
              onClick={() => onTest(hook)}
              disabled={loading}
              className="p-1.5 rounded hover:bg-muted transition-colors"
              title="Test Hook"
            >
              <PlayCircle className="w-4 h-4 text-primary" />
            </button>
            <button
              onClick={() => onEdit(hook)}
              disabled={loading}
              className="p-1.5 rounded hover:bg-muted transition-colors"
              title="Edit"
            >
              <Pencil className="w-4 h-4 text-muted-foreground" />
            </button>
            <button
              onClick={() => onDelete(hook)}
              disabled={loading}
              className="p-1.5 rounded hover:bg-destructive/10 transition-colors"
              title="Delete"
            >
              <Trash2 className="w-4 h-4 text-destructive" />
            </button>
            <button
              onClick={() => setExpanded(!expanded)}
              className="p-1.5 rounded hover:bg-muted transition-colors"
              title={expanded ? "Collapse" : "Expand"}
            >
              {expanded ? (
                <ChevronUp className="w-4 h-4 text-muted-foreground" />
              ) : (
                <ChevronDown className="w-4 h-4 text-muted-foreground" />
              )}
            </button>
          </div>
        </div>

        {/* Expanded details */}
        {expanded && (
          <div className="mt-4 pt-4 border-t border-border space-y-3">
            {/* Action config */}
            <div>
              <h4 className="text-xs font-medium text-muted-foreground mb-2">
                Action Configuration
              </h4>
              <pre className="text-xs bg-muted/50 rounded p-2 overflow-x-auto">
                {JSON.stringify(hook.action_config, null, 2)}
              </pre>
            </div>

            {/* Conditions */}
            {hook.conditions.length > 0 && (
              <div>
                <h4 className="text-xs font-medium text-muted-foreground mb-2">Conditions</h4>
                <div className="space-y-1">
                  {hook.conditions.map((condition, index) => (
                    <div key={`${condition.variable}-${index}`} className="text-xs bg-muted/50 rounded px-2 py-1 font-mono">
                      {condition.variable} {condition.operator} {JSON.stringify(condition.value)}
                    </div>
                  ))}
                </div>
              </div>
            )}

            {/* Metadata */}
            <div className="flex gap-4 text-xs text-muted-foreground">
              <span>Order: {hook.execution_order}</span>
              <span>Created: {new Date(hook.created_at).toLocaleDateString()}</span>
              <span>Updated: {new Date(hook.updated_at).toLocaleDateString()}</span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
