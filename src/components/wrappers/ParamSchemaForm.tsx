/**
 * ParamSchemaForm
 *
 * Generic schema-driven form for wrapper actions and (Phase 3+) workflow
 * step parameters. Reads a `paramSchema` (the JSON-Schema-subset shape
 * `paramSchemaOf` produces in `@qontinui/ui-bridge-wrapper`) and renders one
 * input per top-level property:
 *
 *   - `type: "string"` (no enum)  → text input (textarea if `format: "multiline"`)
 *   - `type: "string"` with enum  → select
 *   - `type: "number" | "integer"` → number input
 *   - `type: "boolean"`            → checkbox
 *   - `type: "array"`              → comma-separated text input → string[]
 *   - `type: "object"`             → JSON textarea (best-effort; the runner
 *                                    framework's paramSchemas are flat in
 *                                    practice, so this is a fallback for
 *                                    forward-compat)
 *
 * Required vs optional is taken from the schema's `required: string[]`. If a
 * property has a `default`, that's the initial value. Submitting calls
 * `onSubmit(params)` with the assembled object.
 *
 * The component is deliberately presentation-light so callers can wrap it in
 * whatever container suits their layout (inline panel for "Try it", modal for
 * step config, etc.). No side effects beyond `onSubmit`.
 *
 * # Public API
 *
 * ```tsx
 * <ParamSchemaForm
 *   schema={action.paramSchema}
 *   onSubmit={(params) => dispatchAction(wrapper.id, action.id, params)}
 *   submitLabel="Run"           // optional; default "Submit"
 *   submitting={isDispatching}  // optional; disables the button + shows spinner
 *   compact                     // optional; tighter spacing for inline panels
 * />
 * ```
 *
 * Phase 3 will reuse this same component for workflow step config; Phase 4
 * tool-injection layers can use it too. Keep the prop surface stable.
 */

import { useEffect, useMemo, useState } from "react";
import { Loader2 } from "lucide-react";
import type { ParamSchema, ParamSchemaProperty } from "@/lib/wrappers/types";

export interface ParamSchemaFormProps {
  /** The action's paramSchema (object-typed JSON-Schema subset). */
  schema: ParamSchema | undefined | null;
  /** Initial values, useful for "edit existing config" flows. */
  initialValues?: Record<string, unknown>;
  /** Called with the assembled params when the form is submitted. */
  onSubmit: (params: Record<string, unknown>) => void | Promise<void>;
  /** Optional cancel handler. If omitted, no cancel button is rendered. */
  onCancel?: () => void;
  /** Submit button label. Defaults to "Submit". */
  submitLabel?: string;
  /** Disables submit + shows a spinner. */
  submitting?: boolean;
  /** Tighter spacing for inline panels. */
  compact?: boolean;
  /** Forwarded id for accessibility / form association. */
  id?: string;
}

interface FieldDef {
  name: string;
  property: ParamSchemaProperty;
  required: boolean;
}

function inferFields(schema: ParamSchema | undefined | null): FieldDef[] {
  if (!schema || !schema.properties) return [];
  const required = new Set(schema.required ?? []);
  return Object.entries(schema.properties).map(([name, property]) => ({
    name,
    property,
    required: required.has(name),
  }));
}

function defaultFor(property: ParamSchemaProperty): unknown {
  if ("default" in property) return property.default;
  switch (property.type) {
    case "boolean":
      return false;
    case "number":
    case "integer":
      return "";
    case "array":
      return "";
    case "object":
      return "";
    default:
      return "";
  }
}

function coerce(property: ParamSchemaProperty, raw: unknown): unknown {
  if (property.type === "boolean") return Boolean(raw);
  if (property.type === "number" || property.type === "integer") {
    if (raw === "" || raw == null) return undefined;
    const n = Number(raw);
    return Number.isFinite(n) ? n : undefined;
  }
  if (property.type === "array") {
    if (Array.isArray(raw)) return raw;
    if (typeof raw === "string") {
      const trimmed = raw.trim();
      if (!trimmed) return undefined;
      return trimmed
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
    }
    return undefined;
  }
  if (property.type === "object") {
    if (raw && typeof raw === "object") return raw;
    if (typeof raw === "string") {
      const trimmed = raw.trim();
      if (!trimmed) return undefined;
      try {
        return JSON.parse(trimmed);
      } catch {
        return undefined;
      }
    }
    return undefined;
  }
  // string and unknown types
  if (raw === "" || raw == null) return undefined;
  return raw;
}

const labelClass = "block text-xs font-medium text-foreground mb-1.5";
const inputBase =
  "w-full rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground " +
  "placeholder:text-muted-foreground/70 focus:outline-none focus:ring-2 focus:ring-cyan-500/40 " +
  "focus:border-cyan-500/60 transition-colors";

export function ParamSchemaForm({
  schema,
  initialValues,
  onSubmit,
  onCancel,
  submitLabel = "Submit",
  submitting = false,
  compact = false,
  id,
}: ParamSchemaFormProps) {
  const fields = useMemo(() => inferFields(schema), [schema]);

  const [values, setValues] = useState<Record<string, unknown>>(() => {
    const seed: Record<string, unknown> = {};
    for (const f of fields) {
      seed[f.name] =
        initialValues && f.name in initialValues ? initialValues[f.name] : defaultFor(f.property);
    }
    return seed;
  });

  const [errors, setErrors] = useState<Record<string, string>>({});

  // Re-seed when schema swaps (different action selected).
  useEffect(() => {
    const seed: Record<string, unknown> = {};
    for (const f of fields) {
      seed[f.name] =
        initialValues && f.name in initialValues ? initialValues[f.name] : defaultFor(f.property);
    }
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset form values on schema swap
    setValues(seed);
    setErrors({});
    // initialValues intentionally omitted — re-seeding when the parent's
    // identity-stable initialValues prop changes is the caller's job via
    // remounting; we only re-seed on schema swap.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fields]);

  const handleChange = (name: string, raw: unknown) => {
    setValues((prev) => ({ ...prev, [name]: raw }));
    if (errors[name]) {
      setErrors((prev) => {
        const next = { ...prev };
        delete next[name];
        return next;
      });
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (submitting) return;
    const params: Record<string, unknown> = {};
    const nextErrors: Record<string, string> = {};
    for (const f of fields) {
      const coerced = coerce(f.property, values[f.name]);
      if (f.required && (coerced === undefined || coerced === "" || coerced === null)) {
        nextErrors[f.name] = "Required";
        continue;
      }
      // Reject malformed JSON for object fields that had non-empty input.
      if (f.property.type === "object") {
        const raw = values[f.name];
        if (typeof raw === "string" && raw.trim() && coerced === undefined) {
          nextErrors[f.name] = "Invalid JSON";
          continue;
        }
      }
      if (coerced !== undefined) {
        params[f.name] = coerced;
      }
    }
    if (Object.keys(nextErrors).length > 0) {
      setErrors(nextErrors);
      return;
    }
    setErrors({});
    await onSubmit(params);
  };

  if (fields.length === 0) {
    return (
      <form id={id} onSubmit={handleSubmit} className={compact ? "space-y-3" : "space-y-4"}>
        <p className="text-sm text-muted-foreground italic">This action takes no parameters.</p>
        <div className="flex items-center gap-2">
          {onCancel && (
            <button
              type="button"
              onClick={onCancel}
              disabled={submitting}
              className="px-3 py-1.5 rounded-md border border-border bg-card text-sm text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors disabled:opacity-50"
            >
              Cancel
            </button>
          )}
          <button
            type="submit"
            disabled={submitting}
            className="inline-flex items-center gap-2 px-4 py-1.5 rounded-md bg-gradient-to-r from-cyan-600 to-purple-600 text-white text-sm font-medium hover:from-cyan-500 hover:to-purple-500 transition-colors disabled:opacity-50"
          >
            {submitting && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
            {submitLabel}
          </button>
        </div>
      </form>
    );
  }

  return (
    <form id={id} onSubmit={handleSubmit} className={compact ? "space-y-3" : "space-y-4"}>
      {fields.map((f) => (
        <Field
          key={f.name}
          field={f}
          value={values[f.name]}
          error={errors[f.name]}
          onChange={(raw) => handleChange(f.name, raw)}
        />
      ))}

      <div className="flex items-center gap-2 pt-1">
        {onCancel && (
          <button
            type="button"
            onClick={onCancel}
            disabled={submitting}
            className="px-3 py-1.5 rounded-md border border-border bg-card text-sm text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
        )}
        <button
          type="submit"
          disabled={submitting}
          className="inline-flex items-center gap-2 px-4 py-1.5 rounded-md bg-gradient-to-r from-cyan-600 to-purple-600 text-white text-sm font-medium hover:from-cyan-500 hover:to-purple-500 transition-colors disabled:opacity-50"
        >
          {submitting && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
          {submitLabel}
        </button>
      </div>
    </form>
  );
}

interface FieldProps {
  field: FieldDef;
  value: unknown;
  error?: string;
  onChange: (raw: unknown) => void;
}

function Field({ field, value, error, onChange }: FieldProps) {
  const { name, property, required } = field;
  const description = property.description;
  const enumValues = Array.isArray(property.enum) ? property.enum : null;

  const labelEl = (
    <label htmlFor={`param-${name}`} className={labelClass}>
      <span>{name}</span>
      {required && <span className="ml-1 text-cyan-400">*</span>}
      {!required && (
        <span className="ml-2 text-[10px] uppercase tracking-wider text-muted-foreground/70">
          optional
        </span>
      )}
    </label>
  );

  const renderInput = (): React.ReactNode => {
    if (enumValues && enumValues.length > 0) {
      return (
        <select
          id={`param-${name}`}
          value={(value as string | number | undefined) ?? ""}
          onChange={(e) => onChange(e.target.value)}
          className={inputBase}
        >
          {!required && <option value="">— none —</option>}
          {enumValues.map((opt) => (
            <option key={String(opt)} value={String(opt)}>
              {String(opt)}
            </option>
          ))}
        </select>
      );
    }
    if (property.type === "boolean") {
      return (
        <label className="inline-flex items-center gap-2 cursor-pointer">
          <input
            id={`param-${name}`}
            type="checkbox"
            checked={Boolean(value)}
            onChange={(e) => onChange(e.target.checked)}
            className="h-4 w-4 rounded border-border bg-background text-cyan-500 focus:ring-cyan-500/40"
          />
          <span className="text-sm text-foreground">{name}</span>
        </label>
      );
    }
    if (property.type === "number" || property.type === "integer") {
      return (
        <input
          id={`param-${name}`}
          type="number"
          step={property.type === "integer" ? 1 : "any"}
          value={(value as string | number | undefined) ?? ""}
          onChange={(e) => onChange(e.target.value)}
          className={inputBase}
        />
      );
    }
    if (property.type === "object") {
      return (
        <textarea
          id={`param-${name}`}
          value={typeof value === "string" ? value : value ? JSON.stringify(value, null, 2) : ""}
          onChange={(e) => onChange(e.target.value)}
          rows={4}
          placeholder="{}"
          className={`${inputBase} font-mono text-xs`}
        />
      );
    }
    if (property.type === "array") {
      return (
        <input
          id={`param-${name}`}
          type="text"
          value={Array.isArray(value) ? value.join(", ") : ((value as string | undefined) ?? "")}
          onChange={(e) => onChange(e.target.value)}
          placeholder="value1, value2, value3"
          className={inputBase}
        />
      );
    }
    // string default
    const isMultiline = property.format === "multiline";
    if (isMultiline) {
      return (
        <textarea
          id={`param-${name}`}
          value={(value as string | undefined) ?? ""}
          onChange={(e) => onChange(e.target.value)}
          rows={3}
          className={inputBase}
        />
      );
    }
    return (
      <input
        id={`param-${name}`}
        type="text"
        value={(value as string | undefined) ?? ""}
        onChange={(e) => onChange(e.target.value)}
        className={inputBase}
      />
    );
  };

  const input = renderInput();

  // For boolean we already render the label inline with the checkbox.
  if (property.type === "boolean") {
    return (
      <div>
        {input}
        {description && <p className="mt-1 ml-6 text-xs text-muted-foreground">{description}</p>}
        {error && <p className="mt-1 ml-6 text-xs text-red-400">{error}</p>}
      </div>
    );
  }

  return (
    <div>
      {labelEl}
      {input}
      {description && <p className="mt-1 text-xs text-muted-foreground">{description}</p>}
      {error && <p className="mt-1 text-xs text-red-400">{error}</p>}
    </div>
  );
}
