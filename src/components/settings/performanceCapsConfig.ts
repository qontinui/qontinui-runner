/**
 * Declarative descriptors for the Performance & Caps controls (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 8).
 *
 * Same shape as `@qontinui/workflow-utils`' `WORKFLOW_SETTINGS_CONFIG`
 * (`{ key, type, label, description, min, max }` per field, rendered by a
 * generic loop) so a new cap is a data change rather than more JSX. It is a
 * local mirror rather than a direct reuse of `NumberSettingDef` because
 * that type keys on `string` and carries a workflow-features `visible`
 * predicate; here the key is `keyof PerfCaps`, which is what keeps the
 * renderer and the Rust struct from drifting apart silently.
 *
 * Two extra flags exist for the honesty gate — a control must say what it
 * cannot promise:
 *
 * - `requiresRestart` — the value is read once at process start, so saving
 *   it does nothing until the runner restarts.
 * - `pendingPhase` — the knob is persisted and served, but its consumer has
 *   not landed on `main` yet. Naming the PR/phase in the UI is the honest
 *   alternative to a control that silently does nothing.
 */

import type { PerfCaps } from "@/lib/perfCaps";

export interface PerfCapNumberField {
  key: keyof PerfCaps;
  type: "number";
  label: string;
  description: string;
  min: number;
  max: number;
  /** Unit suffix rendered after the input (e.g. "ms", "panes"). */
  unit?: string;
  /** Effective only after a runner restart. */
  requiresRestart?: boolean;
  /** Consumer not yet merged — the text names what is still missing. */
  pendingPhase?: string;
}

export interface PerfCapBooleanField {
  key: keyof PerfCaps;
  type: "boolean";
  label: string;
  description: string;
  requiresRestart?: boolean;
  pendingPhase?: string;
}

/**
 * Tri-state: `null` = inherit from another field, `true`/`false` = pinned.
 * Modelled explicitly because `Intent::effective_redact_secrets` really is
 * tri-state on the Rust side, and collapsing it to a checkbox would lose the
 * "follows sharing" default that every existing install has.
 */
export interface PerfCapTristateField {
  key: keyof PerfCaps;
  type: "tristate";
  label: string;
  description: string;
  /** Label for the `null` option. */
  inheritLabel: string;
  requiresRestart?: boolean;
  pendingPhase?: string;
}

export type PerfCapField = PerfCapNumberField | PerfCapBooleanField | PerfCapTristateField;

/** A number field's value clamped into its declared range. */
export function clampPerfField(field: PerfCapNumberField, value: unknown): number {
  const n = typeof value === "number" && Number.isFinite(value) ? value : field.min;
  return Math.max(field.min, Math.min(field.max, Math.round(n)));
}

/** Is this value outside the field's declared range (so a save will move it)? */
export function isOutOfRange(field: PerfCapNumberField, value: unknown): boolean {
  return (
    typeof value === "number" && Number.isFinite(value) && value !== clampPerfField(field, value)
  );
}

/**
 * Clamp every declared number field of a draft into range.
 *
 * Applied once at save rather than per keystroke: `scrollback_capacity_bytes`
 * has a six-digit minimum, so clamping on change makes the field impossible to
 * retype (the first digit you enter is instantly replaced by the minimum, and
 * the next keystroke appends to *that*). Non-number fields pass through
 * untouched.
 */
export function clampPerfCaps(draft: PerfCaps, fields: readonly PerfCapField[]): PerfCaps {
  const out: PerfCaps = { ...draft };
  for (const field of fields) {
    if (field.type !== "number") continue;
    // Only number-typed keys reach here, so the cast is narrowing the union
    // member, not inventing a shape.
    (out[field.key] as number) = clampPerfField(field, draft[field.key]);
  }
  return out;
}

export const PERF_CAP_FIELDS: readonly PerfCapField[] = [
  {
    key: "max_sessions_warn",
    type: "number",
    label: "Session-count advisory threshold",
    description:
      "Show a dismissible banner on the Terminal page once this many sessions are open. Advisory only — nothing is ever blocked or refused, and opening more sessions always works.",
    min: 0,
    max: 500,
    unit: "sessions",
  },
  {
    key: "max_webgl_panes",
    type: "number",
    label: "Max WebGL terminal panes",
    description:
      "How many panes may hold a WebGL rendering context at once; the rest fall back to the Canvas renderer. Browsers cap live contexts at roughly 8-16 per process, so raising this past your GPU's real limit trades our eviction for the driver's.",
    min: 1,
    max: 64,
    unit: "panes",
    pendingPhase: "the WebGL context LRU (perf plan Phase 2)",
  },
  {
    key: "background_flush_interval_ms",
    type: "number",
    label: "Background pane flush interval",
    description:
      "Output flush cadence for a pane that is mounted but not the one you are looking at. Higher means less render work and choppier updates on those panes; 0 disables coalescing entirely. Applies to terminals opened after the save.",
    min: 0,
    max: 10_000,
    unit: "ms",
  },
  {
    key: "unwatched_flush_interval_ms",
    type: "number",
    label: "Unwatched session flush interval",
    description:
      "Flush cadence for a session with no mounted pane at all. 0 means no webview emit — output accumulates in the scrollback ring and replays when you reveal the pane, which is the cheapest and the default. A positive value keeps such sessions streaming at that cadence instead, so a reveal needs no replay. Applies to terminals opened after the save.",
    min: 0,
    max: 60_000,
    unit: "ms",
  },
  {
    key: "scrollback_capacity_bytes",
    type: "number",
    label: "Scrollback ring size",
    description:
      "Bytes of output kept per session in the backend ring — the source of truth a hidden pane replays from. Applies to terminals opened after the save; existing terminals keep the ring they were allocated. Values under 64 KiB are clamped to 64 KiB.",
    min: 65_536,
    max: 67_108_864,
    unit: "bytes",
  },
  {
    key: "grid_scan_interval_ms",
    type: "number",
    label: "Grid-scan interval",
    description:
      "How often the auto-response and usage-limit scanners sweep every session's rendered grid. Read once when the runner starts, so a change needs a restart. QONTINUI_AUTO_RESPONSE_SCAN_INTERVAL_MS and QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS still override this. Values under 200 ms are clamped to 200 ms.",
    min: 200,
    max: 60_000,
    unit: "ms",
    requiresRestart: true,
  },
  {
    key: "share_terminal_output",
    type: "boolean",
    label: "Share terminal output with coord",
    description:
      "Declares Intent.share_output on terminal sessions mirrored into coord. This is a configuration and privacy choice, not a performance one — no per-terminal output pipe exists either way. Applies to terminals opened after the save.",
  },
  {
    key: "redact_terminal_secrets",
    type: "tristate",
    label: "Redact secrets in shared output",
    description:
      "Whether shared terminal output is scrubbed for secrets. Left on 'Follow sharing' it matches the historical behavior: redaction is on whenever sharing is on.",
    inheritLabel: "Follow sharing (default)",
  },
] as const;
