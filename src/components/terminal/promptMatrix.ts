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
 * Upper bound on one rendered prompt's {@link promptArgvCost}.
 *
 * Phase 6 passes the prompt to `claude` as a positional argv element. Windows
 * caps a whole command line at 32,767 UTF-16 characters (`CreateProcessW`),
 * and that budget is shared with the executable path, `--session-id`,
 * `--settings` and every other flag. The runner enforces exactly this bound,
 * with exactly this measure, at `POST /fanout` (`fanout/model.rs`
 * `MAX_PROMPT_ARGV_COST` / `prompt_argv_cost`), so a row the preview passes is
 * a row the server accepts. A prompt over it is a typed row error, never a
 * truncation.
 */
export const MAX_PROMPT_ARGV_COST = 24 * 1024;

/**
 * Hard ceiling on members the runner accepts in one run (`fanout/model.rs`
 * `MAX_MEMBERS`). `DEFAULT_MAX_MEMBERS` is the preview's own, lower ceiling.
 */
export const SERVER_MAX_MEMBERS = 64;

/** Longest member title, in characters, the runner accepts (`MAX_TITLE_CHARS`). */
export const MAX_TITLE_CHARS = 200;

const utf8 = new TextEncoder();

/** UTF-8 byte length of `text`. */
export function utf8ByteLength(text: string): number {
  return utf8.encode(text).length;
}

/**
 * The worst-case length `text` occupies on a Windows command line, computed
 * the same way on every platform and the same way the runner computes it
 * (`fanout/model.rs` `prompt_argv_cost`).
 *
 * Each argv element is quoted the MSVC way: wrapped in quotes, every `"`
 * escaped with a backslash, and a run of backslashes doubled where a quote
 * follows it. Counting every `"` and every backslash once more bounds the
 * escaped length from above, and UTF-8 bytes bound UTF-16 units from above —
 * so neither a quote-heavy prompt nor a non-ASCII one can pass the preview
 * and then fail at spawn.
 */
export function promptArgvCost(text: string): number {
  let escapes = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c === 0x22 || c === 0x5c) escapes++;
  }
  return utf8ByteLength(text) + escapes + 2;
}

/**
 * Trim the way the runner's Rust `str::trim` does for its blank checks: JS
 * `trim()` does not strip U+0085 (NEL) and Rust does, so a NEL-only title
 * would pass a JS-trimmed check and be refused by the server. (JS also strips
 * U+FEFF, which Rust keeps — that only makes the preview stricter.)
 */
export function serverTrim(text: string): string {
  return text.replace(/^[\s\u0085]+|[\s\u0085]+$/g, "");
}

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

/**
 * Row errors mirror the runner's `build_members` refusals one for one, so a
 * row the preview passes is a row `POST /fanout` accepts.
 */
export type FanoutRowError =
  /** The rendered prompt's escaped argv length exceeds `MAX_PROMPT_ARGV_COST`. */
  | { kind: "prompt_too_long"; cost: number; max: number }
  /** The rendered prompt is blank. */
  | { kind: "empty_prompt" }
  /** The rendered prompt contains a NUL, which no argv element can carry. */
  | { kind: "prompt_contains_nul" }
  /** The rendered title is blank after trimming. */
  | { kind: "empty_title" }
  /** The rendered title (trimmed) exceeds `MAX_TITLE_CHARS` characters. */
  | { kind: "title_too_long"; length: number; max: number };

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
 * `identical_prompts` warning. A prompt whose {@link promptArgvCost} exceeds
 * `maxPromptCost` is a `prompt_too_long` row error; a blank prompt, a NUL in the prompt,
 * a blank title and an over-long title are row errors too, matching the
 * runner's `POST /fanout` validation.
 */
export function planFanout(
  template: FanoutTemplate,
  fixedValues: PromptParamValues,
  members: readonly MatrixMember[],
  titleTemplate: string,
  maxPromptCost: number = MAX_PROMPT_ARGV_COST,
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
    const cost = promptArgvCost(prompt);
    if (cost > maxPromptCost) {
      errors.push({ kind: "prompt_too_long", cost, max: maxPromptCost });
    }
    if (serverTrim(prompt).length === 0) errors.push({ kind: "empty_prompt" });
    if (prompt.includes("\0")) errors.push({ kind: "prompt_contains_nul" });
    const trimmedTitle = serverTrim(title);
    const titleChars = Array.from(trimmedTitle).length;
    if (titleChars === 0) errors.push({ kind: "empty_title" });
    else if (titleChars > MAX_TITLE_CHARS) {
      errors.push({ kind: "title_too_long", length: titleChars, max: MAX_TITLE_CHARS });
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
