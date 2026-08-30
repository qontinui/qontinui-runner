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
 * *was already in that state*. `countOf` accepts a count, a boolean, a list
 * of what it touched, a Set/Map, or a BAG whose count lives under one of the
 * names in {@link COUNT_FIELDS}. An effect that hands back `undefined` is
 * reported as zero, which is the honest reading: it told us nothing, so we
 * know of nothing that happened.
 *
 * ## An unreadable shape is a TYPE error, not a silent zero
 *
 * The docstring above used to claim `countOf` "accepts all of the shapes an
 * effect naturally has ... so an effect never has to be reshaped to be
 * readable". It did not, and the same commit that wrote that sentence added a
 * shape it could not read: `RestartOutcome`'s success arm is `{restarted:
 * true, tabId, retiredTabId}`, no field of which was in the vocabulary. The
 * bag fell through to `return 0`, and `/restart` rendered a fully successful
 * restart as a red `restarted 0 of 1 session`.
 *
 * Nothing could have caught that, because `produced` was typed `unknown` — the
 * one type that accepts every shape INCLUDING the ones the reader cannot read.
 * So the vocabulary is now a closed, exported list ({@link COUNT_FIELDS}) and
 * {@link DeriveVerdictInput.produced} is typed {@link EffectEvidence}, which is
 * DERIVED from it. A handler that hands `deriveVerdict` a bag with no
 * recognised field no longer compiles, and the fix is a one-word addition to
 * the vocabulary rather than a defect an operator has to notice on screen.
 *
 * `countOf` itself still takes `unknown` and still folds an unrecognised value
 * to zero: it is the RUNTIME boundary, where a value can arrive from an older
 * build or across the Tauri wire, and there the honest answer really is "it
 * told us nothing". The type gate sits one level up, at the authoring site,
 * which is where the mistake is made.
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
export const COUNT_FIELDS = [
  "affected",
  "delivered",
  "changed",
  "count",
  "moved",
  /**
   * `RestartOutcome`. Added because it was MISSING — `/restart` reported a
   * successful restart as `restarted 0 of 1 session` for exactly as long as
   * this list did not name it. See the module docstring.
   */
  "restarted",
] as const;

// Deliberately NOT here: `exported` (`ctx.exportAll`'s `{exported, cancelled}`)
// and `total` (`ctx.sortZones`'s `{moved, total}`). Both are shapes an effect
// returns, and neither is routed through `countOf` — their handlers read the
// field directly. Adding a name nothing folds would make the list a catalogue
// of field names rather than a statement about what this reader can read, and
// the omission is not a hazard: routing one of them through `deriveVerdict`
// later is a compile error that names the missing word.

/** One of the names {@link countOf} will look for inside a bag. */
export type CountField = (typeof COUNT_FIELDS)[number];

/**
 * Every shape {@link countOf} can actually read.
 *
 * A bag qualifies when it carries AT LEAST ONE {@link CountField} — that is
 * what the mapped-type-indexed union below says, and it is why a bag naming
 * none of them is a compile error at a {@link deriveVerdict} call site rather
 * than a zero on the operator's screen.
 */
export type EffectEvidence =
  | number
  | boolean
  | null
  | undefined
  | readonly unknown[]
  | ReadonlySet<unknown>
  | ReadonlyMap<unknown, unknown>
  | CountBag;

/** An object carrying at least one {@link CountField}. */
export type CountBag = {
  [K in CountField]: { readonly [P in K]: EffectEvidence };
}[CountField];

export function countOf(produced: unknown): number {
  if (Array.isArray(produced)) return produced.length;
  if (typeof produced === "number") return clampCount(produced);
  if (typeof produced === "boolean") return produced ? 1 : 0;
  if (produced instanceof Set || produced instanceof Map) return produced.size;
  if (typeof produced === "object" && produced !== null) {
    const bag = produced as Record<string, unknown>;
    for (const field of COUNT_FIELDS) {
      if (field in bag) return countOf(bag[field]);
    }
  }
  return 0;
}

/**
 * Whether {@link countOf} could actually READ this value, as opposed to
 * folding it to zero because it recognised nothing.
 *
 * The two are indistinguishable in `countOf`'s own return — `0` is both "it
 * did nothing" and "I cannot read this" — and conflating them is what made
 * D1 invisible. Tests and the differential harness use this to assert that
 * every shape the product hands back is one the vocabulary covers.
 */
export function isReadableEvidence(produced: unknown): boolean {
  if (produced === null || produced === undefined) return false;
  if (Array.isArray(produced)) return true;
  if (typeof produced === "number") return Number.isFinite(produced);
  if (typeof produced === "boolean") return true;
  if (produced instanceof Set || produced instanceof Map) return true;
  if (typeof produced !== "object") return false;
  const bag = produced as Record<string, unknown>;
  return COUNT_FIELDS.some((field) => field in bag);
}

function clampCount(n: number): number {
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.floor(n);
}

/** Inputs to {@link deriveVerdict}. */
export interface DeriveVerdictInput {
  /**
   * What the effect reported.
   *
   * Typed {@link EffectEvidence}, NOT `unknown`. `unknown` was the original
   * choice — "the whole point is that this value is not trusted, and typing it
   * would invite a caller to skip `countOf`" — and it is what let a caller
   * hand in a bag `countOf` could not read. The value is still normalised
   * through {@link countOf} at runtime; the type only refuses a shape that
   * normalisation is known to fold to zero. Distrust and unreadability are
   * different problems, and `unknown` was answering the wrong one.
   */
  produced: EffectEvidence;
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
  const head = report.kind === "state" ? describeState(report) : describeCount(report);
  return report.detail ? `${head} — ${report.detail}` : head;
}

function describeState(report: EffectReport): string {
  return report.affected === 0
    ? `${report.noun} was already ${report.verb}`
    : `${report.verb} ${report.noun}`;
}

/**
 * The counting head — and the one place `requested` is allowed to survive
 * into a ZERO.
 *
 * The `n === 0` branch used to fire FIRST and unconditionally, which dropped
 * `requested` on the floor: `/approve-all` with three panes in needs-input and
 * all three writes refused on the wire rendered `no sessions approved`, the
 * same head sentence, in the same grey, as `/approve-all` with nothing waiting
 * at all. Only a trailing `detail` clause separated "there was nothing to do"
 * from "three approvals were attempted and every one failed".
 *
 * A zero is only a bare "nothing happened" when nothing was ASKED FOR. When a
 * positive target was named, the target is the load-bearing half of the
 * sentence and it is stated: `approved 0 of 3 sessions`. See
 * {@link statusKindOf}, which colours that same distinction.
 */
function describeCount(report: EffectReport): string {
  const n = report.affected;
  const requested = report.requested;
  if (n === 0) {
    return requested !== undefined && requested > 0
      ? `${report.verb} 0 of ${requested} ${pluralize(report, requested)}`
      : `no ${pluralize(report, 0)} ${report.verb}`;
  }
  return requested !== undefined && requested !== n
    ? `${report.verb} ${n} of ${requested} ${pluralize(report, n)}`
    : `${report.verb} ${n} ${pluralize(report, n)}`;
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


// ── The render half of a verdict ─────────────────────────────────────

/**
 * The three verdicts the CommandBar can paint, and the value of the
 * `data-status-kind` attribute a UI Bridge assertion reads.
 *
 * Lives HERE rather than in `CommandBar.tsx` for the reason `describeReport`
 * does: the component's job is to pick a colour, not to decide what happened.
 * Keeping the decision in the component also put it out of reach of the
 * headless harness — `__golden__/pipeline-golden.txt` pinned `ok` /
 * `error:<code>` at the `CommandResult` level, and a no-op IS `ok` there
 * (a deliberate product decision, stated at `CommandBar.tsx`'s `StatusLine`).
 * So the entire subject of this phase — whether a command paints `✓` or `·` —
 * was structurally outside what 91,784 corpus inputs could characterise.
 * Now the harness calls this function, the same one the component calls.
 */
export type StatusKind = "ok" | "noop" | "error";

/** What the status line paints, and what `data-status-kind` says. */
export interface RenderedStatus {
  kind: StatusKind;
  text: string;
}

/**
 * Classify a report into a painted verdict.
 *
 * Three arms, and the third is the one that was missing:
 *
 *  - `affected > 0` → **ok**. Something happened.
 *  - `affected === 0` with no positive target → **noop**. `/mute` when already
 *    muted, `/tag-clear` with no filter active. Legitimate outcomes of correct
 *    commands; grey, never red (see `CommandBar.tsx`'s `StatusLine` doc).
 *  - `affected === 0` with a POSITIVE target → **error**. Every one of the N
 *    things the operator asked for failed. Three refused `TERMINAL_WRITE_FAILED`
 *    writes to three live panes is not "nothing to do"; it rendered as one
 *    because the render only looked at `affected`. `data-status-kind` said
 *    `noop` for a total delivery failure, which is precisely the machine-readable
 *    signal an automated check trusts.
 *
 * Note this cannot make a NEW command red: `deriveVerdict` already FAILS when
 * `affected < requested`, so the only reports that reach here carrying a
 * positive `requested` are the ones built by hand — today `/approve-all`,
 * which omits `requested` from `deriveVerdict` on purpose so a partial
 * delivery still reports the approvals that landed.
 */
export function statusKindOf(report: EffectReport): StatusKind {
  if (report.affected > 0) return "ok";
  if (report.kind !== "state" && report.requested !== undefined && report.requested > 0) {
    return "error";
  }
  return "noop";
}

/** Glyph per verdict — the leading mark on the status line. */
const STATUS_GLYPH: Record<StatusKind, string> = { ok: "✓", noop: "·", error: "✗" };

/**
 * Compose the whole status line for a SUCCESSFUL `CommandResult`.
 *
 * A handler that reports an {@link EffectReport} gets its numbers rendered;
 * one that does not still gets the bare `✓`, so a handler can be converted
 * independently.
 */
export function renderCommandStatus(slash: string, value: unknown): RenderedStatus {
  if (!isEffectReport(value)) return { kind: "ok", text: `${slash} ✓` };
  const kind = statusKindOf(value);
  return { kind, text: `${slash} ${STATUS_GLYPH[kind]} ${describeReport(value)}` };
}
