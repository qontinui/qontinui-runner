/**
 * The click-resume liveness gate (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 3).
 *
 * On 2026-10-27 a UI Resume click on a conversation that a live claude already
 * held in another pane typed `claude --resume <id>` into a new pane. Two
 * processes then wrote one transcript and edited one worktree. Every operator
 * resume now reads the liveness oracle (`fetchLiveClaudeSessions`) FIRST and
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
 * - `failed` — a resume was attempted and did not come back (refused before a
 *   tab, no tab created, or the handshake did not verify). Not in the plan's
 *   four-value list: without it a failed attempt would have to be counted as
 *   `resumed`, and the bulk summary would claim a resume that did not happen.
 */
export type ResumeOutcome = "resumed" | "skipped-live" | "skipped-unknown" | "cancelled" | "failed";

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
 * its fork: `… --resume <id> --fork-session`. Claude Code ≥ 2.1.312 documents
 * the flag as "When resuming, create a new session ID instead of reusing the
 * original (use with --resume or --continue)".
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
// The click funnel
// ---------------------------------------------------------------------------

type FailedAttempt = Extract<ResumeAttempt, { kind: "failed" }>;

export interface ClickResumeDeps {
  /** The liveness oracle (`fetchLiveClaudeSessions`): rows, or `null` when unreadable. */
  readLive: () => Promise<LiveClaudeSession[] | null>;
  /** Open the tab and type the resume (`startResume` in `useShellIntegration`). */
  start: (target: ResumeTarget, mode: StartResumeMode) => Promise<ResumeAttempt>;
  /** Open the resume-live dialog and resolve with the operator's choice. */
  ask: (prompt: ResumeGatePrompt) => Promise<ResumeGateChoice>;
  /** An attempt that did not come back — surfaced so a click is never silent. */
  onFailed?: (attempt: FailedAttempt, target: ResumeTarget) => void;
  /** An attempt that THREW. */
  onError?: (err: unknown, target: ResumeTarget) => void;
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

async function attempt(
  deps: Pick<ClickResumeDeps, "start" | "onFailed" | "onError">,
  target: ResumeTarget,
  mode: StartResumeMode,
): Promise<ResumeOutcome> {
  let result: ResumeAttempt;
  try {
    result = await deps.start(target, mode);
  } catch (err: unknown) {
    deps.onError?.(err, target);
    return "failed";
  }
  if (result.kind === "verified") return "resumed";
  deps.onFailed?.(result, target);
  return "failed";
}

/**
 * THE operator resume funnel: read liveness, then resume, ask, or skip. Every
 * click-resume (Session Manager, transcript panel, Previous Sessions, the
 * keyboard shortcut) and the bulk "resume selected" end here.
 */
export async function runClickResume(
  deps: ClickResumeDeps,
  target: ResumeTarget,
  opts: ClickResumeOptions = {},
): Promise<ResumeOutcome> {
  const interactive = opts.interactive !== false;
  const decision = decideClickResume(await deps.readLive(), target.claudeSessionId);
  if (decision.kind === "free") {
    return attempt(deps, target, { fork: false, allowLive: false });
  }
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
      // The dialog host navigated to the holder. Nothing was resumed, because
      // the conversation is already live there.
      return decision.kind === "live" && paneHolderOf(holders) ? "skipped-live" : "cancelled";
    case "fork":
      if (!canFork) return "cancelled";
      return attempt(deps, target, {
        fork: true,
        allowLive: true,
        forkTitle: forkTabTitle(conversationName, target.claudeSessionId),
      });
    case "resume-anyway":
      return attempt(deps, target, { fork: false, allowLive: true });
    case "cancel":
    default:
      return "cancelled";
  }
}

// ---------------------------------------------------------------------------
// The restore-failed Retry
// ---------------------------------------------------------------------------

export interface RetryGateDeps {
  readLive: () => Promise<LiveClaudeSession[] | null>;
  ask: (prompt: ResumeGatePrompt) => Promise<ResumeGateChoice>;
  /** Re-run the verified type-and-check into the tab (`runVerifiedResume`). */
  retry: () => Promise<"verified" | "failed">;
}

/**
 * Gate the `ResumeFailedBanner` Retry, which types into its OWN tab through
 * `runVerifiedResume` and so bypasses the click funnel. A row held in this
 * very tab is not another writer — the resume landed after all, and
 * `runVerifiedResume`'s pane probe verifies it without typing — so it is left
 * out of the decision. Fork is not offered: the tab is bound to this id, and
 * its restore record waits on this id's handshake.
 */
export async function runGatedRetry(
  deps: RetryGateDeps,
  args: { tabId: string; claudeSessionId: string; conversationName?: string | null },
): Promise<ResumeOutcome> {
  const rows = await deps.readLive();
  const others = rows === null ? null : rows.filter((r) => r?.terminalId !== args.tabId);
  const decision = decideClickResume(others, args.claudeSessionId);
  const run = async (): Promise<ResumeOutcome> =>
    (await deps.retry()) === "verified" ? "resumed" : "failed";
  if (decision.kind === "free") return run();
  const holders = decision.kind === "live" ? decision.holders : [];
  const choice = await deps.ask({
    kind: decision.kind,
    claudeSessionId: args.claudeSessionId,
    conversationName: conversationNameOf(holders, args.conversationName),
    holders,
    canFork: false,
  });
  if (choice === "resume-anyway") return run();
  if (choice === "go-to-pane" && decision.kind === "live" && paneHolderOf(holders)) {
    return "skipped-live";
  }
  return "cancelled";
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
 * after an item that tried to spawn.
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
      case "failed":
      default:
        tally.failed.push(label);
        break;
    }
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
