/**
 * Construction of the synthetic `KeyboardEvent`s the UI-Bridge `dispatch_key`
 * handler fires at `window` / `document` / `body` / `activeElement`.
 *
 * Extracted out of `useControlEvents.ts`'s dispatch loop so the event shape is
 * assertable without mounting the hook — the loop lives inside a `switch` inside
 * a `useCallback` and had no test at all, which is how the two defects fixed
 * here survived the change that introduced the field.
 *
 * # Relationship to ui-bridge `core/key-events.ts`
 *
 * The SDK owns the same three functions and names this copy as a duplicate to
 * retire. **It is not yet retireable, and the reason is not availability.**
 * `@qontinui/ui-bridge@0.25.1` publishes `buildKeyboardEventInit`, so the
 * "unpublished builder" that blocked the retirement when this code landed is
 * gone. What blocks it now is one substantive disagreement:
 *
 * > The SDK's `keyToKeyCode` reports a punctuation key's own CHARACTER code
 * > (`;` → 59) and documents that as a deliberate approximation, because "the
 * > layout-specific table is not derivable from a `key` value alone". This
 * > module carries the US-layout virtual-key table (`;` → 186), which is what a
 * > browser actually reports on `keydown` and what every `e.keyCode === 186`
 * > handler is written against.
 *
 * Switching to the SDK today would therefore REGRESS punctuation `keydown` for
 * the sake of removing a duplicate. The retirement is unblocked by moving the
 * US-layout table INTO the SDK, not by deleting this one; until then this file
 * is the single construction site on the runner side, so there is exactly one
 * place to change when that happens.
 *
 * Every other value here agrees with the SDK exactly, including the two things
 * this module fixes relative to its previous inline form.
 */

/** Modifier flags accepted alongside a key — the `dispatch_key` payload shape. */
export interface KeyModifiers {
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  meta?: boolean;
}

/** Which event of the `keydown` → `keypress` → `keyup` triple is being built. */
export type KeyEventType = "keydown" | "keypress" | "keyup";

/**
 * Map a `KeyboardEvent.key` value to a `KeyboardEvent.code` (the PHYSICAL key,
 * and how a layout-independent shortcut is bound). Leaving it unset ships every
 * synthetic event with `code: ""`, so any listener reading `e.code` can never
 * match one — a dispatch that reports success while reaching nothing.
 *
 * Best-effort and identical to the SDK's `keyToCode`: letters and digits map to
 * `KeyX` / `DigitN`, space to `Space`, and any other name to itself.
 */
export function keyToCode(key: string): string {
  if (!key || typeof key !== "string") return "";
  if (key.length === 1) {
    const upper = key.toUpperCase();
    if (upper >= "A" && upper <= "Z") return `Key${upper}`;
    if (upper >= "0" && upper <= "9") return `Digit${upper}`;
    if (key === " ") return "Space";
  }
  return key;
}

/**
 * Legacy `keyCode` values for the named (non-single-character) keys.
 *
 * COVERAGE RULE, mirroring the SDK's: every named key a caller can plausibly
 * name carries a non-zero code, so a key the API ACCEPTS never dispatches with
 * `keyCode: 0` for want of a table entry — that is the same silent no-op the
 * legacy fields were added to prevent, reached through the front door.
 *
 * `Undo`, `Redo`, `Copy`, `Cut`, `Paste`, `Fn` and `Symbol` are deliberately
 * absent: the legacy model assigns them no code, so a browser reports 0 for
 * them and returning 0 is the honest answer rather than a gap.
 */
const NAMED_KEY_CODES: Readonly<Record<string, number>> = {
  Cancel: 3,
  Backspace: 8,
  Tab: 9,
  Clear: 12,
  Enter: 13,
  Shift: 16,
  Control: 17,
  Alt: 18,
  Pause: 19,
  CapsLock: 20,
  Escape: 27,
  PageUp: 33,
  PageDown: 34,
  End: 35,
  Home: 36,
  ArrowLeft: 37,
  ArrowUp: 38,
  ArrowRight: 39,
  ArrowDown: 40,
  Select: 41,
  PrintScreen: 44,
  Insert: 45,
  Delete: 46,
  Help: 47,
  Meta: 91,
  ContextMenu: 93,
  NumLock: 144,
  ScrollLock: 145,
  AltGraph: 225,
};

/**
 * Punctuation `keyCode`s are the US-layout PHYSICAL key numbers, which is what
 * a `keyCode` has always meant — `;` and `:` are the same physical key, so both
 * are 186. This is the one table that intentionally diverges from the SDK; see
 * the module header.
 */
const PUNCTUATION_KEY_CODES: Readonly<Record<string, number>> = {
  ";": 186,
  ":": 186,
  "=": 187,
  "+": 187,
  ",": 188,
  "<": 188,
  "-": 189,
  _: 189,
  ".": 190,
  ">": 190,
  "/": 191,
  "?": 191,
  "`": 192,
  "~": 192,
  "[": 219,
  "{": 219,
  "\\": 220,
  "|": 220,
  "]": 221,
  "}": 221,
  "'": 222,
  '"': 222,
};

/** `F1`–`F24` — the only named keys whose codes are a range rather than a row. */
const FUNCTION_KEY = /^F([1-9]|1[0-9]|2[0-4])$/;

/**
 * Map a `KeyboardEvent.key` value to its legacy `keyCode` — the value a
 * `keydown` / `keyup` reports.
 *
 * The legacy model is PHYSICAL-key shaped, so `a` and `A` share 65. Returns 0
 * for a key that cannot be placed, which is exactly what the platform reports
 * for an unidentified key — never a fabricated code.
 */
export function keyToKeyCode(key: string): number {
  if (!key || typeof key !== "string") return 0;
  if (key.length === 1) {
    const upper = key.toUpperCase();
    if (upper >= "A" && upper <= "Z") return upper.charCodeAt(0);
    if (key >= "0" && key <= "9") return key.charCodeAt(0);
    if (key === " ") return 32;
    const punctuation = PUNCTUATION_KEY_CODES[key];
    if (punctuation !== undefined) return punctuation;
    return upper.charCodeAt(0);
  }
  const named = NAMED_KEY_CODES[key];
  if (named !== undefined) return named;
  const fn = FUNCTION_KEY.exec(key);
  if (fn) return 111 + Number(fn[1]);
  return 0;
}

/**
 * Build the `KeyboardEventInit` for ONE synthetic key event.
 *
 * `keyCode` / `which` / `charCode` are deprecated in the UI Events spec and
 * still read by a large amount of shipped handler code — xterm.js, CodeMirror,
 * every `e.keyCode === 13` Enter check, the jQuery-era `e.which` idiom. An init
 * built without them reports `keyCode: 0`, so every such handler silently
 * no-ops while the dispatch reports success.
 *
 * **The event type matters, and reusing one init across the triple is wrong.**
 * Browsers report the legacy fields differently per event, and this is the
 * defect the extraction fixes:
 *
 * - `keydown` / `keyup` — `keyCode` is the VIRTUAL key code (physical, so `b`
 *   and `B` are both 66) and `charCode` is 0.
 * - `keypress` — `keyCode`, `charCode` and `which` are all the CHARACTER's own
 *   code point, case intact (`b` is 98, `B` is 66), so a handler doing
 *   `String.fromCharCode(e.charCode || e.which)` recovers the typed character.
 *
 * The previous inline form built one init on `keydown` terms and reused it for
 * all three events, so every synthetic `keypress` carried `charCode: 0` and a
 * case-folded `which` — the same "reports success while reaching nothing"
 * failure the legacy fields were added to close, one event over.
 *
 * `which` always mirrors `keyCode`, which is what `e.which || e.keyCode`
 * feature-probes expect.
 */
export function buildKeyboardEventInit(
  key: string,
  mods?: KeyModifiers,
  type: KeyEventType = "keydown",
): KeyboardEventInit {
  const m = mods ?? {};
  const isKeypress = type === "keypress";
  const charCode = isKeypress && key.length === 1 ? key.charCodeAt(0) : 0;
  const keyCode = isKeypress ? charCode : keyToKeyCode(key);

  return {
    key,
    code: keyToCode(key),
    bubbles: true,
    cancelable: true,
    ctrlKey: !!m.ctrl,
    shiftKey: !!m.shift,
    altKey: !!m.alt,
    metaKey: !!m.meta,
    keyCode,
    which: keyCode,
    charCode,
  };
}

/**
 * Whether a key produces a `keypress` event: only printable characters, and
 * only without Ctrl/Alt/Meta. Same rule as the SDK's `sendKeys` handler
 * (`ui-bridge/packages/ui-bridge/src/react/commandHandlers.ts`).
 */
export function shouldKeypress(key: string, mods?: KeyModifiers): boolean {
  const m = mods ?? {};
  return key.length === 1 && !m.ctrl && !m.alt && !m.meta;
}
