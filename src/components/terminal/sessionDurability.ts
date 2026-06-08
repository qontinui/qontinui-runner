/**
 * Session durability classification — Phase 5 of
 * `2026-06-06-runner-dev-loop-and-restart-resilience` (the "honesty label").
 *
 * GROUND TRUTH (Phase 0 verdict): not every live session survives a runner
 * restart, and the operator must never believe a non-durable surface is safe.
 *
 *   - AI / chat sessions  → DURABLE. The conversation is persisted to the
 *     Claude CLI transcript on disk and auto-resumed via `--resume` after a
 *     restart (Phases 3/4). On the frontend a tab is an AI session iff it has
 *     captured a `claudeSessionId` (the transcript id `useTabSessionIdCapture`
 *     binds once the JSONL appears). The startup {@link SessionRecoveryBanner}
 *     lists exactly these after a restart.
 *
 *   - Interactive PTY terminal sessions → NON-DURABLE. A plain shell PTY is an
 *     OS child process with NO transcript persist/replay substrate. A runner
 *     restart ends the process and its scrollback; nothing brings it back. The
 *     recovery banner will NOT list it.
 *
 *   - Plan tabs (markdown viewers, `type === "plan"`) are not sessions at all
 *     — they carry no live process and have nothing to lose on restart, so we
 *     never mark them.
 *
 * This module is the single source of truth for that distinction so the
 * marker (and its copy) stay accurate and consistent across surfaces. Pure +
 * dependency-free so it is unit-testable in the runner's `node` vitest env
 * without mounting React.
 */

import type { TerminalTab } from "./useTerminalManager";

/** A live session's durability across a runner restart. */
export type SessionDurability = "durable" | "non-durable";

/**
 * Minimal shape needed to classify durability. A {@link TerminalTab}
 * structurally satisfies this — kept as its own interface so the classifier
 * can be exercised with plain fixtures (no full tab) in tests.
 */
export interface DurabilityInput {
  /** Claude CLI transcript session id, set once the session is bound. */
  claudeSessionId?: string;
  /** Coordinator task-run id for worker-backed (AI) sessions. */
  taskRunId?: string;
  /** Tab kind — "plan" tabs are markdown viewers, not sessions. */
  type?: "terminal" | "plan";
}

/**
 * Classify whether a tab's session survives a runner restart.
 *
 * Derived from EXISTING fields — no new backend payload was needed:
 *   - a captured `claudeSessionId` (or a worker `taskRunId`) ⇒ a transcript-
 *     backed AI session ⇒ `"durable"` (auto-resumed via `--resume`);
 *   - anything else that is a real `"terminal"` tab is a plain interactive PTY
 *     shell ⇒ `"non-durable"`.
 *
 * Plan tabs are not sessions; callers should use {@link isMarkableSession} to
 * skip them before showing any durability affordance.
 */
export function classifyTabDurability(tab: DurabilityInput): SessionDurability {
  if (tab.claudeSessionId || tab.taskRunId) return "durable";
  return "non-durable";
}

/**
 * Whether a tab represents a live session that a durability affordance is
 * meaningful for. Plan tabs (markdown viewers) carry no process, so they get
 * no marker either way.
 */
export function isMarkableSession(tab: DurabilityInput): boolean {
  return tab.type !== "plan";
}

/** True only for the genuinely non-durable interactive PTY case. */
export function isNonDurablePty(tab: TerminalTab): boolean {
  return isMarkableSession(tab) && classifyTabDurability(tab) === "non-durable";
}

/**
 * Honest, calm tooltip copy for a NON-durable interactive PTY session.
 * Ties explicitly back to the {@link SessionRecoveryBanner} mental model: the
 * banner brings back durable AI sessions; this one is what gets lost.
 */
export const NON_DURABLE_TOOLTIP =
  "Interactive terminal sessions are not saved — a runner restart ends " +
  "this session and its scrollback. AI chat sessions resume automatically " +
  "(and appear in the startup recovery summary); this one will not.";

/** Short inline label for the non-durable marker. */
export const NON_DURABLE_LABEL = "ephemeral";

/**
 * Subtle tooltip for the DURABLE AI-session affordance. Kept low-key per the
 * Phase 5 brief (the priority is honesty about the non-durable case, not
 * cluttering the durable one).
 */
export const DURABLE_TOOLTIP =
  "AI chat session — resumes automatically after a runner restart " +
  "(listed in the startup recovery summary).";
