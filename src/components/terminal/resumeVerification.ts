/**
 * Resume-landed verification for the registry-driven boot restore — Phase 3 of
 * `2026-06-12-runner-session-registry-and-restore-hardening` (issue #548).
 *
 * The boot restore types `claude --resume <id>` into a freshly-created plain
 * shell and previously walked away. When the command silently never landed
 * (lost keystrokes into a still-initializing ConPTY, a wedged prompt, …) the
 * pane sat as a bare shell while the backend liveness poll eventually flipped
 * the durable record `poll-dead` — destroying the very state a retry needs.
 *
 * This module closes the loop: after typing the resume command we poll the
 * backend scrollback ring (`terminal_get_scrollback`, the raw PTY byte
 * history) for the Claude UI handshake. On failure we retype the same command
 * ONCE; on persistent failure the tab is parked in an explicit
 * "resume failed — retry" state (see `runVerifiedResume` in
 * `useTerminalInitialization.ts`) instead of pretending the resume worked.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";
import { writeWhenReady, type TerminalRefsMap } from "./writeWhenReady";

/** Strip ANSI escape sequences so patterns match rendered text. */
export function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, "").replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "");
}

/**
 * Markers that the Claude Code TUI actually rendered in this pane — i.e. the
 * resume command landed and the CLI took over the terminal. Deliberately
 * Claude-UI-shaped (rounded input box, status-line hints) rather than "any
 * output grew": a shell prompt echo or an error message must NOT count.
 *
 * The interactive resume-size picker ("Resume from summary?") is itself
 * Claude UI — a pane wedged on it still counts as HANDSHAKE OK here (the
 * resume landed; answering the picker is a separate concern).
 */
const CLAUDE_HANDSHAKE_PATTERNS: RegExp[] = [
  /\? for shortcuts/, // persistent status-line hint under the input box
  /esc to interrupt/i, // shown while Claude is actively working
  /bypass permissions/i, // permission-mode indicator in the status line
  /Welcome (?:back )?to Claude/i, // launch banner
  /[╭╰]─{3,}/, // rounded input-box / dialog frame
];

/** True when the pane's recent output shows the Claude Code UI. */
export function detectClaudeHandshake(text: string): boolean {
  const stripped = stripAnsi(text);
  return CLAUDE_HANDSHAKE_PATTERNS.some((p) => p.test(stripped));
}

/** How many trailing characters of the (decoded) ring to scan per probe. */
const TAIL_SCAN_CHARS = 16_000;

/**
 * Read the tail of a terminal's live scrollback ring as decoded text.
 * Returns `null` when the ring can't be read (terminal gone / IPC failure) —
 * callers treat that as "no evidence yet", never as success.
 */
export async function readScrollbackTail(tabId: string): Promise<string | null> {
  try {
    const resp = await invoke<CommandResponse>("terminal_get_scrollback", {
      terminalId: tabId,
    });
    const encoded = (resp?.data as { data?: string } | undefined)?.data;
    if (typeof encoded !== "string") return null;
    const raw = atob(encoded);
    const decoded = new TextDecoder().decode(Uint8Array.from(raw, (c) => c.charCodeAt(0)));
    return decoded.slice(-TAIL_SCAN_CHARS);
  } catch {
    return null;
  }
}

export interface HandshakeWaitOptions {
  /** Total time to wait for the handshake before giving up. */
  timeoutMs?: number;
  /** Poll interval between scrollback probes. */
  intervalMs?: number;
  /** Injectable probe (tests); defaults to {@link readScrollbackTail}. */
  readTail?: (tabId: string) => Promise<string | null>;
  /**
   * Called once per probe with the decoded tail — hook for picker detection
   * (the "Resume from summary?" answerer) without coupling it in here.
   */
  onProbe?: (tail: string) => void;
}

/**
 * Poll the pane's scrollback until the Claude UI handshake appears or the
 * timeout elapses. Resolves `"verified"` or `"timeout"`; never throws.
 */
export async function waitForClaudeHandshake(
  tabId: string,
  options: HandshakeWaitOptions = {},
): Promise<"verified" | "timeout"> {
  const { timeoutMs = 15_000, intervalMs = 1_000, readTail = readScrollbackTail, onProbe } = options;
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const tail = await readTail(tabId);
    if (tail !== null) {
      onProbe?.(tail);
      if (detectClaudeHandshake(tail)) return "verified";
    }
    if (Date.now() >= deadline) return "timeout";
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

/**
 * ESC clears any partially-typed line in PSReadLine / readline before a
 * retry retype, so a half-landed first attempt can't corrupt the second.
 */
const CLEAR_LINE = "\x1b";

export interface TypeAndVerifyOptions extends HandshakeWaitOptions {
  /** Total attempts (initial type + retries). Spec: retry ONCE → 2. */
  attempts?: number;
  /** Delay between typing and the first probe, and before a retry retype. */
  settleMs?: number;
  /** Injectable writer (tests); defaults to {@link writeWhenReady}. */
  write?: (refs: TerminalRefsMap, tabId: string, text: string) => void;
}

/**
 * Type `resumeCmd` into the tab and verify the Claude UI handshake within the
 * timeout; on failure retype the SAME command once and verify again. Resolves
 * `"verified"` on handshake, `"failed"` after all attempts are exhausted.
 */
export async function typeResumeAndVerify(
  terminalRefs: TerminalRefsMap,
  tabId: string,
  resumeCmd: string,
  options: TypeAndVerifyOptions = {},
): Promise<"verified" | "failed"> {
  const { attempts = 2, settleMs = 500, write = writeWhenReady, ...waitOpts } = options;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    if (attempt > 1) {
      // Clear any half-typed line from the failed attempt, then retype.
      write(terminalRefs, tabId, CLEAR_LINE);
      await new Promise((r) => setTimeout(r, settleMs));
    }
    write(terminalRefs, tabId, resumeCmd);
    await new Promise((r) => setTimeout(r, settleMs));
    const outcome = await waitForClaudeHandshake(tabId, waitOpts);
    if (outcome === "verified") return "verified";
    console.warn(
      `[resumeVerification] handshake not observed for ${tabId} (attempt ${attempt}/${attempts})`,
    );
  }
  return "failed";
}
