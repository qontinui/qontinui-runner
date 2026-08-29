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
 * positionally alongside a free-form trailing prompt.
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
 * Split a string into whitespace-separated tokens, treating
 * double-quoted runs as a single token. Quotes are stripped from
 * the resulting tokens.
 */
export function tokenize(input: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let inQuote = false;
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (c === '"') {
      inQuote = !inQuote;
      continue;
    }
    if (!inQuote && /\s/.test(c)) {
      if (current.length > 0) {
        tokens.push(current);
        current = "";
      }
      continue;
    }
    current += c;
  }
  if (current.length > 0) tokens.push(current);
  return tokens;
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
 * Split declared `--flags` (see [`FLAG_PREFIX`]) out of a token stream.
 * Returns the parsed flag values keyed by bare name, plus the remaining
 * tokens in order for positional binding.
 *
 * Exported for unit tests — the ordering guarantee (flags never disturb
 * positional fields) is the whole point of the pre-pass.
 */
export function extractFlags(
  tokens: string[],
  schemaKeys: string[],
): { flags: Record<string, unknown>; rest: string[] } {
  // Only keys the action DECLARED as flags (`--name` in its paramSchema) are
  // recognized. An unknown `--foo` stays in the positional stream so a
  // free-form `context` prompt containing a dash-dash word survives intact.
  const known = new Set(
    schemaKeys.filter((k) => k.startsWith(FLAG_PREFIX)).map((k) => k.slice(FLAG_PREFIX.length)),
  );
  const flags: Record<string, unknown> = {};
  const rest: string[] = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (!token.startsWith(FLAG_PREFIX)) {
      rest.push(token);
      continue;
    }
    const body = token.slice(FLAG_PREFIX.length);
    const eq = body.indexOf("=");
    // `--name=value`
    if (eq > 0) {
      const name = body.slice(0, eq);
      if (known.has(name)) {
        flags[name] = coerceToken(body.slice(eq + 1));
        continue;
      }
      rest.push(token);
      continue;
    }
    // `--name value` — consumes the NEXT token as the value.
    if (known.has(body) && i + 1 < tokens.length) {
      flags[body] = coerceToken(tokens[i + 1]);
      i++;
      continue;
    }
    rest.push(token);
  }
  return { flags, rest };
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
  const { flags, rest: positional } = extractFlags(tokenize(rest), schemaKeys);
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
 * Two-state read of a TEXT argument, with a supplied non-string
 * preserved rather than dropped.
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
 */
export function readTextArg(
  args: Record<string, unknown>,
  field: string,
): { kind: "absent" } | { kind: "text"; text: string } {
  const v = args[field];
  if (v === undefined || v === null) return { kind: "absent" };
  if (typeof v === "string")
    return v.trim() === "" ? { kind: "absent" } : { kind: "text", text: v };
  if (typeof v === "number" || typeof v === "boolean") {
    return { kind: "text", text: String(v) };
  }
  return { kind: "absent" };
}

/** The text of a supplied text arg, or `""` when it was never supplied. */
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
