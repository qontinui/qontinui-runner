/**
 * Action Executor View
 *
 * Allows executing actions on selected UI Bridge elements.
 * Provides action buttons, parameter inputs, and result display.
 */

import { useState, useCallback } from "react";
import { cn } from "../../lib/utils";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import { Card, CardContent } from "../ui/Card";
import {
  MousePointer,
  Type,
  Eye,
  EyeOff,
  Trash2,
  Check,
  ToggleLeft,
  ToggleRight,
  List,
  Play,
  Loader2,
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import type { UIBridgeElement } from "./inspector-types";
import { describeThrown } from "@/lib/utils";

interface ActionExecutorViewProps {
  element?: UIBridgeElement;
  onExecuteAction?: (
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ) => Promise<unknown>;
  disabled?: boolean;
}

export interface ActionConfig {
  id: string;
  label: string;
  icon: React.ReactNode;
  requiresParams?: boolean;
  paramType?: "text" | "value" | "options";
  description?: string;
}

const COMMON_ACTIONS: ActionConfig[] = [
  {
    id: "click",
    label: "Click",
    icon: <MousePointer className="w-4 h-4" />,
    description: "Simulate a mouse click",
  },
  {
    id: "doubleClick",
    label: "Double Click",
    icon: <MousePointer className="w-4 h-4" />,
    description: "Simulate a double click",
  },
  {
    id: "focus",
    label: "Focus",
    icon: <Eye className="w-4 h-4" />,
    description: "Focus the element",
  },
  {
    id: "blur",
    label: "Blur",
    icon: <EyeOff className="w-4 h-4" />,
    description: "Remove focus from element",
  },
  {
    id: "hover",
    label: "Hover",
    icon: <MousePointer className="w-4 h-4" />,
    description: "Simulate mouse hover",
  },
];

const INPUT_ACTIONS: ActionConfig[] = [
  {
    id: "type",
    label: "Type",
    icon: <Type className="w-4 h-4" />,
    requiresParams: true,
    paramType: "text",
    description: "Type text into the input",
  },
  {
    id: "clear",
    label: "Clear",
    icon: <Trash2 className="w-4 h-4" />,
    description: "Clear the input value",
  },
];

const CHECKBOX_ACTIONS: ActionConfig[] = [
  {
    id: "check",
    label: "Check",
    icon: <Check className="w-4 h-4" />,
    description: "Check the checkbox",
  },
  {
    id: "uncheck",
    label: "Uncheck",
    icon: <ToggleLeft className="w-4 h-4" />,
    description: "Uncheck the checkbox",
  },
  {
    id: "toggle",
    label: "Toggle",
    icon: <ToggleRight className="w-4 h-4" />,
    description: "Toggle the checkbox state",
  },
];

const SELECT_ACTIONS: ActionConfig[] = [
  {
    id: "select",
    label: "Select Option",
    icon: <List className="w-4 h-4" />,
    requiresParams: true,
    paramType: "value",
    description: "Select an option by value",
  },
];

// `setValue` writes the value field's text straight into the element (the
// SDK's `{ value }` param — the same field `select` reads). Since
// `@qontinui/ui-bridge` 0.46 every auto-registered text input advertises it, so
// the inspector offers it wherever it is advertised. With no advertised list the
// kind fallback offers it on `input` / `textarea` only, beside `type`.
const SET_VALUE_ACTION: ActionConfig = {
  id: "setValue",
  label: "Set Value",
  icon: <Type className="w-4 h-4" />,
  requiresParams: true,
  paramType: "value",
  description: "Set the element's value directly",
};

/** Every verb this view has a control for, each once. */
const ALL_ACTIONS: ActionConfig[] = [
  ...COMMON_ACTIONS,
  ...INPUT_ACTIONS,
  ...CHECKBOX_ACTIONS,
  ...SELECT_ACTIONS,
  SET_VALUE_ACTION,
];

/**
 * The verbs to offer for an element, in this view's order.
 *
 * An element that advertises its verbs is offered each verb this view has a
 * control for that it advertises, and no other. Since `@qontinui/ui-bridge`
 * 0.46 every producer reads them from ONE table that also reads the node's
 * structure (`core/element-kind.ts`), so the list is authoritative — a
 * `<input role="combobox">` takes `type` AND `select`, which no switch on
 * `type` would offer — and the runner refuses anything outside it anyway.
 *
 * With no advertised list, the kind decides.
 */
export function availableActionsFor(
  element: Pick<UIBridgeElement, "type" | "actions">,
): ActionConfig[] {
  if (element.actions && element.actions.length > 0) {
    const advertised = element.actions;
    return ALL_ACTIONS.filter((a) => advertised.includes(a.id));
  }

  const actions: ActionConfig[] = [...COMMON_ACTIONS];
  switch (element.type) {
    case "input":
    case "textarea":
      actions.push(...INPUT_ACTIONS, SET_VALUE_ACTION);
      break;
    case "checkbox":
    case "radio":
      actions.push(...CHECKBOX_ACTIONS);
      break;
    case "select":
    case "combobox":
      actions.push(...SELECT_ACTIONS);
      break;
  }
  return actions;
}

export function ActionExecutorView({
  element,
  onExecuteAction,
  disabled = false,
}: ActionExecutorViewProps) {
  const [executing, setExecuting] = useState(false);
  const [lastResult, setLastResult] = useState<{
    success: boolean;
    action: string;
    error?: string;
  } | null>(null);
  const [textParam, setTextParam] = useState("");
  const [valueParam, setValueParam] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);

  const getAvailableActions = useCallback(
    (): ActionConfig[] => (element ? availableActionsFor(element) : []),
    [element],
  );

  // Execute action
  const handleExecute = useCallback(
    async (action: ActionConfig) => {
      if (!element || !onExecuteAction || disabled || executing) return;

      setExecuting(true);
      setLastResult(null);

      try {
        let params: Record<string, unknown> = {};

        // Add parameters based on action type
        if (action.paramType === "text" && textParam) {
          params = { text: textParam };
        } else if (action.paramType === "value" && valueParam) {
          params = { value: valueParam };
        }

        await onExecuteAction(element.id, action.id, params);

        setLastResult({
          success: true,
          action: action.label,
        });
      } catch (err) {
        setLastResult({
          success: false,
          action: action.label,
          error: describeThrown(err, "Action failed"),
        });
      } finally {
        setExecuting(false);
      }
    },
    [element, onExecuteAction, disabled, executing, textParam, valueParam],
  );

  // Quick action button
  const renderActionButton = (action: ActionConfig, size: "sm" | "md" = "sm") => {
    const isDisabled =
      disabled ||
      executing ||
      (action.requiresParams &&
        ((action.paramType === "text" && !textParam) ||
          (action.paramType === "value" && !valueParam)));

    return (
      <Button
        key={action.id}
        variant="outline"
        size={size}
        disabled={isDisabled}
        onClick={() => handleExecute(action)}
        title={action.description}
        className="flex items-center gap-1"
      >
        {executing ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : action.icon}
        {action.label}
      </Button>
    );
  };

  if (!element) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Play className="w-8 h-8 opacity-50" />
        <p>Select an element to execute actions</p>
        <p className="text-xs">Use the Elements tab or element picker to select</p>
      </div>
    );
  }

  const availableActions = getAvailableActions();
  const quickActions = availableActions.filter((a) => !a.requiresParams);
  const paramActions = availableActions.filter((a) => a.requiresParams);

  return (
    <div className="flex flex-col h-full gap-3">
      {/* Selected element info */}
      <Card variant="borderless" className="bg-muted/30">
        <CardContent className="py-2">
          <div className="flex items-center gap-2">
            <Badge variant="default">{element.type}</Badge>
            <span className="text-sm font-mono truncate flex-1">{element.id}</span>
            {element.label && (
              <span className="text-xs text-muted-foreground truncate">{element.label}</span>
            )}
          </div>
          {element.value !== undefined && (
            <div className="mt-1 text-xs text-muted-foreground">
              Value: <span className="font-mono">{element.value || "(empty)"}</span>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Result display */}
      {lastResult && (
        <div
          className={cn(
            "flex items-center gap-2 p-2 rounded-md text-sm",
            lastResult.success
              ? "bg-accent/10 text-accent border border-accent/30"
              : "bg-destructive/10 text-destructive border border-destructive/30",
          )}
        >
          {lastResult.success ? (
            <CheckCircle className="w-4 h-4" />
          ) : (
            <AlertCircle className="w-4 h-4" />
          )}
          <span>
            {lastResult.success
              ? `${lastResult.action} executed successfully`
              : `${lastResult.action} failed: ${lastResult.error}`}
          </span>
        </div>
      )}

      {/* Quick actions */}
      <div>
        <div className="text-xs font-semibold text-muted-foreground mb-2">Quick Actions</div>
        <div className="flex flex-wrap gap-2">
          {quickActions.map((action) => renderActionButton(action))}
        </div>
      </div>

      {/* Parameter actions */}
      {paramActions.length > 0 && (
        <div>
          <div className="text-xs font-semibold text-muted-foreground mb-2">
            Actions with Parameters
          </div>

          {/* Text input for type action */}
          {paramActions.some((a) => a.paramType === "text") && (
            <div className="mb-3">
              <label className="text-xs text-muted-foreground mb-1 block">Text to type:</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={textParam}
                  onChange={(e) => setTextParam(e.target.value)}
                  placeholder="Enter text..."
                  className="flex-1 px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary"
                  disabled={disabled || executing}
                />
                {paramActions
                  .filter((a) => a.paramType === "text")
                  .map((action) => renderActionButton(action))}
              </div>
            </div>
          )}

          {/* Value input for select / setValue */}
          {paramActions.some((a) => a.paramType === "value") && (
            <div className="mb-3">
              <label className="text-xs text-muted-foreground mb-1 block">Value:</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={valueParam}
                  onChange={(e) => setValueParam(e.target.value)}
                  placeholder="Enter value..."
                  className="flex-1 px-2 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary"
                  disabled={disabled || executing}
                />
                {paramActions
                  .filter((a) => a.paramType === "value")
                  .map((action) => renderActionButton(action))}
              </div>
            </div>
          )}
        </div>
      )}

      {/* Advanced section */}
      <div className="mt-auto">
        <button
          className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={() => setShowAdvanced(!showAdvanced)}
        >
          {showAdvanced ? (
            <ChevronDown className="w-3.5 h-3.5" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5" />
          )}
          Advanced Options
        </button>

        {showAdvanced && (
          <div className="mt-2 p-2 bg-muted/20 rounded-md">
            <div className="text-xs text-muted-foreground mb-2">Element Details</div>
            <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
              <div className="text-muted-foreground">Tag:</div>
              <div className="font-mono">{element.tagName}</div>
              <div className="text-muted-foreground">Visible:</div>
              <div>{element.visible ? "Yes" : "No"}</div>
              <div className="text-muted-foreground">Enabled:</div>
              <div>{element.enabled ? "Yes" : "No"}</div>
              <div className="text-muted-foreground">Focused:</div>
              <div>{element.focused ? "Yes" : "No"}</div>
              <div className="text-muted-foreground">Position:</div>
              <div className="font-mono">
                {element.bounds.x.toFixed(0)}, {element.bounds.y.toFixed(0)}
              </div>
              <div className="text-muted-foreground">Size:</div>
              <div className="font-mono">
                {element.bounds.width.toFixed(0)} x {element.bounds.height.toFixed(0)}
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default ActionExecutorView;
