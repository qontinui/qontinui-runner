/**
 * Verdicts — the one place a `CommandResult` is built, and the one place a
 * verdict is DERIVED from what an effect reported.
 *
 * ## Why this module exists
 *
 * `ok` / `fail` were defined three times (`useTerminalCommands.ts`,
 * `orchestrateCommand.ts`, `usePromptLibraryCommands.ts`) with three slightly
 * different signatures — `usePromptLibraryCommands`'s `ok()` took no value at
 * all, so a prompt action structurally could not report anything. Three copies
 * of a two-line helper is not the cost; three copies of the CONTRACT is,
 * because the contract is the thing this phase is changing.
 *
 * ## The deriver
 *
 * {@link deriveVerdict} generalizes `spawnVerdict`, which was the only honest
 * verdict on this page. Its five load-bearing properties are preserved here
 * deliberately, and every one of them is a defect that was found in this
 * pipeline:
 *
 *  1. **Pure.** No React, no context, no `ctx`. Takes what was observed plus
 *     what was requested, returns a `CommandResult`. That is why it is
 *     testable without mounting anything.
 *  2. **Normalises the untrustworthy return at the boundary.** The effects it
 *     reads are typed `Promise<string[] | void>`, `() => void`, or an object
 *     an older build might not have. {@link countOf} folds every non-answer
 *     into ZERO — "produced nothing" — rather than letting a `void` read as
 *     success. `Array.isArray(result) ? result : []` was the original of this
 *     idea and it is the reason `/spawn 2` stopped rendering `✓` for two
 *     terminals that were never created.
 *  3. **Compares produced against requested**, not "did the call return".
 *     A partial result is a FAILURE the operator has to see, because the page
 *     will not look like what they asked for.
 *  4. **Composes the message from both sides** — `spawned 1 of 3 terminals`
 *     names the shortfall, not just the failure.
 *  5. **Parameterised noun**, so one deriver serves every call site instead of
 *     one deriver per command.
 *
 * ## What an effect has to hand back
 *
 * The minimum useful signal is whatever distinguishes *did the thing* from
 * *was already in that state*. `countOf` accepts all of the shapes an effect
 * naturally has — a count, a boolean, a list of what it touched, or an
 * `{affected}` bag — so an effect never has to be reshaped to be readable.
 * An effect that hands back `undefined` is reported as zero, which is the
 * honest reading: it told us nothing, so we know of nothing that happened.
 */

import type { CommandResult } from "./types";

/** Build a successful result. */
export function ok<T>(value?: T): CommandResult<T> {
  return { ok: true, value };
}

/** Build a failure result with a stable machine code. */
export function fail(code: string, message?: string): CommandResult<never> {
  return { ok: false, code, message };
}

/**
 * What a handler reports about what its effect actually did.
 *
 * Carried as `CommandResult.value` and read by `CommandBar.tsx::renderStatus`,
 * which is what turns `/approve-all ✓` into `/approve-all ✓ approved 3
 * sessions` — and, crucially, into `/approve-all: no sessions approved` when
 * the count is zero. A no-op that renders identically to an effect is the
 * defect this whole phase exists to make visible.
 *
 * `verb` is PAST TENSE ("approved", "closed", "cleared") because it reads
 * correctly in both the affirmative and the zero form.
 */
export interface EffectReport {
  /** Past-tense verb: `"approved"`, `"closed"`, `"selected"`, `"cleared"`. */
  verb: string;
  /** SINGULAR noun: `"session"`, `"zone"`, `"tag filter"`. */
  noun: string;
  /** How many things the effect actually changed. Zero is a legitimate answer. */
  affected: number;
  /**
   * How many were TARGETED, when that can differ from `affected`. Present on
   * a partial result (`approved 2 of 3 sessions`) and omitted when the command
   * has no target count of its own (`/tag-clear` clears whatever is there).
   */
  requested?: number;
  /** Irregular plural, when `noun + "s"` is wrong. */
  nounPlural?: string;
  /**
   * How to READ this report — the two things a command on this page can do.
   *
   * - `"count"` (default): the effect touched N of something countable.
   *   `verb` is a past-tense action and `noun` a countable thing:
   *   `approved 3 sessions`, `no zones selected`.
   * - `"state"`: the effect put ONE named thing into a state. `verb` is the
   *   resulting state ("enabled", "muted", "maximized") and `affected` is 1
   *   when it moved and 0 when it was already there:
   *   `enabled focus mode`, `sound was already muted`.
   *
   * Without this split, every preference toggle rendered as `enabled 1 focus
   * mode`, which is worse than the bare `✓` it replaced. A verdict surface
   * that reads badly gets ignored, and an ignored verdict is the failure mode
   * this phase exists to fix.
   */
  kind?: "count" | "state";
  /**
   * One line of extra colour for the status line, e.g. which layout was
   * already set, or why the shortfall happened. Never load-bearing.
   */
  detail?: string;
}

/** Structural test — `CommandResult.value` is `unknown` at the render site. */
export function isEffectReport(value: unknown): value is EffectReport {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Partial<EffectReport>;
  return typeof v.verb === "string" && typeof v.noun === "string" && typeof v.affected === "number";
}

/** Build an {@link EffectReport}. Terser than the object literal at 20 call sites. */
export function effect(
  verb: string,
  noun: string,
  affected: number,
  extra: Omit<EffectReport, "verb" | "noun" | "affected"> = {},
): EffectReport {
  return { verb, noun, affected, ...extra };
}

/** `noun` in the right number. */
export function pluralize(report: Pick<EffectReport, "noun" | "nounPlural">, n: number): string {
  return n === 1 ? report.noun : (report.nounPlural ?? `${report.noun}s`);
}

/**
 * Fold whatever an effect handed back into a COUNT — property 2.
 *
 * Every arm here is a shape some effect on this page actually returns, and
 * the final `return 0` is the whole point: an effect that reported nothing
 * has told us of nothing that happened, and a verdict built on that must not
 * read as success. Booleans map to 1/0 because `{changed: boolean}` is the
 * minimum useful signal for a toggle.
 *
 * Non-integers and negatives are clamped rather than trusted: they can only
 * come from a caller bug, and a fractional "affected" would make the
 * produced-vs-requested comparison meaningless (`3 < 2.7` is false — the
 * exact shape that let `/spawn 2.7` create three terminals and report
 * success).
 */
export function countOf(produced: unknown): number {
  if (Array.isArray(produced)) return produced.length;
  if (typeof produced === "number") return clampCount(produced);
  if (typeof produced === "boolean") return produced ? 1 : 0;
  if (produced instanceof Set || produced instanceof Map) return produced.size;
  if (typeof produced === "object" && produced !== null) {
    const bag = produced as Record<string, unknown>;
    for (const field of ["affected", "delivered", "changed", "count", "moved"] as const) {
      if (field in bag) return countOf(bag[field]);
    }
  }
  return 0;
}

function clampCount(n: number): number {
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.floor(n);
}

/** Inputs to {@link deriveVerdict}. */
export interface DeriveVerdictInput {
  /**
   * What the effect reported. DELIBERATELY `unknown`: the whole point is that
   * this value is not trusted, and typing it would invite a caller to skip
   * {@link countOf}.
   */
  produced: unknown;
  /**
   * How many the operator asked for. Omit when the command has no target of
   * its own — `/tag-clear` clears whatever is there, and a zero result from
   * it is a no-op rather than a shortfall.
   */
  requested?: number;
  /** Past-tense verb for the message and the report. */
  verb: string;
  /** SINGULAR noun. */
  noun: string;
  /** Irregular plural. */
  nounPlural?: string;
  /** Failure code used when `produced < requested`. */
  code?: string;
  /** Extra colour carried through onto a successful report. */
  detail?: string;
}

/**
 * Derive a verdict from what an effect reported — see the module docstring
 * for the five properties this preserves.
 *
 * Returns a FAILURE when a target count was named and the effect fell short;
 * otherwise a success carrying the {@link EffectReport}, INCLUDING when
 * nothing happened. Zero-with-no-target is not an error — see the product
 * call recorded in `CommandBar.tsx::renderStatus` — it is a distinct neutral
 * verdict the status line renders differently from `✓`.
 */
export function deriveVerdict(input: DeriveVerdictInput): CommandResult<EffectReport> {
  const affected = countOf(input.produced);
  const report: EffectReport = {
    verb: input.verb,
    noun: input.noun,
    affected,
    ...(input.requested === undefined ? {} : { requested: input.requested }),
    ...(input.nounPlural === undefined ? {} : { nounPlural: input.nounPlural }),
    ...(input.detail === undefined ? {} : { detail: input.detail }),
  };
  if (input.requested !== undefined && affected < input.requested) {
    return fail(
      input.code ?? "effect-fell-short",
      `${input.verb} ${affected} of ${input.requested} ${pluralize(report, input.requested)}`,
    );
  }
  return ok(report);
}

/**
 * Render an {@link EffectReport} as the operator-facing status text.
 *
 * Kept beside the deriver rather than in `CommandBar.tsx` so the wording is
 * unit-testable without a DOM, and so the CommandBar's job stays "pick a
 * colour" rather than "compose a sentence".
 */
export function describeReport(report: EffectReport): string {
  const n = report.affected;
  const head =
    report.kind === "state"
      ? n === 0
        ? `${report.noun} was already ${report.verb}`
        : `${report.verb} ${report.noun}`
      : n === 0
        ? `no ${pluralize(report, 0)} ${report.verb}`
        : report.requested !== undefined && report.requested !== n
          ? `${report.verb} ${n} of ${report.requested} ${pluralize(report, n)}`
          : `${report.verb} ${n} ${pluralize(report, n)}`;
  return report.detail ? `${head} — ${report.detail}` : head;
}

/** Build a `kind: "state"` report — the toggle/preference shape. */
export function stateEffect(
  resultingState: string,
  noun: string,
  changed: boolean,
  extra: Omit<EffectReport, "verb" | "noun" | "affected" | "kind"> = {},
): EffectReport {
  return { verb: resultingState, noun, affected: changed ? 1 : 0, kind: "state", ...extra };
}
