/**
 * Fan-out preview rules — pure functions behind the `PromptModal` "Fan out…"
 * mode (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
 * Phase 7).
 *
 * The preview's job is that what the operator SEES is exactly what the runner
 * RUNS: `POST /fanout` never re-expands a matrix, so the request is built from
 * the very rows the table rendered, filtered by the operator's ticks. Everything
 * that decides whether Create is enabled lives here so it is testable under the
 * `node` vitest environment (no React, no Tauri, no fetch).
 */

import type { ConflictReport } from "./useSessionManager";
import type { ConfigDirPolicy, CreateFanoutRequest, FanoutCapOutcome } from "./fanoutApi";
import type { PromptTemplate } from "./promptLibraryApi";
import type { PromptParamValues } from "./renderPromptTemplate";
import {
  expandMatrix,
  parseMatrix,
  planFanout,
  type FanoutRow,
  type FanoutRowError,
  type FanoutRowWarning,
  type MatrixAxis,
  type MatrixExpandError,
  type MatrixMode,
  type MatrixParseError,
  serverTrim,
} from "./promptMatrix";

/** The cap a new run carries unless the operator changes it (server default too). */
export const FANOUT_DEFAULT_MAX_CONCURRENT = 3;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

export function matrixParseErrorMessage(e: MatrixParseError): string {
  switch (e.kind) {
    case "empty_matrix":
      return "Enter a matrix, e.g. platform:iOS,Android;lang:swift,kotlin";
    case "empty_axis":
      return `Axis ${e.segment + 1} is blank (check for a doubled or trailing ";")`;
    case "missing_separator":
      return `Axis ${e.segment + 1} ("${e.text}") has no ":" between its name and values`;
    case "empty_axis_name":
      return `Axis ${e.segment + 1} has no name before ":"`;
    case "invalid_axis_name":
      return `Axis name "${e.name}" cannot be used as a {{placeholder}} (letters, digits, _ . - only)`;
    case "duplicate_axis":
      return `Axis "${e.name}" is declared twice`;
    case "empty_value":
      return `Axis "${e.axis}" has a blank value at position ${e.position + 1}`;
  }
}

export function matrixExpandErrorMessage(e: MatrixExpandError): string {
  switch (e.kind) {
    case "no_axes":
      return "The matrix has no axes";
    case "unequal_zip_lengths":
      return (
        "zip needs every axis to have the same number of values: " +
        e.axes.map((a) => `${a.name}=${a.length}`).join(", ") +
        " (switch to product, or even the lists up)"
      );
    case "too_many_members":
      return `The matrix expands to ${e.count} members; the ceiling is ${e.max}`;
  }
}

export function rowErrorLabel(e: FanoutRowError): string {
  switch (e.kind) {
    case "prompt_too_long":
      return `prompt is ${e.bytes} bytes; the argv bound is ${e.max}`;
    case "empty_prompt":
      return "prompt is blank";
    case "prompt_contains_nul":
      return "prompt contains a NUL byte";
    case "empty_title":
      return "title is blank";
    case "title_too_long":
      return `title is ${e.length} characters; the limit is ${e.max}`;
  }
}

export function rowWarningLabel(w: FanoutRowWarning): string {
  switch (w.kind) {
    case "identical_prompts":
      return w.message;
    case "unknown_title_placeholder":
      return `title placeholder {{${w.name}}} has no value`;
  }
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

export type FanoutPreview =
  | { kind: "error"; message: string }
  | { kind: "ok"; axes: MatrixAxis[]; rows: FanoutRow[] };

/** A sensible default title: the template slug, then every axis value. */
export function defaultTitleTemplate(slug: string, axes: readonly MatrixAxis[]): string {
  if (axes.length === 0) return slug;
  return `${slug} — ${axes.map((a) => `{{${a.name}}}`).join(" · ")}`;
}

/**
 * Matrix text → preview rows, or the one message that says why there are none.
 * Pure composition of `parseMatrix` → `expandMatrix` → `planFanout`.
 */
export function planPreview(input: {
  template: Pick<PromptTemplate, "body" | "parameters">;
  fixedValues: PromptParamValues;
  matrixText: string;
  mode: MatrixMode;
  titleTemplate: string;
  maxMembers?: number;
}): FanoutPreview {
  const parsed = parseMatrix(input.matrixText);
  if (!parsed.ok) return { kind: "error", message: matrixParseErrorMessage(parsed.error) };
  const expanded = expandMatrix(parsed.axes, input.mode, input.maxMembers);
  if (!expanded.ok) return { kind: "error", message: matrixExpandErrorMessage(expanded.error) };
  const rows = planFanout(input.template, input.fixedValues, expanded.members, input.titleTemplate);
  return { kind: "ok", axes: parsed.axes, rows };
}

/** A row the operator may not create: any error, or any missing parameter. */
export function rowIsBlocked(row: FanoutRow): boolean {
  return row.errors.length > 0 || row.missing.length > 0;
}

/** The runner's OS family, which decides what `Path::is_absolute` accepts. */
export type PathPlatform = "windows" | "posix";

/** The runner's path family, read from the webview (the runner is local to it). */
export function detectPathPlatform(navigatorPlatform: string | undefined): PathPlatform {
  return navigatorPlatform?.startsWith("Win") ? "windows" : "posix";
}

/**
 * True when the runner's `Path::is_absolute` would accept `dir`: a drive root
 * (`C:\` / `C:/`) or a UNC path on Windows, a leading `/` elsewhere. A POSIX
 * path is not absolute on Windows, nor a drive path on Linux, so the check is
 * per platform — the preview must not pass a dir the server refuses.
 */
export function isAbsolutePath(dir: string, platform: PathPlatform): boolean {
  if (platform === "windows") return /^[A-Za-z]:[\\/]/.test(dir) || /^[\\/]{2}[^\\/]/.test(dir);
  return dir.startsWith("/");
}

/** Largest cap the runner's `u32` field can carry. */
export const MAX_CONCURRENT_WIRE = 4_294_967_295;

export interface FanoutGateInput {
  rows: readonly FanoutRow[];
  ticked: ReadonlySet<number>;
  workingDir: string;
  maxConcurrent: number;
  policy: ConfigDirPolicy;
  platform: PathPlatform;
}

export interface FanoutGate {
  canCreate: boolean;
  /** Ticked rows, in index order. */
  tickedCount: number;
  /** Ticked rows that carry an error or a missing parameter. */
  blockedIndices: number[];
  /** Why Create is disabled, or `null` when it is enabled. */
  reason: string | null;
}

/**
 * Whether Create is enabled. Disabled while ANY ticked row has an error or a
 * missing parameter (an unticked row is not sent, so it does not block), while
 * nothing is ticked, and while a run-level field the runner would refuse is
 * invalid. The first failing rule is the reason shown.
 */
export function fanoutGate(input: FanoutGateInput): FanoutGate {
  const tickedRows = input.rows.filter((r) => input.ticked.has(r.index));
  const blockedIndices = tickedRows.filter(rowIsBlocked).map((r) => r.index);
  const base = { tickedCount: tickedRows.length, blockedIndices };

  let reason: string | null = null;
  if (tickedRows.length === 0) {
    reason = "Tick at least one member";
  } else if (blockedIndices.length > 0) {
    reason =
      `${blockedIndices.length} ticked member${blockedIndices.length === 1 ? " has" : "s have"} ` +
      `an error or a missing parameter (#${blockedIndices.map((i) => i + 1).join(", #")}) — ` +
      "fix it or untick it";
  } else if (input.workingDir.trim().length === 0) {
    reason = "Set a working directory";
  } else if (!isAbsolutePath(input.workingDir.trim(), input.platform)) {
    reason = "The working directory must be an absolute path";
  } else if (!Number.isInteger(input.maxConcurrent) || input.maxConcurrent < 1) {
    reason = "Max concurrent must be a whole number of at least 1";
  } else if (input.maxConcurrent > MAX_CONCURRENT_WIRE) {
    reason = "Max concurrent is too large (the runner clamps it to the fan-out bound anyway)";
  } else if (input.policy.kind === "fixed" && input.policy.configDir.trim().length === 0) {
    reason = "Pick the account every member launches under";
  }
  return { ...base, canCreate: reason === null, reason };
}

/**
 * The `POST /fanout` body: exactly the ticked rows, in index order, with the
 * title trimmed the way the server trims it. Nothing is re-rendered here.
 */
export function buildCreateFanoutRequest(input: {
  rows: readonly FanoutRow[];
  ticked: ReadonlySet<number>;
  templateSlug: string;
  templateVersion: number;
  maxConcurrent: number;
  policy: ConfigDirPolicy;
  workingDir: string;
  tenantId?: string | null;
}): CreateFanoutRequest {
  const req: CreateFanoutRequest = {
    templateSlug: input.templateSlug,
    templateVersion: input.templateVersion,
    maxConcurrent: input.maxConcurrent,
    configDirPolicy: input.policy,
    workingDir: input.workingDir.trim(),
    members: input.rows
      .filter((r) => input.ticked.has(r.index))
      .map((r) => ({ title: serverTrim(r.title), prompt: r.prompt })),
  };
  if (input.tenantId) req.tenantId = input.tenantId;
  return req;
}

// ---------------------------------------------------------------------------
// Create outcome
// ---------------------------------------------------------------------------

export type FanoutCreateVerdict =
  | { ok: true; runId: string; message: string; clampNote: string | null }
  | { ok: false; message: string };

/**
 * Judge a create the way `aiSessionSpawnEnvelope` judges a spawn: a response is
 * a success only when the run holds EVERY member that was posted. A run with
 * fewer members than were sent is a short create and is reported as a failure
 * naming the shortfall, never as "created".
 */
export function judgeFanoutCreate(posted: number, outcome: FanoutCapOutcome): FanoutCreateVerdict {
  const held = outcome.run.members.length;
  if (held !== posted) {
    return {
      ok: false,
      message: `the runner holds ${held} of ${posted} posted members (run ${outcome.run.id})`,
    };
  }
  const clampNote =
    outcome.clampedFrom !== null
      ? `Max concurrent clamped from ${outcome.clampedFrom} to ${outcome.run.maxConcurrent} ` +
        `(this tenant's fan-out bound is ${outcome.fanoutBound})`
      : null;
  return {
    ok: true,
    runId: outcome.run.id,
    message:
      `Queued ${posted} member${posted === 1 ? "" : "s"} — ` +
      `up to ${outcome.run.maxConcurrent} run at once`,
    clampNote,
  };
}

// ---------------------------------------------------------------------------
// Run-level warnings
// ---------------------------------------------------------------------------

/**
 * The shared-cwd warning (plan Risks: "N sessions editing one repo").
 *
 * Whether members get isolated worktrees is decided by the RUNNER at spawn:
 * `QONTINUI_AGENT_WORKTREE_MODE` (on unless set to `0`/`false`/`no`) AND a
 * working directory inside a known repo checkout. No route reports that flag,
 * so the page cannot know it — the warning says isolation is UNKNOWN rather
 * than assuming either answer. Shown only when more than one member would run.
 */
export function sharedCwdWarning(tickedCount: number, workingDir: string): string | null {
  if (tickedCount < 2) return null;
  return (
    `Worktree isolation is UNKNOWN from this page: the runner isolates each member only when ` +
    `QONTINUI_AGENT_WORKTREE_MODE is on and ${workingDir.trim() || "the working directory"} is ` +
    `inside a known repo checkout. If it is off, all ${tickedCount} members share that directory ` +
    `and can edit the same files.`
  );
}

// ---------------------------------------------------------------------------
// Per-member collision probe
// ---------------------------------------------------------------------------

export type CollisionProbeState =
  | { kind: "pending" }
  | { kind: "unknown"; error: string }
  | { kind: "ok"; report: ConflictReport };

/** One short cell label for a member's collision probe. */
export function collisionProbeLabel(state: CollisionProbeState | undefined): {
  text: string;
  tone: "muted" | "warn" | "ok" | "unknown";
  title: string;
} {
  if (!state || state.kind === "pending") {
    return { text: "probing…", tone: "muted", title: "Collision probe in flight" };
  }
  if (state.kind === "unknown") {
    return { text: "UNKNOWN", tone: "unknown", title: `Collision probe failed: ${state.error}` };
  }
  const n = state.report.predicted_collisions.length;
  const degraded = state.report.ai_status !== "Ok";
  const files = state.report.predicted_collisions.map((c) => c.file_path).join(", ");
  if (n > 0) {
    return {
      text: `${n} collision${n === 1 ? "" : "s"}`,
      tone: "warn",
      title: `Predicted collisions with live sessions: ${files}`,
    };
  }
  return {
    text: degraded ? "none (regex only)" : "none",
    tone: "ok",
    title: degraded
      ? `No predicted collisions, but the AI extractor was ${String(state.report.ai_status)} — regex-only result`
      : "No predicted collisions with live sessions",
  };
}
