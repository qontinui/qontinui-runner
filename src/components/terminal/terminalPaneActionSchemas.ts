/**
 * terminalPaneActionSchemas — the argument declarations of the terminal pane's
 * parameterised custom actions, shared by BOTH paths that serve them.
 *
 * `terminal-input-<id>` is registered by whichever of two components currently
 * owns it: `TerminalInstance` when the pane is mounted, and
 * `TerminalBridgeProxies` when flow-grid virtualization has mounted nothing.
 * The two deliberately differ in HOW they reach the PTY (a live xterm backend
 * vs. the id-addressed runner routes), but a caller cannot tell which one it is
 * talking to — so the set of keys each action ACCEPTS must be one declaration,
 * not two hand-written copies that can drift the way `sendKeys` once did
 * (translated on one path, raw on the other).
 *
 * Each handler binds through `guardedHandler(id, SCHEMA, run,
 * { valuesCheckedBy: "handler" })`: the guard refuses a non-object bag and any
 * undeclared key (neither path did, so `{text: "ls\r", zzz: 1}` was a
 * successful write that silently dropped `zzz`), and every VALUE still goes to
 * the typed validator that owns it — `toPtySequence`, `requireTextPayload`,
 * `requireMaxLines` — with its machine-readable code unchanged.
 *
 * A leaf module (no imports) so tests can read it under the node environment,
 * where `TerminalInstance.tsx` cannot be imported at all.
 *
 * The idea of one definition for both paths comes from qontinui-runner#1301's
 * `terminalPaneCustomActions.ts`; main had meanwhile made the two paths'
 * EFFECTS legitimately different (bracketed-paste read by id, focus/blur
 * refusals, per-path `effect` rationale), so what is shared here is the part
 * that must never differ: the accepted arguments.
 */

/** `keys`: a raw string, an array of key names, or the SDK's descriptor array. */
export const SEND_KEYS_SCHEMA = {
  keys: 'string | string[] | Array<{key, modifiers}> (e.g. "ls\\r", ["Enter"], [{key:"c",modifiers:{ctrl:true}}])',
} as const;

/** Shared by `writeToTerminal` and `pasteText` — same field, same contract. */
export const TEXT_PAYLOAD_SCHEMA = {
  text: "string (required)",
} as const;

export const GET_SCROLLBACK_SCHEMA = {
  maxLines: "positive integer (optional, defaults to 500)",
} as const;
