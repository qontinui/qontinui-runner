/**
 * Prompt matrix fan-out — pure functions only (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
 * Phase 5).
 *
 * One prompt template × a matrix of variables → N planned members, each with
 * its own rendered prompt and title. Nothing here spawns anything: the
 * Phase 7 preview renders the rows `planFanout` returns, and the Phase 6
 * runner scheduler receives the rows the operator approved.
 *
 * Every failure is a typed value, never a throw and never a silent repair:
 * unequal `zip` axes are an error (not a truncation), an oversized matrix is a
 * refusal (not a clamp), an unknown placeholder is reported (not rendered as
 * an invisible ""), and an over-long prompt is a row error (not a truncation).
 */

import type { PromptParameter, PromptTemplate } from "./promptLibraryApi";
import {
  missingRequired,
  renderPromptTemplate,
  type PromptParamValues,
} from "./renderPromptTemplate";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** Default hard ceiling on the number of members one matrix may expand to. */
export const DEFAULT_MAX_MEMBERS = 24;

/**
 * Upper bound on one rendered prompt, in UTF-16 code units (JS `.length`).
 *
 * Phase 6 passes the prompt to `claude` as a positional argv element. Windows
 * caps a whole command line at 32,767 UTF-16 characters (`CreateProcessW`),
 * and that budget is shared with the executable path, `--session-id`,
 * `--settings` and every other flag, plus quoting/escaping growth. 24,000
 * leaves ~8 K of headroom for all of that. A prompt over this bound is a
 * typed row error, never a truncation.
 */
export const MAX_ARGV_PROMPT_CHARS = 24_000;

/** Same placeholder grammar `renderPromptTemplate` substitutes. */
const PLACEHOLDER_RE = /\{\{\s*([\w.-]+)\s*\}\}/g;

/** A matrix axis name must be usable as a `{{name}}` placeholder. */
const AXIS_NAME_RE = /^[\w.-]+$/;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** One matrix axis: a variable name and the values it takes, in order. */
export interface MatrixAxis {
  name: string;
  values: string[];
}

/** How axes combine into members. */
export type MatrixMode = "zip" | "product";

/** One member's matrix assignment, keyed by axis name. */
export type MatrixMember = Record<string, string>;

export type MatrixParseError =
  | { kind: "empty_matrix" }
  /** A `;`-separated segment that is blank (e.g. `a:1;;b:2` or a trailing `;`). */
  | { kind: "empty_axis"; segment: number }
  /** A segment with no `:` between the axis name and its values. */
  | { kind: "missing_separator"; segment: number; text: string }
  | { kind: "empty_axis_name"; segment: number }
  /** An axis name that could not be referenced as a `{{name}}` placeholder. */
  | { kind: "invalid_axis_name"; segment: number; name: string }
  | { kind: "duplicate_axis"; name: string }
  /** A blank value in an axis's `,`-separated list (e.g. `a:1,,2` or `a:`). */
  | { kind: "empty_value"; axis: string; position: number };

export type MatrixParseResult =
  | { ok: true; axes: MatrixAxis[] }
  | { ok: false; error: MatrixParseError };

export type MatrixExpandError =
  | { kind: "no_axes" }
  /** `zip` requires every axis to have the same length; never truncated. */
  | { kind: "unequal_zip_lengths"; axes: { name: string; length: number }[] }
  /** The expansion would exceed the member ceiling; nothing is expanded. */
  | { kind: "too_many_members"; count: number; max: number };

export type MatrixExpandResult =
  | { ok: true; members: MatrixMember[] }
  | { ok: false; error: MatrixExpandError };

export type FanoutRowError =
  /** The rendered prompt exceeds `MAX_ARGV_PROMPT_CHARS` (argv-unsafe). */
  { kind: "prompt_too_long"; length: number; max: number };

export type FanoutRowWarning =
  /** The prompt body references no matrix axis, so every member gets the same prompt. */
  | { kind: "identical_prompts"; count: number; message: string }
  /** A title placeholder with no value; it rendered as "". */
  | { kind: "unknown_title_placeholder"; name: string };

/** One planned fan-out member, as the Phase 7 preview shows it. */
export interface FanoutRow {
  /** 0-based position in the member list (also the scheduler's member index). */
  index: number;
  /** The values this member was rendered with: `{...fixedValues, ...member}`. */
  values: PromptParamValues;
  title: string;
  prompt: string;
  /**
   * Parameter names that have no value for this member: required parameters
   * still unfilled (`missingRequired`), plus every body placeholder that is
   * neither a declared parameter nor supplied by `fixedValues`/the matrix —
   * `renderPromptTemplate` would otherwise render it as an invisible "".
   */
  missing: string[];
  errors: FanoutRowError[];
  warnings: FanoutRowWarning[];
}

/** The template fields `planFanout` reads. */
export type FanoutTemplate = Pick<PromptTemplate, "body" | "parameters">;

// ---------------------------------------------------------------------------
// parseMatrix
// ---------------------------------------------------------------------------

/**
 * Parse `"platform:iOS,Android;lang:swift,kotlin"` into axes.
 *
 * Grammar: axes separated by `;`, each `name:value,value,…`. Whitespace
 * around names and values is trimmed. Strict: blank segments, blank values,
 * blank or non-placeholder names and duplicate names are typed errors.
 */
export function parseMatrix(input: string): MatrixParseResult {
  if (input.trim().length === 0) return { ok: false, error: { kind: "empty_matrix" } };

  const axes: MatrixAxis[] = [];
  const seen = new Set<string>();
  const segments = input.split(";");

  for (let i = 0; i < segments.length; i++) {
    const segment = segments[i].trim();
    if (segment.length === 0) return { ok: false, error: { kind: "empty_axis", segment: i } };

    const colon = segment.indexOf(":");
    if (colon < 0) {
      return { ok: false, error: { kind: "missing_separator", segment: i, text: segment } };
    }

    const name = segment.slice(0, colon).trim();
    if (name.length === 0) return { ok: false, error: { kind: "empty_axis_name", segment: i } };
    if (!AXIS_NAME_RE.test(name)) {
      return { ok: false, error: { kind: "invalid_axis_name", segment: i, name } };
    }
    if (seen.has(name)) return { ok: false, error: { kind: "duplicate_axis", name } };
    seen.add(name);

    const rawValues = segment.slice(colon + 1).split(",");
    const values: string[] = [];
    for (let p = 0; p < rawValues.length; p++) {
      const v = rawValues[p].trim();
      if (v.length === 0) {
        return { ok: false, error: { kind: "empty_value", axis: name, position: p } };
      }
      values.push(v);
    }
    axes.push({ name, values });
  }

  return { ok: true, axes };
}

// ---------------------------------------------------------------------------
// expandMatrix
// ---------------------------------------------------------------------------

/**
 * Expand axes into members.
 *
 * - `zip` (default): member i takes value i of every axis. All axes must be
 *   the same length — unequal lengths are an `unequal_zip_lengths` error
 *   naming every axis and its length, never a truncation to the shortest.
 * - `product`: the cartesian product, first axis varying slowest.
 *
 * The member count is computed BEFORE expanding, so an oversized product is
 * refused without materialising it.
 */
export function expandMatrix(
  axes: readonly MatrixAxis[],
  mode: MatrixMode = "zip",
  maxMembers: number = DEFAULT_MAX_MEMBERS,
): MatrixExpandResult {
  if (axes.length === 0) return { ok: false, error: { kind: "no_axes" } };

  let count: number;
  if (mode === "zip") {
    const lengths = new Set(axes.map((a) => a.values.length));
    if (lengths.size > 1) {
      return {
        ok: false,
        error: {
          kind: "unequal_zip_lengths",
          axes: axes.map((a) => ({ name: a.name, length: a.values.length })),
        },
      };
    }
    count = axes[0].values.length;
  } else {
    count = axes.reduce((n, a) => n * a.values.length, 1);
  }

  if (count > maxMembers) {
    return { ok: false, error: { kind: "too_many_members", count, max: maxMembers } };
  }

  const members: MatrixMember[] = [];
  if (mode === "zip") {
    for (let i = 0; i < count; i++) {
      const m: MatrixMember = {};
      for (const a of axes) m[a.name] = a.values[i];
      members.push(m);
    }
  } else {
    let acc: MatrixMember[] = [{}];
    for (const a of axes) {
      const next: MatrixMember[] = [];
      for (const partial of acc) {
        for (const v of a.values) next.push({ ...partial, [a.name]: v });
      }
      acc = next;
    }
    members.push(...acc);
  }

  return { ok: true, members };
}

// ---------------------------------------------------------------------------
// planFanout
// ---------------------------------------------------------------------------

/** Distinct `{{name}}` placeholder names in `text`, in first-seen order. */
export function templatePlaceholders(text: string): string[] {
  const names: string[] = [];
  for (const m of text.matchAll(PLACEHOLDER_RE)) {
    if (!names.includes(m[1])) names.push(m[1]);
  }
  return names;
}

function hasValue(values: PromptParamValues, name: string): boolean {
  return (
    Object.prototype.hasOwnProperty.call(values, name) &&
    values[name] !== undefined &&
    values[name] !== null
  );
}

/**
 * Plan one row per member by rendering the template with
 * `{...fixedValues, ...member}` (a matrix value overrides a fixed value of
 * the same name).
 *
 * `missing[]` per row is `missingRequired(parameters, values)` plus every
 * body placeholder that has no value AND is not a declared parameter. A
 * declared OPTIONAL parameter left unset is not missing — rendering it as ""
 * is the documented contract of `renderPromptTemplate`; an UNDECLARED
 * placeholder with no value is, because it would otherwise vanish silently.
 *
 * When there is more than one member and the body references no matrix axis
 * (or every rendered prompt comes out identical anyway), every row carries an
 * `identical_prompts` warning. A prompt longer than `MAX_ARGV_PROMPT_CHARS`
 * is a `prompt_too_long` row error.
 */
export function planFanout(
  template: FanoutTemplate,
  fixedValues: PromptParamValues,
  members: readonly MatrixMember[],
  titleTemplate: string,
  maxPromptChars: number = MAX_ARGV_PROMPT_CHARS,
): FanoutRow[] {
  const parameters: readonly PromptParameter[] = template.parameters ?? [];
  const declared = new Set(parameters.map((p) => p.name));
  const bodyPlaceholders = templatePlaceholders(template.body);
  const titlePlaceholders = templatePlaceholders(titleTemplate);

  const axisNames = new Set<string>();
  for (const m of members) for (const k of Object.keys(m)) axisNames.add(k);

  const rows: FanoutRow[] = members.map((member, index) => {
    const values: PromptParamValues = { ...fixedValues, ...member };
    const prompt = renderPromptTemplate(template.body, values);
    const title = renderPromptTemplate(titleTemplate, values);

    const missing = missingRequired(parameters, values);
    for (const name of bodyPlaceholders) {
      if (!declared.has(name) && !hasValue(values, name) && !missing.includes(name)) {
        missing.push(name);
      }
    }

    const errors: FanoutRowError[] = [];
    if (prompt.length > maxPromptChars) {
      errors.push({ kind: "prompt_too_long", length: prompt.length, max: maxPromptChars });
    }

    const warnings: FanoutRowWarning[] = [];
    for (const name of titlePlaceholders) {
      if (!hasValue(values, name)) warnings.push({ kind: "unknown_title_placeholder", name });
    }

    return { index, values, title, prompt, missing, errors, warnings };
  });

  if (rows.length > 1) {
    const referencesAxis = bodyPlaceholders.some((n) => axisNames.has(n));
    const allSame = rows.every((r) => r.prompt === rows[0].prompt);
    if (!referencesAxis || allSame) {
      const warning: FanoutRowWarning = {
        kind: "identical_prompts",
        count: rows.length,
        message: `all ${rows.length} prompts are identical`,
      };
      for (const r of rows) r.warnings.push(warning);
    }
  }

  return rows;
}
