/**
 * terminalTextPayload — the `text` half of what `terminalKeySequence.ts` does
 * for `keys`: validate an automation payload BEFORE any of it can reach a live
 * PTY, and fail with a machine-readable code rather than coercing.
 *
 * ## Why this exists (manual-test-loop iter 24, items 2 and 3)
 *
 * Iteration 21 fixed the `keys` grammar and iteration 23 fixed it a second time
 * on the proxy path, but BOTH fixes stopped at `sendKeys`. The sibling handlers
 * — `writeToTerminal` and `pasteText`, on both the mounted
 * (`TerminalInstance.tsx`) and the proxy (`TerminalBridgeProxies.tsx`) path —
 * still read
 *
 *     const { text } = (params || {}) as { text?: string };
 *     if (!text) throw new Error("writeToTerminal: 'text' is required");
 *
 * which is a `string` *assertion*, not a check. A non-string `text` sails past
 * the truthiness guard and lands in `TextEncoder.encode`, which coerces
 * anything via `String()`. Measured on a live runner: `{text: 42}` wrote the two
 * bytes `42` into a real shell, `{text: {a:1}}` wrote `[object Object]`, and
 * `{text: ["a","b"]}` wrote `a,b` — each answered HTTP **200** with a byte
 * count, because the write genuinely reached the PTY. That is the exact failure
 * class as the `sendKeys` P0: silent corruption of someone's live work,
 * reported green.
 *
 * `pasteText` was worse in one respect: on the mounted path the value reached
 * `preparePasteData`, whose `text.replace(...)` threw `TypeError: Er.replace is
 * not a function` — a MINIFIED internal identifier handed back to an automation
 * caller as the entire diagnosis (iteration 24, item 3).
 *
 * ## Why not a truthiness check
 *
 * `""` and `"0"` are both falsy, and only ONE of them is invalid. `{text:"0"}`
 * is a perfectly ordinary write — typing a zero into a shell — and the old
 * `if (!text)` rejected it with "'text' is required". So the check is on the
 * TYPE first and the LENGTH second, never on truthiness.
 *
 * Leaf module — zero imports — for the same reason `terminalKeySequence.ts` and
 * `terminalWriteResult.ts` are: `TerminalInstance` transitively pulls
 * `@xterm/addon-canvas`, which touches `self` at module init and crashes under
 * the runner's `environment: "node"` vitest config.
 */

/** Machine-readable failure code: `writeToTerminal`'s `text` was not a string. */
export const WRITE_TEXT_INVALID = "WRITE_TEXT_INVALID";

/** Machine-readable failure code: a paste payload's `text` was not a string. */
export const PASTE_TEXT_INVALID = "PASTE_TEXT_INVALID";

/** Build a typed, coded error in the same shape `toPtySequence` throws. */
function invalid(code: string, detail: string): Error {
  const err = new Error(`${code}: ${detail}`) as Error & { code?: string };
  err.code = code;
  return err;
}

/**
 * Describe a rejected payload WITHOUT leaking its contents.
 *
 * A rejected `text` can be anything an automation caller sent, including
 * something derived from a credential; the diagnosis it deserves is its type,
 * not its value. `null` and arrays are called out by name because `typeof`
 * answers `"object"` for both and that reads as a bug report.
 */
function describe(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "an array";
  const kind = typeof value;
  return kind === "object" || kind === "undefined" ? `an ${kind}` : `a ${kind}`;
}

/**
 * Validate a `text` payload bound for a PTY write.
 *
 * @param text    The caller-supplied value, untrusted and of unknown type.
 * @param code    {@link WRITE_TEXT_INVALID} or {@link PASTE_TEXT_INVALID}.
 * @param action  Handler name, for the human half of the message.
 * @param allowEmpty  `true` for a pure FORMATTER like `preparePasteData`, whose
 *                    business is the type only — whether an empty paste is
 *                    worth sending is the calling handler's decision, and it is
 *                    already made before the formatter runs. Handlers leave
 *                    this `false`: an automation call with no text is a mistake
 *                    worth reporting, not a zero-byte success.
 * @returns the value, narrowed to `string`.
 * @throws a coded `Error` when `text` is not an acceptable string. NOTHING is
 * written before this throws — that is the whole point of the guard.
 */
export function requireTextPayload(
  text: unknown,
  code: string,
  action: string,
  allowEmpty = false,
): string {
  if (typeof text !== "string") {
    throw invalid(
      code,
      `${action}: 'text' must be a string; received ${describe(text)}. ` +
        "The value was NOT written to the terminal.",
    );
  }
  if (!allowEmpty && text.length === 0) {
    throw invalid(code, `${action}: 'text' was an empty string; nothing to write.`);
  }
  return text;
}
