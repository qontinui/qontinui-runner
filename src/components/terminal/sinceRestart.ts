/**
 * The "Since restart" roster — pure model (plan
 * `2026-10-04-runner-session-roster-restore-picker`, Phases 4-5).
 *
 * Everything the strip and the review panel DECIDE lives here, so it is tested
 * without React or Tauri (the runner's vitest env is `node`): which generation
 * to review, what each row says, which rows are pre-checked, what the strip
 * says, the sequential resume queue, and a Finish/Unfinish row's state.
 *
 * The source is the session ledger report (`session_ledger_report`) — the one
 * roster of record — joined with what the boot restore did on THIS boot
 * (`useTerminalInitialization`'s per-page completion, its drain deferral and
 * its `needs-account` records).
 */

import type {
  LedgerEntry,
  LedgerGeneration,
  LedgerOutcome,
  LedgerReport,
  ResumeAccount,
} from "@/lib/session-ledger";
import { describeThrown } from "@/lib/utils";
import type { ResumeTarget } from "./resumeInNewTab";
import type { NeedsAccountReason } from "./useTerminalInitialization";

// ---------------------------------------------------------------------------
// Stable UI-Bridge control ids
// ---------------------------------------------------------------------------

/** The strip (container) — present in every form, so a check can read its text. */
export const SINCE_RESTART_STRIP_ID = "terminal.since-restart-strip";
/** The strip's "Review" button (opens Previous → "Before the last restart"). */
export const SINCE_RESTART_REVIEW_ID = "terminal.since-restart-review";
/** The strip's dismiss button. */
export const SINCE_RESTART_DISMISS_ID = "terminal.since-restart-dismiss";
/** The panel section (container). */
export const SINCE_RESTART_SECTION_ID = "terminal.since-restart-section";
/** The panel's "Resume selected (n)" button. */
export const SINCE_RESTART_RESUME_SELECTED_ID = "terminal.since-restart-resume-selected";
/** The panel's generation selector. */
export const SINCE_RESTART_GENERATION_ID = "terminal.since-restart-generation";
/** The panel's "Done reviewing" button. */
export const SINCE_RESTART_DONE_ID = "terminal.since-restart-done";
/** The pre-rebuild preview's "Capture now" button. */
export const SINCE_RESTART_CAPTURE_ID = "terminal.since-restart-capture";
/** The panel's refresh button. */
export const SINCE_RESTART_REFRESH_ID = "terminal.since-restart-refresh";
/** The pre-rebuild preview's show/hide-sessions toggle (one id for both labels). */
export const SINCE_RESTART_PREVIEW_TOGGLE_ID = "terminal.since-restart-preview-toggle";

/** One row (container) of the review panel, keyed by session id. */
export function sinceRestartRowId(claudeSessionId: string): string {
  return `terminal.since-restart-row-${claudeSessionId}`;
}
/** One row's checkbox. */
export function sinceRestartCheckId(claudeSessionId: string): string {
  return `terminal.since-restart-check-${claudeSessionId}`;
}
/** One row's Finish/Unfinish button (one id for both labels). */
export function sinceRestartFinishId(claudeSessionId: string): string {
  return `terminal.since-restart-finish-${claudeSessionId}`;
}
/** One row's account chooser (needs-account rows). */
export function sinceRestartAccountId(claudeSessionId: string): string {
  return `terminal.since-restart-account-${claudeSessionId}`;
}
/** One row's Retry button (a failed resume). */
export function sinceRestartRetryId(claudeSessionId: string): string {
  return `terminal.since-restart-retry-${claudeSessionId}`;
}
/** A Previous Sessions cohort card's Finish/Unfinish button (one id for both labels). */
export function pastSessionFinishId(claudeSessionId: string): string {
  return `terminal.past-session-finish-${claudeSessionId}`;
}

/**
 * The app's main-view navigation event (handled by `useAppNavigation`, the
 * same door UI-Bridge navigation uses): `detail.page` is a `PAGE_TO_TAB` key.
 */
export const APP_NAVIGATE_EVENT = "ui-bridge-navigate";

/**
 * Bring the Terminal main view up. The strip sits in the app-wide advisory
 * column, so its Review can be clicked from ANY view; opening the session
 * sidebar alone left the roster section on a hidden page.
 */
export function requestTerminalView(target: EventTarget = window): void {
  target.dispatchEvent(new CustomEvent(APP_NAVIGATE_EVENT, { detail: { page: "terminal" } }));
}

// ---------------------------------------------------------------------------
// Restore progress on THIS boot
// ---------------------------------------------------------------------------

/** What the boot restore has done so far, per terminal page. */
export interface RestoreProgress {
  /** Pages whose restore has fully drained (the hook's `restoreCompletePages`). */
  completePages: ReadonlySet<string>;
  /** Pages whose restore the coord device drain DEFERRED (re-runs when it lifts). */
  deferredPages: ReadonlySet<string>;
  /** Every page in the persisted layout. */
  knownPageIds: readonly string[];
}

/**
 * Has this page's restore settled? A page in the layout settles when its own
 * restore drains — restore runs lazily, the first time a page is opened. A
 * row naming NO live page is an orphan, which the FIRST page to restore adopts
 * (`recordBelongsToRestore`), so it settles once any page has.
 */
export function pageRestoreSettled(pageId: string, progress: RestoreProgress): boolean {
  if (progress.completePages.has(pageId)) return true;
  return !progress.knownPageIds.includes(pageId) && progress.completePages.size > 0;
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/** What a row says happened to its session. */
export type RowState =
  /** It is open (or was restored) again. */
  | "back"
  /** Unfinished, its page restored, and it did not come back. */
  | "missing"
  /** It did not come back, and its account is unknown — the operator must choose one. */
  | "needs-account"
  /** Marked finished — not expected back. */
  | "finished"
  /** The operator closed it on purpose since — not expected back, never pre-checked. */
  | "closed-by-user"
  /** Its page has not been opened since the restart, so its restore has not run yet. */
  | "waiting-page"
  /** Its page's restore is deferred by the coord device drain. */
  | "waiting-drain";

/** One row of the review panel. */
export interface SinceRestartRow {
  outcome: LedgerOutcome;
  state: RowState;
  /** The outcome chip's text. */
  label: string;
  /** Tooltip detail behind the chip, when there is more to say. */
  detail: string | null;
  /** The account a resume types — the operator's choice when one was made. */
  account: ResumeAccount;
  /** The account came from the operator's chooser, not from evidence. */
  accountChosen: boolean;
  /** Can be checked for "Resume selected". */
  selectable: boolean;
  /** Why it cannot, when it cannot (shown on the disabled checkbox). */
  blockedReason: string | null;
  /** The directory a resume opens in (the launch dir — Claude scopes sessions by it). */
  resumeDir: string | null;
}

/** Operator-facing sentence for a `needs-account` reason. */
export function describeNeedsAccount(reason: NeedsAccountReason | "unrecorded"): string {
  switch (reason) {
    case "config-dir-rejected":
      return "account unknown — its recorded account directory is not shell-safe";
    case "default-home-unknown":
      return "account unknown — its recorded directory may be the default account, which could not be read";
    case "no-transcript-holder":
      return "account unknown — no account directory holds its transcript";
    case "ambiguous-transcript":
      return "account unknown — several account directories hold its transcript";
    case "transcript-unprobed":
      return "account unknown — none was recorded and where its transcript lives was not checked";
    case "unrecorded":
      return "account unknown — none was recorded and no single account holds its transcript";
  }
}

const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;

const MISSING_DETAIL: Record<string, string> = {
  "no-attempt": "it was resumable and nothing brought it back",
  "not-restorable":
    "it was never identity-restorable (unconfirmed, or no transcript), so a resume cannot bring its conversation back",
  "restorability-unknown": "whether it can be resumed could not be determined",
};

/** Sort order: what needs the operator first. */
const STATE_ORDER: Record<RowState, number> = {
  missing: 0,
  "needs-account": 1,
  "waiting-page": 2,
  "waiting-drain": 3,
  back: 4,
  "closed-by-user": 5,
  finished: 6,
};

export interface RowContext {
  progress: RestoreProgress;
  /** Boot-restore candidates not resumed because their account is unknown, by id. */
  needsAccount: ReadonlyMap<string, NeedsAccountReason>;
  /** Accounts the operator chose for needs-account rows, by id. */
  accountChoices: ReadonlyMap<string, ResumeAccount>;
}

/** Build one row. Exported for tests. */
export function buildRow(outcome: LedgerOutcome, ctx: RowContext): SinceRestartRow {
  const id = outcome.claudeSessionId;
  const chosen = ctx.accountChoices.get(id);
  const restoreReason = ctx.needsAccount.get(id);
  // The boot restore's own verdict outranks the ledger's capture-time
  // resolution: it probed the transcript on THIS boot.
  const evidenceAccount: ResumeAccount =
    restoreReason !== undefined ? { known: false, configDir: null } : outcome.resumeAccount;
  const account = chosen ?? evidenceAccount;
  // THE resume-dir rule is the backend's (`session_ledger::resume_dir_of`: the
  // exact launch dir, else the worktree root) — the same dir the copy line uses.
  const resumeDir = outcome.resumeDir?.trim() || null;

  let state: RowState;
  let label: string;
  let detail: string | null = null;
  if (outcome.outcome === "back") {
    state = "back";
    label = "back";
  } else if (outcome.outcome === "finished") {
    state = "finished";
    label = "finished";
    detail = "marked finished — not expected back";
  } else if (outcome.outcome === "closed-by-user") {
    state = "closed-by-user";
    label = "closed by you";
    detail = "you closed it after this roster was captured — not expected back";
  } else if (ctx.progress.deferredPages.has(outcome.pageId)) {
    state = "waiting-drain";
    label = "waiting: restore deferred by drain";
    detail = "the coord device drain defers restores; this page restores when it lifts";
  } else if (!pageRestoreSettled(outcome.pageId, ctx.progress)) {
    state = "waiting-page";
    label = "waiting: page not opened";
    detail = `page "${outcome.pageId}" has not been opened since the restart, so its restore has not run`;
  } else if (!account.known) {
    state = "needs-account";
    label = "needs-account";
    detail = describeNeedsAccount(restoreReason ?? "unrecorded");
  } else {
    state = "missing";
    label = `missing: ${outcome.reason ?? "unknown"}`;
    detail = outcome.reason ? (MISSING_DETAIL[outcome.reason] ?? null) : null;
  }

  let blockedReason: string | null = null;
  // A row closed on purpose can still be ticked by hand; it is just never
  // pre-checked (see `preCheckedIds`).
  if (state !== "missing" && state !== "closed-by-user") {
    blockedReason =
      state === "needs-account" ? "choose an account first" : `nothing to resume: ${label}`;
  } else if (!account.known) {
    blockedReason = "the account it ran under is unknown";
  } else if (outcome.reason === "not-restorable") {
    blockedReason = "never identity-restorable — use Copy command to try by hand";
  } else if (!SESSION_ID_RE.test(id)) {
    blockedReason = "the session id is not shell-safe";
  } else if (resumeDir === null) {
    blockedReason = "no working directory was recorded";
  }

  return {
    outcome,
    state,
    label,
    detail,
    account,
    accountChosen: chosen !== undefined,
    selectable: blockedReason === null,
    blockedReason,
    resumeDir,
  };
}

/** Every row of a generation, the ones needing attention first. */
export function buildRows(gen: LedgerGeneration, ctx: RowContext): SinceRestartRow[] {
  return [...gen.missing, ...gen.returned, ...gen.closedByUser, ...gen.finished]
    .map((o) => buildRow(o, ctx))
    .sort(
      (a, b) =>
        STATE_ORDER[a.state] - STATE_ORDER[b.state] ||
        a.outcome.displayName.localeCompare(b.outcome.displayName),
    );
}

/**
 * The rows pre-checked for "Resume selected": unfinished, missing, resumable,
 * and KNOWN to be resumable (`restorable === true`). A needs-account row is
 * not pre-checked until an account is chosen (it is not `missing` until then);
 * a `restorability-unknown` row is selectable but left to the operator.
 */
export function preCheckedIds(rows: readonly SinceRestartRow[]): Set<string> {
  return new Set(
    rows
      .filter(
        (r) =>
          r.selectable &&
          r.state === "missing" &&
          !r.outcome.finished &&
          r.outcome.restorable === true,
      )
      .map((r) => r.outcome.claudeSessionId),
  );
}

/** The review selection, and which rows it has already pre-checked or not. */
export interface SelectionState {
  /** The generation file it belongs to. */
  generationFile: string | null;
  ids: ReadonlySet<string>;
  /** Rows already seen selectable — pre-checked (or not) exactly once. */
  seen: ReadonlySet<string>;
}

export const EMPTY_SELECTION: SelectionState = {
  generationFile: null,
  ids: new Set(),
  seen: new Set(),
};

/**
 * Bring the selection up to date with `rows` — the SAME object back when
 * nothing changed, so a caller can set it during render without looping.
 *
 * A new generation starts from its pre-checked rows. Within one generation a
 * row is pre-checked (or not) once, when it first becomes selectable — a row
 * whose page was still restoring joins when its page settles — and the
 * operator's own checks and un-checks are never overridden.
 *
 * `precheck` is false for every generation but the NEWEST: an older
 * generation's sessions may have been closed or superseded since (the ledger
 * records no user-close), so nothing there is ticked for the operator — they
 * may still tick rows themselves.
 */
export function reconcileSelection(
  prev: SelectionState,
  generationFile: string | null,
  rows: readonly SinceRestartRow[],
  precheck: boolean,
): SelectionState {
  const fresh = generationFile !== prev.generationFile;
  const selectable = rows.filter((r) => r.selectable).map((r) => r.outcome.claudeSessionId);
  const unseen = fresh ? selectable : selectable.filter((id) => !prev.seen.has(id));
  if (!fresh && unseen.length === 0) return prev;
  const prechecked = precheck ? preCheckedIds(rows) : new Set<string>();
  const ids = new Set(fresh ? [] : prev.ids);
  const seen = new Set(fresh ? [] : prev.seen);
  for (const id of unseen) {
    seen.add(id);
    if (prechecked.has(id)) ids.add(id);
  }
  return { generationFile, ids, seen };
}

/**
 * What one "Resume selected" row resumes — or `null` when it cannot be
 * resumed now (not selectable, no directory, account unknown).
 *
 * A KNOWN account with no dir is the default home: the target carries its
 * EXPLICIT path (`defaultHome`, from the backend) so the verified resume
 * RECORDS it on the registry row — which then stops reading "account unknown"
 * on the next restart — while the typed command drops it again
 * (`runVerifiedResume`).
 */
export function resumeTargetFor(
  row: SinceRestartRow,
  defaultHome: string | null,
): ResumeTarget | null {
  if (!row.selectable || row.resumeDir === null || !row.account.known) return null;
  return {
    claudeSessionId: row.outcome.claudeSessionId,
    displayName: row.outcome.displayName,
    workingDir: row.resumeDir,
    configDir: row.account.configDir ?? defaultHome ?? undefined,
    provider: row.outcome.provider ?? undefined,
  };
}

/** The ids in `selected` that are still selectable rows — what "Resume selected" acts on. */
export function selectedResumable(
  rows: readonly SinceRestartRow[],
  selected: ReadonlySet<string>,
): SinceRestartRow[] {
  return rows.filter((r) => r.selectable && selected.has(r.outcome.claudeSessionId));
}

// ---------------------------------------------------------------------------
// Which generation to review
// ---------------------------------------------------------------------------

/**
 * The generation the panel opens on: the NEWEST retained generation (the
 * restart that just happened), when it has at least one unfinished miss the
 * operator has not marked reviewed. `null` — the normal running state — means
 * the panel shows the current roster (the pre-rebuild preview) instead.
 *
 * Never an OLDER generation: its sessions may have been closed or finished in
 * a later life, and the ledger records no user-close, so opening on it would
 * offer back sessions the operator already dealt with. Older generations stay
 * reachable through the selector.
 */
export function defaultGenerationFile(
  report: LedgerReport | null,
  reviewed: ReadonlySet<string>,
): string | null {
  const newest = thisBootGeneration(report);
  return newest && newest.missing.length > 0 && !reviewed.has(newest.file) ? newest.file : null;
}

/**
 * The generation THIS runner boot retired (the restart that just happened),
 * or `null` when this boot retained none — then `generations[0]` is an OLDER
 * boot's roster and nothing may default to it. Read off the backend's explicit
 * `thisBoot` mark, never off stamp order (a clock stepped backwards between
 * boots mis-orders `rotatedAtMs`).
 */
export function thisBootGeneration(report: LedgerReport | null): LedgerGeneration | null {
  const first = report?.generations[0];
  return first?.thisBoot ? first : null;
}

// ---------------------------------------------------------------------------
// The strip
// ---------------------------------------------------------------------------

export type StripModel =
  | { kind: "deferred"; generationFile: string; waiting: number; text: string }
  | { kind: "all-back"; generationFile: string; total: number; text: string }
  | {
      kind: "some-missing";
      generationFile: string;
      back: number;
      total: number;
      missing: number;
      /** Rows whose page has not restored yet — not counted as missing. */
      waiting: number;
      text: string;
    };

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

/**
 * What the strip says about the restart that just happened (the NEWEST
 * generation), or `null` for nothing.
 *
 * - While the coord drain defers restore: "restore deferred by drain — N waiting".
 * - Otherwise nothing until the FIRST page's restore has settled (never on the
 *   boot census latch, which runs before any restore). Restore is lazy per
 *   page, so waiting for EVERY page would hide the strip for as long as the
 *   operator leaves one page unopened — exactly when it is needed. Rows whose
 *   page has not restored yet are counted as WAITING, never as missing, and the
 *   strip names them (honesty about uncertainty).
 * - K = 0, W = 0: "All M sessions from before the restart are back".
 * - Otherwise: "N of M sessions from before the restart are back", then
 *   "· K didn't come back" when K > 0 and "· W waiting on pages not opened
 *   yet" when W > 0. With an `unknown` verdict (no restore census this boot)
 *   K is an upper bound and says so.
 */
export function stripModel(report: LedgerReport | null, ctx: RowContext): StripModel | null {
  const gen = thisBootGeneration(report);
  if (!gen) return null;
  const generationFile = gen.file;
  if (ctx.progress.deferredPages.size > 0) {
    // Only the rows whose page has NOT settled are waiting; a missed row on a
    // page whose restore already ran is a miss, not a wait.
    const waiting = gen.missing.filter((o) => !pageRestoreSettled(o.pageId, ctx.progress)).length;
    if (waiting === 0) return null;
    return {
      kind: "deferred",
      generationFile,
      waiting,
      text: `Restore deferred by drain — ${waiting} ${plural(waiting, "session", "sessions")} waiting`,
    };
  }
  const expected = [...gen.returned, ...gen.missing];
  if (expected.length === 0) return null;
  if (ctx.progress.completePages.size === 0) return null;

  const missingRows = gen.missing.map((o) => buildRow(o, ctx));
  const k = missingRows.filter((r) => !r.state.startsWith("waiting")).length;
  const waiting = missingRows.length - k;
  const back = gen.returned.length;
  const total = expected.length;
  if (k === 0 && waiting === 0) {
    return {
      kind: "all-back",
      generationFile,
      total,
      text:
        total === 1
          ? "The session from before the restart is back"
          : `All ${total} sessions from before the restart are back`,
    };
  }
  const upperBound = gen.verdict === "unknown" ? "up to " : "";
  const parts = [
    `${back} of ${total} ${plural(total, "session", "sessions")} from before the restart are back`,
  ];
  if (k > 0) parts.push(`${upperBound}${k} didn't come back`);
  if (waiting > 0)
    parts.push(`${waiting} waiting on ${plural(waiting, "a page", "pages")} not opened yet`);
  return {
    kind: "some-missing",
    generationFile,
    back,
    total,
    missing: k,
    waiting,
    text: parts.join(" · "),
  };
}

// ---------------------------------------------------------------------------
// The sequential resume queue
// ---------------------------------------------------------------------------

/** One row's progress through "Resume selected". */
export type ResumeProgress =
  | { state: "queued" }
  | { state: "resuming" }
  | { state: "back" }
  | { state: "failed"; reason: string };

/** Display text for a row's progress. */
export function describeProgress(p: ResumeProgress): string {
  return p.state === "failed" ? `failed: ${p.reason}` : p.state;
}

/** What one resume came to. */
export type ResumeResult = { ok: true } | { ok: false; reason: string };

/** The reason a queued resume that never ran reports once its queue is cancelled. */
export const RESUME_CANCELLED = "cancelled";

/**
 * Resume `ids` ONE AT A TIME: each in turn goes `resuming` → `back` |
 * `failed: <reason>`, and the next starts only when the previous has settled.
 * A failure (or a throw) never stops the queue; an aborted `signal` does —
 * checked before each item, so nothing new spawns after it fires, and every
 * item not yet started is marked `failed: cancelled`.
 *
 * Sequential on purpose: the box's resource guard already queues spawns, and a
 * parallel burst of 30+ resumes is the pane storm earlier plans fought. Each
 * `resumeOne` takes its own spawn through that guard — no bulk bypass.
 */
export async function runResumeQueue(
  ids: readonly string[],
  resumeOne: (id: string) => Promise<ResumeResult>,
  onProgress: (id: string, progress: ResumeProgress) => void,
  signal?: AbortSignal,
): Promise<void> {
  for (const [index, id] of ids.entries()) {
    if (signal?.aborted) {
      for (const rest of ids.slice(index)) {
        onProgress(rest, { state: "failed", reason: RESUME_CANCELLED });
      }
      return;
    }
    onProgress(id, { state: "resuming" });
    try {
      const result = await resumeOne(id);
      onProgress(id, result.ok ? { state: "back" } : { state: "failed", reason: result.reason });
    } catch (err) {
      onProgress(id, { state: "failed", reason: describeThrown(err, "resume failed") });
    }
  }
}

/**
 * The panel's ONE resume queue: every enqueue chains behind the previous one
 * (one resume in flight, whichever button started it), an id already queued or
 * in flight is never queued again, and {@link ResumeQueue.cancel} (the panel
 * unmounting) stops it before the next spawn.
 */
export class ResumeQueue {
  private readonly pending = new Set<string>();
  private chain: Promise<void> = Promise.resolve();
  private readonly abort = new AbortController();

  /**
   * Queue `ids`. Admission is SYNCHRONOUS — each admitted id is marked
   * `queued` before this returns — so two clicks in one tick cannot both admit
   * the same session (a double resume would fork its transcript). Returns the
   * ids admitted and a promise settling when they have all run.
   */
  enqueue(
    ids: readonly string[],
    resumeOne: (id: string) => Promise<ResumeResult>,
    onProgress: (id: string, progress: ResumeProgress) => void,
  ): { admitted: string[]; done: Promise<void> } {
    const admitted = [...new Set(ids)].filter((id) => !this.pending.has(id));
    if (admitted.length === 0 || this.abort.signal.aborted) {
      return { admitted: [], done: Promise.resolve() };
    }
    for (const id of admitted) {
      this.pending.add(id);
      onProgress(id, { state: "queued" });
    }
    // A batch that REJECTS (an `onProgress` that throws) must neither wedge
    // the chain — every later batch is chained on it — nor surface as an
    // unhandled rejection: it is logged and the chain settles.
    const done = this.chain
      .then(() => runResumeQueue(admitted, resumeOne, onProgress, this.abort.signal))
      .catch((err: unknown) => {
        console.warn("[since-restart] a resume batch failed:", err);
      })
      .finally(() => {
        for (const id of admitted) this.pending.delete(id);
      });
    this.chain = done;
    return { admitted, done };
  }

  /** Stop: nothing further is spawned; queued items report `failed: cancelled`. */
  cancel(): void {
    this.abort.abort();
  }
}

// ---------------------------------------------------------------------------
// Finish / Unfinish
// ---------------------------------------------------------------------------

/** A Finish/Unfinish in flight for one row, or the last one that failed. */
export type FinishOp =
  | { phase: "pending"; to: boolean }
  | { phase: "failed"; to: boolean; message: string };

export interface FinishButtonModel {
  label: string;
  /** What a click requests. */
  target: boolean;
  disabled: boolean;
  title: string;
}

/**
 * The Finish/Unfinish control for a row whose session is `finished` per the
 * roster. Pending shows the transition and disables the button; a failed op
 * keeps the roster's truth and says why, so a retry is the same click.
 */
export function finishButtonModel(finished: boolean, op: FinishOp | undefined): FinishButtonModel {
  if (op?.phase === "pending") {
    return {
      label: op.to ? "Finishing…" : "Unfinishing…",
      target: op.to,
      disabled: true,
      title: op.to ? "Marking finished…" : "Clearing the finished mark…",
    };
  }
  const target = !finished;
  const base = target
    ? "Mark finished — it will not be brought back by the next restart (reversible)"
    : "Unfinish — it will be brought back by the next restart again";
  return {
    label: target ? "Finish" : "Unfinish",
    target,
    disabled: false,
    title: op?.phase === "failed" ? `${base}. Last attempt failed: ${op.message}` : base,
  };
}

// ---------------------------------------------------------------------------
// The pre-rebuild preview (Phase 5)
// ---------------------------------------------------------------------------

/** The current roster's headline: what a restart now would bring back. */
export interface PreviewModel {
  unfinished: LedgerEntry[];
  finished: LedgerEntry[];
  text: string;
  /** "saved 2m ago" / "not saved yet this boot", with the up-to-date caveat. */
  savedText: string;
  /** The saved roster lags the current one (it catches up within one poll tick). */
  savedStale: boolean;
}

/** "just now" / "42s ago" / "3m ago" / "2h ago". */
export function formatAgo(ms: number, now: number): string {
  const s = Math.max(0, Math.floor((now - ms) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  return `${Math.floor(m / 60)}h ago`;
}

export function previewModel(report: LedgerReport, now: number): PreviewModel {
  const sessions = report.current.sessions;
  const unfinished = sessions.filter((s) => !s.finished);
  const finished = sessions.filter((s) => s.finished);
  const n = unfinished.length;
  const text = `If the runner restarts now: ${n} unfinished ${plural(n, "session", "sessions")} will come back`;
  let savedText: string;
  if (report.savedAtMs === null) savedText = "not captured yet this boot";
  else savedText = `last captured ${formatAgo(report.savedAtMs, now)}`;
  return {
    unfinished,
    finished,
    text,
    savedText,
    savedStale: report.savedAtMs !== null && !report.savedMatchesCurrent,
  };
}
