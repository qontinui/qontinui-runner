/**
 * BuiltinConstraintRow.tsx
 *
 * A single built-in constraint with a toggle switch.
 * Built-in constraints are non-editable — only the enabled/disabled state
 * can be toggled.
 */

import type { Constraint } from "@qontinui/shared-types/constraints";
import { severityBadgeColor, severityLabel } from "@qontinui/workflow-utils";

interface BuiltinConstraintRowProps {
  constraint: Constraint;
  onToggle: (enabled: boolean) => void;
}

export function BuiltinConstraintRow({ constraint, onToggle }: BuiltinConstraintRowProps) {
  return (
    <div className="flex items-center justify-between py-2 px-3 bg-zinc-800/50 rounded-md">
      <div className="flex-1 min-w-0 mr-3">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-zinc-200 truncate">{constraint.name}</span>
          <span
            className={`inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium ${severityBadgeColor(constraint.severity)}`}
          >
            {severityLabel(constraint.severity)}
          </span>
        </div>
        {constraint.description && (
          <p className="text-xs text-zinc-500 mt-0.5 line-clamp-1">{constraint.description}</p>
        )}
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={constraint.enabled}
        onClick={() => onToggle(!constraint.enabled)}
        className={`
          relative inline-flex h-6 w-11 flex-shrink-0 cursor-pointer rounded-full border-2 border-transparent
          transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-blue-500/50
          ${constraint.enabled ? "bg-blue-600" : "bg-zinc-600"}
        `}
      >
        <span
          className={`
            pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0
            transition duration-200 ease-in-out
            ${constraint.enabled ? "translate-x-5" : "translate-x-0"}
          `}
        />
      </button>
    </div>
  );
}
