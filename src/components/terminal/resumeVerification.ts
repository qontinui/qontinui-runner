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
import { instanceStorage } from "@/lib/instance-storage";
import type { CommandResponse } from "./types";
import type { HandshakePatterns } from "./providerAdapter";
import { writeWhenReady, type TerminalRefsMap } from "./writeWhenReady";

// ---------------------------------------------------------------------------
// "Resume from summary?" picker policy (#548 item 3)
// ---------------------------------------------------------------------------

/**
 * What an unattended resume should do when the Claude CLI offers the
 * resume-size picker for a large session:
 *
 * - `"full"` (DEFAULT): resume the full conversation as-is. Context loss is
 *   silent and unrecoverable; token cost is visible and the operator can
 *   `/compact` afterwards. NEVER default to "summary".
 * - `"summary"`: opt-in for cost-sensitive setups — accept the CLI's
 *   "Resume from summary (recommended)" option.
 *
 * Stored under the instance-scoped `resume-summary-policy` key (Settings →
 * Advanced). Any value other than the literal `"summary"` resolves to
 * `"full"`.
 */
export type ResumeSummaryPolicy = "full" | "summary";

export function getResumeSummaryPolicy(): ResumeSummaryPolicy {
  try {
    return instanceStorage.getItem("resume-summary-policy") === "summary" ? "summary" : "full";
  } catch {
    return "full";
  }
}

/**
 * Picker frames from Claude Code's resume-size prompt (verified against the
 * v2.1.175 CLI bundle: options are "Resume from summary (recommended)",
 * "Resume full session as-is", "Don't ask me again"). Matched against
 * ANSI-stripped text.
 */
const RESUME_PICKER_PATTERNS: RegExp[] = [
  /Resume from summary/i,
  /Resume full session as-is/i,
];

/** True when the pane is showing the CLI's resume-size picker. */
export function detectResumePicker(text: string): boolean {
  const stripped = stripAnsi(text);
  return RESUME_PICKER_PATTERNS.some((p) => p.test(stripped));
}

/**
 * Keystrokes that answer the picker for a policy. The CLI's select lists
 * accept number-key selection; option order in v2.1.175 is
 * 1) summary (recommended), 2) full as-is, 3) don't ask again. The trailing
 * Enter is harmless if the digit already submitted (it lands on the now-open
 * Claude input as an empty submit).
 */
export function buildPickerAnswer(policy: ResumeSummaryPolicy): string {
  return policy === "summary" ? "1\r" : "2\r";
}

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

/**
 * Definitive evidence that the REQUESTED session did NOT resume — evaluated
 * BEFORE the handshake patterns on every probe. "The Claude TUI appeared" is
 * not the same claim as "the requested session resumed": a
 * `claude --resume <bogus-id>` can fall through to an error dialog, a fresh
 * session, or the interactive session picker, all of which still render
 * Claude-UI frames and previously read as a VERIFIED restore (boot-restore
 * remediation item 4). Matching any of these parks the tab `resumeFailed`
 * with the durable restore-pending marker kept.
 *
 * Wording sourced from the Claude Code CLI's unknown-session error and the
 * `--resume` session-picker frames; extend from real scrollback if a real
 * failed resume ever verifies (same maintenance rule as the picker patterns
 * above).
 */
const RESUME_FAILURE_PATTERNS: RegExp[] = [
  /No conversation found/i, // `--resume <unknown-id>` error
  /No conversations? (?:found|to resume)/i, // empty-history variants
  /Select a (?:session|conversation) to resume/i, // interactive session picker frame
];

/**
 * True when the pane's recent output shows a definitive resume FAILURE
 * (unknown session id / fell through to the session picker). Checked before
 * {@link detectClaudeHandshake} — failure frames are themselves provider UI.
 *
 * Phase 4 (provider-agnostic): when `patterns` is supplied (the per-adapter
 * `HandshakePatterns` from the provider descriptor), its `failure` substrings
 * are matched (case-insensitive) on the ANSI-stripped text. Omitting it falls
 * back to the built-in Claude regex set — the live default for Claude restores.
 */
export function detectResumeFailure(text: string, patterns?: HandshakePatterns): boolean {
  const stripped = stripAnsi(text);
  if (patterns) return matchSubstrings(stripped, patterns.failure);
  return RESUME_FAILURE_PATTERNS.some((p) => p.test(stripped));
}

/**
 * True when the pane's recent output shows the provider's resume handshake.
 * With `patterns` supplied, matches its `success` substrings (case-insensitive)
 * on ANSI-stripped text; otherwise falls back to the built-in Claude regex set.
 */
export function detectClaudeHandshake(text: string, patterns?: HandshakePatterns): boolean {
  const stripped = stripAnsi(text);
  if (patterns) return matchSubstrings(stripped, patterns.success);
  return CLAUDE_HANDSHAKE_PATTERNS.some((p) => p.test(stripped));
}

/** Case-insensitive substring match of any pattern in `text`. */
function matchSubstrings(text: string, substrings: string[]): boolean {
  if (substrings.length === 0) return false;
  const lower = text.toLowerCase();
  return substrings.some((s) => s.length > 0 && lower.includes(s.toLowerCase()));
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
  /**
   * Per-adapter resume success/failure substrings (Phase 4). When set, the
   * detection uses the provider descriptor's patterns instead of the built-in
   * Claude regex sets — so a Gemini resume verifies against Gemini's banners.
   * Omit to use the Claude default.
   */
  handshakePatterns?: HandshakePatterns;
}

/**
 * Poll the pane's scrollback until the Claude UI handshake appears, a
 * definitive resume failure shows, or the timeout elapses. Resolves
 * `"verified"`, `"failed"` (negative patterns — checked FIRST, since failure
 * frames are themselves Claude UI), or `"timeout"`; never throws.
 */
export async function waitForClaudeHandshake(
  tabId: string,
  options: HandshakeWaitOptions = {},
): Promise<"verified" | "failed" | "timeout"> {
  const {
    timeoutMs = 15_000,
    intervalMs = 1_000,
    readTail = readScrollbackTail,
    onProbe,
    handshakePatterns,
  } = options;
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const tail = await readTail(tabId);
    if (tail !== null) {
      onProbe?.(tail);
      // Negative evidence wins over positive: an error dialog / session
      // picker renders TUI frames too, so the handshake check alone would
      // false-positive exactly the wrong-content case being fixed.
      if (detectResumeFailure(tail, handshakePatterns)) return "failed";
      if (detectClaudeHandshake(tail, handshakePatterns)) return "verified";
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
  /**
   * Keystrokes to type (at most ONCE per call) when a probe shows the CLI's
   * "Resume from summary?" picker — see {@link buildPickerAnswer}. This is
   * the FALLBACK answerer: the typed resume command already suppresses the
   * picker via env thresholds under the default full-resume policy (see
   * `buildResumeCmd`), so this only fires on CLI version drift or under the
   * opt-in "summary" policy. Omit to disable.
   */
  pickerAnswer?: string;
}

/**
 * Type `resumeCmd` into the tab and verify the Claude UI handshake within the
 * timeout; on failure retype the SAME command once and verify again. Resolves
 * `"verified"` on handshake, `"failed"` after all attempts are exhausted.
 *
 * When `pickerAnswer` is set and a probe shows the resume-size picker, the
 * answer is typed once so an unattended resume can't wedge on it. The picker
 * is itself Claude UI, so the same probe also verifies the handshake.
 */
export async function typeResumeAndVerify(
  terminalRefs: TerminalRefsMap,
  tabId: string,
  resumeCmd: string,
  options: TypeAndVerifyOptions = {},
): Promise<"verified" | "failed"> {
  const {
    attempts = 2,
    settleMs = 500,
    write = writeWhenReady,
    pickerAnswer,
    onProbe,
    ...waitOpts
  } = options;
  let pickerAnswered = false;
  const probe = (tail: string) => {
    onProbe?.(tail);
    if (pickerAnswer && !pickerAnswered && detectResumePicker(tail)) {
      pickerAnswered = true;
      write(terminalRefs, tabId, pickerAnswer);
    }
  };
  for (let attempt = 1; attempt <= attempts; attempt++) {
    if (attempt > 1) {
      // Clear any half-typed line from the failed attempt, then retype.
      write(terminalRefs, tabId, CLEAR_LINE);
      await new Promise((r) => setTimeout(r, settleMs));
    }
    write(terminalRefs, tabId, resumeCmd);
    await new Promise((r) => setTimeout(r, settleMs));
    const outcome = await waitForClaudeHandshake(tabId, { ...waitOpts, onProbe: probe });
    if (outcome === "verified") return "verified";
    if (outcome === "failed") {
      // Definitive negative evidence (unknown session id / session picker):
      // retyping the same command can only reproduce it, and the failure
      // frames persist in the scrollback tail anyway — fail fast. The retry
      // exists for LOST keystrokes (timeout), not for a CLI that answered.
      console.warn(
        `[resumeVerification] resume FAILURE frames observed for ${tabId} (attempt ${attempt}/${attempts})`,
      );
      return "failed";
    }
    console.warn(
      `[resumeVerification] handshake not observed for ${tabId} (attempt ${attempt}/${attempts})`,
    );
  }
  return "failed";
}
