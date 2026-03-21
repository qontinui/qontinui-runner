/**
 * ConstraintCard.tsx
 *
 * Expandable card for a custom (project:) constraint.
 * Collapsed: name, severity badge, check type label, enabled toggle, expand/delete buttons.
 * Expanded: full editor for all fields including check-type-specific inputs.
 */

import { useState } from "react";
import { ArrowUpCircle, ChevronDown, ChevronRight, Plus, Trash2, X } from "lucide-react";
import type {
  Constraint,
  ConstraintCheck,
  ConstraintCheckType,
  ConstraintSeverity,
} from "@qontinui/shared-types/constraints";
import {
  severityBadgeColor,
  severityLabel,
  constraintCheckTypeLabel,
  isAiConstraint,
  DEFAULT_COMMAND_TIMEOUT_SECS,
} from "@qontinui/workflow-utils";

interface ConstraintCardProps {
  constraint: Constraint;
  onUpdate: (updates: Partial<Omit<Constraint, "id">>) => void;
  onRemove: () => void;
  /** Called when an AI-proposed constraint should be promoted to a project constraint. */
  onPromote?: () => void;
}

const inputClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 text-sm";
const selectClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 text-sm";

const CHECK_TYPE_OPTIONS: { value: ConstraintCheckType; label: string }[] = [
  { value: "grep_forbidden", label: "Grep Forbidden" },
  { value: "grep_required", label: "Grep Required" },
  { value: "file_scope", label: "File Scope" },
  { value: "command", label: "Command" },
];

const SEVERITY_OPTIONS: { value: ConstraintSeverity; label: string }[] = [
  { value: "block", label: "Block" },
  { value: "warn", label: "Warn" },
  { value: "log", label: "Log" },
];

/** Build a default check object for a given type. */
function defaultCheck(type: ConstraintCheckType): ConstraintCheck {
  switch (type) {
    case "grep_forbidden":
      return { type: "grep_forbidden", pattern: "" };
    case "grep_required":
      return { type: "grep_required", pattern: "" };
    case "file_scope":
      return { type: "file_scope", allowed_paths: ["src/"] };
    case "command":
      return { type: "command", cmd: "", timeout_secs: DEFAULT_COMMAND_TIMEOUT_SECS };
  }
}

export function ConstraintCard({ constraint, onUpdate, onRemove, onPromote }: ConstraintCardProps) {
  const [expanded, setExpanded] = useState(false);
  const isAi = isAiConstraint(constraint.id);

  const handleCheckTypeChange = (newType: ConstraintCheckType) => {
    if (newType !== constraint.check.type) {
      onUpdate({ check: defaultCheck(newType) });
    }
  };

  return (
    <div
      className={`rounded-md border overflow-hidden ${
        isAi
          ? "border-dashed border-purple-500/40 bg-purple-500/5"
          : "border-zinc-700/50 bg-zinc-800/30"
      }`}
    >
      {/* Collapsed header */}
      <div className="flex items-center gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => !isAi && setExpanded(!expanded)}
          className={`shrink-0 transition-colors ${
            isAi ? "text-zinc-600 cursor-default" : "text-zinc-400 hover:text-zinc-200"
          }`}
        >
          {expanded && !isAi ? (
            <ChevronDown className="w-4 h-4" />
          ) : (
            <ChevronRight className="w-4 h-4" />
          )}
        </button>

        <div className="flex-1 min-w-0 flex items-center gap-2">
          {isAi && (
            <span className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-purple-500/15 text-purple-400">
              AI
            </span>
          )}
          <span className="text-sm font-medium text-zinc-200 truncate">
            {constraint.name || "Unnamed constraint"}
          </span>
          <span
            className={`inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium ${severityBadgeColor(constraint.severity)}`}
          >
            {severityLabel(constraint.severity)}
          </span>
          <span className="text-[10px] text-zinc-500">
            {constraintCheckTypeLabel(constraint.check.type)}
          </span>
        </div>

        {/* Promote button (AI constraints only) */}
        {isAi && onPromote && (
          <button
            type="button"
            onClick={onPromote}
            className="flex items-center gap-1 shrink-0 px-2 py-0.5 text-xs text-purple-400 hover:text-purple-300 bg-purple-500/10 hover:bg-purple-500/20 rounded transition-colors"
            title="Promote to project constraint"
          >
            <ArrowUpCircle className="w-3.5 h-3.5" />
            Promote
          </button>
        )}

        {/* Enable toggle (hidden for AI constraints) */}
        {!isAi && (
          <button
            type="button"
            role="switch"
            aria-checked={constraint.enabled}
            onClick={() => onUpdate({ enabled: !constraint.enabled })}
            className={`
              relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full border-2 border-transparent
              transition-colors duration-200 ease-in-out focus:outline-hidden focus:ring-2 focus:ring-blue-500/50
              ${constraint.enabled ? "bg-blue-600" : "bg-zinc-600"}
            `}
          >
            <span
              className={`
                pointer-events-none inline-block h-4 w-4 transform rounded-full bg-white shadow ring-0
                transition duration-200 ease-in-out
                ${constraint.enabled ? "translate-x-4" : "translate-x-0"}
              `}
            />
          </button>
        )}

        {/* Delete button (hidden for AI constraints) */}
        {!isAi && (
          <button
            type="button"
            onClick={onRemove}
            className="shrink-0 p-1 text-zinc-500 hover:text-red-400 rounded transition-colors"
            title="Remove constraint"
          >
            <Trash2 className="w-3.5 h-3.5" />
          </button>
        )}
      </div>

      {/* Expanded editor (not available for AI constraints) */}
      {expanded && !isAi && (
        <div className="px-3 pb-3 pt-1 space-y-3 border-t border-zinc-700/30 animate-slideDown">
          {/* Name */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">Name</label>
            <input
              type="text"
              value={constraint.name}
              onChange={(e) => onUpdate({ name: e.target.value })}
              placeholder="Constraint name..."
              className={inputClass}
            />
          </div>

          {/* Description */}
          <div>
            <label className="block text-xs font-medium text-zinc-400 mb-1">Description</label>
            <textarea
              value={constraint.description}
              onChange={(e) => onUpdate({ description: e.target.value })}
              placeholder="Why this constraint exists (shown to the AI on violation)..."
              rows={2}
              className={`${inputClass} resize-none`}
            />
          </div>

          {/* Severity + Check Type row */}
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs font-medium text-zinc-400 mb-1">Severity</label>
              <select
                value={constraint.severity}
                onChange={(e) => onUpdate({ severity: e.target.value as ConstraintSeverity })}
                className={selectClass}
              >
                {SEVERITY_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="block text-xs font-medium text-zinc-400 mb-1">Check Type</label>
              <select
                value={constraint.check.type}
                onChange={(e) => handleCheckTypeChange(e.target.value as ConstraintCheckType)}
                className={selectClass}
              >
                {CHECK_TYPE_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {/* Check-type-specific fields */}
          <div className="p-2.5 bg-zinc-800/50 rounded-md space-y-2.5">
            <CheckEditor check={constraint.check} onChange={(check) => onUpdate({ check })} />
          </div>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Check-type-specific editor (inline, keeps ConstraintCard self-contained)
// =============================================================================

function CheckEditor({
  check,
  onChange,
}: {
  check: ConstraintCheck;
  onChange: (check: ConstraintCheck) => void;
}) {
  switch (check.type) {
    case "grep_forbidden":
    case "grep_required":
      return (
        <>
          <div>
            <label className="block text-[10px] font-medium text-zinc-500 mb-0.5">
              {check.type === "grep_forbidden"
                ? "Forbidden Pattern (regex)"
                : "Required Pattern (regex)"}
            </label>
            <input
              type="text"
              value={check.pattern}
              onChange={(e) => onChange({ ...check, pattern: e.target.value })}
              placeholder={
                check.type === "grep_forbidden"
                  ? "e.g., (API_KEY|SECRET|PASSWORD)\\s*="
                  : "e.g., Copyright \\d{4}"
              }
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-[10px] font-medium text-zinc-500 mb-0.5">
              File Glob (optional)
            </label>
            <input
              type="text"
              value={check.file_glob ?? ""}
              onChange={(e) => onChange({ ...check, file_glob: e.target.value || undefined })}
              placeholder="e.g., *.ts, src/**/*.py"
              className={inputClass}
            />
          </div>
        </>
      );

    case "file_scope":
      return (
        <FileScopeEditor
          allowedPaths={check.allowed_paths}
          onChange={(allowed_paths) => onChange({ ...check, allowed_paths })}
        />
      );

    case "command":
      return (
        <>
          <div>
            <label className="block text-[10px] font-medium text-zinc-500 mb-0.5">Command</label>
            <input
              type="text"
              value={check.cmd}
              onChange={(e) => onChange({ ...check, cmd: e.target.value })}
              placeholder="e.g., cargo check --quiet"
              className={inputClass}
            />
          </div>
          <div className="grid grid-cols-2 gap-2">
            <div>
              <label className="block text-[10px] font-medium text-zinc-500 mb-0.5">
                Working Directory (optional)
              </label>
              <input
                type="text"
                value={check.cwd ?? ""}
                onChange={(e) => onChange({ ...check, cwd: e.target.value || undefined })}
                placeholder="Project root"
                className={inputClass}
              />
            </div>
            <div>
              <label className="block text-[10px] font-medium text-zinc-500 mb-0.5">
                Timeout (seconds)
              </label>
              <input
                type="number"
                value={check.timeout_secs}
                onChange={(e) =>
                  onChange({
                    ...check,
                    timeout_secs: parseInt(e.target.value, 10) || DEFAULT_COMMAND_TIMEOUT_SECS,
                  })
                }
                min={1}
                className={inputClass}
              />
            </div>
          </div>
        </>
      );
  }
}

// =============================================================================
// File Scope Path List Editor
// =============================================================================

function FileScopeEditor({
  allowedPaths,
  onChange,
}: {
  allowedPaths: string[];
  onChange: (paths: string[]) => void;
}) {
  return (
    <div>
      <label className="block text-[10px] font-medium text-zinc-500 mb-1">Allowed Paths</label>
      <div className="space-y-1.5">
        {allowedPaths.map((path, index) => (
          <div key={`path-${path}-${index}`} className="flex items-center gap-1.5">
            <input
              type="text"
              value={path}
              onChange={(e) => {
                const updated = [...allowedPaths];
                updated[index] = e.target.value;
                onChange(updated);
              }}
              placeholder="e.g., src/"
              className={inputClass}
            />
            <button
              type="button"
              onClick={() => {
                const updated = allowedPaths.filter((_, i) => i !== index);
                onChange(updated.length > 0 ? updated : ["src/"]);
              }}
              className="shrink-0 p-1 text-zinc-500 hover:text-red-400 rounded transition-colors"
              title="Remove path"
            >
              <X className="w-3.5 h-3.5" />
            </button>
          </div>
        ))}
      </div>
      <button
        type="button"
        onClick={() => onChange([...allowedPaths, ""])}
        className="flex items-center gap-1 mt-1.5 text-xs text-blue-400 hover:text-blue-300 transition-colors"
      >
        <Plus className="w-3 h-3" />
        Add path
      </button>
    </div>
  );
}
