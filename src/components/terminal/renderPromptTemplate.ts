/**
 * Pure prompt-template rendering: `{{name}}` substitution over the body
 * plus the required-parameters validator. Kept free of React/Tauri so the
 * unit tests run without any harness.
 */

import type { PromptParameter } from "./promptLibraryApi";

/** Values collected from the parameter form, keyed by parameter name. */
export type PromptParamValues = Record<string, string | number | boolean | undefined>;

/**
 * Substitute `{{name}}` placeholders in `body` with the collected values.
 * An unfilled placeholder (optional param left empty, or a placeholder
 * with no matching parameter) substitutes to the empty string.
 */
export function renderPromptTemplate(body: string, values: PromptParamValues): string {
  return body.replace(/\{\{\s*([\w.-]+)\s*\}\}/g, (_match, name: string) => {
    const v = values[name];
    if (v === undefined || v === null) return "";
    return String(v);
  });
}

/**
 * Whether `value` counts as "filled" for a required check. Booleans are
 * always filled (false is a real answer); empty/whitespace strings are not.
 */
function isFilled(value: string | number | boolean | undefined): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  return value.trim().length > 0;
}

/**
 * Names of required parameters that are still unfilled. A non-empty
 * result disables the primary action button. Parameters hidden by an
 * unmet `depends_on` are not required (the operator never saw them).
 */
export function missingRequired(
  params: readonly PromptParameter[],
  values: PromptParamValues,
): string[] {
  return params
    .filter((p) => p.required)
    .filter((p) => isParameterVisible(p, values))
    .filter((p) => !isFilled(values[p.name]))
    .map((p) => p.name);
}

/**
 * `depends_on` visibility: a parameter with a dependency renders only
 * when the depended-on parameter currently holds the required value.
 */
export function isParameterVisible(param: PromptParameter, values: PromptParamValues): boolean {
  const dep = param.depends_on;
  if (!dep) return true;
  const current = values[dep.param];
  return String(current ?? "") === String(dep.value ?? "");
}

/**
 * Initial form values: each parameter's declared `default` (stringified
 * per its type), booleans defaulting to false.
 */
export function initialParamValues(params: readonly PromptParameter[]): PromptParamValues {
  const values: PromptParamValues = {};
  for (const p of params) {
    if (p.default !== undefined && p.default !== null) {
      if (p.type === "boolean") values[p.name] = Boolean(p.default);
      else if (p.type === "number") values[p.name] = Number(p.default);
      else values[p.name] = String(p.default);
    } else if (p.type === "boolean") {
      values[p.name] = false;
    }
  }
  return values;
}
