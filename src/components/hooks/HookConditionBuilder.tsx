/**
 * HookConditionBuilder
 *
 * UI for building conditions that determine when a hook should execute.
 */

import { Plus, Trash2 } from "lucide-react";
import type { HookCondition, ConditionOperator } from "../../types/hooks";
import { CONDITION_OPERATOR_DISPLAY_NAMES, CONDITION_VARIABLES } from "../../types/hooks";

interface HookConditionBuilderProps {
  conditions: HookCondition[];
  onChange: (conditions: HookCondition[]) => void;
}

export function HookConditionBuilder({ conditions, onChange }: HookConditionBuilderProps) {
  const addCondition = () => {
    onChange([
      ...conditions,
      {
        variable: "status",
        operator: "eq",
        value: "",
      },
    ]);
  };

  const updateCondition = (index: number, updates: Partial<HookCondition>) => {
    const newConditions = [...conditions];
    newConditions[index] = { ...newConditions[index], ...updates };
    onChange(newConditions);
  };

  const removeCondition = (index: number) => {
    onChange(conditions.filter((_, i) => i !== index));
  };

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <div>
          <h4 className="text-sm font-medium">Conditions</h4>
          <p className="text-xs text-muted-foreground">
            Hook will only execute if all conditions are met
          </p>
        </div>
        <button
          onClick={addCondition}
          className="flex items-center gap-1 px-2 py-1 text-sm text-primary hover:bg-primary/10 rounded transition-colors"
        >
          <Plus className="w-4 h-4" />
          Add Condition
        </button>
      </div>

      {conditions.length === 0 ? (
        <div className="text-sm text-muted-foreground text-center py-4 bg-muted/30 rounded-lg">
          No conditions. Hook will always execute on trigger.
        </div>
      ) : (
        <div className="space-y-2">
          {conditions.map((condition, index) => (
            <div
              key={`${condition.variable}-${index}`}
              className="flex items-center gap-2 p-3 bg-muted/30 rounded-lg"
            >
              {/* Variable selector */}
              <select
                value={condition.variable}
                onChange={(e) => updateCondition(index, { variable: e.target.value })}
                className="px-2 py-1.5 bg-background border border-border rounded text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
              >
                {CONDITION_VARIABLES.map((v) => (
                  <option key={v.name} value={v.name} title={v.description}>
                    {v.name}
                  </option>
                ))}
              </select>

              {/* Operator selector */}
              <select
                value={condition.operator}
                onChange={(e) =>
                  updateCondition(index, {
                    operator: e.target.value as ConditionOperator,
                  })
                }
                className="px-2 py-1.5 bg-background border border-border rounded text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
              >
                {(Object.keys(CONDITION_OPERATOR_DISPLAY_NAMES) as ConditionOperator[]).map(
                  (op) => (
                    <option key={op} value={op}>
                      {CONDITION_OPERATOR_DISPLAY_NAMES[op]}
                    </option>
                  ),
                )}
              </select>

              {/* Value input */}
              <input
                type="text"
                value={
                  typeof condition.value === "string"
                    ? condition.value
                    : JSON.stringify(condition.value)
                }
                onChange={(e) => {
                  // Try to parse as number or boolean, otherwise keep as string
                  let value: string | number | boolean = e.target.value;
                  if (value === "true") value = true;
                  else if (value === "false") value = false;
                  else if (!isNaN(Number(value)) && value !== "") value = Number(value);
                  updateCondition(index, { value });
                }}
                placeholder="value"
                className="flex-1 px-2 py-1.5 bg-background border border-border rounded text-sm focus:outline-none focus:ring-2 focus:ring-primary/50"
              />

              {/* Remove button */}
              <button
                onClick={() => removeCondition(index)}
                className="p-1.5 text-destructive hover:bg-destructive/10 rounded transition-colors"
                title="Remove condition"
              >
                <Trash2 className="w-4 h-4" />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Help text */}
      <div className="text-xs text-muted-foreground space-y-1 pt-2">
        <p>
          <strong>Available variables:</strong>
        </p>
        <ul className="list-disc list-inside space-y-0.5 pl-2">
          {CONDITION_VARIABLES.map((v) => (
            <li key={v.name}>
              <code className="text-primary">{v.name}</code> - {v.description}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
