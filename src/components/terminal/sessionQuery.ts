/**
 * The Session Manager query language — one query string is the whole filter,
 * and chips are editors of it (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D5 / Phase 4).
 *
 * ## Grammar
 *
 * ```text
 * query    := or?
 * or       := and ("OR" and)*            // OR binds looser than AND
 * and      := unary+                     // juxtaposition is AND
 * unary    := "-" unary | atom           // negation
 * atom     := "(" or ")" | term
 * term     := word | "quoted phrase"     // over title, topic, preview + names
 *           | field ":" value            // value may be "quoted"
 *           | metric op value            // idle>30m  age<2h  msgs>=100
 * ```
 *
 * | Term | Matches |
 * |---|---|
 * | `word`, `"a phrase"` | case-insensitive substring of title / topic / preview / names / project / id |
 * | `title:x`, `name:x` | the headline name only |
 * | `text:"…"` | transcript full text (`transcript_search`) |
 * | `is:` | `needs-you` `working` `idle` `stalled` `finished` `closable` `exited` `error` `pinned` `muted` `unseen` `snoozed` |
 * | `where:` | `screen` `hidden` `outside` `device` `not-running` |
 * | `group:` `account:` `repo:` `branch:` `plan:` `unit:` `device:` `page:` | substring (`group:none` = ungrouped) |
 * | `color:` | a group color role, exactly (`color:none` = ungrouped) |
 * | `tag:` | one tag, exactly |
 * | `pr:<n>` | authored PR number `n` |
 * | `idle` `age` (duration `s m h d w`), `msgs` (count) | with `>` `<` `>=` `<=` `=` |
 * | `started:` | `today` `yesterday` `YYYY-MM-DD` (local day) |
 *
 * Every field and value is validated: an unknown field, an unknown `is:` /
 * `where:` value, a malformed number or an unbalanced quote or parenthesis is
 * a {@link QueryError} with its span, and the query does not evaluate at all —
 * a term is never silently dropped.
 *
 * ## Three-valued evaluation
 *
 * A predicate over an UNKNOWN fact (`SessionFacts`, `sessionFacts.ts`) is
 * UNKNOWN — neither a match nor an exclusion. AND is false if any operand is
 * false, else unknown if any is; OR is true if any operand is true, else
 * unknown if any is; NOT keeps unknown unknown. {@link filterSessions}
 * returns `matched` and `unknown` partitions so the panel shows an
 * "Unknown for this filter (N)" tail with each row's reason — a filter never
 * hides a session it could not evaluate (served policy
 * `verification-and-evidence` `silent-empty-is-unknown`).
 *
 * ## `is:closable`
 *
 * Served policy `finished-criteria` `session-finished-at-green-not-at-merged`
 * with the plan's vet correction: a session is closable when its work-axis
 * finished marker is set, OR its process exited 0 and every PR it authored is
 * green (landed / closed count; the CI comes from coord's PR twin). Any PR
 * whose `rebase_block_disposition` is `author_acts` is owed work → not
 * closable. A PR with no CI source, or a red one whose attribution is not
 * established, makes the arm UNKNOWN.
 *
 * Pure: no React, no Tauri.
 */

import type { SessionActivity, SessionLocation } from "./sessionStatusModel";
import { SESSION_GROUP_COLOR_ROLES } from "./sessionOrgTypes";
import type { Fact, PrFact, SessionFacts } from "./sessionFacts";

// ── Vocabulary ───────────────────────────────────────────────────────────────

export const IS_VALUES = [
  "needs-you",
  "working",
  "idle",
  "stalled",
  "finished",
  "closable",
  "exited",
  "error",
  "pinned",
  "muted",
  "unseen",
  "snoozed",
] as const;
export type IsValue = (typeof IS_VALUES)[number];

export const WHERE_VALUES = ["screen", "hidden", "outside", "device", "not-running"] as const;
export type WhereValue = (typeof WHERE_VALUES)[number];

/** `where:` value → the location it names. */
export const WHERE_LOCATION: Readonly<Record<WhereValue, SessionLocation>> = {
  screen: "on-screen",
  hidden: "hidden-here",
  outside: "outside-runner",
  device: "other-device",
  "not-running": "not-running",
};

/** Substring-matched fields. */
export const CONTAINS_FIELDS = [
  "group",
  "account",
  "repo",
  "branch",
  "plan",
  "unit",
  "device",
  "page",
] as const;
export type ContainsField = (typeof CONTAINS_FIELDS)[number];

export const METRIC_FIELDS = ["idle", "age", "msgs"] as const;
export type MetricField = (typeof METRIC_FIELDS)[number];

export type CompareOp = ">" | "<" | ">=" | "<=" | "=";

/** Every `field:` the grammar accepts. */
export const QUERY_FIELDS = [
  "title",
  "name",
  "text",
  "is",
  "where",
  "color",
  "tag",
  "pr",
  "started",
  ...CONTAINS_FIELDS,
] as const;

// ── Tokens ───────────────────────────────────────────────────────────────────

export interface Span {
  start: number;
  end: number;
}

export type QueryToken = Span &
  (
    | { kind: "word"; value: string }
    | { kind: "phrase"; value: string; closed: boolean }
    | {
        kind: "field";
        name: string;
        value: string;
        /** The value was "quoted". */
        quoted: boolean;
        closed: boolean;
        /** Where the value starts (after `name:`). */
        valueStart: number;
      }
    | { kind: "compare"; name: string; op: CompareOp; value: string; valueStart: number }
    | { kind: "or" }
    | { kind: "negate" }
    | { kind: "lparen" }
    | { kind: "rparen" }
  );

const NAME_RE = /^[a-z]+$/i;

function isSpace(c: string): boolean {
  return /\s/.test(c);
}

/** Read a `"…"` starting at `i` (the opening quote). */
function readQuoted(src: string, i: number): { value: string; end: number; closed: boolean } {
  const close = src.indexOf('"', i + 1);
  if (close < 0) return { value: src.slice(i + 1), end: src.length, closed: false };
  return { value: src.slice(i + 1, close), end: close + 1, closed: true };
}

/** Split a query into tokens with source spans. Total: never throws. */
export function tokenizeQuery(src: string): QueryToken[] {
  const out: QueryToken[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (isSpace(c)) {
      i++;
      continue;
    }
    if (c === "(") {
      out.push({ kind: "lparen", start: i, end: i + 1 });
      i++;
      continue;
    }
    if (c === ")") {
      out.push({ kind: "rparen", start: i, end: i + 1 });
      i++;
      continue;
    }
    if (c === "-" && i + 1 < src.length && !isSpace(src[i + 1]) && src[i + 1] !== ")") {
      out.push({ kind: "negate", start: i, end: i + 1 });
      i++;
      continue;
    }
    if (c === '"') {
      const q = readQuoted(src, i);
      out.push({ kind: "phrase", value: q.value, closed: q.closed, start: i, end: q.end });
      i = q.end;
      continue;
    }
    // A bare chunk: up to whitespace, a paren, or a quote that does not
    // follow a `field:`.
    let j = i;
    while (j < src.length && !isSpace(src[j]) && src[j] !== "(" && src[j] !== ")" && src[j] !== '"')
      j++;
    const chunk = src.slice(i, j);
    const colon = chunk.indexOf(":");
    const cmp = /^([a-z]+)(>=|<=|>|<|=)(.*)$/i.exec(chunk);
    // `name:` and `name>` cannot both match: the name's letters end at one or
    // the other.
    if (colon > 0 && NAME_RE.test(chunk.slice(0, colon))) {
      const name = chunk.slice(0, colon).toLowerCase();
      const valueStart = i + colon + 1;
      if (j === valueStart && src[j] === '"') {
        const q = readQuoted(src, j);
        out.push({
          kind: "field",
          name,
          value: q.value,
          quoted: true,
          closed: q.closed,
          valueStart,
          start: i,
          end: q.end,
        });
        i = q.end;
        continue;
      }
      out.push({
        kind: "field",
        name,
        value: chunk.slice(colon + 1),
        quoted: false,
        closed: true,
        valueStart,
        start: i,
        end: j,
      });
      i = j;
      continue;
    }
    if (cmp) {
      out.push({
        kind: "compare",
        name: cmp[1].toLowerCase(),
        op: cmp[2] as CompareOp,
        value: cmp[3],
        valueStart: i + cmp[1].length + cmp[2].length,
        start: i,
        end: j,
      });
      i = j;
      continue;
    }
    if (chunk === "OR") out.push({ kind: "or", start: i, end: j });
    else out.push({ kind: "word", value: chunk, start: i, end: j });
    i = j;
  }
  return out;
}

// ── AST ──────────────────────────────────────────────────────────────────────

export type QueryNode = Span &
  (
    | { kind: "and"; children: QueryNode[] }
    | { kind: "or"; children: QueryNode[] }
    | { kind: "not"; child: QueryNode }
    /** A bare word or phrase over the free-text fields. */
    | { kind: "text"; value: string }
    | { kind: "title"; value: string }
    /** `text:` — transcript full text; `query` is what `transcript_search` takes. */
    | { kind: "transcript"; query: string }
    | { kind: "is"; value: IsValue }
    | { kind: "where"; value: WhereValue }
    | { kind: "contains"; field: ContainsField; value: string }
    | { kind: "color"; value: string }
    | { kind: "tag"; value: string }
    | { kind: "pr"; number: number }
    /** `value` is ms for `idle`/`age`, a count for `msgs`. */
    | { kind: "compare"; field: MetricField; op: CompareOp; value: number }
    | { kind: "started"; day: "today" | "yesterday" | { y: number; m: number; d: number } }
  );

export type QueryErrorCode =
  | "unknown-field"
  | "unknown-value"
  | "missing-value"
  | "invalid-number"
  | "invalid-duration"
  | "invalid-date"
  | "unterminated-quote"
  | "unbalanced-paren"
  | "dangling-or"
  | "dangling-negation";

export interface QueryError extends Span {
  code: QueryErrorCode;
  message: string;
}

export type ParsedQuery = { ok: true; ast: QueryNode | null } | { ok: false; errors: QueryError[] };

const DURATION_UNITS: Readonly<Record<string, number>> = {
  s: 1_000,
  m: 60_000,
  h: 3_600_000,
  d: 86_400_000,
  w: 604_800_000,
};

/** `30m` → ms; `null` when malformed. */
export function parseDuration(raw: string): number | null {
  const m = /^(\d+(?:\.\d+)?)([smhdw])$/i.exec(raw.trim());
  if (!m) return null;
  return Math.round(parseFloat(m[1]) * DURATION_UNITS[m[2].toLowerCase()]);
}

function err(code: QueryErrorCode, message: string, span: Span): QueryError {
  return { code, message, start: span.start, end: span.end };
}

/** Turn one term token into a node, or an error. */
function termNode(tok: QueryToken): QueryNode | QueryError {
  const span = { start: tok.start, end: tok.end };
  switch (tok.kind) {
    case "word":
      return { kind: "text", value: tok.value, ...span };
    case "phrase":
      if (!tok.closed) return err("unterminated-quote", "Missing closing quote", span);
      return { kind: "text", value: tok.value, ...span };
    case "compare": {
      const field = tok.name as MetricField;
      if (!(METRIC_FIELDS as readonly string[]).includes(field)) {
        return err(
          "unknown-field",
          `Unknown comparison "${tok.name}" — use idle, age or msgs`,
          span,
        );
      }
      if (tok.value === "")
        return err("missing-value", `"${tok.name}${tok.op}" needs a value`, span);
      if (field === "msgs") {
        if (!/^\d+$/.test(tok.value))
          return err("invalid-number", `"${tok.value}" is not a message count`, span);
        return { kind: "compare", field, op: tok.op, value: Number(tok.value), ...span };
      }
      const ms = parseDuration(tok.value);
      if (ms === null)
        return err("invalid-duration", `"${tok.value}" is not a duration like 30m or 2h`, span);
      return { kind: "compare", field, op: tok.op, value: ms, ...span };
    }
    case "field": {
      if (!tok.closed) return err("unterminated-quote", "Missing closing quote", span);
      const value = tok.value.trim();
      const name = tok.name;
      if (!(QUERY_FIELDS as readonly string[]).includes(name)) {
        return err(
          "unknown-field",
          `Unknown field "${name}:" — fields are ${QUERY_FIELDS.join(", ")}`,
          span,
        );
      }
      if (value === "") return err("missing-value", `"${name}:" needs a value`, span);
      const lower = value.toLowerCase();
      switch (name) {
        case "title":
        case "name":
          return { kind: "title", value, ...span };
        case "text":
          // A quoted value stays a phrase for transcript_search.
          return { kind: "transcript", query: tok.quoted ? `"${value}"` : value, ...span };
        case "is":
          if (!(IS_VALUES as readonly string[]).includes(lower))
            return err(
              "unknown-value",
              `Unknown "is:${value}" — use ${IS_VALUES.join(", ")}`,
              span,
            );
          return { kind: "is", value: lower as IsValue, ...span };
        case "where":
          if (!(WHERE_VALUES as readonly string[]).includes(lower))
            return err(
              "unknown-value",
              `Unknown "where:${value}" — use ${WHERE_VALUES.join(", ")}`,
              span,
            );
          return { kind: "where", value: lower as WhereValue, ...span };
        case "color":
          if (lower !== "none" && !(SESSION_GROUP_COLOR_ROLES as readonly string[]).includes(lower))
            return err(
              "unknown-value",
              `Unknown "color:${value}" — use ${SESSION_GROUP_COLOR_ROLES.join(", ")}, none`,
              span,
            );
          return { kind: "color", value: lower, ...span };
        case "tag":
          return { kind: "tag", value, ...span };
        case "pr": {
          const n = /^#?(\d+)$/.exec(value);
          if (!n) return err("invalid-number", `"pr:${value}" needs a PR number`, span);
          return { kind: "pr", number: Number(n[1]), ...span };
        }
        case "started": {
          if (lower === "today" || lower === "yesterday")
            return { kind: "started", day: lower, ...span };
          const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
          if (!d)
            return err(
              "invalid-date",
              `"started:${value}" — use today, yesterday or YYYY-MM-DD`,
              span,
            );
          return {
            kind: "started",
            day: { y: Number(d[1]), m: Number(d[2]), d: Number(d[3]) },
            ...span,
          };
        }
        default:
          return { kind: "contains", field: name as ContainsField, value, ...span };
      }
    }
    default:
      return err("unknown-value", "Unexpected token", span);
  }
}

function isError(x: QueryNode | QueryError): x is QueryError {
  return "code" in x;
}

/** Parse a query. The empty (or all-whitespace) query is `{ ok, ast: null }` = all. */
export function parseQuery(src: string): ParsedQuery {
  const tokens = tokenizeQuery(src);
  const errors: QueryError[] = [];
  let pos = 0;

  const spanOf = (nodes: QueryNode[]): Span => ({
    start: nodes[0].start,
    end: nodes[nodes.length - 1].end,
  });

  function parseOr(): QueryNode | null {
    const branches: QueryNode[] = [];
    let first = parseAnd();
    if (first) branches.push(first);
    while (pos < tokens.length && tokens[pos].kind === "or") {
      const orTok = tokens[pos++];
      const next = parseAnd();
      if (!first || !next) errors.push(err("dangling-or", "OR needs a term on both sides", orTok));
      if (next) branches.push(next);
      first = next;
    }
    if (branches.length === 0) return null;
    if (branches.length === 1) return branches[0];
    return { kind: "or", children: branches, ...spanOf(branches) };
  }

  function parseAnd(): QueryNode | null {
    const terms: QueryNode[] = [];
    while (pos < tokens.length && tokens[pos].kind !== "or" && tokens[pos].kind !== "rparen") {
      const t = parseUnary();
      if (t) terms.push(t);
    }
    if (terms.length === 0) return null;
    if (terms.length === 1) return terms[0];
    return { kind: "and", children: terms, ...spanOf(terms) };
  }

  function parseUnary(): QueryNode | null {
    const tok = tokens[pos];
    if (tok.kind === "negate") {
      pos++;
      if (pos >= tokens.length || tokens[pos].kind === "or" || tokens[pos].kind === "rparen") {
        errors.push(err("dangling-negation", "“-” needs a term after it", tok));
        return null;
      }
      const child = parseUnary();
      return child ? { kind: "not", child, start: tok.start, end: child.end } : null;
    }
    if (tok.kind === "lparen") {
      pos++;
      const inner = parseOr();
      if (pos < tokens.length && tokens[pos].kind === "rparen") pos++;
      else errors.push(err("unbalanced-paren", "Missing “)”", tok));
      return inner;
    }
    pos++;
    const node = termNode(tok);
    if (isError(node)) {
      errors.push(node);
      return null;
    }
    return node;
  }

  let ast: QueryNode | null = null;
  const parts: QueryNode[] = [];
  while (pos < tokens.length) {
    const part = parseOr();
    if (part) parts.push(part);
    if (pos < tokens.length && tokens[pos].kind === "rparen") {
      errors.push(err("unbalanced-paren", "Unmatched “)”", tokens[pos]));
      pos++;
    }
  }
  if (parts.length === 1) ast = parts[0];
  else if (parts.length > 1) ast = { kind: "and", children: parts, ...spanOf(parts) };

  if (errors.length > 0) {
    errors.sort((a, b) => a.start - b.start);
    return { ok: false, errors };
  }
  return { ok: true, ast };
}

// ── Evaluation ───────────────────────────────────────────────────────────────

/** Three-valued truth. */
export type Truth = true | false | "unknown";

export interface Verdict {
  value: Truth;
  /** Why it is unknown — the first unknown predicate's reason. */
  reason: string | null;
}

const T: Verdict = { value: true, reason: null };
const F: Verdict = { value: false, reason: null };
const U = (reason: string): Verdict => ({ value: "unknown", reason });
const bool = (b: boolean): Verdict => (b ? T : F);

/** One `text:` query's transcript search, as the evaluator reads it. */
export type TextSearchResult =
  | { status: "pending" }
  | { status: "failed"; reason: string }
  | {
      status: "done";
      /** Claude session ids whose transcript matched. */
      matched: ReadonlySet<string>;
      /** Ids in scope that were NOT searched (time budget, unreadable). */
      notScanned: ReadonlySet<string>;
      /**
       * Named ids the runner found no transcript for. "Not found" may mean
       * "could not read", so a missing id is unknown — never "no match".
       */
      missing: ReadonlySet<string>;
      /** More matched than the limit returned — a non-hit is then unknown. */
      limitHit: boolean;
      /**
       * The scan did not cover its whole scope in a way `notScanned` names:
       * it never ran, counted fewer files than it had without saying which,
       * or was unscoped and stopped early — a non-hit is unknown.
       */
      incomplete: boolean;
    };

export interface QueryContext {
  /** Epoch ms "now" for idle / age / snooze / started. */
  now: number;
  /** `text:` results keyed by {@link QueryNode} `transcript.query`. */
  textSearch?: ReadonlyMap<string, TextSearchResult>;
}

function contains(hay: string, needle: string): boolean {
  return hay.toLowerCase().includes(needle.toLowerCase());
}

function onFact<T>(fact: Fact<T>, test: (v: T) => boolean): Verdict {
  return fact.known ? bool(test(fact.value)) : U(fact.reason);
}

function compare(a: number, op: CompareOp, b: number): boolean {
  switch (op) {
    case ">":
      return a > b;
    case "<":
      return a < b;
    case ">=":
      return a >= b;
    case "<=":
      return a <= b;
    case "=":
      return a === b;
  }
}

function localDayStart(
  now: number,
  day: "today" | "yesterday" | { y: number; m: number; d: number },
): number {
  if (typeof day === "object") return new Date(day.y, day.m - 1, day.d).getTime();
  const d = new Date(now);
  const start = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  return day === "today"
    ? start
    : new Date(d.getFullYear(), d.getMonth(), d.getDate() - 1).getTime();
}

function activityIs(f: SessionFacts, values: readonly SessionActivity[]): Verdict {
  return onFact(f.activity, (a) => values.includes(a));
}

/** Every authored PR green-or-done, three-valued; `author_acts` → false. */
function prsGreen(prs: readonly PrFact[]): Verdict {
  let unknownReason: string | null = null;
  for (const pr of prs) {
    if (pr.authorActs === true) return F;
    if (pr.ci === "pending") return F;
    if (pr.ci === "unknown" || pr.ci === "red") {
      unknownReason ??=
        pr.ci === "red"
          ? `PR #${pr.number} red — code-error attribution not established`
          : `PR #${pr.number} CI unknown${pr.reason ? `: ${pr.reason}` : ""}`;
    }
  }
  return unknownReason ? U(unknownReason) : T;
}

/** `is:closable` — see the module docs. */
export function evaluateClosable(f: SessionFacts): Verdict {
  // Owed work (a conflict only the author can resolve) is never closable,
  // whatever the finished marker says.
  if (f.prs.known && f.prs.value.some((p) => p.authorActs === true)) return F;
  if (f.finished.known && f.finished.value) return T;

  let exitedZero: Verdict;
  if (!f.activity.known) exitedZero = U(f.activity.reason);
  else if (f.activity.value === "exited")
    exitedZero = f.exitCode === 0 ? T : f.exitCode === null ? U("exit code unknown") : F;
  else exitedZero = F;

  if (exitedZero.value === false) return f.finished.known ? F : U(f.finished.reason);
  const green = f.prs.known ? prsGreen(f.prs.value) : U(f.prs.reason);
  if (exitedZero.value === true) return green;
  // Exit unknown: closable only if both could hold, false if either cannot.
  if (green.value === false) return f.finished.known ? F : U(f.finished.reason);
  return exitedZero;
}

function evaluateIs(f: SessionFacts, value: IsValue, ctx: QueryContext): Verdict {
  switch (value) {
    case "needs-you":
      return activityIs(f, ["needs-you"]);
    case "working":
      return activityIs(f, ["working"]);
    case "idle":
      return activityIs(f, ["idle-at-prompt"]);
    case "stalled":
      return activityIs(f, ["no-output-mid-tool"]);
    case "exited":
      return activityIs(f, ["exited", "exited-error"]);
    case "error":
      return activityIs(f, ["exited-error"]);
    case "finished":
      return onFact(f.finished, (b) => b);
    case "closable":
      return evaluateClosable(f);
    case "pinned":
      return onFact(f.pinned, (b) => b);
    case "muted":
      return onFact(f.muted, (b) => b);
    case "snoozed":
      return onFact(f.snoozedUntil, (u) => u !== null && u > ctx.now);
    case "unseen":
      // The card's unseen dot, verbatim — one verdict for the dot and the filter.
      return onFact(f.unseen, (b) => b);
  }
}

function evaluateTranscript(f: SessionFacts, query: string, ctx: QueryContext): Verdict {
  if (!f.claudeSessionId) return U("no transcript to search");
  const r = ctx.textSearch?.get(query);
  if (!r || r.status === "pending") return U("transcript search not answered yet");
  if (r.status === "failed") return U(`transcript search failed: ${r.reason}`);
  if (r.matched.has(f.claudeSessionId)) return T;
  if (r.notScanned.has(f.claudeSessionId)) return U("transcript not searched (scan cut short)");
  if (r.missing.has(f.claudeSessionId)) return U("no transcript found on this runner");
  if (r.limitHit) return U("transcript search hit its result limit");
  if (r.incomplete) return U("transcript search did not cover every transcript");
  return F;
}

function freeText(f: SessionFacts): string[] {
  return [f.title, f.topic, f.preview, ...f.extraText];
}

/** Evaluate one node over one session. */
export function evaluateNode(node: QueryNode, f: SessionFacts, ctx: QueryContext): Verdict {
  switch (node.kind) {
    case "and": {
      let unknown: Verdict | null = null;
      for (const c of node.children) {
        const v = evaluateNode(c, f, ctx);
        if (v.value === false) return F;
        if (v.value === "unknown") unknown ??= v;
      }
      return unknown ?? T;
    }
    case "or": {
      let unknown: Verdict | null = null;
      for (const c of node.children) {
        const v = evaluateNode(c, f, ctx);
        if (v.value === true) return T;
        if (v.value === "unknown") unknown ??= v;
      }
      return unknown ?? F;
    }
    case "not": {
      const v = evaluateNode(node.child, f, ctx);
      return v.value === "unknown" ? v : bool(!v.value);
    }
    case "text":
      return bool(freeText(f).some((s) => contains(s, node.value)));
    case "title":
      return bool(contains(f.title, node.value));
    case "transcript":
      return evaluateTranscript(f, node.query, ctx);
    case "is":
      return evaluateIs(f, node.value, ctx);
    case "where":
      return onFact(f.location, (l) => l === WHERE_LOCATION[node.value]);
    case "color":
      return onFact(f.group, (g) =>
        node.value === "none" ? g === null : g !== null && g.colorRole === node.value,
      );
    case "tag":
      return onFact(f.tags, (tags) =>
        tags.some((t) => t.toLowerCase() === node.value.toLowerCase()),
      );
    case "pr":
      return onFact(f.prs, (prs) => prs.some((p) => p.number === node.number));
    case "contains": {
      const v = node.value;
      switch (node.field) {
        case "group":
          return onFact(f.group, (g) =>
            v.toLowerCase() === "none"
              ? g === null
              : g !== null && (contains(g.title, v) || g.id === v),
          );
        case "account":
          return onFact(f.account, (a) => a !== null && contains(a, v));
        case "repo":
          return onFact(f.repo, (r) => r !== null && contains(r, v));
        case "branch":
          return onFact(f.branch, (b) => b !== null && contains(b, v));
        case "plan":
        case "unit":
          return onFact(f.workUnit, (w) => w !== null && contains(w, v));
        case "device":
          return onFact(f.device, (d) => d !== null && contains(d, v));
        case "page":
          return onFact(f.page, (p) => p !== null && (contains(p.name, v) || p.id === v));
      }
      break;
    }
    case "compare": {
      switch (node.field) {
        case "idle":
          return onFact(f.lastActivityAt, (t) => compare(ctx.now - t, node.op, node.value));
        case "age":
          return onFact(f.startedAt, (t) => compare(ctx.now - t, node.op, node.value));
        case "msgs":
          return onFact(f.messageCount, (n) => compare(n, node.op, node.value));
      }
      break;
    }
    case "started": {
      const start = localDayStart(ctx.now, node.day);
      const end = new Date(start).setDate(new Date(start).getDate() + 1);
      return onFact(f.startedAt, (t) => t >= start && t < end);
    }
  }
  return U("unhandled term");
}

/** A row the filter could not evaluate, with why. */
export interface UnknownRow<F extends SessionFacts = SessionFacts> {
  facts: F;
  reason: string;
}

export type FilterResult<F extends SessionFacts = SessionFacts> =
  | { ok: true; matched: F[]; unknown: UnknownRow<F>[]; excluded: number }
  | { ok: false; errors: QueryError[] };

/**
 * Filter `rows` by `query`. A parse error returns `{ ok: false, errors }` and
 * evaluates nothing; the empty query matches every row.
 */
export function filterSessions<F extends SessionFacts>(
  query: string | ParsedQuery,
  rows: readonly F[],
  ctx: QueryContext,
): FilterResult<F> {
  const parsed = typeof query === "string" ? parseQuery(query) : query;
  if (!parsed.ok) return { ok: false, errors: parsed.errors };
  if (parsed.ast === null) return { ok: true, matched: [...rows], unknown: [], excluded: 0 };
  const matched: F[] = [];
  const unknown: UnknownRow<F>[] = [];
  let excluded = 0;
  for (const f of rows) {
    const v = evaluateNode(parsed.ast, f, ctx);
    if (v.value === true) matched.push(f);
    else if (v.value === "unknown") unknown.push({ facts: f, reason: v.reason ?? "unknown" });
    else excluded++;
  }
  return { ok: true, matched, unknown, excluded };
}

/** Every distinct `text:` query in an AST — what to send `transcript_search`. */
export function collectTextQueries(ast: QueryNode | null): string[] {
  const out = new Set<string>();
  const walk = (n: QueryNode): void => {
    if (n.kind === "transcript") out.add(n.query);
    else if (n.kind === "and" || n.kind === "or") n.children.forEach(walk);
    else if (n.kind === "not") walk(n.child);
  };
  if (ast) walk(ast);
  return [...out];
}

// ── Chips: editors of the query ──────────────────────────────────────────────

/** Source text of a token. */
function tokenText(src: string, t: Span): string {
  return src.slice(t.start, t.end);
}

/** Does a top-level (unparenthesized) `OR` join the query? */
function hasTopLevelOr(tokens: readonly QueryToken[]): boolean {
  let depth = 0;
  for (const t of tokens) {
    if (t.kind === "lparen") depth++;
    else if (t.kind === "rparen") depth = Math.max(0, depth - 1);
    else if (t.kind === "or" && depth === 0) return true;
  }
  return false;
}

/**
 * The first token standing as a positive top-level conjunct — outside any
 * parenthesis and not negated — for which `test` holds. `null` whenever a
 * top-level `OR` joins the query: no term there is a conjunct.
 */
function findTopLevel(
  query: string,
  test: (t: QueryToken, text: string) => boolean,
): QueryToken | null {
  const tokens = tokenizeQuery(query);
  if (hasTopLevelOr(tokens)) return null;
  let depth = 0;
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (t.kind === "lparen") depth++;
    else if (t.kind === "rparen") depth = Math.max(0, depth - 1);
    else if (depth === 0 && test(t, tokenText(query, t))) {
      const negated = i > 0 && tokens[i - 1].kind === "negate" && tokens[i - 1].end === t.start;
      if (!negated) return t;
    }
  }
  return null;
}

/**
 * Where `token` stands as a positive top-level conjunct of `query`, or
 * `null`. Case-insensitive.
 */
export function findToken(query: string, token: string): Span | null {
  const want = token.trim().toLowerCase();
  const t = findTopLevel(query, (_, text) => text.toLowerCase() === want);
  return t ? { start: t.start, end: t.end } : null;
}

/** Is the chip for `token` lit? */
export function hasToken(query: string, token: string): boolean {
  return findToken(query, token) !== null;
}

/** Remove the span and the whitespace it leaves behind. */
function removeSpan(query: string, s: Span): string {
  const before = query.slice(0, s.start).replace(/\s+$/, "");
  const after = query.slice(s.end).replace(/^\s+/, "");
  return before && after ? `${before} ${after}` : before + after;
}

/** Quote a value when the grammar needs it. */
export function quoteValue(value: string): string {
  return /[\s()"]/.test(value) || value === "" ? `"${value.replace(/"/g, "")}"` : value;
}

/** `field:value`, quoted when needed. */
export function fieldToken(field: string, value: string): string {
  return `${field}:${quoteValue(value)}`;
}

/**
 * The chip click: remove `token` when it stands as a top-level conjunct, else
 * append it (AND). The result is the new query string.
 */
export function toggleToken(query: string, token: string): string {
  const at = findToken(query, token);
  if (at) return removeSpan(query, at);
  const base = query.trim();
  if (base === "") return token;
  // Appending beside a top-level OR would change its meaning: group it.
  return hasTopLevelOr(tokenizeQuery(base)) ? `(${base}) ${token}` : `${base} ${token}`;
}

/**
 * The value of the first positive top-level `field:` term, or `null` — what a
 * picker shows as selected. A query joined by a top-level `OR` has none.
 */
export function topLevelFieldValue(query: string, field: string): string | null {
  const name = field.toLowerCase();
  const tok = findTopLevel(query, (t) => t.kind === "field" && t.name === name);
  return tok && tok.kind === "field" ? tok.value : null;
}

/**
 * Set the single value of `field:` — the account / group pickers. Removes
 * every top-level positive `field:` term, then appends `field:value` (or
 * nothing when `value` is null). A query joined by a top-level `OR` has no
 * such conjunct, so the value is ANDed onto the grouped query.
 */
export function setFieldValue(query: string, field: string, value: string | null): string {
  const name = field.toLowerCase();
  let q = query;
  for (;;) {
    const tok = findTopLevel(q, (t) => t.kind === "field" && t.name === name);
    if (!tok) break;
    q = removeSpan(q, tok);
  }
  return value === null ? q.trim() : toggleToken(q, fieldToken(field, value));
}

// ── Highlighting ─────────────────────────────────────────────────────────────

export type HighlightKind =
  | "space"
  | "word"
  | "phrase"
  | "field-name"
  | "field-value"
  | "operator"
  | "negate"
  | "paren"
  | "error";

export interface HighlightSegment extends Span {
  text: string;
  kind: HighlightKind;
}

/**
 * Segments covering `query` end to end for inline highlighting. A token any
 * parse error touches is `error` as a whole (its message is in `parseQuery`).
 */
export function highlightQuery(query: string): HighlightSegment[] {
  const tokens = tokenizeQuery(query);
  const parsed = parseQuery(query);
  const errors = parsed.ok ? [] : parsed.errors;
  const inError = (s: Span) => errors.some((e) => e.start < s.end && e.end > s.start);
  const out: HighlightSegment[] = [];
  let at = 0;
  const push = (start: number, end: number, kind: HighlightKind) => {
    if (end > start) out.push({ start, end, kind, text: query.slice(start, end) });
  };
  for (const t of tokens) {
    push(at, t.start, "space");
    if (inError(t)) push(t.start, t.end, "error");
    else if (t.kind === "field" || t.kind === "compare") {
      push(t.start, t.valueStart, "field-name");
      push(t.valueStart, t.end, "field-value");
    } else {
      const kind: HighlightKind =
        t.kind === "or"
          ? "operator"
          : t.kind === "negate"
            ? "negate"
            : t.kind === "lparen" || t.kind === "rparen"
              ? "paren"
              : t.kind === "phrase"
                ? "phrase"
                : "word";
      push(t.start, t.end, kind);
    }
    at = t.end;
  }
  push(at, query.length, "space");
  return out;
}
