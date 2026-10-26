/**
 * The ONE restore queue of a restart cohort (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7,
 * D6/D7). Every restore button — a placeholder's Restore, a page's "Restore
 * all on this page", the global strip's "Restore all" — feeds this queue:
 *
 * - **one in flight.** Each item calls the backend operator twin
 *   `operator_resume_sessions([id])` and waits for its verified verdict before
 *   the next starts, so a bulk restore is never a pane storm;
 * - **FIFO by page, then zone** within one bulk click;
 * - **bulk skips held, finished and blocked rows** and says how many it
 *   skipped. A row whose hold has not been read yet counts as held: bulk
 *   never guesses; a row the backend marks `blocker` can only be refused;
 * - **a busy box is a wait, not a failure.** The resume core answers
 *   `skipped(resource_guard:<lane>)` when the resource guard refused the
 *   spawn. The row reads "waiting: box busy", goes back to the head of the
 *   queue, and the queue pauses 15 s before asking again — the guard carries
 *   no retry hint of its own, and the next spawn attempt IS the headroom probe;
 * - **a finished row restores like any other.** The resume core lifts a
 *   cohort row's finish mark itself, for the operator origin only, and only
 *   once the resume is verified (Phase 6) — so a refused or failed Restore
 *   leaves the mark where it was. The queue does not un-finish it first;
 * - **every other refusal or failure is `failed: <reason>` with Retry;**
 * - **Restore twice enqueues once:** a row already queued, restoring,
 *   waiting or back is not enqueued again;
 * - **a cohort that ended takes its queue with it.** The queue follows the
 *   cohort store: once the store no longer holds a row's prepare (dismissed
 *   from the panel or the strip, or done), that row is dropped, and a row is
 *   checked again just before its restore starts. Each restore also names its
 *   prepare, so the backend refuses it (`cohort_not_restoring`) if the cohort
 *   ended in between.
 *
 * On `resumed` the placeholder is replaced in its own zone by the live pane
 * the resume core created (`emitPlaceholderEvent({kind: "back"})`, handled per
 * page by `useRestorePlaceholderBinding`).
 */

import { useCallback, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  bulkEligible,
  cohortBucket,
  emitPlaceholderEvent,
  restoreCohortStore,
  type RestorePlaceholder,
} from "./restorePlaceholders";

/** How long a busy box is left alone before the queue asks again. */
export const GUARD_RETRY_MS = 15_000;

/** One row's progress. */
export type RestorePhase =
  | { phase: "queued" }
  | { phase: "restoring" }
  | { phase: "waiting"; reason: string }
  | { phase: "back" }
  | { phase: "failed"; reason: string };

/** The resume core's answer for one id, reduced to what the queue acts on. */
export type ResumeVerdict =
  | { outcome: "resumed"; terminalId: string | null }
  | { outcome: "busy"; lane: string }
  /** The named cohort is no longer restoring: the row is dropped, not failed. */
  | { outcome: "gone" }
  | { outcome: "failed"; reason: string };

/** Mirrors `COHORT_NOT_RESTORING` in `session/resume_door.rs`. */
export const COHORT_NOT_RESTORING = "cohort_not_restoring";

/** The queue's effects, injected so its rules are testable without Tauri. */
export interface RestoreQueueEffects {
  /**
   * Restore one session of cohort `prepareId` through the operator twin.
   * Never throws.
   */
  resume: (claudeSessionId: string, prepareId: string) => Promise<ResumeVerdict>;
  /**
   * Whether cohort `prepareId` is still the one restoring, as the webview
   * knows it. Asked just before each restore starts; a row of an ended
   * cohort is dropped unrun. Absent: always live.
   */
  cohortLive?: (prepareId: string) => boolean;
  /** Protect the open record from the liveness poll while its restore runs. */
  markPending: (claudeSessionId: string) => void;
  /** The restore landed: drop that protection. */
  clearPending: (claudeSessionId: string) => void;
  /** The restore landed: replace the placeholder with the live pane. */
  onBack: (row: RestorePlaceholder, terminalId: string | null) => void;
  setTimer: (fn: () => void, ms: number) => unknown;
  clearTimer: (handle: unknown) => void;
}

export interface BulkResult {
  enqueued: number;
  skippedHeld: number;
  skippedFinished: number;
  /** Rows the backend says cannot restore yet (`blocker`). */
  skippedBlocked: number;
  /** Rows whose account hit its usage limit during the prepare: hand restore only. */
  skippedLimitHit: number;
}

/** PURE: the operator-facing note for a bulk click, or `null` when nothing was skipped. */
export function bulkSkipNotice(r: BulkResult): string | null {
  const parts: string[] = [];
  if (r.skippedHeld > 0) parts.push(`skipped ${r.skippedHeld} held`);
  if (r.skippedFinished > 0) parts.push(`skipped ${r.skippedFinished} finished`);
  if (r.skippedBlocked > 0) parts.push(`skipped ${r.skippedBlocked} not restorable yet`);
  if (r.skippedLimitHit > 0) {
    parts.push(`skipped ${r.skippedLimitHit} usage-limited (restore by hand)`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

/**
 * PURE: what a bulk click on `rows` would restore and skip, without enqueueing
 * anything — the same buckets {@link RestoreQueue.enqueueBulk} uses, with
 * `enqueued` = how many it would enqueue.
 */
export function previewBulk(rows: readonly RestorePlaceholder[]): BulkResult {
  const r: BulkResult = {
    enqueued: 0,
    skippedHeld: 0,
    skippedFinished: 0,
    skippedBlocked: 0,
    skippedLimitHit: 0,
  };
  for (const row of rows) {
    const bucket = cohortBucket(row);
    if (bucket === "finished") r.skippedFinished++;
    else if (bucket === "blocked") r.skippedBlocked++;
    else if (bucket === "limitHit") r.skippedLimitHit++;
    else if (!bulkEligible(row)) r.skippedHeld++;
    else r.enqueued++;
  }
  return r;
}

/**
 * PURE: the standing note beside a bulk button — what it skips, shown BEFORE
 * any click and whether or not the button is enabled, so a page whose every
 * row is held or usage-limited still says why nothing restores. `null` when
 * nothing is skipped.
 */
export function bulkNoteText(r: BulkResult): string | null {
  const parts: string[] = [];
  if (r.skippedHeld > 0) parts.push(`${r.skippedHeld} held`);
  if (r.skippedLimitHit > 0) parts.push(`${r.skippedLimitHit} usage-limited`);
  if (r.skippedFinished > 0) parts.push(`${r.skippedFinished} finished`);
  if (r.skippedBlocked > 0) parts.push(`${r.skippedBlocked} not restorable yet`);
  if (parts.length === 0) return null;
  const list = parts.join(", ");
  return r.enqueued === 0
    ? `Nothing to restore — ${list} (restore those by hand)`
    : `Skips ${list} (restore those by hand)`;
}

/** PURE: `operator_resume_sessions`' `DoorReply` for one id, as a {@link ResumeVerdict}. */
export function verdictFromReply(reply: unknown, claudeSessionId: string): ResumeVerdict {
  const r = reply as { status?: unknown; body?: unknown } | null;
  const body = (r?.body ?? null) as {
    success?: unknown;
    error?: unknown;
    data?: { results?: unknown };
  } | null;
  if (r?.status !== 200 || body?.success !== true) {
    const why = typeof body?.error === "string" ? body.error : `status ${String(r?.status)}`;
    return { outcome: "failed", reason: why };
  }
  const results = Array.isArray(body.data?.results) ? body.data.results : [];
  const row = results.find((x) => (x as { id?: unknown })?.id === claudeSessionId) as
    | { outcome?: unknown; reason?: unknown; evidence?: { terminal_id?: unknown } | null }
    | undefined;
  if (!row) return { outcome: "failed", reason: "no verdict for this session" };
  if (row.outcome === "resumed") {
    const t = row.evidence?.terminal_id;
    return { outcome: "resumed", terminalId: typeof t === "string" && t ? t : null };
  }
  const reason = typeof row.reason === "string" ? row.reason : String(row.outcome);
  if (row.outcome === "skipped" && reason.startsWith("resource_guard:")) {
    return { outcome: "busy", lane: reason.slice("resource_guard:".length) };
  }
  if (row.outcome === "skipped" && reason.startsWith(COHORT_NOT_RESTORING)) {
    return { outcome: "gone" };
  }
  return { outcome: "failed", reason };
}

/**
 * Restore one session through the backend operator twin, naming its cohort so
 * the backend restores nothing once that cohort is no longer restoring.
 */
export async function resumeThroughOperatorTwin(
  claudeSessionId: string,
  prepareId: string,
): Promise<ResumeVerdict> {
  try {
    const reply = await invoke("operator_resume_sessions", {
      ids: [claudeSessionId],
      prepareId,
    });
    return verdictFromReply(reply, claudeSessionId);
  } catch (err) {
    return { outcome: "failed", reason: String(err) };
  }
}

const ACTIVE = new Set(["queued", "restoring", "waiting", "back"]);

export class RestoreQueue {
  private order: RestorePlaceholder[] = [];
  private phases = new Map<string, RestorePhase>();
  private inFlight: string | null = null;
  private pauseTimer: unknown = null;
  private listeners = new Set<() => void>();
  private version = 0;

  constructor(private fx: RestoreQueueEffects) {}

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  /** Changes on every state change (the `useSyncExternalStore` snapshot). */
  getVersion = (): number => this.version;

  phaseOf(claudeSessionId: string): RestorePhase | undefined {
    return this.phases.get(claudeSessionId);
  }

  /** The id being restored right now, if any. */
  current(): string | null {
    return this.inFlight;
  }

  /**
   * Enqueue rows the operator chose one by one (a placeholder's Restore, a
   * Retry). A held or finished row is restored when asked by name. Returns how
   * many were newly enqueued.
   */
  enqueue(rows: readonly RestorePlaceholder[]): number {
    let added = 0;
    for (const row of rows) {
      const phase = this.phases.get(row.claudeSessionId)?.phase;
      if (phase && ACTIVE.has(phase)) continue;
      this.order.push(row);
      this.phases.set(row.claudeSessionId, { phase: "queued" });
      added++;
    }
    if (added > 0) this.changed();
    void this.pump();
    return added;
  }

  /**
   * Enqueue a bulk button's rows: unheld and unfinished only, page then zone.
   * `pageOrder` is the page strip's order; a page not in it sorts last.
   */
  enqueueBulk(rows: readonly RestorePlaceholder[], pageOrder: readonly string[] = []): BulkResult {
    let skippedHeld = 0;
    let skippedFinished = 0;
    let skippedBlocked = 0;
    let skippedLimitHit = 0;
    const eligible: RestorePlaceholder[] = [];
    for (const row of rows) {
      const bucket = cohortBucket(row);
      if (bucket === "finished") skippedFinished++;
      else if (bucket === "blocked") skippedBlocked++;
      else if (bucket === "limitHit") skippedLimitHit++;
      else if (!bulkEligible(row)) skippedHeld++;
      else eligible.push(row);
    }
    const rank = (pageId: string) => {
      const i = pageOrder.indexOf(pageId);
      return i < 0 ? Number.MAX_SAFE_INTEGER : i;
    };
    eligible.sort(
      (a, b) =>
        rank(a.pageId) - rank(b.pageId) ||
        a.pageId.localeCompare(b.pageId) ||
        a.zoneIndex - b.zoneIndex,
    );
    return {
      enqueued: this.enqueue(eligible),
      skippedHeld,
      skippedFinished,
      skippedBlocked,
      skippedLimitHit,
    };
  }

  /**
   * Keep only the work of cohort `prepareId` — the one the cohort store holds
   * now (`null`: none). Rows of any other prepare are dropped from the queue
   * and their progress forgotten; a busy-box wait with nothing left to retry
   * is cancelled. A restore in flight still finishes (the backend refuses it
   * if its cohort ended first).
   */
  retainPrepare(prepareId: string | null): void {
    const keep = (r: RestorePlaceholder) => r.prepareId === prepareId;
    const dropped = this.order.filter((r) => !keep(r));
    if (dropped.length === 0) return;
    this.order = this.order.filter(keep);
    for (const r of dropped) this.phases.delete(r.claudeSessionId);
    if (this.order.length === 0 && this.pauseTimer !== null) {
      this.fx.clearTimer(this.pauseTimer);
      this.pauseTimer = null;
    }
    this.changed();
  }

  /** Forget everything (the cohort was dismissed). A restore in flight still finishes. */
  clear(): void {
    this.order = [];
    this.phases.clear();
    if (this.pauseTimer !== null) {
      this.fx.clearTimer(this.pauseTimer);
      this.pauseTimer = null;
    }
    this.changed();
  }

  private changed(): void {
    this.version++;
    for (const fn of this.listeners) fn();
  }

  private async pump(): Promise<void> {
    if (this.inFlight !== null || this.pauseTimer !== null) return;
    let row = this.order.shift();
    // A row whose cohort ended while it waited is dropped unrun.
    while (row && this.fx.cohortLive && !this.fx.cohortLive(row.prepareId)) {
      this.phases.delete(row.claudeSessionId);
      this.changed();
      row = this.order.shift();
    }
    if (!row) return;
    const id = row.claudeSessionId;
    this.inFlight = id;
    this.phases.set(id, { phase: "restoring" });
    this.changed();
    this.fx.markPending(id);
    let verdict: ResumeVerdict;
    try {
      verdict = await this.fx.resume(id, row.prepareId);
    } catch (err) {
      verdict = { outcome: "failed", reason: String(err) };
    }
    this.inFlight = null;
    if (verdict.outcome === "gone") {
      // The cohort ended before this restore started: nothing was spawned.
      this.phases.delete(id);
      this.fx.clearPending(id);
    } else if (verdict.outcome === "resumed") {
      this.phases.set(id, { phase: "back" });
      this.fx.clearPending(id);
      this.fx.onBack(row, verdict.terminalId);
    } else if (verdict.outcome === "busy") {
      this.phases.set(id, { phase: "waiting", reason: `box busy (${verdict.lane})` });
      this.order.unshift(row);
      this.pauseTimer = this.fx.setTimer(() => {
        this.pauseTimer = null;
        void this.pump();
      }, GUARD_RETRY_MS);
      this.changed();
      return;
    } else {
      this.phases.set(id, { phase: "failed", reason: verdict.reason });
    }
    this.changed();
    void this.pump();
  }
}

/** The app's one queue, wired to the operator twin and the cohort store. */
export const restoreQueue = new RestoreQueue({
  resume: resumeThroughOperatorTwin,
  markPending: (claudeSessionId) => {
    invoke("terminal_session_mark_restore_pending", { claudeSessionId, boot: false }).catch((err) =>
      console.warn(`[RestoreQueue] mark restore-pending failed for ${claudeSessionId}:`, err),
    );
  },
  clearPending: (claudeSessionId) => {
    invoke("terminal_session_clear_restore_pending", { claudeSessionId }).catch(() => {});
  },
  onBack: (row, terminalId) => {
    restoreCohortStore.markBack(row.claudeSessionId);
    emitPlaceholderEvent({
      kind: "back",
      pageId: row.pageId,
      claudeSessionId: row.claudeSessionId,
      zoneIndex: row.zoneIndex,
      terminalId,
    });
  },
  setTimer: (fn, ms) => setTimeout(fn, ms),
  clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  cohortLive: (prepareId) => restoreCohortStore.getSnapshot().prepareId === prepareId,
});

// The queue follows the cohort store: a cohort that ended (dismissed from any
// surface, or done) takes its queued restores with it.
restoreCohortStore.subscribe(() => {
  restoreQueue.retainPrepare(restoreCohortStore.getSnapshot().prepareId);
});

/**
 * Dismiss the restoring cohort: `restart_prep_dismiss` moves the record to
 * `done`, so no row is a placeholder or held from the orphan close any more,
 * and clears the restore-pending marker of every row not back. Every waiting
 * row stays in Past Sessions, resumable by hand. Then the cohort ends here
 * through the one end every dismiss shares (`endCohort`, which the panel's
 * dismiss and the `restart-prep-changed` event reach too): the store
 * empties, the queue drops what it held and every page drops its placeholder
 * tabs. Answers `null` on success, else what went wrong (nothing is cleared
 * then).
 */
export async function dismissRestoreCohort(
  prepareId: string,
  deps: {
    dismiss: (prepareId: string) => Promise<void>;
    store: { endCohort: (prepareId: string) => void };
  } = {
    dismiss: async (id) => {
      await invoke("restart_prep_dismiss", { prepareId: id });
    },
    store: restoreCohortStore,
  },
): Promise<string | null> {
  try {
    await deps.dismiss(prepareId);
  } catch (err) {
    return String(err);
  }
  deps.store.endCohort(prepareId);
  return null;
}

/** Re-render on every queue change; returns the queue. */
export function useRestoreQueue(): RestoreQueue {
  useSyncExternalStore(restoreQueue.subscribe, restoreQueue.getVersion);
  return restoreQueue;
}

/** The operator-facing label of a row's progress. */
export function restorePhaseText(p: RestorePhase | undefined): string | null {
  switch (p?.phase) {
    case undefined:
      return null;
    case "queued":
      return "queued";
    case "restoring":
      return "restoring…";
    case "waiting":
      return `waiting: ${p.reason}`;
    case "back":
      return "back";
    case "failed":
      return `failed: ${p.reason}`;
  }
}

/** Subscribe to the cohort store. */
export function useRestoreCohort() {
  return useSyncExternalStore(restoreCohortStore.subscribe, restoreCohortStore.getSnapshot);
}

/** The bulk-click handler shared by the page bar and the global strip. */
export function useBulkRestore(notify: (message: string) => void) {
  return useCallback(
    (rows: readonly RestorePlaceholder[], pageOrder: readonly string[]) => {
      const result = restoreQueue.enqueueBulk(rows, pageOrder);
      const note = bulkSkipNotice(result);
      if (note) notify(note);
      return result;
    },
    [notify],
  );
}
