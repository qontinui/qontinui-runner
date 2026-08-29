/**
 * The differential harness — the thing nine rounds kept rebuilding and
 * throwing away.
 *
 * It takes TWO pipeline states, runs the same registry-derived corpus through
 * both, and reports the delta in FOUR classes:
 *
 *   1. `action-changed`   — a different action now owns the input
 *   2. `args-changed`     — SAME action, different bound arguments
 *   3. `now-errors`       — ran before, errors now
 *   4. `now-runs`         — errored before, runs now
 *
 * Class 2 is the reason this file exists. Every throwaway harness that
 * reported only "which action won" under-counted: one round it missed 14
 * changes, another 605. An input that keeps its action and loses an argument
 * is the exact shape of the `--tenant` prompt-truncation P0 and of the
 * `/spawn-ai N --tenant <v>` account mis-binding — both invisible to an
 * action-only diff.
 *
 * Two more classes are reported alongside them because a corpus DERIVED from
 * the registry legitimately changes size when the registry does:
 * `input-added` / `input-removed`. They are separated from the four so a
 * pattern addition never inflates the regression count.
 *
 * A delta carries a SET of classes, not one. An input whose args changed AND
 * whose verdict flipped is counted in both — collapsing it to a single
 * "worst" class is how an args change hides behind a verdict change.
 *
 * ## Strict and lenient arms
 *
 * `now-errors` / `now-runs` depend on the stubbed context: the accounts list,
 * the tenant candidates, the live-session reader. When the two sides are two
 * COMMITS, those stub inputs can differ (a new context field, a changed stub
 * shape) and the verdict flips are then environmental, not semantic. So
 * {@link summarize} reports both arms rather than silently picking one:
 *
 *   - `strict`  — every delta, all six classes.
 *   - `lenient` — only the structural classes (`action-changed`,
 *     `args-changed`), which no context stub can move.
 *
 * ## Cross-COMMIT use
 *
 * In-process, the two states are two registry snapshots (see
 * `differential.test.ts`'s self-check). Across commits, the two sides are two
 * checkouts, and the exchange format is the snapshot text below:
 *
 * ```sh
 * git checkout <base>
 * node scripts/terminal-command-corpus.mjs --out /tmp/base.txt --tier fast
 * git checkout <head>
 * node scripts/terminal-command-corpus.mjs --out /tmp/head.txt --tier fast
 * node scripts/terminal-command-corpus.mjs --baseline /tmp/base.txt --tier fast
 * ```
 *
 * Each side runs its OWN real modules and real handlers. Modelling the other
 * side instead of running it is what missed a whole class in one round.
 */

import type { CommandAction } from "./types";
import { canonicalArgs, run, type Route } from "./pipeline.testkit";

/** What the pipeline did with one input, in a form that compares cleanly. */
export interface OutcomeRecord {
  route: Route;
  /** `"-"` when nothing matched. */
  actionId: string;
  /** {@link canonicalArgs} output. */
  args: string;
  /** `none` | `unbound` | `ok` | `error:<code>` | `threw`. */
  verdict: string;
}

/** input → outcome, for one side of the comparison. */
export type Snapshot = Map<string, OutcomeRecord>;

/** One side: a registry contents plus a name for reporting. */
export interface PipelineState {
  name: string;
  actions: readonly CommandAction[];
}

export type DeltaClass =
  | "action-changed"
  | "args-changed"
  | "now-errors"
  | "now-runs"
  | "input-added"
  | "input-removed";

/** The four classes the brief requires; the other two are corpus drift. */
export const REGRESSION_CLASSES: readonly DeltaClass[] = [
  "action-changed",
  "args-changed",
  "now-errors",
  "now-runs",
];

/** The classes no context stub can move — see the module docstring. */
export const STRUCTURAL_CLASSES: readonly DeltaClass[] = ["action-changed", "args-changed"];

export interface Delta {
  input: string;
  classes: DeltaClass[];
  before: OutcomeRecord | null;
  after: OutcomeRecord | null;
}

/**
 * Run `corpus` through `state` and record every outcome.
 *
 * Installs `state` into the module registry, so callers must save and restore
 * the registry themselves if they care about it afterwards (the specs do).
 */
export async function captureSnapshot(
  state: PipelineState,
  corpus: readonly string[],
): Promise<Snapshot> {
  const registry = await import("./registry");
  registry.__resetForTest();
  for (const a of state.actions) registry.register(a);
  const lookup = (id: string): CommandAction => {
    const found = registry.getById(id);
    if (!found) throw new Error(`no such action id: ${id}`);
    return found;
  };
  const out: Snapshot = new Map();
  for (const input of corpus) {
    const o = await run(input, lookup);
    out.set(input, {
      route: o.route,
      actionId: o.actionId ?? "-",
      args: canonicalArgs(o.args),
      verdict: o.verdict,
    });
  }
  return out;
}

const ran = (verdict: string): boolean => verdict === "ok";

/** Compare two snapshots. Deltas come back sorted by input. */
export function diffSnapshots(before: Snapshot, after: Snapshot): Delta[] {
  const inputs = new Set([...before.keys(), ...after.keys()]);
  const deltas: Delta[] = [];
  for (const input of Array.from(inputs).sort()) {
    const b = before.get(input) ?? null;
    const a = after.get(input) ?? null;
    if (b === null && a === null) continue;
    if (b === null) {
      deltas.push({ input, classes: ["input-added"], before: null, after: a });
      continue;
    }
    if (a === null) {
      deltas.push({ input, classes: ["input-removed"], before: b, after: null });
      continue;
    }
    const classes: DeltaClass[] = [];
    // Every applicable class is recorded. An args change that also flips the
    // verdict must appear in BOTH — collapsing to one is how the args half
    // hides behind the verdict half.
    if (b.actionId !== a.actionId) classes.push("action-changed");
    else if (b.args !== a.args) classes.push("args-changed");
    if (ran(b.verdict) && !ran(a.verdict)) classes.push("now-errors");
    if (!ran(b.verdict) && ran(a.verdict)) classes.push("now-runs");
    if (classes.length > 0) deltas.push({ input, classes, before: b, after: a });
  }
  return deltas;
}

export interface DeltaSummary {
  /** Per-class counts over every delta. */
  strict: Record<DeltaClass, number>;
  /** Per-class counts restricted to {@link STRUCTURAL_CLASSES}. */
  lenient: Record<DeltaClass, number>;
  /** Deltas hitting at least one of {@link REGRESSION_CLASSES}. */
  strictTotal: number;
  /** Deltas hitting at least one of {@link STRUCTURAL_CLASSES}. */
  lenientTotal: number;
}

const EMPTY = (): Record<DeltaClass, number> => ({
  "action-changed": 0,
  "args-changed": 0,
  "now-errors": 0,
  "now-runs": 0,
  "input-added": 0,
  "input-removed": 0,
});

export function summarize(deltas: readonly Delta[]): DeltaSummary {
  const strict = EMPTY();
  const lenient = EMPTY();
  let strictTotal = 0;
  let lenientTotal = 0;
  for (const d of deltas) {
    for (const c of d.classes) {
      strict[c]++;
      if (STRUCTURAL_CLASSES.includes(c)) lenient[c]++;
    }
    if (d.classes.some((c) => REGRESSION_CLASSES.includes(c))) strictTotal++;
    if (d.classes.some((c) => STRUCTURAL_CLASSES.includes(c))) lenientTotal++;
  }
  return { strict, lenient, strictTotal, lenientTotal };
}

/** Human-readable delta report — printed by the CLI and on test failure. */
export function formatDelta(
  deltas: readonly Delta[],
  before: string,
  after: string,
  limit = 40,
): string {
  const s = summarize(deltas);
  const lines: string[] = [];
  lines.push(`differential: ${before} -> ${after}`);
  lines.push(
    `  STRICT  ${s.strictTotal} regressions  ` +
      REGRESSION_CLASSES.map((c) => `${c}=${s.strict[c]}`).join("  ") +
      `  (corpus drift: added=${s.strict["input-added"]} removed=${s.strict["input-removed"]})`,
  );
  lines.push(
    `  LENIENT ${s.lenientTotal} structural   ` +
      STRUCTURAL_CLASSES.map((c) => `${c}=${s.lenient[c]}`).join("  "),
  );
  for (const d of deltas.slice(0, limit)) {
    lines.push(`  ${d.input}   [${d.classes.join(",")}]`);
    lines.push(`      before: ${d.before ? fmt(d.before) : "(absent)"}`);
    lines.push(`      after : ${d.after ? fmt(d.after) : "(absent)"}`);
  }
  if (deltas.length > limit) lines.push(`  … and ${deltas.length - limit} more`);
  return lines.join("\n");
}

const fmt = (r: OutcomeRecord): string => `${r.route}\t${r.actionId}\t${r.args}\t${r.verdict}`;

// ── Snapshot file format ─────────────────────────────────────────────
//
// One TAB-separated line per input, sorted by input. Plain per-input lines
// rather than anything grouped or compressed, because the DIFF is the
// feature: a regression must read as "this input's row changed", on one line,
// in a `git diff` nobody has to decode.

const HEADER = [
  "# terminal CommandBar pipeline — golden characterization",
  "#",
  "# GENERATED. Do not hand-edit. Regenerate with:",
  "#   node scripts/terminal-command-corpus.mjs --update",
  "#",
  "# One line per corpus input:",
  "#   <input> TAB <route> TAB <actionId> TAB <args> TAB <verdict>",
  "#",
  "# The corpus is DERIVED from the action registry (slash forms, aliases and",
  "# every Tier-2 pattern, crossed with argument tails, quoting shapes and",
  "# declared-flag spellings), so adding a pattern adds rows here. That is",
  "# intended: the diff this file produces IS the review surface.",
  "#",
  "# A row changing action or args is a semantic change and needs a reason in",
  "# the PR. A row changing only its verdict may be the stubbed context in",
  "# realRegistry.testkit.ts moving instead — check that first.",
];

export function formatSnapshot(snapshot: Snapshot, tier: string): string {
  const lines = [...HEADER, `# tier: ${tier}   inputs: ${snapshot.size}`, ""];
  for (const input of Array.from(snapshot.keys()).sort()) {
    const r = snapshot.get(input) as OutcomeRecord;
    lines.push(`${input}\t${r.route}\t${r.actionId}\t${r.args}\t${r.verdict}`);
  }
  return lines.join("\n") + "\n";
}

export function parseSnapshot(text: string): Snapshot {
  const out: Snapshot = new Map();
  for (const line of text.split(/\r?\n/)) {
    if (line.length === 0 || line.startsWith("#")) continue;
    const parts = line.split("\t");
    if (parts.length < 5) continue;
    const [input, route, actionId, args, verdict] = parts;
    out.set(input, { route: route as Route, actionId, args, verdict });
  }
  return out;
}
