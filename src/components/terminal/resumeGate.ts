/**
 * The click-resume liveness gate (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 3).
 *
 * On 2026-10-27 a UI Resume click on a conversation that a live claude already
 * held in another pane typed `claude --resume <id>` into a new pane. Two
 * processes then wrote one transcript and edited one worktree. Every operator
 * resume now reads the liveness oracle (`fetchLiveClaudeHolders`) FIRST and
 * acts on one of three answers:
 *
 * - `free` — no live process holds the id: resume as before.
 * - `live` — some process holds it. An interactive click opens the
 *   resume-live dialog (go to the existing pane / fork / resume anyway /
 *   cancel); a non-interactive caller (bulk) skips it and reports
 *   `skipped-live`.
 * - `unknown` — the registry could not be read. The gate withholds clearance
 *   (D2): the dialog opens in its UNKNOWN form with Cancel as the default, and
 *   a non-interactive caller reports `skipped-unknown`. It never falls through
 *   to a resume that may fork.
 *
 * The decision is the click-side twin of `decideColdResume`
 * (`useTerminalInitialization.ts`), over the same oracle: for the same input,
 * `decideClickResume` is `free` exactly when `decideColdResume` is `respawn`
 * (pinned by `resumeGate.test.ts`).
 */

import { conversationLabel, id8 } from "./paneLabel";
import { hasLiveSessionId, isOperatorNamed, type LiveClaudeSession } from "./liveClaudeSessions";
import type { ResumeAttempt, ResumeTarget } from "./resumeInNewTab";
import { revealTabInLayout, type RevealableZoneLayout } from "./tabReveal";

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

export type ClickResumeDecision =
  | { kind: "free" }
  | { kind: "live"; holders: LiveClaudeSession[] }
  | { kind: "unknown" };

/**
 * Decide a click-resume of `claudeSessionId` from the live registry rows.
 *
 * `rows === null` is an UNREADABLE registry, which is `unknown` — never
 * `free`. A row counts as a holder by the same predicate the id projection
 * uses (`hasLiveSessionId`), so this and `decideColdResume` cannot disagree.
 * Holders inside a runner pane come first, so the dialog's "go to the existing
 * pane" target is `holders[0]` whenever any holder has a pane.
 */
export function decideClickResume(
  rows: readonly LiveClaudeSession[] | null,
  claudeSessionId: string,
): ClickResumeDecision {
  if (rows === null) return { kind: "unknown" };
  const holders = rows.filter((r) => hasLiveSessionId(r) && r.sessionId === claudeSessionId);
  if (holders.length === 0) return { kind: "free" };
  const inPane = holders.filter((h) => !!h.terminalId);
  const outside = holders.filter((h) => !h.terminalId);
  return { kind: "live", holders: [...inPane, ...outside] };
}

/** The first holder that sits in a runner pane, or `null` when none does. */
export function paneHolderOf(holders: readonly LiveClaudeSession[]): LiveClaudeSession | null {
  return holders.find((h) => !!h.terminalId) ?? null;
}

// ---------------------------------------------------------------------------
// Outcomes, prompts and choices
// ---------------------------------------------------------------------------

/**
 * What an operator resume came to. Distinct from `resumeVerification`'s
 * `ResumeOutcome` (`verified` / `failed`), which is the handshake verdict of
 * one typed command.
 *
 * - `resumed` — a tab was opened and its resume (or fork) verified.
 * - `skipped-live` — not resumed because a live process holds the
 *   conversation: a non-interactive caller skipped it, or the operator chose
 *   "go to the existing pane".
 * - `skipped-unknown` — not resumed because liveness could not be read
 *   (non-interactive callers only; a click asks instead).
 * - `cancelled` — the operator cancelled the dialog.
 * - `refused` — a resume was attempted and refused before any tab was opened
 *   (an unsafe id, an account that cannot be typed, or no tab was created).
 *   Nothing was typed.
 * - `failed` — a resume opened a tab and did not come back (the handshake did
 *   not verify, the fork could not be typed), or it threw.
 *
 * `refused` and `failed` are not in the plan's four-value list: without them
 * a failed attempt would have to be counted as `resumed`, and the bulk summary
 * would claim a resume that did not happen. They are told apart because only
 * a `failed` attempt left a tab behind: bulk waits its spawn spacing after
 * that, never after a refusal. A refusal of the pre-type liveness re-read is
 * not either: it reports `skipped-live` / `skipped-unknown`, like the gate.
 */
export type ResumeOutcome =
  | "resumed"
  | "skipped-live"
  | "skipped-unknown"
  | "cancelled"
  | "refused"
  | "failed";

/** The operator's answer to the resume-live dialog. */
export type ResumeGateChoice = "go-to-pane" | "fork" | "resume-anyway" | "cancel";

/** What the resume-live dialog shows. */
export interface ResumeGatePrompt {
  /** `live`: a process holds the conversation. `unknown`: liveness could not be read. */
  kind: "live" | "unknown";
  claudeSessionId: string;
  /** The conversation's name, when one is known. */
  conversationName: string | null;
  /** The live holders (`[]` for `unknown`), runner-pane holders first. */
  holders: LiveClaudeSession[];
  /**
   * Whether "Fork as a new conversation" is offered: a Claude session (the
   * only provider with `--fork-session`) on a path that opens a NEW tab. The
   * restore-failed Retry types into its own bound tab, so it offers no fork.
   */
  canFork: boolean;
}

/** How `startResume` types the resume. */
export interface StartResumeMode {
  /** Type `--resume <id> --fork-session`: a NEW conversation from a copy. */
  fork: boolean;
  /**
   * Skip the pre-type liveness re-read: the operator chose "Resume anyway"
   * (or a fork, which writes a new transcript). Otherwise `resumeInNewTab`
   * re-reads liveness right before typing and refuses a live or unknown id.
   */
  allowLive: boolean;
  /** The fork's tab title (`fork of <name> · <id8>`). Required with `fork`. */
  forkTitle?: string;
}

/** Fork is a Claude-only flag. */
export function canForkTarget(target: Pick<ResumeTarget, "provider">): boolean {
  return !target.provider || target.provider === "claude";
}

/**
 * The conversation's best-known name: an operator-chosen registry name from a
 * live holder (the string in its window, most recently updated first), else
 * the caller's (the transcript's `--resume` name), else `null`.
 */
export function conversationNameOf(
  holders: readonly LiveClaudeSession[],
  fallback: string | null | undefined,
): string | null {
  const named = holders
    .filter((h) => isOperatorNamed(h))
    .sort((a, b) => (b.updatedAt || b.startedAt || 0) - (a.updatedAt || a.startedAt || 0));
  if (named.length > 0) return named[0].name.trim();
  const f = typeof fallback === "string" ? fallback.trim() : "";
  return f.length > 0 ? f : null;
}

/** The fork's tab title: `fork of <name> · <id8>` (or `fork of (unnamed) · <id8>`). */
export function forkTabTitle(name: string | null | undefined, claudeSessionId: string): string {
  return `fork of ${conversationLabel(name, claudeSessionId)}`;
}

/**
 * One line naming where a holder runs, for the dialog: its pane when it sits
 * in one, else its pid, account and working directory.
 */
export function describeHolder(h: LiveClaudeSession): string {
  if (h.terminalId) {
    const page = h.pageId ? ` on page ${id8(h.pageId)}` : "";
    return `runner pane ${id8(h.terminalId)}${page} (pid ${h.pid})`;
  }
  const account = h.account?.label?.trim() ? h.account.label.trim() : "unknown account";
  const dir = h.workingDir?.trim() ? h.workingDir.trim() : "unknown directory";
  return `pid ${h.pid} · account ${account} · ${dir} (outside any runner pane)`;
}

// ---------------------------------------------------------------------------
// The fork argument builder
// ---------------------------------------------------------------------------

export const FORK_SESSION_FLAG = "--fork-session";

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Turn the backend-rendered resume command (`build_ai_resume_command`:
 * `[env…] claude <permission> --resume <id>`, plus the trailing `\r`) into
 * its fork: `… --resume <id> --fork-session`. Claude Code documents the flag
 * (help text checked in 2.1.310–2.1.312) as "When resuming, create a new
 * session ID instead of reusing the original (use with --resume or
 * --continue)".
 *
 * `null` when the command does not END with `--resume <id>` (bare or quoted):
 * then appending a flag would not land on the claude argv, so nothing is
 * typed. A command that already forks is returned unchanged.
 */
export function withForkSession(resumeCmd: string, claudeSessionId: string): string | null {
  const body = resumeCmd.replace(/[\r\n]+$/, "");
  const eol = resumeCmd.slice(body.length);
  const trimmed = body.replace(/\s+$/, "");
  if (!claudeSessionId) return null;
  const id = escapeRegExp(claudeSessionId);
  const forked = new RegExp(`(?:^|\\s)--resume\\s+(['"]?)${id}\\1\\s+${FORK_SESSION_FLAG}$`);
  if (forked.test(trimmed)) return `${trimmed}${eol}`;
  const resume = new RegExp(`(?:^|\\s)--resume\\s+(['"]?)${id}\\1$`);
  if (!resume.test(trimmed)) return null;
  return `${trimmed} ${FORK_SESSION_FLAG}${eol}`;
}

// ---------------------------------------------------------------------------
// Resumes in flight
// ---------------------------------------------------------------------------

/**
 * How long a conversation stays "in flight" after a resume left a tab behind.
 * Claude Code writes its registry file (`<config>/sessions/<pid>.json`) only
 * once the new process is up, so for a few seconds after a verified resume the
 * liveness oracle can still read the id as free — and a second click in that
 * window would start a second writer.
 */
export const RESUME_IN_FLIGHT_GRACE_MS = 5_000;

/**
 * The conversations a resume is in flight for, in THIS window (module scope:
 * one per webview). A resume claims its id BEFORE its first liveness read and
 * holds it until it resolves, plus {@link RESUME_IN_FLIGHT_GRACE_MS} when it
 * opened a tab. Two quick clicks on one free conversation would otherwise both
 * read "free" and both type `--resume`.
 */
const resumesInFlight = new Map<string, object>();

/** A held claim on one conversation id ({@link claimResume}). */
export interface ResumeClaim {
  /** Release the claim, after `graceMs` (default 0). Idempotent. */
  release: (graceMs?: number) => void;
}

/**
 * Claim `claudeSessionId` for one resume in this window, or `null` when a
 * resume of it is already in flight (or inside its grace). A release only
 * ever drops its OWN claim, so a late grace timer cannot free a newer one.
 */
export function claimResume(claudeSessionId: string): ResumeClaim | null {
  if (resumesInFlight.has(claudeSessionId)) return null;
  const token = {};
  resumesInFlight.set(claudeSessionId, token);
  const drop = () => {
    if (resumesInFlight.get(claudeSessionId) === token) resumesInFlight.delete(claudeSessionId);
  };
  let released = false;
  return {
    release: (graceMs = 0) => {
      if (released) return;
      released = true;
      if (graceMs > 0) setTimeout(drop, graceMs);
      else drop();
    },
  };
}

/** Is a resume of `claudeSessionId` in flight (or in its grace) in this window? */
export function isResumeInFlight(claudeSessionId: string): boolean {
  return resumesInFlight.has(claudeSessionId);
}

/** Forget every claim. Tests only: the registry is module state. */
export function clearResumesInFlight(): void {
  resumesInFlight.clear();
}

/**
 * Run one resume `start` under a claim on `target`'s id — the "Since restart"
 * queue's door (`useShellIntegration().resumeTarget`), which types through
 * `resumeInNewTab` without the click funnel. A second resume of an id already
 * in flight is refused as `in-flight` and starts nothing.
 */
export async function runClaimedResume(
  target: Pick<ResumeTarget, "claudeSessionId">,
  start: () => Promise<ResumeAttempt>,
): Promise<ResumeAttempt> {
  const claim = claimResume(target.claudeSessionId);
  if (claim === null) return { kind: "failed", failure: "in-flight", tabId: null };
  let result: ResumeAttempt | undefined;
  try {
    result = await start();
    return result;
  } finally {
    // A throw leaves it unknown whether a tab was opened: keep the grace.
    claim.release(result !== undefined && result.tabId === null ? 0 : RESUME_IN_FLIGHT_GRACE_MS);
  }
}

// ---------------------------------------------------------------------------
// Automatic restores (the boot drain, the zone-profile restore)
// ---------------------------------------------------------------------------

/** The cold-restore liveness verdict (`useTerminalInitialization.decideColdResume`). */
export type RestoreDecision = "respawn" | "skip-alive" | "skip-unknown";

/** What one automatic restore resume came to under the in-flight claim. */
export type ClaimedRestoreResult =
  /** Typing started. `settled` resolves (never rejects) once it is done. */
  | { kind: "typing"; settled: Promise<void> }
  /**
   * Not typed: a live process holds the id, or (`inFlight`) another resume of
   * it — a click, a Retry, the queue, another restore — is in flight in this
   * window.
   */
  | { kind: "skip-alive"; inFlight: boolean }
  /** Not typed: the registry could not be read. */
  | { kind: "skip-unknown" };

/**
 * Type one AUTOMATIC restore resume — the boot cold-restore drain and the
 * zone-profile restore — under the same in-flight claim a click takes
 * ({@link claimResume}). Without it, a click in the seconds after a restore
 * typed `--resume`, before the restored claude has written its registry file,
 * reads the id as free and starts a second writer.
 *
 * - The id is already claimed: `skip-alive` (`inFlight: true`). Nothing is
 *   read or typed — another resume of it is under way in this window.
 * - Otherwise `decide()` reads liveness right before typing. A `skip-*`
 *   verdict releases the claim at once: nothing was typed.
 * - `respawn`: `type()` starts and is NOT awaited (one verification can take
 *   ~30 s, and the drain must not serialize on it). The claim is held until
 *   it settles, then for {@link RESUME_IN_FLIGHT_GRACE_MS}: the restore's tab
 *   already exists, and its claude registers only once it is up. A `type()`
 *   that throws is logged, and keeps the grace — nobody knows what it typed.
 */
export async function runClaimedRestoreResume(
  claudeSessionId: string,
  decide: () => Promise<RestoreDecision>,
  type: () => Promise<unknown>,
): Promise<ClaimedRestoreResult> {
  const claim = claimResume(claudeSessionId);
  if (claim === null) return { kind: "skip-alive", inFlight: true };
  let decision: RestoreDecision;
  try {
    decision = await decide();
  } catch (err: unknown) {
    claim.release();
    throw err;
  }
  if (decision === "skip-alive") {
    claim.release();
    return { kind: "skip-alive", inFlight: false };
  }
  if (decision === "skip-unknown") {
    claim.release();
    return { kind: "skip-unknown" };
  }
  const settled = (async () => {
    try {
      await type();
    } catch (err: unknown) {
      console.warn(`[resumeGate] restore resume of ${claudeSessionId} threw:`, err);
    } finally {
      claim.release(RESUME_IN_FLIGHT_GRACE_MS);
    }
  })();
  return { kind: "typing", settled };
}

// ---------------------------------------------------------------------------
// The click funnel
// ---------------------------------------------------------------------------

type FailedAttempt = Extract<ResumeAttempt, { kind: "failed" }>;

/**
 * How long the gate waits for its liveness read. The id's in-flight claim is
 * held across the read, so a read that never answers would lock the id until
 * the window reloads.
 */
export const GATE_LIVENESS_READ_TIMEOUT_MS = 15_000;

/**
 * The gate's liveness read, bounded by {@link GATE_LIVENESS_READ_TIMEOUT_MS}.
 * A read that does not answer in time, or that throws, is `null`: liveness
 * UNKNOWN (D2), which asks an interactive click and skips a bulk item — never
 * a resume.
 */
export async function readLiveWithin(
  read: () => Promise<LiveClaudeSession[] | null>,
  timeoutMs: number = GATE_LIVENESS_READ_TIMEOUT_MS,
): Promise<LiveClaudeSession[] | null> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timedOut = new Promise<null>((resolve) => {
    timer = setTimeout(() => {
      console.warn(`[resumeGate] liveness read gave no answer in ${timeoutMs} ms: UNKNOWN`);
      resolve(null);
    }, timeoutMs);
  });
  const answered = (async () => {
    try {
      return await read();
    } catch (err: unknown) {
      console.warn("[resumeGate] liveness read threw: UNKNOWN", err);
      return null;
    }
  })();
  try {
    return await Promise.race([answered, timedOut]);
  } finally {
    clearTimeout(timer);
  }
}

export interface ClickResumeDeps {
  /** The liveness oracle (`fetchLiveClaudeHolders`): rows, or `null` when unreadable. */
  readLive: () => Promise<LiveClaudeSession[] | null>;
  /** Open the tab and type the resume (`startResume` in `useShellIntegration`). */
  start: (target: ResumeTarget, mode: StartResumeMode) => Promise<ResumeAttempt>;
  /** Open the resume-live dialog and resolve with the operator's choice. */
  ask: (prompt: ResumeGatePrompt) => Promise<ResumeGateChoice>;
  /** An attempt that did not come back — surfaced so a click is never silent. */
  onFailed?: (attempt: FailedAttempt, target: ResumeTarget) => void;
  /** An attempt that THREW. */
  onError?: (err: unknown, target: ResumeTarget) => void;
  /**
   * The immediate acknowledgement of an interactive click: liveness is being
   * read. Called before the first read, so a slow read is never a dead click.
   */
  onChecking?: (target: ResumeTarget) => void;
  /**
   * The liveness read of an interactive click returned (or timed out): what
   * {@link onChecking} said is no longer true, so its notice can be withdrawn.
   * Called before the dialog opens or the resume starts.
   */
  onChecked?: (target: ResumeTarget) => void;
  /** The click was refused because a resume of the same id is already in flight. */
  onInFlight?: (target: ResumeTarget) => void;
}

export interface ClickResumeOptions {
  /**
   * `false` (bulk): never open the dialog — a live id returns `skipped-live`
   * and an unreadable registry `skipped-unknown`. Default `true`.
   */
  interactive?: boolean;
  /** The conversation's name from the caller (the transcript's `--resume` name). */
  conversationName?: string | null;
}

/**
 * What a failed {@link ResumeAttempt} comes to. A refusal of the pre-type
 * liveness re-read is the gate's own answer, read late (`already-running` →
 * `skipped-live`, `liveness-unknown` → `skipped-unknown`), as is a resume of
 * an id already in flight; any other failure is `refused` when no tab was
 * opened and `failed` when one was.
 */
export function outcomeOfFailedAttempt(a: FailedAttempt): ResumeOutcome {
  switch (a.failure) {
    case "already-running":
    case "in-flight":
      return "skipped-live";
    case "liveness-unknown":
      return "skipped-unknown";
    default:
      return a.tabId === null ? "refused" : "failed";
  }
}

/** One started attempt: its outcome, and whether it may have left a tab behind. */
interface AttemptResult {
  outcome: ResumeOutcome;
  /** A tab was opened — or the start threw, and nobody knows. */
  openedTab: boolean;
}

async function attempt(
  deps: Pick<ClickResumeDeps, "start" | "onFailed" | "onError">,
  target: ResumeTarget,
  mode: StartResumeMode,
): Promise<AttemptResult> {
  let result: ResumeAttempt;
  try {
    result = await deps.start(target, mode);
  } catch (err: unknown) {
    deps.onError?.(err, target);
    return { outcome: "failed", openedTab: true };
  }
  if (result.kind === "verified") return { outcome: "resumed", openedTab: true };
  deps.onFailed?.(result, target);
  return { outcome: outcomeOfFailedAttempt(result), openedTab: result.tabId !== null };
}

/**
 * THE operator resume funnel: read liveness, then resume, ask, or skip. Every
 * click-resume (Session Manager, transcript panel, Previous Sessions, the
 * keyboard shortcut) and the bulk "resume selected" end here.
 *
 * The id is claimed ({@link claimResume}) BEFORE the first liveness read and
 * held until the resume resolves, plus {@link RESUME_IN_FLIGHT_GRACE_MS} when
 * it opened a tab. A second request for an id in flight starts nothing and
 * reports `skipped-live` (`onInFlight` says why). The read is bounded
 * ({@link readLiveWithin}), and the dialog is answered `cancel` when its page
 * stops showing it (`ResumeGateHost`) or its page unmounts
 * (`useShellIntegration`), so neither can hold the claim forever.
 */
export async function runClickResume(
  deps: ClickResumeDeps,
  target: ResumeTarget,
  opts: ClickResumeOptions = {},
): Promise<ResumeOutcome> {
  const interactive = opts.interactive !== false;
  const claim = claimResume(target.claudeSessionId);
  if (claim === null) {
    deps.onInFlight?.(target);
    return "skipped-live";
  }
  const started: { openedTab: boolean } = { openedTab: false };
  const run = async (mode: StartResumeMode): Promise<ResumeOutcome> => {
    const r = await attempt(deps, target, mode);
    started.openedTab = r.openedTab;
    return r.outcome;
  };
  try {
    if (interactive) deps.onChecking?.(target);
    const rows = await readLiveWithin(deps.readLive);
    if (interactive) deps.onChecked?.(target);
    const decision = decideClickResume(rows, target.claudeSessionId);
    if (decision.kind === "free") return await run({ fork: false, allowLive: false });
    if (!interactive) return decision.kind === "live" ? "skipped-live" : "skipped-unknown";

    const holders = decision.kind === "live" ? decision.holders : [];
    const conversationName = conversationNameOf(holders, opts.conversationName);
    const canFork = canForkTarget(target);
    const choice = await deps.ask({
      kind: decision.kind,
      claudeSessionId: target.claudeSessionId,
      conversationName,
      holders,
      canFork,
    });
    switch (choice) {
      case "go-to-pane":
        // The dialog host navigated to the holder. Nothing was resumed,
        // because the conversation is already live there.
        return decision.kind === "live" && paneHolderOf(holders) ? "skipped-live" : "cancelled";
      case "fork":
        if (!canFork) return "cancelled";
        return await run({
          fork: true,
          allowLive: true,
          forkTitle: forkTabTitle(conversationName, target.claudeSessionId),
        });
      case "resume-anyway":
        return await run({ fork: false, allowLive: true });
      case "cancel":
      default:
        return "cancelled";
    }
  } finally {
    claim.release(started.openedTab ? RESUME_IN_FLIGHT_GRACE_MS : 0);
  }
}

// ---------------------------------------------------------------------------
// The restore-failed Retry
// ---------------------------------------------------------------------------

export interface RetryGateDeps {
  /** The gate's read WITH holding panes (`fetchLiveClaudeHolders`). */
  readLive: () => Promise<LiveClaudeSession[] | null>;
  ask: (prompt: ResumeGatePrompt) => Promise<ResumeGateChoice>;
  /** Re-run the verified type-and-check into the tab (`runVerifiedResume`). */
  retry: () => Promise<"verified" | "failed">;
  /** The Retry was refused because a resume of the same id is already in flight. */
  onInFlight?: () => void;
}

/**
 * Gate the `ResumeFailedBanner` Retry, which types into its OWN tab through
 * `runVerifiedResume` and so bypasses the click funnel. A row held in this
 * very tab is not another writer — the resume landed after all, and
 * `runVerifiedResume`'s pane probe verifies it without typing — so it is left
 * out of the decision. Fork is not offered: the tab is bound to this id, and
 * its restore record waits on this id's handshake.
 *
 * Held under the same in-flight claim as a click ({@link claimResume}), so a
 * Retry and a click on one conversation cannot both type `--resume` while its
 * registry file is still being written.
 */
export async function runGatedRetry(
  deps: RetryGateDeps,
  args: { tabId: string; claudeSessionId: string; conversationName?: string | null },
): Promise<ResumeOutcome> {
  const claim = claimResume(args.claudeSessionId);
  if (claim === null) {
    deps.onInFlight?.();
    return "skipped-live";
  }
  const typed: { into: boolean } = { into: false };
  const run = async (): Promise<ResumeOutcome> => {
    typed.into = true;
    return (await deps.retry()) === "verified" ? "resumed" : "failed";
  };
  try {
    const rows = await readLiveWithin(deps.readLive);
    const others = rows === null ? null : rows.filter((r) => r?.terminalId !== args.tabId);
    const decision = decideClickResume(others, args.claudeSessionId);
    if (decision.kind === "free") return await run();
    const holders = decision.kind === "live" ? decision.holders : [];
    const choice = await deps.ask({
      kind: decision.kind,
      claudeSessionId: args.claudeSessionId,
      conversationName: conversationNameOf(holders, args.conversationName),
      holders,
      canFork: false,
    });
    if (choice === "resume-anyway") return await run();
    if (choice === "go-to-pane" && decision.kind === "live" && paneHolderOf(holders)) {
      return "skipped-live";
    }
    return "cancelled";
  } finally {
    claim.release(typed.into ? RESUME_IN_FLIGHT_GRACE_MS : 0);
  }
}

// ---------------------------------------------------------------------------
// Bulk "resume selected"
// ---------------------------------------------------------------------------

export interface BulkResumeTally {
  resumed: string[];
  live: string[];
  unknown: string[];
  failed: string[];
  cancelled: string[];
}

/** The gap between two spawns: lets each new tab initialize before the next. */
export const BULK_RESUME_SPACING_MS = 500;

/**
 * Resume `items` one at a time through the NON-interactive funnel, and tally
 * what each came to under its label. The funnel's per-item liveness read is
 * the pre-type re-read the spacing needs: an id that went live while an
 * earlier item was resuming is skipped, not forked. The spacing is waited only
 * after an item that opened a tab (`resumed` / `failed`) — never after a skip
 * or a refusal, which left no tab to initialize. A `refused` item is tallied
 * under `failed`: both are resumes that did not happen.
 */
export async function runBulkResume<T>(
  items: readonly T[],
  labelOf: (item: T) => string,
  resumeOne: (item: T) => Promise<ResumeOutcome>,
  opts: { spacingMs?: number; sleep?: (ms: number) => Promise<void> } = {},
): Promise<BulkResumeTally> {
  const spacingMs = opts.spacingMs ?? BULK_RESUME_SPACING_MS;
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const tally: BulkResumeTally = { resumed: [], live: [], unknown: [], failed: [], cancelled: [] };
  let spawned = false;
  for (const item of items) {
    if (spawned && spacingMs > 0) await sleep(spacingMs);
    let outcome: ResumeOutcome;
    try {
      outcome = await resumeOne(item);
    } catch (err: unknown) {
      console.warn("[resumeGate] bulk resume item threw:", err);
      outcome = "failed";
    }
    const label = labelOf(item);
    switch (outcome) {
      case "resumed":
        tally.resumed.push(label);
        break;
      case "skipped-live":
        tally.live.push(label);
        break;
      case "skipped-unknown":
        tally.unknown.push(label);
        break;
      case "cancelled":
        tally.cancelled.push(label);
        break;
      case "refused":
      case "failed":
      default:
        tally.failed.push(label);
        break;
    }
    // Only an outcome that left a tab behind (a throw counts: it is `failed`).
    spawned = outcome === "resumed" || outcome === "failed";
  }
  return tally;
}

/**
 * The acknowledged summary of a bulk resume — `N resumed, M already live:
 * <names>, K liveness unknown: <names>, J failed: <names>` — or `null` when
 * nothing was asked. `success` only when every item resumed.
 */
export function bulkResumeSummary(
  t: BulkResumeTally,
): { message: string; type: "success" | "error" } | null {
  const total =
    t.resumed.length + t.live.length + t.unknown.length + t.failed.length + t.cancelled.length;
  if (total === 0) return null;
  const parts = [`${t.resumed.length} resumed`];
  if (t.live.length > 0) parts.push(`${t.live.length} already live: ${t.live.join(", ")}`);
  if (t.unknown.length > 0) {
    parts.push(`${t.unknown.length} liveness unknown: ${t.unknown.join(", ")}`);
  }
  if (t.failed.length > 0) parts.push(`${t.failed.length} failed: ${t.failed.join(", ")}`);
  if (t.cancelled.length > 0) parts.push(`${t.cancelled.length} cancelled`);
  return {
    message: `Resume selected — ${parts.join(", ")}`,
    type: t.resumed.length === total ? "success" : "error",
  };
}

// ---------------------------------------------------------------------------
// "Go to the existing pane"
// ---------------------------------------------------------------------------

export interface GoToPaneDeps {
  /** The page the operator is viewing. */
  currentPageId: string;
  /** Any page's session value in THIS window (`usePageSessionGetter`). */
  getPage: (
    pageId: string,
  ) => { tabs: ReadonlyArray<{ id: string }>; zoneLayout: RevealableZoneLayout } | null;
  /** Activate a page (the `terminal-open-page` event). */
  openPage: (pageId: string) => void;
  /** Visibly flash the pane (`useStateTransitionEffects().flashTab`). */
  flash: (tabId: string) => void;
}

/**
 * "Go to the existing pane": switch to the holder's page, focus its pane and
 * flash it. `not-found` when the pane is not in this window (a pop-out, or a
 * page this window does not mount) — the caller says so rather than silently
 * doing nothing.
 */
export function goToHolderPane(
  deps: GoToPaneDeps,
  holder: Pick<LiveClaudeSession, "terminalId" | "pageId">,
): "revealed" | "not-found" {
  const tabId = holder.terminalId;
  if (!tabId) return "not-found";
  const pageId = holder.pageId ?? deps.currentPageId;
  const page = deps.getPage(pageId);
  if (!page || !page.tabs.some((t) => t.id === tabId)) return "not-found";
  if (pageId !== deps.currentPageId) deps.openPage(pageId);
  revealTabInLayout(page.zoneLayout, tabId);
  deps.flash(tabId);
  return "revealed";
}
