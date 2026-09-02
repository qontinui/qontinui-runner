/**
 * terminalPaneCustomActions — the FOUR custom actions a terminal pane serves,
 * written once for both of the code paths that serve them.
 *
 * ## Why one module rather than two copies
 *
 * `terminal-input-<id>` is registered by whichever of two components currently
 * owns it: `TerminalInstance` when the pane is mounted, and
 * `TerminalBridgeProxies` when flow-grid virtualization has mounted nothing.
 * Both had their own hand-written copy of `sendKeys`, `writeToTerminal`,
 * `pasteText` and `getScrollback`.
 *
 * That duplication has already produced the same defect twice. Iteration 21
 * hardened the mounted `sendKeys` to translate its three key grammars; the
 * proxy `sendKeys`, added afterwards, handed the raw value to
 * `TextEncoder.encode` and typed the literal text `Enter` / `[object Object]`
 * into live sessions, answering `success: true` — "the exact failure
 * `terminalKeySequence.ts` was written to prevent, reintroduced by a second
 * code path", as that handler's own comment now records. Iteration 12 then
 * found `writeToTerminal` typing `[object Object]` into a live PTY and
 * `pasteText` reporting `Er.replace is not a function` — in BOTH copies.
 *
 * A fix applied to one copy of a duplicated handler is the signature failure
 * of this whole loop: *the defective shape lived somewhere the fix did not
 * reach*. So the handlers are built here, from one set of definitions, and the
 * two components supply only what genuinely differs between them — how bytes
 * reach the PTY, whether bracketed-paste mode is knowable, and where the
 * scrollback comes from.
 *
 * ## What guards them
 *
 * Every one is a {@link guardedCustomAction}: the bag is refused unless it is
 * an object whose keys are all declared and whose values are all text or
 * finite numbers, BEFORE any byte reaches a process. `keys` is the single
 * declared exception (see `structuredParams`), because the SDK's contract for
 * it genuinely admits two array grammars — and it has `toPtySequence` as its
 * own validator.
 */

import { guardedCustomAction, type GuardedCustomAction } from "@/lib/ui-bridge/guardedAction";
import { textArg } from "./commands/parse";
import { preparePasteData } from "./preparePaste";
import { toPtySequence } from "./terminalKeySequence";
import { throwIfWriteFailed, type TerminalWriteResult } from "./terminalWriteResult";

/**
 * `keys` accepts a raw string, an array of key names, or the SDK's descriptor
 * array — so it is bound as a {@link GuardedActionSpec.structuredParams}
 * field and validated by `toPtySequence` instead of by per-value coercion.
 */
export const SEND_KEYS_SCHEMA = {
  keys: 'string | string[] | Array<{key, modifiers}> (e.g. "ls\\r", ["Enter"], [{key:"c",modifiers:{ctrl:true}}])',
} as const;

/** The one field whose value is passed through un-coerced. */
export const SEND_KEYS_STRUCTURED = ["keys"] as const;

/** Shared by `writeToTerminal` and `pasteText` — same field, same contract. */
export const WRITE_TEXT_SCHEMA = {
  text: "string (required)",
} as const;

export const GET_SCROLLBACK_SCHEMA = {
  maxLines: "number (optional, defaults to 500)",
} as const;

/**
 * What a pane must supply. Exactly the three things the mounted and proxied
 * paths genuinely disagree about; everything else is shared above.
 */
export interface TerminalPaneEffects {
  /** Send bytes to this pane's PTY, returning the write envelope. */
  writePty: (data: string) => Promise<TerminalWriteResult>;
  /**
   * Whether the pane's terminal is in bracketed-paste mode.
   *
   * A property of a live xterm backend, so the proxy path — which has no
   * mounted xterm — answers `false`. Read at INVOCATION time, never captured:
   * the flag flips whenever the foreground program changes.
   */
  bracketedPasteMode: () => boolean;
  /** Read back at most `maxLines` lines of scrollback as plain text. */
  readScrollback: (maxLines: number) => string | Promise<string>;
}

/** How each action describes itself, per path — the only prose that differs. */
export interface TerminalPaneDescriptions {
  sendKeys: string;
  writeToTerminal: string;
  pasteText: string;
  getScrollback: string;
}

/**
 * Build the four guarded custom actions for one pane.
 *
 * `paste` is NOT here: it reads `navigator.clipboard`, which only a mounted
 * pane has any business doing, and it takes no parameters at all. Adding it
 * would mean giving the proxy path a clipboard it must not have.
 */
export function buildTerminalPaneCustomActions(
  effects: TerminalPaneEffects,
  descriptions: TerminalPaneDescriptions,
): Record<string, GuardedCustomAction> {
  return {
    sendKeys: guardedCustomAction({
      id: "sendKeys",
      description: descriptions.sendKeys,
      paramSchema: SEND_KEYS_SCHEMA,
      structuredParams: SEND_KEYS_STRUCTURED,
      // `toPtySequence` owns the missing/empty/untranslatable `keys` rejection,
      // so there is no separate guard for it here: an untranslatable key must
      // THROW, never type its own name into someone's live session.
      run: async (args) => throwIfWriteFailed(await effects.writePty(toPtySequence(args.keys))),
    }),
    writeToTerminal: guardedCustomAction({
      id: "writeToTerminal",
      description: descriptions.writeToTerminal,
      paramSchema: WRITE_TEXT_SCHEMA,
      // `textArg`, not `args.text as string`: binding coerces a clean numeric
      // token, so `{text: "5"}` arrives as the number 5 and only `textArg`
      // turns it back into the five the caller sent.
      run: async (args) => {
        const text = textArg(args, "text");
        if (!text) throw new Error("writeToTerminal: 'text' is required");
        return throwIfWriteFailed(await effects.writePty(text));
      },
    }),
    pasteText: guardedCustomAction({
      id: "pasteText",
      description: descriptions.pasteText,
      paramSchema: WRITE_TEXT_SCHEMA,
      run: async (args) => {
        const text = textArg(args, "text");
        if (!text) throw new Error("pasteText: 'text' is required");
        const prepared = preparePasteData(text, effects.bracketedPasteMode());
        // Same envelope as sendKeys / writeToTerminal: this is an automation
        // surface, so a write that reached no process must not answer
        // `success: true`.
        return throwIfWriteFailed(await effects.writePty(prepared));
      },
    }),
    getScrollback: guardedCustomAction({
      id: "getScrollback",
      description: descriptions.getScrollback,
      paramSchema: GET_SCROLLBACK_SCHEMA,
      run: async (args) => {
        const raw = args.maxLines;
        // Binding has already refused a non-scalar, so the only thing left to
        // reject is a supplied value that is not a usable line count. Refusing
        // rather than falling back to 500 is deliberate: a caller who asked
        // for `maxLines: "lots"` and silently got 500 cannot tell that their
        // bound was ignored.
        if (raw !== undefined && (typeof raw !== "number" || !Number.isInteger(raw) || raw < 1)) {
          throw new Error("getScrollback: 'maxLines' must be a positive whole number");
        }
        return effects.readScrollback(raw === undefined ? 500 : raw);
      },
    }),
  };
}
