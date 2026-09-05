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
 * runner's scrollback ring (the raw PTY byte history, read through
 * `readLocalScrollbackRing`) for the Claude UI handshake. On failure we retype
 * the same command ONCE; on persistent failure the tab is parked in an explicit
 * "resume failed — retry" state (see `runVerifiedResume` in
 * `useTerminalInitialization.ts`) instead of pretending the resume worked.
 */

import { instanceStorage } from "@/lib/instance-storage";
import { readLocalScrollbackRing } from "./backends/localScrollbackRing";
import {
  CLAUDE_HANDSHAKE_REGEXES,
  CLAUDE_RESUME_FAILURE_REGEXES,
  type HandshakePatterns,
} from "./providerAdapter";
import { writeWhenReady, type TerminalRefsMap } from "./writeWhenReady";
import { TERMINAL_EXITED, type TerminalWriteResult } from "./terminalWriteResult";

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
const RESUME_PICKER_PATTERNS: RegExp[] = [/Resume from summary/i, /Resume full session as-is/i];

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
 * True when the pane's recent output shows a definitive resume FAILURE
 * (unknown session id / fell through to the session picker). Checked before
 * {@link detectClaudeHandshake} — failure frames are themselves provider UI.
 *
 * Matches the UNION of the descriptor's `failure` substrings and its
 * `failurePatterns` regexes; with no descriptor it falls back to Claude's own
 * regex set. See {@link HandshakePatterns} for why the union (and not the
 * former `if (patterns) return substringsOnly`) is the correct shape: the
 * boot-restore path always supplies a descriptor, so an either/or made every
 * regex marker dead in production.
 */
export function detectResumeFailure(text: string, patterns?: HandshakePatterns): boolean {
  const stripped = stripAnsi(text);
  if (patterns) {
    return (
      matchSubstrings(stripped, patterns.failure) ||
      matchRegexes(stripped, patterns.failurePatterns)
    );
  }
  return matchRegexes(stripped, CLAUDE_RESUME_FAILURE_REGEXES);
}

/**
 * True when the pane's recent output shows the provider's resume handshake.
 * Same union rule as {@link detectResumeFailure}: descriptor substrings ∪
 * descriptor regexes, falling back to Claude's regex set when no descriptor is
 * supplied. The Claude descriptor's regexes carry the rounded input-box frame
 * marker, which no substring can express — a restored pane that has painted
 * only the frame verifies here.
 */
export function detectClaudeHandshake(text: string, patterns?: HandshakePatterns): boolean {
  const stripped = stripAnsi(text);
  if (patterns) {
    return (
      matchSubstrings(stripped, patterns.success) ||
      matchRegexes(stripped, patterns.successPatterns)
    );
  }
  return matchRegexes(stripped, CLAUDE_HANDSHAKE_REGEXES);
}

/** Case-insensitive substring match of any pattern in `text`. */
function matchSubstrings(text: string, substrings: string[]): boolean {
  if (substrings.length === 0) return false;
  const lower = text.toLowerCase();
  return substrings.some((s) => s.length > 0 && lower.includes(s.toLowerCase()));
}

/** Regex match of any pattern in `text`. An absent/empty set never matches. */
function matchRegexes(text: string, patterns?: RegExp[]): boolean {
  if (!patterns || patterns.length === 0) return false;
  return patterns.some((p) => p.test(text));
}

/** How many trailing characters of the (decoded) ring to scan per probe. */
const TAIL_SCAN_CHARS = 16_000;

/**
 * Read the tail of a terminal's live scrollback ring as decoded text.
 * Returns `null` when the ring can't be read (terminal gone / IPC failure) —
 * callers treat that as "no evidence yet", never as success.
 *
 * Reads the LOCAL ring module rather than `ITerminalBackend.readScrollbackRing`
 * because this probe holds no backend: it is addressed by tab id from the boot
 * restore (`useTerminalInitialization`), whose `TerminalRefsMap` carries
 * `TerminalInstanceHandle`s only, and the tab may be virtualized away while
 * the poll runs. The injectable `readTail` option is the seam tests use.
 */
export async function readScrollbackTail(tabId: string): Promise<string | null> {
  try {
    const ring = await readLocalScrollbackRing(tabId);
    if (!ring) return null;
    const decoded = new TextDecoder().decode(ring.bytes);
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
  /**
   * Injectable writer (tests); defaults to {@link writeWhenReady}.
   *
   * Resolving a {@link TerminalWriteResult} is what lets this loop tell a
   * write that reached the PTY from one refused with `TERMINAL_EXITED` /
   * `TERMINAL_WRITE_FAILED`. A doubled writer that resolves `void` (or
   * returns nothing at all) is treated as NO INFORMATION and the loop behaves
   * exactly as it did before — only an explicit failure envelope changes the
   * outcome, so a test double never has to fabricate a success envelope.
   */
  write?: (
    refs: TerminalRefsMap,
    tabId: string,
    text: string,
  ) => Promise<TerminalWriteResult> | TerminalWriteResult | void;
  /**
   * Called with the typed envelope when a resume write is REFUSED. The loop
   * already short-circuits on it; this is the hook that lets the caller log or
   * surface the reason instead of the generic "handshake not observed".
   */
  onWriteFailure?: (failure: Extract<TerminalWriteResult, { success: false }>) => void;
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
 *
 * REFUSED WRITES short-circuit. The writer now resolves a
 * {@link TerminalWriteResult}; a `TERMINAL_EXITED` envelope fails immediately
 * (no process to hand-shake with, and a retype cannot resurrect the pty) and a
 * `TERMINAL_WRITE_FAILED` one skips straight to the retype. Either way the
 * ~31 s scrollback poll is not spent re-deriving a diagnosis the write already
 * returned — see `writeWhenReady`'s header for the defect this closes.
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
    onWriteFailure,
    ...waitOpts
  } = options;
  let pickerAnswered = false;
  const probe = (tail: string) => {
    onProbe?.(tail);
    if (pickerAnswer && !pickerAnswered && detectResumePicker(tail)) {
      pickerAnswered = true;
      void write(terminalRefs, tabId, pickerAnswer);
    }
  };
  /**
   * Type `text` and report the REFUSAL envelope, if any. `null` means "no
   * refusal" — either the write landed or the injected writer resolved
   * nothing (no information; treated as landed, the pre-envelope behaviour).
   */
  const typeAndCheck = async (
    text: string,
  ): Promise<Extract<TerminalWriteResult, { success: false }> | null> => {
    const result = await write(terminalRefs, tabId, text);
    if (result && result.success === false) {
      onWriteFailure?.(result);
      console.warn(
        `[resumeVerification] resume write REFUSED for ${tabId}: ${result.error} ${result.hint}`,
      );
      return result;
    }
    return null;
  };
  for (let attempt = 1; attempt <= attempts; attempt++) {
    if (attempt > 1) {
      // Clear any half-typed line from the failed attempt, then retype.
      void write(terminalRefs, tabId, CLEAR_LINE);
      await new Promise((r) => setTimeout(r, settleMs));
    }
    const refusal = await typeAndCheck(resumeCmd);
    if (refusal) {
      // The write never reached a process, so there is nothing to hand-shake
      // WITH: polling the scrollback for up to `timeoutMs` (twice) would spend
      // the whole ~31 s budget confirming what the envelope already said.
      //
      // `TERMINAL_EXITED` is terminal — the pty is gone and a retype cannot
      // change that, so fail immediately. `TERMINAL_WRITE_FAILED` is an IPC
      // failure against a pane that is NOT marked exited, which is exactly the
      // transient the retry exists for: skip this attempt's poll and retype.
      if (refusal.code === TERMINAL_EXITED || attempt === attempts) return "failed";
      continue;
    }
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
