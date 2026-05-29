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
 */

import type { CommandAction } from "./types";

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
 * Extract the args portion of the input (everything after the slash
 * form's trailing whitespace) and project it onto the action's
 * paramSchema field order.
 */
export function parseArgs(input: string, action: CommandAction): Record<string, unknown> {
  const trimmed = input.trim();
  const firstSpace = trimmed.search(/\s/);
  if (firstSpace === -1) return {};
  const rest = trimmed.slice(firstSpace + 1).trim();
  if (rest.length === 0) return {};
  const tokens = tokenize(rest);
  const fieldOrder = action.paramSchema ? Object.keys(action.paramSchema) : [];
  const args: Record<string, unknown> = {};
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
