/**
 * The input corpus — DERIVED FROM THE REGISTRY, never hand-listed.
 *
 * A hand-listed corpus is a corpus that stops growing the day it is written,
 * and every phrasing regression this directory has suffered was a phrasing
 * nobody had thought to list. So the heads here come out of the registry's
 * own `slash` / `aliases` / `patterns`, and adding a pattern to an action
 * adds inputs to every test that reads this file — including the golden
 * snapshot, where the addition shows up as a reviewable diff.
 *
 * ## Cross the dimensions, do not append them
 *
 * The single measurement mistake that made one round's regression count 14x
 * too low was appending quoting shapes ONLY to bare slash forms instead of
 * crossing them with argument tails. `spawn-ai "x"` and `spawn-ai 1 gmail
 * "--tenant"` are not the same test, and the second one is where the bugs
 * were. Everything below is a true cross product:
 *
 *     head  x  argument tail  x  quoting shape  x  flag spelling
 *
 * ## Three tiers
 *
 * `golden` backs the committed characterization file, `fast` backs the
 * invariants that run on every build, and `full` is the exhaustive cross, run
 * behind `TERMINAL_CORPUS=full`. Sizes and the measured runtimes behind them
 * are on {@link CorpusTier}.
 */

import type { CommandAction } from "./types";

// ── Pattern-source expansion ─────────────────────────────────────────

/** Canned fillers for the primitive classes this registry's patterns use. */
const PRIMITIVE_FILL: Record<string, string> = {
  d: "3",
  w: "gmail",
  S: "gmail",
  s: " ",
  ".": "fix the bug",
};

const MAX_PER_PATTERN = 16;

interface Atom {
  /** The alternative strings this atom can produce. */
  options: string[];
}

/**
 * Expand a regex SOURCE into concrete strings that satisfy it.
 *
 * Deliberately partial: it understands the subset the command registry
 * actually uses (anchors, literals, `\s`/`\d`/`\w`/`\S`/`.`, simple char
 * classes, `?`/`+`/`*`, non-capturing and named groups, alternation) and
 * nothing else. That is safe because {@link patternExemplars} DISCARDS any
 * expansion the pattern does not actually match, and
 * `corpus.test.ts::"every pattern yields at least one exemplar"` fails when
 * a new pattern outgrows this expander — so the limitation is loud, not
 * silent.
 */
export function expandSource(src: string): string[] {
  let i = 0;

  const parseAlternation = (): string[] => {
    const branches: string[][] = [parseSequence()];
    while (i < src.length && src[i] === "|") {
      i++;
      branches.push(parseSequence());
    }
    return dedupe(branches.flat());
  };

  const parseSequence = (): string[] => {
    let acc: string[] = [""];
    while (i < src.length && src[i] !== "|" && src[i] !== ")") {
      const atom = parseAtom();
      if (atom === null) continue;
      const next: string[] = [];
      for (const prefix of acc) {
        for (const opt of atom.options) {
          next.push(prefix + opt);
          if (next.length >= MAX_PER_PATTERN * 4) break;
        }
        if (next.length >= MAX_PER_PATTERN * 4) break;
      }
      acc = dedupe(next);
    }
    return acc;
  };

  const parseAtom = (): Atom | null => {
    const c = src[i];
    // Anchors contribute nothing to the string.
    if (c === "^" || c === "$") {
      i++;
      return null;
    }
    let base: { options: string[]; primitive: string | null };
    if (c === "(") {
      i++;
      // `(?:` / `(?<name>` / `(?=` — only the first two appear here.
      if (src[i] === "?") {
        i++;
        if (src[i] === ":") i++;
        else if (src[i] === "<") {
          while (i < src.length && src[i] !== ">") i++;
          i++;
        }
      }
      const inner = parseAlternation();
      if (src[i] === ")") i++;
      base = { options: inner, primitive: null };
    } else if (c === "[") {
      i++;
      base = parseClass();
    } else if (c === "\\") {
      i++;
      const esc = src[i];
      i++;
      base =
        esc in PRIMITIVE_FILL
          ? { options: [PRIMITIVE_FILL[esc]], primitive: esc }
          : { options: [esc], primitive: null };
    } else if (c === ".") {
      i++;
      base = { options: [PRIMITIVE_FILL["."]], primitive: "." };
    } else {
      i++;
      base = { options: [c], primitive: null };
    }

    // Quantifier.
    const q = src[i];
    if (q === "?") {
      i++;
      return { options: dedupe(["", ...base.options]) };
    }
    if (q === "+" || q === "*") {
      i++;
      // `*` can also match nothing; `+` always produces the filled form. The
      // fillers themselves already stand in for the repeated run (`\d+` ->
      // "3", `\s+` -> a single space, `.+` -> a phrase).
      return { options: q === "*" ? dedupe(["", ...base.options]) : base.options };
    }
    if (q === "{") {
      while (i < src.length && src[i] !== "}") i++;
      i++;
      return { options: base.options };
    }
    return { options: base.options };
  };

  const parseClass = (): { options: string[]; primitive: string | null } => {
    const members: string[] = [];
    let primitive: string | null = null;
    let negated = false;
    if (src[i] === "^") {
      negated = true;
      i++;
    }
    while (i < src.length && src[i] !== "]") {
      if (src[i] === "\\") {
        i++;
        const esc = src[i];
        i++;
        if (esc in PRIMITIVE_FILL) primitive = esc;
        else members.push(esc);
        continue;
      }
      // A `-` is a RANGE only between two literals; trailing `-` (as in
      // `[ -]`, which is a space and a hyphen) is a literal.
      if (src[i] === "-" && members.length > 0 && src[i + 1] !== "]") {
        primitive = primitive ?? "w";
        i += 2;
        continue;
      }
      members.push(src[i]);
      i++;
    }
    i++; // closing ]
    if (negated) return { options: ["x"], primitive: "w" };
    if (primitive && members.length === 0) {
      return { options: [PRIMITIVE_FILL[primitive]], primitive };
    }
    if (primitive) {
      return { options: dedupe([...members, PRIMITIVE_FILL[primitive]]), primitive };
    }
    return { options: dedupe(members), primitive: null };
  };

  const out = parseAlternation();
  return out.filter((s) => s.length > 0).slice(0, MAX_PER_PATTERN);
}

function dedupe(xs: readonly string[]): string[] {
  return Array.from(new Set(xs));
}

/**
 * Concrete inputs that the given pattern REALLY matches.
 *
 * The `pattern.test` filter is what keeps {@link expandSource}'s partiality
 * honest: an expansion the regex rejects never enters the corpus.
 */
export function patternExemplars(pattern: RegExp): string[] {
  const probe = new RegExp(pattern.source, pattern.flags.replace(/[gy]/g, ""));
  return expandSource(pattern.source)
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && probe.test(s));
}

/**
 * The literal head of a pattern — everything before its first placeholder.
 *
 * `^spawn\s+(?<count>\d+)…` yields `spawn`; `^focus[ -]mode$` yields both
 * `focus mode` and `focus-mode`. These are the heads that get crossed with
 * the generic argument tails, which is how the corpus reaches the shapes
 * nobody thought to write down (`/sort zones`, `/export all`, `/mute please
 * stop`).
 */
export function patternHeads(pattern: RegExp): string[] {
  const cut = pattern.source.search(/\(\?<|\\d|\\S|\\w|\.\+|\.\*/);
  const prefix = cut === -1 ? pattern.source : pattern.source.slice(0, cut);
  return expandSource(prefix)
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && /^[\w -]+$/.test(s));
}

// ── The cross ────────────────────────────────────────────────────────

/** Every head the registry offers, slashed and slashless. */
export function heads(actions: readonly CommandAction[]): string[] {
  const set = new Set<string>();
  for (const a of actions) {
    for (const slash of [a.slash, ...(a.aliases ?? [])]) {
      const body = slash.replace(/^\//, "");
      set.add(`/${body}`);
      set.add(body);
    }
    for (const p of a.patterns ?? []) {
      for (const h of patternHeads(p)) {
        set.add(`/${h}`);
        set.add(h);
      }
    }
  }
  return Array.from(set).sort();
}

/** Full inputs that are guaranteed to reach Tier 2, slashed and slashless. */
export function exemplars(actions: readonly CommandAction[]): string[] {
  const set = new Set<string>();
  for (const a of actions) {
    for (const p of a.patterns ?? []) {
      for (const ex of patternExemplars(p)) {
        set.add(ex);
        set.add(`/${ex}`);
      }
    }
  }
  return Array.from(set).sort();
}

/**
 * Argument tails. Chosen to hit the shapes the nine rounds actually broke on:
 * a count, a word that is not a count, an account label, a two-number pair,
 * a free-form prompt, and the trailing nouns that belong to a pattern
 * (`zones`, `all`, `workflow`, `plain`, `library`, `mode`, `everything`).
 */
export const TAILS_GOLDEN = ["", "3 best", "1 gmail fix the bug"];

export const TAILS_FAST = [
  "",
  "1",
  "4.9",
  "3 best",
  "zones",
  "please stop",
  "1 gmail fix the bug",
];

export const TAILS_FULL = [
  "",
  "1",
  "3",
  // Every tail here was an INTEGER for nine rounds, so the whole fractional
  // class — `/close 4.9`, `/swap 1 2.5` — was invisible to a 91,784-input
  // corpus. `coerceToken` accepts `-?\d+(\.\d+)?`, so a fraction reaches a
  // zone index as a `number` that indexes nothing.
  "4.9",
  "-1",
  "next",
  "prev",
  "best",
  "gmail",
  "zones",
  "all",
  "plain",
  "workflow",
  "library",
  "mode",
  "everything",
  "please stop",
  "2 5",
  "1 gmail",
  "3 best fix the bug",
  "1 gmail fix the bug",
];

/**
 * Declared-flag spellings. `--tenant` is the only declared flag in the
 * registry today; `--nope` stands for an UNDECLARED flag, which must survive
 * into a free-form field rather than being eaten.
 */
export const FLAGS_GOLDEN = ["", "--tenant"];

export const FLAGS_FAST = ["", "--tenant=2299", "--tenant"];

export const FLAGS_FULL = [
  "",
  "--tenant 2299",
  "--tenant=2299",
  "--tenant=",
  "--tenant",
  "--tenant acme",
  "--nope x",
];

/** Quoting shapes, applied to the TAIL (never appended on their own). */
export type QuoteShape = (tail: string) => string;

const wrapAll: QuoteShape = (t) => (t.length > 0 ? `"${t}"` : t);
const wrapLast: QuoteShape = (t) => {
  const parts = t.split(" ").filter(Boolean);
  if (parts.length === 0) return t;
  parts[parts.length - 1] = `"${parts[parts.length - 1]}"`;
  return parts.join(" ");
};
const emptyQuoted: QuoteShape = () => `""`;
const quoteFlagSpelling: QuoteShape = (t) => (t.length > 0 ? `${t} "--tenant"` : `"--tenant"`);

export const QUOTES_GOLDEN: readonly QuoteShape[] = [(t) => t, wrapAll];
export const QUOTES_FAST: readonly QuoteShape[] = [(t) => t, wrapAll];
export const QUOTES_FULL: readonly QuoteShape[] = [
  (t) => t,
  wrapAll,
  wrapLast,
  emptyQuoted,
  quoteFlagSpelling,
];

/**
 * Three tiers, sized on measured wall-clock (see `differential.test.ts`):
 *
 * | tier     | inputs | bind+handler | what it is for                      |
 * |----------|--------|--------------|-------------------------------------|
 * | `golden` |  ~2.1k |      ~130 ms | the committed characterization file |
 * | `fast`   |  ~7.3k |      ~400 ms | invariants, every build              |
 * | `full`   | ~92k   |      ~5.0 s  | `TERMINAL_CORPUS=full`, on demand   |
 *
 * `golden` is smaller than `fast` on purpose and NOT because of runtime —
 * 400 ms would be fine every build. It is sized for REVIEW: the golden file
 * is the deliverable whose diff a human reads, and the `fast` corpus's 7,338
 * rows is 534 KB of fixture. `golden` keeps every registry-derived head and
 * every pattern exemplar (so it still grows the day a pattern is added) and
 * spends the saving on the crossed tails, which is where rows repeat.
 */
export type CorpusTier = "golden" | "fast" | "full";

/**
 * Build the corpus for `tier`. Deterministic and sorted, so the golden
 * snapshot's diff is stable under re-generation.
 */
export function buildCorpus(actions: readonly CommandAction[], tier: CorpusTier): string[] {
  const tails = tier === "golden" ? TAILS_GOLDEN : tier === "fast" ? TAILS_FAST : TAILS_FULL;
  const flags = tier === "golden" ? FLAGS_GOLDEN : tier === "fast" ? FLAGS_FAST : FLAGS_FULL;
  const quotes = tier === "golden" ? QUOTES_GOLDEN : tier === "fast" ? QUOTES_FAST : QUOTES_FULL;
  const out = new Set<string>();

  for (const head of heads(actions)) {
    for (const tail of tails) {
      for (const shape of quotes) {
        const shaped = shape(tail);
        for (const flag of flags) {
          out.add([head, shaped, flag].filter((s) => s.length > 0).join(" "));
        }
      }
    }
  }
  // Pattern exemplars carry the shapes the generic tails cannot reach
  // (a pattern whose argument is deep in the phrase). Crossed with flags,
  // not appended to them.
  for (const ex of exemplars(actions)) {
    for (const flag of flags) {
      out.add([ex, flag].filter((s) => s.length > 0).join(" "));
    }
  }
  return Array.from(out).sort();
}

// ── Probe corpora: the two routes the TEXT corpus structurally cannot reach ──

/**
 * A hand-authored argument bag aimed at ONE action, with the key the snapshot
 * files it under.
 *
 * The text corpus above can only produce arguments a human could TYPE, which
 * is exactly why two routes went nine rounds unmeasured: Tier 3 hands over
 * whatever JSON the model emitted, and `callRegistry` hands over whatever the
 * calling component wrote in its source. Neither is reachable from a string.
 */
export interface ProbeCase {
  /**
   * Snapshot key. Starts with `«`, which no typed input can contain, so probe
   * rows can never collide with — or be mistaken for — a corpus input.
   */
  key: string;
  actionId: string;
  /** Which {@link ARG_BAGS} entry this is, for reading the golden diff. */
  bag: string;
  args: Record<string, unknown>;
  /** The raw text the operator typed. Empty for the direct route. */
  input: string;
}

/**
 * A plausible value per DECLARED argument name, so the `valid` bag actually
 * runs rather than erroring for an unrelated reason.
 *
 * Keyed on the registry's own schema names, and
 * `corpus.test.ts::"every declared argument name has a fill"` fails when an
 * action declares a name this table does not know — so a new argument shows up
 * as a red spec, not as a probe that silently degenerates into "gmail".
 */
export const ARG_FILL: Record<string, string | number> = {
  count: 1,
  zone: 1,
  tabId: "tab-1",
  a: 1,
  b: 2,
  target: "next",
  preset: "six-pack",
  state: "idle",
  type: "progress",
  tag: "gmail",
  goal: "fix the bug",
  account: "gmail",
  context: "fix the bug",
  command: "ls",
  action: "list",
  pattern: "yes",
  tenant: "acme",
};

/** Bare argument names an action declares — a `--flag` under its bare name. */
export function declaredArgNames(action: CommandAction): string[] {
  return Object.keys(action.paramSchema ?? {}).map((k) =>
    k.startsWith("--") ? k.slice(2) : k,
  );
}

/**
 * The bag shapes. `bool` / `object` / `array` are the three the brief names as
 * reaching handlers raw today; `alien` is a key no schema declares; `nullish`
 * is JSON's way of saying "absent", which must not be confused with
 * supplied-and-empty.
 */
export const ARG_BAGS: ReadonlyArray<{
  name: string;
  build(action: CommandAction): Record<string, unknown>;
}> = [
  { name: "empty", build: () => ({}) },
  {
    name: "valid",
    build: (a) =>
      Object.fromEntries(
        declaredArgNames(a).map((k) => [k, ARG_FILL[k] ?? "gmail"] as const),
      ),
  },
  { name: "bool", build: (a) => ({ [declaredArgNames(a)[0] ?? "count"]: true }) },
  { name: "alien", build: () => ({ nonsense: "x" }) },
  { name: "object", build: (a) => ({ [declaredArgNames(a)[0] ?? "zone"]: {} }) },
  { name: "array", build: (a) => ({ [declaredArgNames(a)[0] ?? "target"]: [] }) },
  { name: "nullish", build: (a) => ({ [declaredArgNames(a)[0] ?? "zone"]: null }) },
];

const BAGS_GOLDEN = ["empty", "valid", "bool", "alien"];
const BAGS_FAST = [...BAGS_GOLDEN, "object", "array"];

function bagsFor(tier: CorpusTier): typeof ARG_BAGS {
  if (tier === "full") return ARG_BAGS;
  const want = tier === "golden" ? BAGS_GOLDEN : BAGS_FAST;
  return ARG_BAGS.filter((b) => want.includes(b.name));
}

/**
 * Free text a Tier-3 probe rides on.
 *
 * Deliberately a phrase no slash form and no pattern claims, so `chooseTier`
 * takes its Tier-3 arm rather than one of the two that already have coverage.
 * The `--tenant` spelling is in the `full` tier because the AI route runs
 * `applyDeclaredFlags` over the RAW INPUT like every other route does, and
 * that interaction had no test at all.
 */
export const AI_INPUTS_SMALL = ["do the thing"];
export const AI_INPUTS_FULL = ["do the thing", "do the thing --tenant=2299"];

/** Tier-3 probes: the model named `action` and returned `bag`. */
export function buildAiProbes(
  actions: readonly CommandAction[],
  tier: CorpusTier,
): ProbeCase[] {
  const inputs = tier === "full" ? AI_INPUTS_FULL : AI_INPUTS_SMALL;
  const out: ProbeCase[] = [];
  for (const action of actions) {
    for (const bag of bagsFor(tier)) {
      for (const input of inputs) {
        out.push({
          key: `«ai» ${action.id} [${bag.name}] ${input}`,
          actionId: action.id,
          bag: bag.name,
          args: bag.build(action),
          input,
        });
      }
    }
  }
  return out.sort((x, y) => (x.key < y.key ? -1 : x.key > y.key ? 1 : 0));
}

/** Direct probes: `callRegistry(actionId, args)` — no CommandBar at all. */
export function buildDirectProbes(
  actions: readonly CommandAction[],
  tier: CorpusTier,
): ProbeCase[] {
  const out: ProbeCase[] = [];
  for (const action of actions) {
    for (const bag of bagsFor(tier)) {
      out.push({
        key: `«direct» ${action.id} [${bag.name}]`,
        actionId: action.id,
        bag: bag.name,
        args: bag.build(action),
        input: "",
      });
    }
  }
  return out.sort((x, y) => (x.key < y.key ? -1 : x.key > y.key ? 1 : 0));
}
