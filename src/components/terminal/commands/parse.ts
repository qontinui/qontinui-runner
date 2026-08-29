/**
 * Slash-command arg parser. Tokenizes the input after the slash and maps
 * tokens positionally to the action's `paramSchema` field order.
 *
 * Why positional and not named? Operators don't want to type
 * `/spawn-ai count=3 account=best` — they want `/spawn-ai 3 best`.
 * Named args are valuable for the Tier-3 AI tier (Phase 8) where the
 * model emits a structured tool-call, but not at the keyboard. Phase 2's
 * positional shape covers every action in `./useTerminalCommands.ts`
 * since their `paramSchema` field orders match the natural English
 * phrasing (`count` first, `account` second, `context` last).
 *
 * Numeric coercion: bare integers/decimals become `number`; everything
 * else stays a `string`. The handlers in `./useTerminalCommands.ts`
 * already do `typeof v === "number"` checks, so the coercion lines up
 * with what they expect.
 *
 * Quoted strings: `"hello world"` is one token. Used so the `context`
 * field of `/spawn-ai 3 best "fix the failing test"` is a single arg.
 * Backslash escapes aren't supported in Phase 2 — operators wanting
 * a literal `"` in their context can use the palette or wait for the
 * AI tier.
 *
 * Named flags: a `paramSchema` key written as `"--name"` opts that field
 * OUT of positional binding; it is filled only by `--name value` or
 * `--name=value` anywhere on the line (see `FLAG_PREFIX`). Added for
 * `/spawn-ai --tenant <slug|uuid>`, whose optionality can't be expressed
 * positionally alongside a free-form trailing prompt. A flag spelling
 * inside a QUOTED run is prompt text, never syntax — see {@link Token}.
 */

import type { CommandAction } from "./types";

/**
 * Marker that makes a `paramSchema` key a NAMED FLAG rather than a positional
 * field. A key declared as `"--tenant"` is never positionally bound; it is
 * only filled by `--tenant <value>` / `--tenant=<value>` on the input line,
 * and lands in the parsed args under its bare name (`tenant`).
 *
 * Why: `/spawn-ai`'s last positional field is a free-form prompt, and the
 * catch-all rule below joins every excess token into it. A positional
 * optional field placed before it would shift on omission, and one placed
 * after it would be swallowed by the tail. Flags sidestep both — they can
 * appear anywhere on the line and are removed before positional binding.
 */
export const FLAG_PREFIX = "--";

/**
 * One token, WITH the fact that decided whether it is text or syntax.
 *
 * `quoted` is the whole reason this type exists. {@link tokenize} strips
 * the quote characters, and a stripped token is indistinguishable from one
 * the operator never quoted — which is how `/spawn-ai 1 gmail "--tenant"`
 * came to read its PROMPT as a declared flag (supplied-but-empty tenant,
 * zero terminals), and how a quoted `--tenant` inside a longer prompt came
 * to eat the word after it. Quoting is not decoration on this surface; it
 * is the operator saying "this run is text", and every consumer that
 * decides text-vs-syntax needs to hear it.
 */
export interface Token {
  /** The token's text, with the surrounding quote characters removed. */
  text: string;
  /**
   * True when the token BEGAN inside a double-quoted run — i.e. the
   * operator opened a quote before any of its characters.
   *
   * Deliberately "began", not "contains": `--tenant="my org"` is a FLAG
   * whose value happens to be quoted, and reading it as prompt text would
   * break the one spelling that always worked.
   */
  quoted: boolean;
}

/**
 * Split a string into whitespace-separated tokens, treating double-quoted
 * runs as a single token, and REPORT which tokens were quoted.
 *
 * {@link tokenize} is the text-only projection of this; use that where the
 * quoting genuinely does not matter, and this everywhere a token's meaning
 * depends on whether the operator quoted it.
 */
export function tokenizeRich(input: string): Token[] {
  const tokens: Token[] = [];
  let current = "";
  let quoted = false;
  let inQuote = false;
  const flush = (): void => {
    if (current.length > 0) tokens.push({ text: current, quoted });
    current = "";
    quoted = false;
  };
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (c === '"') {
      // The quote OPENS the token → the token is text, not syntax. A quote
      // that opens after content (`--tenant="x y"`) leaves the token alone.
      if (current.length === 0 && !inQuote) quoted = true;
      inQuote = !inQuote;
      continue;
    }
    if (!inQuote && /\s/.test(c)) {
      flush();
      continue;
    }
    current += c;
  }
  flush();
  return tokens;
}

/**
 * Split a string into whitespace-separated tokens, treating
 * double-quoted runs as a single token. Quotes are stripped from
 * the resulting tokens.
 *
 * Byte-for-byte the old behaviour — it is {@link tokenizeRich} with the
 * quoting dropped, so the two can never disagree about where the token
 * boundaries are.
 */
export function tokenize(input: string): string[] {
  return tokenizeRich(input).map((t) => t.text);
}

/**
 * Coerce a token to a `number` when it's a clean numeric literal;
 * otherwise keep it as a `string`.
 */
export function coerceToken(token: string): string | number {
  if (/^-?\d+(\.\d+)?$/.test(token)) {
    const n = Number(token);
    if (Number.isFinite(n)) return n;
  }
  return token;
}

/**
 * One declared flag as it was actually spelled on the line.
 *
 * `consumed` is what makes the SCRUB in {@link applyDeclaredFlags}
 * precise rather than a re-parse. A re-parse over already-bound text asks
 * "does this look like a flag?", which is a question the text can no
 * longer answer once its quoting is gone; `consumed` asks "is this the
 * exact run the operator typed as a flag?", which it can.
 */
export interface FlagHit {
  /** Bare flag name, without {@link FLAG_PREFIX}. */
  name: string;
  /** The coerced value bound for the flag. */
  value: string | number;
  /**
   * The token TEXTS this flag consumed, in order: one for `--name=value`,
   * two for `--name value`.
   */
  consumed: string[];
}

/** Normalize a mixed token list to {@link Token}s. A bare string is
 *  UNQUOTED — the callers that pass strings are tests and callers for
 *  whom quoting genuinely cannot apply. */
function asTokens(tokens: readonly (string | Token)[]): Token[] {
  return tokens.map((t) => (typeof t === "string" ? { text: t, quoted: false } : t));
}

/**
 * Split declared `--flags` (see [`FLAG_PREFIX`]) out of a token stream.
 * Returns the parsed flag values keyed by bare name, the remaining tokens
 * in order for positional binding, and the per-flag {@link FlagHit}s.
 *
 * A QUOTED token is never a flag NAME. `/spawn-ai 1 gmail "--tenant"` is
 * an operator quoting their prompt, and reading it as a bare declared flag
 * answered "tenant was supplied but empty" and spawned nothing. A quoted
 * token is still usable as a flag's VALUE (`--tenant "my org"`), because
 * there the quoting is about the value's spaces, not about its meaning.
 *
 * Exported for unit tests — the ordering guarantee (flags never disturb
 * positional fields) is the whole point of the pre-pass.
 */
export function extractFlags(
  tokens: readonly (string | Token)[],
  schemaKeys: readonly string[],
): { flags: Record<string, unknown>; rest: string[]; hits: FlagHit[] } {
  // Only keys the action DECLARED as flags (`--name` in its paramSchema) are
  // recognized. An unknown `--foo` stays in the positional stream so a
  // free-form `context` prompt containing a dash-dash word survives intact.
  const known = new Set(
    schemaKeys.filter((k) => k.startsWith(FLAG_PREFIX)).map((k) => k.slice(FLAG_PREFIX.length)),
  );
  const flags: Record<string, unknown> = {};
  const rest: string[] = [];
  const hits: FlagHit[] = [];
  const list = asTokens(tokens);
  for (let i = 0; i < list.length; i++) {
    const token = list[i];
    if (token.quoted || !token.text.startsWith(FLAG_PREFIX)) {
      rest.push(token.text);
      continue;
    }
    const body = token.text.slice(FLAG_PREFIX.length);
    const eq = body.indexOf("=");
    // `--name=value`
    if (eq > 0) {
      const name = body.slice(0, eq);
      if (known.has(name)) {
        const value = coerceToken(body.slice(eq + 1));
        flags[name] = value;
        hits.push({ name, value, consumed: [token.text] });
        continue;
      }
      rest.push(token.text);
      continue;
    }
    // `--name value` — consumes the NEXT token as the value.
    if (known.has(body)) {
      const next = list[i + 1];
      // A declared flag with nothing usable after it was SUPPLIED and left
      // EMPTY — the same state as `--name=`, and it must read as the same
      // state. Leaving the token in the positional stream instead made
      // `/spawn-ai 1 gmail --tenant` bind `context: "--tenant"`: the tenant
      // read back ABSENT (so the spawn silently took the device default) and
      // the literal flag text was typed into the new session as its prompt.
      if (next === undefined || isDeclaredFlagToken(next, known)) {
        flags[body] = "";
        hits.push({ name: body, value: "", consumed: [token.text] });
        continue;
      }
      const value = coerceToken(next.text);
      flags[body] = value;
      hits.push({ name: body, value, consumed: [token.text, next.text] });
      i++;
      continue;
    }
    rest.push(token.text);
  }
  return { flags, rest, hits };
}

/** True when `token` spells one of the `known` declared flags. A QUOTED
 *  token never does — it is the flag's value, not the next flag. */
function isDeclaredFlagToken(token: Token, known: ReadonlySet<string>): boolean {
  if (token.quoted || !token.text.startsWith(FLAG_PREFIX)) return false;
  const body = token.text.slice(FLAG_PREFIX.length);
  const eq = body.indexOf("=");
  return known.has(eq > 0 ? body.slice(0, eq) : body);
}

/**
 * Remove the EXACT runs `hits` consumed from an already-bound field value,
 * and return the value re-joined with its quoting resolved.
 *
 * Exact-run removal, not a re-parse. A re-parse over this text was the D1
 * defect: `context: "fix the --tenant handling"` (quotes already gone) was
 * re-scanned, `--tenant` read as a bare declared flag, and it ate the word
 * after it — so `/spawn-ai 1 gmail --tenant=2299 "fix the --tenant
 * handling"` typed `fix the` into the new session and the top-level
 * `tenant` overwrote the swallowed one, making the loss invisible.
 *
 * Matching on `consumed` cannot do that: it removes only text the operator
 * really typed as a top-level flag, once per occurrence, and never a token
 * that opened with a quote.
 */
function stripFlagRuns(value: string, hits: readonly FlagHit[]): { text: string; removed: number } {
  const toks = tokenizeRich(value);
  let removed = 0;
  for (const hit of hits) {
    for (let i = 0; i < toks.length; i++) {
      const t = toks[i];
      if (t.quoted || t.text !== hit.consumed[0]) continue;
      if (hit.consumed.length === 2 && toks[i + 1]?.text !== hit.consumed[1]) continue;
      toks.splice(i, hit.consumed.length);
      removed++;
      break;
    }
  }
  return { text: toks.map((t) => t.text).join(" "), removed };
}

/**
 * Where an arg bag came from — which decides whether its string fields
 * still carry the operator's RAW text or have already been normalized.
 *
 * This distinction is the D1 fix. The previous version had no notion of
 * origin and re-scanned every route's bound fields, so on the SLASH route
 * it re-parsed text {@link parseArgs} had already stripped of both flags
 * and quotes, and deleted words out of the operator's prompt.
 */
export type ArgOrigin =
  /** {@link parseArgs} produced the bag from the raw input, quoting intact. */
  | "parsed"
  /** A higher tier (a Tier-2 regex group, Tier-3 model output) bound it
   *  from text that never saw the action's `paramSchema`. */
  | "preset";

/**
 * Merge an action's DECLARED `--flags` into a PRESET arg bag, and resolve
 * the raw text its string fields still carry.
 *
 * This is the route-independent half of flag handling, and it exists because
 * making it a property of {@link parseArgs} made it a property of ONE ROUTE.
 * `CommandBar.execute` reads `presetArgs ?? parseArgs(...)`, so a Tier-2 /
 * Tier-3 hit skipped the parser entirely — and `/spawn-ai`'s Tier-2 pattern
 * ends in `(?<context>.+)`, which swallowed `--tenant=` whole. The declared
 * flag was never extracted, the handler's three-state tenant guard therefore
 * never ran, and `/spawn-ai 1 gmail --tenant=` spawned under the device
 * default while the byte-identical `/spawn-best 1 gmail --tenant=` correctly
 * refused. Same action, same args, split purely on which regex matched first.
 *
 * A per-flag recovery in the handler (the deleted `splitTenantFlag`) closed
 * that for `--tenant` only; the next declared flag would inherit the bug
 * silently, and any future pattern with a `.+` tail would re-open it. So the
 * extraction is applied to EVERY PRESET route's args instead, from the RAW
 * INPUT, which is the only text guaranteed to still carry the flag whatever
 * the matching tier did with it.
 *
 * ## `"parsed"` is a NO-OP, and the claim that it was "idempotent" was false
 *
 * The previous version said this step was "idempotent on the slash route
 * where `parseArgs` already stripped them". It was not, and the difference
 * cost an operator their prompt:
 *
 *     /spawn-ai 1 gmail --tenant=2299 "fix the --tenant handling"
 *       status  /spawn-ai ✓   tenant  2299…  (correct)
 *       typed into the session:  "fix the"   ← "--tenant handling" DELETED
 *
 * `parseArgs` had already bound `context: "fix the --tenant handling"` —
 * correctly, because `tokenize` kept the quoted run whole. The scrub then
 * re-tokenized THAT value, where the quotes no longer exist, read
 * `--tenant` as a bare declared flag, and let it eat `handling`. The
 * top-level `tenant` overwrote the swallowed one on the way out, so the
 * verdict line looked right and the loss was invisible.
 *
 * So the parsed route returns untouched: {@link parseArgs} extracted the
 * declared flags from the raw input with the quoting still present, which
 * is strictly more information than this function can recover afterwards.
 * There is nothing left here to do that would not be damage.
 *
 * ## The QUOTING half is NOT gated on declared flags
 *
 * Only the FLAG half is skipped for an action that declares none. Gating
 * the quote resolution on it too was a defect found by re-deriving the
 * resolution delta rather than by reasoning about the change: `/tag`,
 * `/orchestrate` and `/auto-approve` bind through Tier-2 groups as well,
 * declare no flags, and so kept their raw quote characters — `/orchestrate
 * "fix the thing"` handed the conductor a goal spelled with the quotes in
 * it, which the slash route never did.
 */
export function applyDeclaredFlags(
  args: Record<string, unknown>,
  input: string,
  action: CommandAction,
  origin: ArgOrigin,
): Record<string, unknown> {
  if (origin === "parsed") return args;
  const schemaKeys = action.paramSchema ? Object.keys(action.paramSchema) : [];

  // The RAW INPUT is the authority. Only a flag the operator typed as a
  // top-level token counts; one that appears only inside a QUOTED run is
  // prompt text they quoted on purpose — `tokenizeRich` is what carries
  // that distinction this far, and `extractFlags` is what honours it.
  const trimmed = input.trim();
  const firstSpace = trimmed.search(/\s/);
  const tail = firstSpace === -1 ? "" : trimmed.slice(firstSpace + 1).trim();
  const { flags: found, hits } =
    tail.length > 0 && schemaKeys.some((k) => k.startsWith(FLAG_PREFIX))
      ? extractFlags(tokenizeRich(tail), schemaKeys)
      : { flags: {} as Record<string, unknown>, hits: [] as FlagHit[] };

  // Every string field is resolved, not only the ones a flag landed in.
  // A Tier-2 group is a slice of the RAW input, so it still carries the
  // operator's quote characters (`context: '"fix the thing"'`); leaving
  // them in typed them verbatim into the spawned session, which the slash
  // route never did. One normalization, so the two routes agree.
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    if (typeof value !== "string") {
      out[key] = value;
      continue;
    }
    const { text, removed } = stripFlagRuns(value, hits);
    // Nothing survived AND a flag is what ate it: the field was not
    // supplied. Dropping the key rather than binding "" is deliberate —
    // `readTextArg` reads "" as supplied-but-empty, which is a different
    // (and erroring) state.
    if (text.length === 0 && removed > 0) continue;
    // Otherwise the RESOLVED text is what the operator supplied, empty or
    // not. Restoring the raw `value` here instead was D8: `tokenizeRich`
    // resolves `""` to zero tokens, `removed` is 0 because no flag was
    // involved, and the raw text — two literal quote CHARACTERS — was put
    // back. `/orchestrate ""` then POSTed `start_orchestration_run` with
    // `goal: "\"\""`, spending a conductor run on an empty argument, while
    // `/orchestrate " "` correctly answered "a goal … is required".
    // `/auto-approve add ""` armed a `""` rule and `/tag ""` reported
    // `"""" is not a zone tag`. An empty quoted run is an EMPTY ARGUMENT,
    // and supplied-and-empty is exactly the state "" already means here —
    // so binding the resolution keeps that arm rather than inventing text.
    out[key] = text;
  }
  return { ...out, ...found };
}

/**
 * Extract the args portion of the input (everything after the slash
 * form's trailing whitespace) and project it onto the action's
 * paramSchema field order. Declared `--flags` are pulled out first and
 * merged in under their bare names.
 */
export function parseArgs(input: string, action: CommandAction): Record<string, unknown> {
  const trimmed = input.trim();
  const firstSpace = trimmed.search(/\s/);
  if (firstSpace === -1) return {};
  const rest = trimmed.slice(firstSpace + 1).trim();
  if (rest.length === 0) return {};
  const schemaKeys = action.paramSchema ? Object.keys(action.paramSchema) : [];
  // Pull declared `--flags` out BEFORE positional binding, so an optional
  // flag anywhere in the line can't shift the positional fields (and can't be
  // swallowed by the free-form catch-all tail below).
  const { flags, rest: positional } = extractFlags(tokenizeRich(rest), schemaKeys);
  const tokens = positional;
  const fieldOrder = schemaKeys.filter((k) => !k.startsWith(FLAG_PREFIX));
  const args: Record<string, unknown> = { ...flags };
  // Bind the first N tokens to the first N fields. Excess tokens are
  // silently dropped — the resolver doesn't reject "too many args"
  // because most actions accept a trailing free-form last arg
  // (e.g. /spawn-ai's `context` is a multi-word prompt that could
  // contain quoted whitespace; we'd rather have the operator's intent
  // even if quoting got lost).
  for (let i = 0; i < Math.min(tokens.length, fieldOrder.length); i++) {
    args[fieldOrder[i]] = coerceToken(tokens[i]);
  }
  // If the last schema field is the "free-form catch-all" position
  // (e.g. `context` or `command`), join any extra tokens into it so
  // operators don't lose the tail of a prompt to a missing quote.
  if (tokens.length > fieldOrder.length && fieldOrder.length > 0) {
    const last = fieldOrder[fieldOrder.length - 1];
    const tailStart = fieldOrder.length - 1;
    args[last] = tokens.slice(tailStart).join(" ");
  }
  return args;
}

/**
 * Positional tokens the action's `paramSchema` cannot absorb.
 *
 * `parseArgs`'s free-form catch-all is guarded on `fieldOrder.length > 0`,
 * so a command with an EMPTY schema silently drops everything typed after
 * it: `/mute please stop` bound `{}`, ran the bare `/mute` and rendered
 * `✓` — an honest verdict for a command the operator did not type. Every
 * other honesty fix in this surface is about the same thing: a `✓` must
 * describe what happened, and here it described a subset.
 *
 * Returns `[]` when the schema HAS positional fields, because there the
 * catch-all folds the tail into the last one on purpose (a `/spawn-ai`
 * context is a multi-word prompt that may have lost its quoting).
 */
export function unboundTokens(input: string, action: CommandAction): string[] {
  const trimmed = input.trim();
  const firstSpace = trimmed.search(/\s/);
  if (firstSpace === -1) return [];
  const rest = trimmed.slice(firstSpace + 1).trim();
  if (rest.length === 0) return [];
  const schemaKeys = action.paramSchema ? Object.keys(action.paramSchema) : [];
  const fieldOrder = schemaKeys.filter((k) => !k.startsWith(FLAG_PREFIX));
  if (fieldOrder.length > 0) return [];
  return extractFlags(tokenizeRich(rest), schemaKeys).rest;
}

/**
 * THREE-state read of a TEXT argument — the exact mirror of
 * `useTerminalCommands.ts::readZoneArg` and `::readCountArg`, and for
 * the exact same reason.
 *
 * Same family as `useTerminalCommands.ts::readCountArg`: {@link coerceToken}
 * turns a clean numeric literal into a `number`, so `/spawn-with 2 5`
 * bound `command: 5` and every `typeof args.x === "string" ? x : ""` in
 * the HANDLERS read that as ABSENT. `/spawn-with 2 5` answered "command is
 * required" for a command that was supplied, and `/spawn-ai 2 3` was
 * worse than that — `resolveAccountConfigDir` maps `""` to "best", so a
 * mistyped account silently launched the highest-headroom one instead.
 * Stringifying the token the operator actually typed keeps the field
 * SUPPLIED, which is what lets the existing "unknown account" /
 * "invalid command" paths report the truth.
 *
 * That closed the `number` case only, and the two-state shape it left
 * behind reopened the same hole one spelling over. A field SUPPLIED but
 * empty (`--tenant=`, `{account: {}}`) mapped to ABSENT, and absent is
 * the one state where a handler is entitled to guess:
 * `resolveAccountConfigDir` reads `""` as `"best"` and launches the
 * highest-headroom account, and `/spawn-ai 2 gmail --tenant=` binds the
 * device default — verbatim the mis-bindings the tenant feature and this
 * docstring both exist to prevent. `invalid` is now its own arm, so a
 * supplied-but-unusable value names itself instead of vanishing.
 */
export type TextArgRead =
  | { kind: "absent" }
  | { kind: "invalid"; raw: string }
  | { kind: "text"; text: string };

export function readTextArg(args: Record<string, unknown>, field: string): TextArgRead {
  const v = args[field];
  if (v === undefined || v === null) return { kind: "absent" };
  if (typeof v === "string")
    return v.trim() === "" ? { kind: "invalid", raw: v } : { kind: "text", text: v };
  if (typeof v === "number" || typeof v === "boolean") {
    return { kind: "text", text: String(v) };
  }
  // An object/array reached the bag (`{account: {}}`). It was SUPPLIED;
  // reporting it as absent is what let it become "best".
  return { kind: "invalid", raw: typeof v === "object" ? JSON.stringify(v) : String(v) };
}

/**
 * The text of a supplied text arg, or `""` for absent AND invalid.
 *
 * Lossy by construction — it is kept for the callers whose field is
 * genuinely optional and free-form, where "" and "not given" mean the
 * same thing. Any caller that would GUESS on an empty value must use
 * `useTerminalCommands.ts::resolveText` instead, the way every count
 * caller uses `resolveCount`.
 */
export function textArg(args: Record<string, unknown>, field: string): string {
  const read = readTextArg(args, field);
  return read.kind === "text" ? read.text : "";
}

/**
 * Format an action's `paramSchema` field list as the inline hint the
 * palette appends to a row label (`" (count, account, context)"`), or
 * `""` when the action takes no arguments.
 *
 * Lives here rather than in the palette projection because BOTH command
 * surfaces now score this text — see `fuzzy.ts::PARAMS_HINT_BAND`. When
 * only the palette composed it, only the palette could match it, which
 * is precisely how the two surfaces came to answer different questions.
 */
export function describeParams(schema: Record<string, unknown> | undefined): string {
  if (!schema) return "";
  const keys = Object.keys(schema);
  if (keys.length === 0) return "";
  return ` (${keys.join(", ")})`;
}
