/**
 * terminalKeySequence — pure translation from a UI Bridge `sendKeys` payload
 * into the bytes a PTY actually expects.
 *
 * WHY THIS EXISTS. Until `@qontinui/ui-bridge@0.24.0` the SDK resolved built-in
 * actions BEFORE an element's own `customActions`, so the `sendKeys` handler
 * `TerminalInstance` registers on `terminal-input-<id>` was permanently
 * shadowed and had never executed. ui-bridge#165 flipped that precedence, so
 * the handler now owns EVERY `sendKeys` aimed at a terminal pane — including
 * the two array grammars that the built-in used to serve:
 *
 *   1. `{ keys: "ls\r" }`                    — raw text, the runner's own form
 *   2. `{ keys: ["Enter"] }`                 — bare key names (what the
 *                                              runner's spec workflows emit,
 *                                              see `spec-prompt-builder.ts`)
 *   3. `{ keys: [{ key: "c", modifiers: { ctrl: true } }] }`
 *                                            — the SDK's canonical descriptor
 *                                              form (`SendKeysAction`)
 *
 * The handler as written took `keys` to be a string and handed it straight to
 * `writePty`, whose `TextEncoder.encode` coerces anything else via `String()`.
 * Un-shadowed, forms 2 and 3 would therefore have typed the literal text
 * `Enter` and `[object Object]` into the pane — and answered `success: true`
 * with a byte count, because the write genuinely reached the PTY. On a runner
 * those panes are live Claude/PowerShell sessions, so that is silent corruption
 * of someone's real work, reported green. Hence: translate, and throw on any
 * key name we cannot translate rather than typing its name.
 *
 * Bytes rather than synthetic `KeyboardEvent`s (which is how the built-in used
 * to do it) is deliberate: routing every form through `writePty` is what gives
 * the dead-PTY detection — the `TERMINAL_EXITED` envelope in
 * `terminalWriteResult.ts` — to all three grammars. That ghost-write on a dead
 * pane was the other half of what ui-bridge#165 set out to fix.
 *
 * Sequences are xterm.js's defaults in NORMAL cursor mode (DECCKM reset). The
 * application-cursor-mode variants (`\x1bOA` …) are not emitted: a caller that
 * needs them can pass the raw string form.
 *
 * ## `modifiers` is VALIDATED, not cast (manual-test-loop iteration 25, P0)
 *
 * `encodeKey` used to open with `const mods = desc.modifiers ?? {}` — a cast
 * over a value that arrives from an untrusted HTTP body, and the same class of
 * defect as the `text` assertion that `terminalTextPayload.ts` exists to close.
 * A malformed `modifiers` did not fail; it silently degraded to "no modifiers"
 * and the WRONG BYTES went into a live PTY. Measured on BOTH paths (they share
 * this module), 2/2 reps each, with a `#MARK ` written first so the echo was
 * unambiguous:
 *
 *   | payload                                   | result                       |
 *   |-------------------------------------------|------------------------------|
 *   | `{key:"c", modifiers:{ctrl:true}}`        | `^C` interrupt — correct     |
 *   | `{key:"c", modifiers:"ctrl"}`             | literal `c` — WRONG          |
 *   | `{key:"c", modifiers:["ctrl"]}`           | literal `C` — WRONG          |
 *   | `{key:"c", modifiers:{Ctrl:true}}`        | literal `c` — WRONG          |
 *   | `{key:"c", modifiers:4}`                  | literal `c` — WRONG          |
 *
 * The array case is the sharpest illustration of why a cast is not a check:
 * `["ctrl"]` produced a CAPITAL `C`, because `mods.shift` resolved to
 * `Array.prototype.shift` — a function, and therefore truthy. Every one of
 * those answered HTTP 200 with `bytes: 1`, byte-for-byte indistinguishable from
 * the correct interrupt, so a caller could not detect it. An automation trying
 * to Ctrl-C a runaway command instead TYPES A CHARACTER INTO IT.
 *
 * The call: unknown and mis-cased modifier names are REJECTED, not normalized.
 * `{Ctrl: true}` is a caller who means Ctrl, and the one thing it must never
 * quietly mean is "no modifiers" — but silently accepting it would also make
 * this module the arbiter of which spellings of which flags exist, and the SDK's
 * `SendKeysAction` already names exactly four, all lower-case. Rejecting says so
 * in one typed error the caller can act on; normalizing would hide the payload
 * bug until the next unrecognised spelling. Non-boolean flag VALUES are rejected
 * on the same grounds: `{ctrl: "false"}` is truthy, so coercing it would turn a
 * caller's explicit "off" into Ctrl being ON.
 *
 * Absent/`undefined` `modifiers` keeps meaning "no modifiers" — that is the
 * documented shape (`{ keys: ["Enter"] }`) and the overwhelmingly common one.
 *
 * Leaf module — zero imports — for the same reason `terminalWriteResult.ts` is
 * one: `TerminalInstance` transitively pulls `@xterm/addon-canvas`, which
 * touches `self` at module init and crashes under the runner's
 * `environment: "node"` vitest config.
 */

/** Machine-readable failure code: the `keys` payload could not be translated. */
export const SEND_KEYS_INVALID = "SEND_KEYS_INVALID";

/** Control Sequence Introducer — the `ESC [` that opens every CSI sequence. */
const CSI = "\x1b[";

/** Modifier flags, matching the SDK's `SendKeysAction` key descriptor. */
export interface KeyModifiers {
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  meta?: boolean;
}

/** One key in the SDK's canonical descriptor form. */
export interface KeyDescriptor {
  key?: string;
  modifiers?: KeyModifiers;
}

/** Every `keys` grammar the handler accepts. */
export type SendKeysPayload = string | Array<string | KeyDescriptor>;

/**
 * Named keys → their normal-cursor-mode escape sequence.
 *
 * Lookup is case-insensitive on the caller's side (see `lookupNamedKey`) so
 * `"enter"`, `"Enter"` and `"ENTER"` all resolve — automation payloads are
 * hand-written far more often than they are generated.
 */
const NAMED_KEYS: Readonly<Record<string, string>> = {
  enter: "\r",
  return: "\r",
  tab: "\t",
  escape: "\x1b",
  esc: "\x1b",
  backspace: "\x7f",
  delete: "\x1b[3~",
  del: "\x1b[3~",
  insert: "\x1b[2~",
  space: " ",
  arrowup: "\x1b[A",
  arrowdown: "\x1b[B",
  arrowright: "\x1b[C",
  arrowleft: "\x1b[D",
  up: "\x1b[A",
  down: "\x1b[B",
  right: "\x1b[C",
  left: "\x1b[D",
  home: "\x1b[H",
  end: "\x1b[F",
  pageup: "\x1b[5~",
  pagedown: "\x1b[6~",
  f1: "\x1bOP",
  f2: "\x1bOQ",
  f3: "\x1bOR",
  f4: "\x1bOS",
  f5: "\x1b[15~",
  f6: "\x1b[17~",
  f7: "\x1b[18~",
  f8: "\x1b[19~",
  f9: "\x1b[20~",
  f10: "\x1b[21~",
  f11: "\x1b[23~",
  f12: "\x1b[24~",
};

function lookupNamedKey(key: string): string | undefined {
  return NAMED_KEYS[key.toLowerCase()];
}

/**
 * Control-code for `Ctrl+<char>`, or `undefined` where the pairing has no
 * canonical byte.
 *
 * `Ctrl+A`…`Ctrl+Z` are 0x01–0x1a; `@ [ \ ] ^ _` fill 0x00 and 0x1b–0x1f;
 * `Ctrl+Space` is the NUL that shells bind to "set mark", and `Ctrl+?` is DEL.
 */
function controlCode(char: string): string | undefined {
  if (char.length !== 1) return undefined;
  const upper = char.toUpperCase();
  if (upper >= "A" && upper <= "Z") {
    return String.fromCharCode(upper.charCodeAt(0) - 64);
  }
  switch (char) {
    case "@":
    case " ":
      return "\x00";
    case "[":
      return "\x1b";
    case "\\":
      return "\x1c";
    case "]":
      return "\x1d";
    case "^":
      return "\x1e";
    case "_":
      return "\x1f";
    case "?":
      return "\x7f";
    default:
      return undefined;
  }
}

function invalid(detail: string): Error {
  const err = new Error(`${SEND_KEYS_INVALID}: ${detail}`) as Error & { code?: string };
  err.code = SEND_KEYS_INVALID;
  return err;
}

/**
 * Describe a rejected value by TYPE, never by content.
 *
 * A rejected payload is whatever an automation caller sent, which may be
 * derived from a credential; its type is the diagnosis it deserves. `null` and
 * arrays are named explicitly because `typeof` answers `"object"` for both and
 * that reads as a bug report. Duplicated from `terminalTextPayload.ts` rather
 * than shared, to keep this module's deliberate zero-import property (see the
 * header): `TerminalInstance` transitively pulls `@xterm/addon-canvas`, which
 * touches `self` at module init and crashes under `environment: "node"`.
 */
function describe(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "an array";
  const kind = typeof value;
  return kind === "object" || kind === "undefined" ? `an ${kind}` : `a ${kind}`;
}

/** The only flags a key descriptor may carry — the SDK's `SendKeysAction` set. */
const MODIFIER_NAMES = ["ctrl", "shift", "alt", "meta"] as const;

/**
 * Validate one descriptor's `modifiers` BEFORE any byte is chosen for it.
 *
 * @param value  The caller-supplied value, untrusted and of unknown type.
 * @param key    The descriptor's key name, for the human half of the message.
 * @returns the modifier flags, narrowed — `{}` when absent.
 * @throws a coded {@link SEND_KEYS_INVALID} `Error` for anything that is not
 * absent and not a plain object of known boolean flags. NOTHING is written
 * before this throws, exactly as for `{ key: { nested: 1 } }` — that a
 * malformed payload writes ZERO bytes rather than the wrong one is the whole
 * point.
 */
function requireModifiers(value: unknown, key: string): KeyModifiers {
  if (value === undefined) return {};
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw invalid(
      `'modifiers' for key '${key}' must be an object of boolean flags ` +
        `(${MODIFIER_NAMES.join(", ")}); received ${describe(value)}. ` +
        'Example: { key: "c", modifiers: { ctrl: true } }. ' +
        "NOTHING was sent to the terminal.",
    );
  }
  const out: KeyModifiers = {};
  for (const [name, flag] of Object.entries(value as Record<string, unknown>)) {
    // An optional property explicitly set to `undefined` is idiomatic TS and
    // means exactly what omitting it means.
    if (flag === undefined) continue;
    if (!(MODIFIER_NAMES as readonly string[]).includes(name)) {
      throw invalid(
        `unknown modifier '${name}' for key '${key}'. The modifiers are ` +
          `${MODIFIER_NAMES.join(", ")} — all lower-case; '${name.toLowerCase()}' ` +
          "may be what was meant. Rejected rather than guessed: a modifier this " +
          "module does not recognise must never quietly mean NO modifiers, which " +
          "is how a Ctrl-C became a typed 'c'. NOTHING was sent to the terminal.",
      );
    }
    if (typeof flag !== "boolean") {
      throw invalid(
        `modifier '${name}' for key '${key}' must be true or false; received ` +
          `${describe(flag)}. Not coerced: '${name}: "false"' is truthy, so ` +
          "coercing would turn an explicit off into ON. NOTHING was sent to the " +
          "terminal.",
      );
    }
    out[name as (typeof MODIFIER_NAMES)[number]] = flag;
  }
  return out;
}

/**
 * Translate ONE key descriptor into PTY bytes.
 *
 * Order matters: a named key is resolved first, so `Ctrl+ArrowLeft` becomes the
 * modified CSI form rather than being mistaken for a printable character.
 */
function encodeKey(desc: KeyDescriptor): string {
  const key = desc.key;
  if (typeof key !== "string" || key.length === 0) {
    throw invalid(
      "each entry of the 'keys' array must be a key name or a { key } descriptor " +
        '(example: { keys: [{ key: "Enter" }] }).',
    );
  }
  const mods = requireModifiers(desc.modifiers, key);
  const named = lookupNamedKey(key);

  if (named) {
    // CSI-modified form for the cursor/navigation keys, which is how a
    // terminal receives e.g. Ctrl+Left (word-back) or Shift+Up (select line).
    // Parameter is the xterm modifier bitmask + 1.
    const bits =
      (mods.shift ? 1 : 0) + (mods.alt ? 2 : 0) + (mods.ctrl ? 4 : 0) + (mods.meta ? 8 : 0);
    if (bits === 0) return named;

    const param = bits + 1;
    // Matched by prefix rather than a regex literal: an ESC inside a regex
    // trips eslint's `no-control-regex`, and the shapes here are only two.
    if (named.startsWith(CSI)) {
      const body = named.slice(CSI.length);
      // `CSI <n> ~` → `CSI <n> ; <param> ~`   (Delete, Insert, Page*, F5–F12)
      if (/^\d+~$/.test(body)) return `${CSI}${body.slice(0, -1)};${param}~`;
      // `CSI <letter>` → `CSI 1 ; <param> <letter>`   (Arrow*, Home, End)
      if (/^[A-Z]$/.test(body)) return `${CSI}1;${param}${body}`;
    }
    // Non-CSI named keys (Enter, Tab, Escape, Backspace, Space, F1–F4).
    // Ctrl/Alt on these have no CSI form; Alt is the standard ESC prefix and
    // Ctrl on a control character is already that character.
    if (mods.ctrl && key.toLowerCase() === "space") return "\x00";
    return mods.alt || mods.meta ? `\x1b${named}` : named;
  }

  if (key.length !== 1) {
    throw invalid(
      `unknown key '${key}'. Use a known key name (Enter, Tab, Escape, Backspace, ` +
        "Delete, Home, End, PageUp, PageDown, Arrow*, F1–F12), a single character, " +
        "or pass 'keys' as a raw string to write bytes verbatim.",
    );
  }

  let char = key;
  // Only synthesize a shifted letter — a caller that means `!` sends `!`, and
  // guessing a keyboard layout for `Shift+1` would be wrong on most of them.
  if (mods.shift && char >= "a" && char <= "z") char = char.toUpperCase();

  if (mods.ctrl) {
    const code = controlCode(char);
    if (code === undefined) {
      throw invalid(`Ctrl+'${key}' has no control-code equivalent.`);
    }
    return mods.alt || mods.meta ? `\x1b${code}` : code;
  }

  return mods.alt || mods.meta ? `\x1b${char}` : char;
}

/**
 * Normalize any `sendKeys` payload into the byte string to write to the PTY.
 *
 * @throws when `keys` is missing, empty, or contains a key that cannot be
 * translated — never silently, and never by typing the key's own name.
 */
export function toPtySequence(keys: unknown): string {
  if (typeof keys === "string") {
    if (keys.length === 0) {
      throw invalid("'keys' was an empty string; nothing to send.");
    }
    return keys;
  }
  if (!Array.isArray(keys)) {
    throw invalid(
      "'keys' is required — either a raw string written verbatim to the PTY, or a " +
        'non-empty array of key names / { key, modifiers } descriptors (example: { keys: [{ key: "Enter" }] }).',
    );
  }
  if (keys.length === 0) {
    throw invalid("'keys' was an empty array; nothing to send.");
  }
  return keys
    .map((entry) => encodeKey(typeof entry === "string" ? { key: entry } : (entry ?? {})))
    .join("");
}
