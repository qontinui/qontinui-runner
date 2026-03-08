/**
 * SkillParameterForm.tsx
 *
 * Renders parameter inputs for a skill definition. Each parameter is rendered
 * based on its type: string, number, boolean, or select. Validates required
 * parameters and shows validation errors.
 *
 * Can be used standalone (with onSubmit/onCancel) or embedded (with onChange).
 */

import React, { useState, useMemo, useCallback } from "react";
import type { SkillDefinition, SkillParameter } from "@qontinui/shared-types/workflow";
import { validateSkillParams } from "@qontinui/workflow-utils";

// =============================================================================
// Types
// =============================================================================

/** Standalone form with submit/cancel buttons */
export interface SkillParameterFormStandaloneProps {
  skill: SkillDefinition;
  onSubmit: (params: Record<string, unknown>) => void;
  onCancel: () => void;
}

/** Embedded form (controlled by parent) */
export interface SkillParameterFormEmbeddedProps {
  skill: SkillDefinition;
  values: Record<string, unknown>;
  onChange: (name: string, value: unknown) => void;
  errors?: string[];
}

// =============================================================================
// Standalone Form (with submit/cancel)
// =============================================================================

export function SkillParameterFormStandalone({
  skill,
  onSubmit,
  onCancel,
}: SkillParameterFormStandaloneProps) {
  const [values, setValues] = useState<Record<string, unknown>>(() => {
    const defaults: Record<string, unknown> = {};
    for (const param of skill.parameters) {
      if (param.default !== undefined) {
        defaults[param.name] = param.default;
      }
    }
    return defaults;
  });

  const validationErrors = useMemo(() => validateSkillParams(skill, values), [skill, values]);

  const handleChange = useCallback((name: string, value: unknown) => {
    setValues((prev) => ({ ...prev, [name]: value }));
  }, []);

  const handleSubmit = useCallback(() => {
    if (validationErrors.length > 0) return;
    onSubmit(values);
  }, [values, validationErrors, onSubmit]);

  return (
    <div className="space-y-4">
      {/* Skill info header */}
      <div className="pb-3 border-b border-zinc-700/50">
        <h3 className="text-sm font-medium text-zinc-200">{skill.name}</h3>
        <p className="text-xs text-zinc-500 mt-0.5">{skill.description}</p>
      </div>

      {/* Parameter fields */}
      <ParameterFields parameters={skill.parameters} values={values} onChange={handleChange} />

      {/* Validation errors */}
      {validationErrors.length > 0 && (
        <div className="space-y-1">
          {validationErrors.map((err, i) => (
            <p key={i} className="text-xs text-red-400">
              {err}
            </p>
          ))}
        </div>
      )}

      {/* Buttons */}
      <div className="flex justify-end gap-2 pt-2">
        <button
          className="px-4 py-2 text-sm text-zinc-400 hover:text-zinc-200 rounded-md hover:bg-zinc-800 transition-colors"
          onClick={onCancel}
        >
          Cancel
        </button>
        <button
          className="px-4 py-2 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded-md disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          onClick={handleSubmit}
          disabled={validationErrors.length > 0}
        >
          Add Skill
        </button>
      </div>
    </div>
  );
}

// =============================================================================
// Embedded Form (controlled by parent, no buttons)
// =============================================================================

export function SkillParameterForm({
  skill,
  values,
  onChange,
  errors,
}: SkillParameterFormEmbeddedProps) {
  if (skill.parameters.length === 0) {
    return (
      <p className="text-sm text-zinc-500 italic py-2">
        No parameters needed -- this skill is ready to use.
      </p>
    );
  }

  return (
    <div className="space-y-3">
      <ParameterFields parameters={skill.parameters} values={values} onChange={onChange} />
      {errors && errors.length > 0 && (
        <div className="space-y-1">
          {errors.map((err, i) => (
            <p key={i} className="text-xs text-red-400">
              {err}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Parameter Fields
// =============================================================================

interface ParameterFieldsProps {
  parameters: SkillParameter[];
  values: Record<string, unknown>;
  onChange: (name: string, value: unknown) => void;
}

function ParameterFields({ parameters, values, onChange }: ParameterFieldsProps) {
  return (
    <div className="space-y-3">
      {parameters.map((param) => (
        <ParameterField
          key={param.name}
          param={param}
          value={values[param.name]}
          onChange={(value) => onChange(param.name, value)}
        />
      ))}
    </div>
  );
}

// =============================================================================
// Individual Field
// =============================================================================

interface ParameterFieldProps {
  param: SkillParameter;
  value: unknown;
  onChange: (value: unknown) => void;
}

function ParameterField({ param, value, onChange }: ParameterFieldProps) {
  const inputClasses =
    "w-full bg-zinc-800 border border-zinc-700 rounded-md px-3 py-2 text-sm text-zinc-200 " +
    "placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-500 focus:border-zinc-500";

  return (
    <div>
      <label className="flex items-center gap-1 text-sm text-zinc-300 mb-1">
        {param.label}
        {param.required && <span className="text-red-400 text-xs">*</span>}
      </label>
      {param.description && <p className="text-xs text-zinc-500 mb-1.5">{param.description}</p>}

      {param.type === "string" && (
        <input
          type="text"
          className={inputClasses}
          value={(value as string) ?? ""}
          onChange={(e) => onChange(e.target.value)}
          placeholder={param.placeholder}
        />
      )}

      {param.type === "number" && (
        <input
          type="number"
          className={inputClasses}
          value={value !== undefined && value !== null ? String(value) : ""}
          onChange={(e) => {
            const num = e.target.value === "" ? undefined : Number(e.target.value);
            onChange(num);
          }}
          placeholder={param.placeholder}
        />
      )}

      {param.type === "boolean" && (
        <button
          type="button"
          role="switch"
          aria-checked={!!value}
          className={`
            relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full
            transition-colors focus:outline-none focus:ring-2 focus:ring-zinc-500 focus:ring-offset-2 focus:ring-offset-zinc-900
            ${value ? "bg-blue-500" : "bg-zinc-700"}
          `}
          onClick={() => onChange(!value)}
        >
          <span
            className={`
              pointer-events-none inline-block h-5 w-5 rounded-full bg-white shadow
              transform transition-transform mt-0.5
              ${value ? "translate-x-5 ml-0.5" : "translate-x-0.5"}
            `}
          />
        </button>
      )}

      {param.type === "select" && param.options && (
        <select
          className={inputClasses}
          value={(value as string) ?? ""}
          onChange={(e) => onChange(e.target.value)}
        >
          <option value="">Select...</option>
          {param.options.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
      )}
    </div>
  );
}
