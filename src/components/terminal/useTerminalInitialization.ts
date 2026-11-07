import { useEffect, useRef, useCallback, useState } from "react";
import { autonomousResumeDetector, subscribeCoordDrainState } from "@/hooks/useCoordDrainState";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LAYOUT_PRESETS, FLOW_GRID_ID, resolveLayout } from "./useZoneLayout";
import type { LayoutRequestSource } from "./terminalWorkMode";
import type { TerminalRefsMap } from "./writeWhenReady";
import {
  typeResumeAndVerify,
  getResumeSummaryPolicy,
  buildPickerAnswer,
  type ResumeSummaryPolicy,
  type ResumeOutcome,
  type TypeAndVerifyOptions,
} from "./resumeVerification";
import { providerDescriptorFor } from "./providerAdapter";
import type { TerminalTab } from "./useTerminalManager";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { CommandResponse, TerminalSessionRecord } from "./types";
import type { SaveSessionLayoutParams } from "./useSessionPersistence";
import { noteRecordedZone, recordedZoneLedgerFor, type SessionOpenArgs } from "./sessionRecordArgs";
import { rememberSessionId } from "./lastKnownSessionIds";
import { loadKnownPageIds, readPinnedPageId } from "./useTerminalPages";
import { subscribeTerminalPageRemoved } from "./terminalPageRemoval";
import { getPageBindings, type PageBindings } from "@/lib/pageBindings";
import { fetchLiveClaudeSessionIds } from "./liveClaudeSessions";
import {
  guardWaitMs,
  parseResourceGuardReading,
  publishResourceGuardWait,
  withdrawResourceGuardWait,
  ResourceGuardDeferral,
  type ResourceGuardSource,
} from "@/lib/resourceGuard";
import {
  loadDefaultConfigHome,
  resolveAccountDir,
  sanitizeConfigDir,
  typedConfigDir,
} from "./defaultConfigHome";
import { displayNameOf } from "@/lib/session-ledger";

/**
 * Fetch the durable RESTORABLE session records for `pageId` from the backend
 * registry, filtered to this page and deduped by `claudeSessionId` (defensive
 * — the registry should already enforce one row per id, but a duplicate would
 * otherwise spawn two tabs for one session). This is the SOURCE OF TRUTH for
 * the zone↔session binding on restore, replacing the ephemeral-tabId
 * creation-order mapping the localStorage snapshot used to drive.
 *
 * `terminal_session_list_open` returns the restorable superset: `open` records
 * (hard-crash case) PLUS in-grace `closed`/`pty-exit` records (graceful-restart
 * case, where `handleExit` flipped every live PTY to `closed`). The backend
 * owns the state/reason/grace gating, so we deliberately do NOT re-filter on
 * `state === "open"` here — that would drop the pty-exit records the backend
 * just decided are restorable.
 *
 * Exported for unit testing the restore-binding logic without booting React.
 */
export async function fetchOpenRecords(
  pageId: string,
  opts?: { knownPageIds?: readonly string[]; adoptOrphans?: boolean },
): Promise<TerminalSessionRecord[]> {
  return (await fetchRestoreSet(pageId, opts)).records;
}

/**
 * Wait for the runner's PTY-holder boot sweep to settle — every pane a holder
 * kept alive across the last runner exit adopted back as a live terminal (or,
 * if it could not be adopted, ended so its session is resumed normally) and
 * every dead holder's leftovers reaped — before the restore reads
 * `terminal_list` (plan
 * `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`, Phase 3).
 * Otherwise a surviving session would look dead and be cold-resumed beside
 * its still-running `claude`. The backend bounds the wait itself. Returns
 * whether the sweep SETTLED: `false` only when the wait ran out — the restore
 * then proceeds, and the records it refuses as "live" meanwhile are
 * re-decided once the sweep settles ({@link recheckRefusedAfterSweep}). An
 * error (an older runner without the command, so no sweep) counts as settled.
 * Exported for tests.
 */
export async function waitForHolderSweep(): Promise<boolean> {
  try {
    const resp = await invoke<CommandResponse>("terminal_holder_sweep_settled");
    const settled = (resp?.data as { settled?: boolean } | undefined)?.settled;
    if (settled === false) {
      console.warn(
        "[TerminalPage] the PTY-holder boot sweep did not settle in time; restoring anyway, " +
          "and re-deciding the sessions it refuses as live once it settles",
      );
      return false;
    }
    return true;
  } catch (err) {
    console.warn(`[TerminalPage] terminal_holder_sweep_settled unavailable: ${err}`);
    return true;
  }
}

/** The boot sweep as the restore sees it before it starts. */
export interface HolderSweepStatus {
  settled: boolean;
  /**
   * Terminal ids of live holder panes still being adopted. `null` when the
   * census did not come in within the runner's wait — the restore then waits
   * for the whole sweep instead.
   */
  held: ReadonlySet<string> | null;
}

/**
 * The PTY-holder boot sweep's status as soon as its CENSUS is in — a few
 * seconds at most, not the whole sweep (review round 4, M1): which panes are
 * HELD (live, still being adopted). The restore restores every other record
 * at once, and waits only for the held ones. An error (an older runner
 * without the command, so no sweep) is "settled, nothing held". Exported for
 * tests.
 */
export async function fetchHolderSweepStatus(): Promise<HolderSweepStatus> {
  try {
    const resp = await invoke<CommandResponse>("terminal_holder_sweep_status");
    const data = resp?.data as { settled?: boolean; held?: string[] | null } | undefined;
    return {
      settled: data?.settled !== false,
      held: Array.isArray(data?.held) ? new Set(data.held) : null,
    };
  } catch (err) {
    console.warn(`[TerminalPage] terminal_holder_sweep_status unavailable: ${err}`);
    return { settled: true, held: new Set() };
  }
}

/**
 * Split the restore's records by the sweep's census: those whose terminal a
 * holder pane is still being adopted into are HELD BACK (they come back as an
 * adopted tab, or through the re-run after the sweep settles); every other
 * record is restored NOW. A settled sweep holds nothing back. Pure; exported
 * for tests.
 */
export function splitRestoreByHold<T extends { terminalId: string }>(
  records: readonly T[],
  status: HolderSweepStatus,
): { now: T[]; heldBack: T[] } {
  if (status.settled || status.held === null || status.held.size === 0) {
    return { now: [...records], heldBack: [] };
  }
  const held = status.held;
  return {
    now: records.filter((r) => !held.has(r.terminalId)),
    heldBack: records.filter((r) => held.has(r.terminalId)),
  };
}

/** How many more times {@link recheckRefusedAfterSweep} waits for the sweep. */
export const HOLDER_SWEEP_RECHECK_ATTEMPTS = 3;

/**
 * The restore ran before the PTY-holder boot sweep settled, and refused to
 * cold-resume `refused` because a live `claude` hosted each of them. Some of
 * those processes may have been a DEAD holder's leftovers that the sweep then
 * reaped, or a holder it ended after a failed adoption — sessions that would
 * otherwise come back with no tab and no resume. Wait for the sweep (a few
 * more bounded waits), then return exactly those refused session ids that are
 * no longer hosted by a live process: the ones the restore must now resume.
 * A sweep that never settles, or a live registry that cannot be read, returns
 * none (fail closed: a missing tab is recoverable, a forked session is not).
 * Exported for tests.
 */
export async function recheckRefusedAfterSweep(
  refused: readonly string[],
  liveIds: () => Promise<ReadonlySet<string> | null> = fetchLiveClaudeSessionIds,
  heldBack: readonly string[] = [],
): Promise<string[]> {
  if (refused.length === 0 && heldBack.length === 0) return [];
  let settled = false;
  for (let i = 0; i < HOLDER_SWEEP_RECHECK_ATTEMPTS && !settled; i++) {
    settled = await waitForHolderSweep();
  }
  // Held-back records are always restored after the wait — the re-run
  // reconnects the ones that were adopted, and applies the same live-process
  // check to the rest. Refused ones only once the sweep SETTLED.
  if (!settled) return [...heldBack];
  const live = refused.length > 0 ? await liveIds() : null;
  return [...heldBack, ...refused.filter((id) => decideColdResume(live, id) === "respawn")];
}

/** The runner's event for a session handed back to the restore (Rust: `HOLDER_SESSION_RELEASED_EVENT`). */
export const HOLDER_SESSION_RELEASED_EVENT = "pty-holder-session-released";

/** Its payload. */
export interface HolderSessionReleased {
  sessionId: string;
  terminalId: string;
  pageId: string | null;
}

/**
 * A holder whose adoption the runner was still retrying DIED (review round 6,
 * T1). The restore had refused its session's resume while that holder's
 * `claude` was alive, and nothing else would re-decide it: re-decide it now
 * — the same recheck a late sweep settle runs — and `schedule` a re-run of
 * the restore for its page when it is resumable. Returns whether one was
 * scheduled. Exported for tests.
 */
export async function onHolderSessionReleased(
  payload: HolderSessionReleased,
  schedule: (pageId: string, sessionIds: string[]) => void,
  liveIds: () => Promise<ReadonlySet<string> | null> = fetchLiveClaudeSessionIds,
): Promise<boolean> {
  const resumable = await recheckRefusedAfterSweep([payload.sessionId], liveIds);
  if (resumable.length === 0) return false;
  schedule(payload.pageId ?? "default", resumable);
  return true;
}

/**
 * Whether THIS window owns the restore of `pageId` and so handles a released
 * session on it (review round 7, N4). Every window — the main one and each
 * pop-out — mounts its own `TerminalPage`, so its own restore hook, and the
 * runner broadcasts the event to all of them. A page-pinned pop-out owns its
 * one page; the main window owns every page not detached into a pop-out —
 * the same split `useTerminalPages` uses to decide which window restores a
 * page. Exported for tests.
 */
export function ownsReleasedPage(
  pageId: string,
  pinnedPageId: string | null,
  bindings: PageBindings,
): boolean {
  if (pinnedPageId !== null) return pageId === pinnedPageId;
  return !bindings[pageId];
}

/** How many times an ABORTED restore re-runs on its own for ids it still owes (review V1). */
export const ABORT_RETRY_LIMIT = 3;
/**
 * The first such re-run's delay; each later one doubles it (2 s, 4 s, 8 s), so
 * the last retry fires ~14 s after the first abort — well inside the ~3 min
 * (4 × 45 s ticks) before the lifecycle poll retires an unmatched record.
 */
export const ABORT_RETRY_BASE_MS = 2_000;

/** How a restore run of a page ended. */
export type RestoreRunOutcome = "restored" | "aborted" | "deferred";

/**
 * Serializes the targeted re-runs of each page's restore (review round 7,
 * U1). A late sweep settle and a released holder can both ask for a re-run of
 * the same page, for the same session, while a run is still in flight; two
 * runs racing would each cold-resume it. Ids scheduled while a run of the
 * page is in flight are only merged, and the run's end triggers ONE re-run
 * for all of them. A page this window has not begun restoring is not re-run
 * at all: its first full restore, whenever it runs, covers every id — and a
 * window that does not own the page never restores it.
 *
 * An ABORTED run (indeterminate reconnect) restored nothing, but its page
 * stays known and every id it owed is KEPT (review V1, coord finding
 * c7d478a8): dropping them left a released session unresumed until the
 * operator re-activated the page, and the lifecycle poll retires such a
 * record within minutes. The queue re-runs the page itself, at most
 * {@link ABORT_RETRY_LIMIT} times in a row with a doubling backoff; past
 * that, the ids wait for the next schedule or activation — never a loop. A
 * page with no completed restore re-runs IN FULL, which covers the ids. A
 * drain-deferred run drops them: the drain lift re-runs the page in full.
 * Exported for tests.
 */
export class RestoreRerunQueue {
  private readonly inFlight = new Set<string>();
  private readonly restored = new Set<string>();
  /** Consecutive aborted runs per page; a page in here is known. */
  private readonly aborted = new Map<string, number>();
  private readonly pending = new Map<string, Set<string>>();
  /** The ids each in-flight run took, given back if it aborts. */
  private readonly taken = new Map<string, Set<string>>();

  /** Merge `sessionIds` for `pageId`; true when the caller must trigger a re-run now. */
  schedule(pageId: string, sessionIds: readonly string[]): boolean {
    const known =
      this.restored.has(pageId) || this.inFlight.has(pageId) || this.aborted.has(pageId);
    if (!known || sessionIds.length === 0) return false;
    const merged = this.pending.get(pageId) ?? new Set<string>();
    for (const id of sessionIds) merged.add(id);
    this.pending.set(pageId, merged);
    return !this.inFlight.has(pageId);
  }

  /** Whether ids for `pageId` are still owed a re-run. */
  owes(pageId: string): boolean {
    return (this.pending.get(pageId)?.size ?? 0) > 0;
  }

  /**
   * `pageId` was removed: forget it, so a page that exists under that id again
   * is a page this window has not restored. A run of the removed page still in
   * flight never reports its end here.
   */
  forget(pageId: string): void {
    this.inFlight.delete(pageId);
    this.restored.delete(pageId);
    this.aborted.delete(pageId);
    this.pending.delete(pageId);
    this.taken.delete(pageId);
  }

  /** Whether a restore of `pageId` has completed — a re-run of it merges. */
  restoredOnce(pageId: string): boolean {
    return this.restored.has(pageId);
  }

  /**
   * A run of `pageId` starts. Returns the ids it alone must restore, or
   * `undefined` for a full restore — always, until a restore of the page has
   * completed.
   */
  begin(pageId: string): ReadonlySet<string> | undefined {
    this.inFlight.add(pageId);
    const ids = this.pending.get(pageId);
    this.pending.delete(pageId);
    if (ids) this.taken.set(pageId, ids);
    else this.taken.delete(pageId);
    return this.restored.has(pageId) ? ids : undefined;
  }

  /**
   * The run of `pageId` ended with `outcome`. Returns `null` for no re-run,
   * or the delay in ms after which the caller triggers ONE re-run (`0`: now).
   */
  finish(pageId: string, outcome: RestoreRunOutcome): number | null {
    this.inFlight.delete(pageId);
    const taken = this.taken.get(pageId);
    this.taken.delete(pageId);
    if (outcome === "deferred") {
      this.pending.delete(pageId);
      return null;
    }
    if (outcome === "restored") {
      this.restored.add(pageId);
      this.aborted.delete(pageId);
      return (this.pending.get(pageId)?.size ?? 0) > 0 ? 0 : null;
    }
    const kept = this.pending.get(pageId) ?? new Set<string>();
    for (const id of taken ?? []) kept.add(id);
    if (kept.size > 0) this.pending.set(pageId, kept);
    const streak = (this.aborted.get(pageId) ?? 0) + 1;
    this.aborted.set(pageId, streak);
    if (kept.size === 0 || streak > ABORT_RETRY_LIMIT) return null;
    return ABORT_RETRY_BASE_MS * 2 ** (streak - 1);
  }
}

/**
 * Session-level "cold resume in progress" claims (review round 7, U1): two
 * runs in flight at once — of different pages — never both create a tab for
 * one session. A run claims a session before `createTerminal` and releases
 * its claims when it ends; by the next run, the tab it created is in the live
 * tabs, which a re-run checks. Exported for tests.
 */
export class ColdResumeGuard {
  private readonly claimed = new Set<string>();

  tryClaim(sessionId: string): boolean {
    if (this.claimed.has(sessionId)) return false;
    this.claimed.add(sessionId);
    return true;
  }

  release(sessionId: string): void {
    this.claimed.delete(sessionId);
  }
}

/**
 * One backoff timer per page for an aborted restore's retry (review V1, r1).
 * Setting a page's timer replaces the previous one; a run of the page
 * beginning cancels it (see {@link beginRestoreRun}). While inactive — the
 * hook unmounted — nothing is scheduled and every pending timer is cleared,
 * so a run that ends after unmount cannot leave one behind. Exported for
 * tests.
 */
export class RerunTimers {
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  private active = true;

  /** Mount (`true`) / unmount (`false`); unmounting clears every timer. */
  setActive(active: boolean): void {
    this.active = active;
    if (!active) {
      for (const t of this.timers.values()) clearTimeout(t);
      this.timers.clear();
    }
  }

  get isActive(): boolean {
    return this.active;
  }

  set(pageId: string, delayMs: number, fire: () => void): void {
    if (!this.active) return;
    this.cancel(pageId);
    const timer = setTimeout(() => {
      this.timers.delete(pageId);
      fire();
    }, delayMs);
    this.timers.set(pageId, timer);
  }

  /** Whether a timer is pending under `key`. */
  has(key: string): boolean {
    return this.timers.has(key);
  }

  cancel(pageId: string): void {
    const timer = this.timers.get(pageId);
    if (timer === undefined) return;
    clearTimeout(timer);
    this.timers.delete(pageId);
  }
}

/** The hook's restore re-run wiring, extracted so it is driven by tests. */
export interface RestoreRerunWiring {
  queue: RestoreRerunQueue;
  timers: RerunTimers;
  /** Put `pageId` into the hook's `drainDeferredPages`: its next run MERGES. */
  markMerging: (pageId: string) => void;
  /** Re-run `pageId`'s restore; `merge` marks it a merging re-run. */
  trigger: (pageId: string, merge: boolean) => void;
}

/** A run of `pageId` starts: its pending backoff retry is moot. See {@link RestoreRerunQueue.begin}. */
export function beginRestoreRun(
  w: RestoreRerunWiring,
  pageId: string,
): ReadonlySet<string> | undefined {
  w.timers.cancel(pageId);
  return w.queue.begin(pageId);
}

/** Merge `sessionIds` for a re-run of `pageId`, triggering it when no run is in flight. */
export function scheduleRestoreRerun(
  w: RestoreRerunWiring,
  pageId: string,
  sessionIds: readonly string[],
): void {
  // A page whose restore only ever aborted re-runs as its first restore
  // (unless the aborted run was itself a merging one — it is then in
  // `drainDeferredPages`, which makes the re-run merge however triggered).
  if (w.queue.schedule(pageId, sessionIds)) w.trigger(pageId, w.queue.restoredOnce(pageId));
}

/**
 * A run of `pageId` ended with `outcome`. An aborted merging re-run puts its
 * page back into `drainDeferredPages`, so EVERY retry path — a schedule, an
 * activation, the backoff timer — re-runs it merging. Then: one re-run now
 * for ids scheduled meanwhile, or a backoff retry for the ids an abort still
 * owes. The retry re-reads the queue when it fires: nothing owed (a run took
 * them meanwhile), nothing runs; merging is decided then, not at scheduling.
 * Once unmounted, nothing is scheduled.
 */
export function settleRestoreRun(
  w: RestoreRerunWiring,
  pageId: string,
  outcome: RestoreRunOutcome,
  wasMergingRun: boolean,
): void {
  if (outcome === "aborted" && wasMergingRun) w.markMerging(pageId);
  const after = w.queue.finish(pageId, outcome);
  if (after === null || !w.timers.isActive) return;
  if (after === 0) {
    w.trigger(pageId, true);
    return;
  }
  w.timers.set(pageId, after, () => {
    if (!w.queue.owes(pageId)) return;
    w.trigger(pageId, w.queue.restoredOnce(pageId));
  });
}

/** The key a page's restore publishes its resource-guard wait under. */
export function restoreGuardWaitKey(pageId: string): string {
  return `session-restore:${pageId}`;
}

/**
 * How long a page's auto-save gate may stay closed while a guard retry is owed,
 * from the page's FIRST deferral: 10 minutes. Past it the gate opens even
 * though the retry has not restored the rest — the operator's live edits on the
 * page outweigh the cosmetics (labels, notes, pins) of rows not yet restored,
 * which a save may then drop. The coord drain deferral already chose that
 * direction (it opens the gate at once, since a drain can last hours).
 */
export const GUARD_GATE_HOLD_MS = 10 * 60_000;

/**
 * A page's restore that the resource guard refused, retried later instead of
 * waited on (plan
 * `2026-11-04-unplanned-restart-restore-is-page-gated-and-counts-unverified-resumes`
 * Phase 5).
 *
 * ## Why the retry is a new run and not a wait inside this one
 *
 * The restore loop awaits each `createTerminal` in turn, and the run's
 * `finally` releases its cold-resume claims and clears the page's loading
 * spinner. A wait for room held open inside the loop (a promise retrying the
 * spawn) would hold both for as long as the box stays full, and an unmount or
 * a superseding run mid-wait would leave that promise alive to call
 * `terminal_create` later — a duplicate PTY. So a refusal ENDS the run: the
 * loop stops at the refused record, the run settles as `"deferred"` (like a
 * coord drain deferral: its owed ids are dropped because the retry restores the
 * page in full), and the retry is a fresh MERGING run of the page after
 * {@link guardWaitMs}. A merging run skips every session a live tab already
 * shows, so the records restored before the refusal are not restored twice.
 *
 * The retry rides {@link RerunTimers}, so a run of the page beginning
 * ({@link beginRestoreRun}) cancels it and unmounting clears it — nothing is
 * left to fire into a page that is gone. A run that begins while the page is
 * still owed ({@link owes}) — a recheck-only re-run included — becomes the
 * retry itself: the init effect runs it merging and in full, so cancelling the
 * timer loses nothing. A retry run that ABORTS (an indeterminate reconnect)
 * leaves the page owed, so it is re-armed ({@link rearm}).
 *
 * The wait is published for the guard banner and the restore strip and stays
 * up until a run of the page actually begins, or the hook unmounts
 * ({@link withdrawAll}). When the timer fires it is republished with no
 * `retryAt`: for a page off screen the trigger only arms its next activation,
 * so the honest line is "retries when the page is opened".
 *
 * While a retry is owed the page's auto-save gate stays CLOSED — a save would
 * write a layout missing the unreached records' cosmetics — but for at most
 * {@link GUARD_GATE_HOLD_MS} from the first deferral, after which `onGateRelease`
 * opens it ({@link gateReleased}). The scrollback files are kept until the
 * retry restores, since the cleanup would delete the unreached records'.
 *
 * The backoff counts consecutive deferrals per page and resets when a run of
 * the page restores without one. Exported for tests.
 */
export class RestoreGuardRetry {
  private readonly refusals = new Map<string, number>();
  /** The latest deferral per owed page, for re-arming its wait. */
  private readonly latest = new Map<string, { deferral: ResourceGuardDeferral; label: string }>();
  /** Gate-hold timers per owed page, from its first deferral. */
  private readonly gateTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Owed pages whose gate-hold ran out. */
  private readonly released = new Set<string>();
  /** Epoch ms of each owed page's FIRST deferral, which the gate-hold is measured from. */
  private readonly firstDeferredAt = new Map<string, number>();
  /** The last run token issued ({@link begin}); tokens only grow. */
  private runSeq = 0;
  /**
   * Per removed page, the last run token issued when it was removed
   * ({@link forget}): every run of it begun up to then belongs to the page that
   * is gone. A run of it in flight at the removal settles afterwards, and its
   * refusal must not bring the retry, the wait or the hold back.
   *
   * Scoped to RUN TOKENS, not to the bare page id, because a page id can exist
   * again in the same mount — "default" is re-created by the page reconcile
   * the moment a terminal lands on it, and `openPage` re-opens any id — and a
   * mark on the id would then stop every restore of the new page before its
   * first record, for good. A run begun after the removal has a newer token and
   * is untouched, whatever path brought its page back; nothing has to notice
   * that the page came back for that to hold.
   */
  private readonly removedThrough = new Map<string, number>();
  /** Opens a page's auto-save gate when its hold runs out ({@link setGateRelease}). */
  private onGateRelease: (pageId: string) => void = () => {};

  /** Install what opens a page's auto-save gate when its gate-hold runs out. */
  setGateRelease(release: (pageId: string) => void): void {
    this.onGateRelease = release;
  }

  /**
   * The run of `pageId` was refused by the guard: publish the wait and
   * schedule the merging retry. Nothing is scheduled once unmounted.
   */
  defer(
    w: RestoreRerunWiring,
    pageId: string,
    deferral: ResourceGuardDeferral,
    label: string,
    run: number,
  ) {
    if (!w.timers.isActive || this.removed(pageId, run)) return;
    const refusals = (this.refusals.get(pageId) ?? 0) + 1;
    this.refusals.set(pageId, refusals);
    this.latest.set(pageId, { deferral, label });
    if (!this.firstDeferredAt.has(pageId)) this.firstDeferredAt.set(pageId, Date.now());
    this.arm(w, pageId, run);
  }

  /**
   * A retry run of `pageId` aborted (an indeterminate reconnect) while the page
   * is still owed: publish its wait again and re-schedule the retry, at the
   * current backoff.
   */
  rearm(w: RestoreRerunWiring, pageId: string, run: number): void {
    if (!w.timers.isActive || this.removed(pageId, run) || !this.latest.has(pageId)) return;
    this.arm(w, pageId, run);
  }

  /**
   * `pageId` is gone (closed or deleted) while it may still be owed: drop
   * everything about it — its retry timer, its wait, its gate-hold — for good,
   * and mark every run of it begun so far as removed, so one still in flight
   * cannot bring them back ({@link removed}).
   */
  forget(w: RestoreRerunWiring, pageId: string): void {
    this.removedThrough.set(pageId, this.runSeq);
    w.timers.cancel(pageId);
    withdrawResourceGuardWait(restoreGuardWaitKey(pageId));
    this.refusals.delete(pageId);
    this.latest.delete(pageId);
    this.released.delete(pageId);
    this.firstDeferredAt.delete(pageId);
    this.clearGateTimer(pageId);
  }

  /**
   * Whether run `run` of `pageId` belongs to a page that was removed after it
   * began: such a run is STOPPED — see the hook's `restoreRuns`.
   */
  removed(pageId: string, run: number): boolean {
    return run <= (this.removedThrough.get(pageId) ?? 0);
  }

  /**
   * Whether `pageId` is owed a retry: a run of it was refused and none has
   * restored since. Its next run, however triggered, is that retry.
   */
  owes(pageId: string): boolean {
    return this.refusals.has(pageId);
  }

  /** Whether the owed page's gate-hold ran out, so its auto-save gate is open again. */
  gateReleased(pageId: string): boolean {
    return this.released.has(pageId);
  }

  /** Whether a save of `pageId` must wait: a retry is owed and its gate-hold has not run out. */
  holdsSave(pageId: string): boolean {
    return this.owes(pageId) && !this.gateReleased(pageId);
  }

  /**
   * A run of `pageId` begins — the retry, if one was owed: its wait is over.
   * Returns the run's token, which {@link defer}, {@link rearm} and
   * {@link removed} take.
   */
  begin(pageId: string): number {
    withdrawResourceGuardWait(restoreGuardWaitKey(pageId));
    this.runSeq += 1;
    return this.runSeq;
  }

  /** A run of `pageId` restored without a guard refusal: the backoff starts over. */
  restored(pageId: string): void {
    this.refusals.delete(pageId);
    this.latest.delete(pageId);
    this.released.delete(pageId);
    this.firstDeferredAt.delete(pageId);
    this.clearGateTimer(pageId);
  }

  /**
   * The hook unmounts: every published wait and gate-hold timer goes with it.
   * What is OWED is kept — it is this instance's record, read by the unmount
   * save ({@link holdsSave}) and by a StrictMode re-mount's next run — and so
   * is each page's first-deferral time, from which the next {@link defer} or
   * {@link rearm} re-arms the hold for whatever of it is left.
   */
  withdrawAll(): void {
    for (const pageId of this.refusals.keys()) {
      withdrawResourceGuardWait(restoreGuardWaitKey(pageId));
      this.clearGateTimer(pageId);
    }
  }

  private arm(w: RestoreRerunWiring, pageId: string, run: number): void {
    this.holdGate(pageId);
    const refusals = this.refusals.get(pageId) ?? 1;
    const delay = guardWaitMs(refusals);
    this.publish(pageId, Date.now() + delay);
    w.timers.set(pageId, delay, () => {
      if (this.removed(pageId, run)) return;
      // The trigger runs the page at once when it is on screen (whose run's
      // `begin` withdraws this), and otherwise only arms its next activation.
      this.publish(pageId, null);
      w.trigger(pageId, true);
    });
  }

  private publish(pageId: string, retryAt: number | null): void {
    const latest = this.latest.get(pageId);
    if (!latest) return;
    publishResourceGuardWait({
      key: restoreGuardWaitKey(pageId),
      sourceLabel: latest.label,
      metric: latest.deferral.refusal.metric,
      reading: parseResourceGuardReading(latest.deferral.refusal),
      refusals: this.refusals.get(pageId) ?? 1,
      retryAt,
    });
  }

  /**
   * Arm the owed page's gate-hold unless one is pending or it already ran
   * out, for what is LEFT of {@link GUARD_GATE_HOLD_MS} since the first
   * deferral — so a hold whose timer an unmount cleared (a StrictMode
   * re-mount) still opens at the bound, and opens at once when the bound has
   * already passed.
   */
  private holdGate(pageId: string): void {
    if (!this.owes(pageId) || this.gateTimers.has(pageId) || this.released.has(pageId)) return;
    const first = this.firstDeferredAt.get(pageId) ?? Date.now();
    const left = first + GUARD_GATE_HOLD_MS - Date.now();
    if (left <= 0) {
      this.releaseGate(pageId);
      return;
    }
    this.gateTimers.set(
      pageId,
      setTimeout(() => {
        this.gateTimers.delete(pageId);
        if (this.owes(pageId)) this.releaseGate(pageId);
      }, left),
    );
  }

  private releaseGate(pageId: string): void {
    this.released.add(pageId);
    this.onGateRelease(pageId);
  }

  private clearGateTimer(pageId: string): void {
    const timer = this.gateTimers.get(pageId);
    if (timer === undefined) return;
    clearTimeout(timer);
    this.gateTimers.delete(pageId);
  }
}

/**
 * What the orphan-adoption pass found to adopt onto `pageId`, after the page
 * that owned the orphan-adoption claim was removed (`removedPageIds`: every
 * page this window has removed): the records of its restore set
 * with adoption on that were ALREADY orphans and that the removed pages' runs
 * had not reached: none that closed with a removed page
 * ({@link closedWithRemovedPage}). A drain-withheld set finds nothing to adopt:
 * the drain lift re-runs the page in full.
 *
 * `unreadable` when the registry or the terminal list could not be read: the
 * pass adopted nothing and is still owed. `candidates` names the orphans it
 * would have weighed, when the registry was read. Exported for tests.
 */
export async function readOrphanAdoption(
  pageId: string,
  removedPageIds: ReadonlySet<string>,
  handledByRemoved: ReadonlySet<string>,
): Promise<OrphanAdoptionRead> {
  const restoreSet = await fetchRestoreSet(pageId, {
    knownPageIds: loadKnownPageIds(),
    adoptOrphans: true,
  });
  if (restoreSet.readFailed) return { kind: "unreadable", candidates: null };
  if (restoreSet.deferredByDrain !== null) return { kind: "read", ids: [] };
  const orphans = restoreSet.records.filter((rec) => (rec.pageId ?? "default") !== pageId);
  // Known to have closed before the terminal list is read: for the notice.
  const candidates = orphans.filter(
    (rec) => !closedWithRemovedPage(rec, pageId, removedPageIds, handledByRemoved, new Set()),
  );
  // No candidate: nothing to adopt, whatever the terminal list would say.
  if (candidates.length === 0) return { kind: "read", ids: [] };
  const onRemovedPages = await liveTerminalIdsOnPages(removedPageIds);
  if (onRemovedPages === null) {
    return { kind: "unreadable", candidates: candidates.map((rec) => rec.claudeSessionId) };
  }
  return {
    kind: "read",
    ids: candidates
      .filter(
        (rec) =>
          !closedWithRemovedPage(rec, pageId, removedPageIds, handledByRemoved, onRemovedPages),
      )
      .map((rec) => rec.claudeSessionId),
  };
}

/**
 * Whether `rec`, read by an adopting restore of `pageId` (the orphan-adoption
 * pass's or a run holding the claim), closed with a page this window removed
 * — the ONE test both apply, so closing a page never resurrects its sessions
 * however the claim moved on. A record of `pageId` itself never did: a page
 * that exists under a removed id again owns its records. Otherwise it did
 * when it is a removed page's own record (whether or not the backend has
 * closed it yet — `terminal_close` is fire-and-forget), an orphan a removed
 * page's runs had restored or held (`closedSessions`), or a record whose live
 * tab is still bound to a removed page (`liveOnRemovedPages`).
 * Exported for tests.
 */
export function closedWithRemovedPage(
  rec: TerminalSessionRecord,
  pageId: string,
  removedPageIds: ReadonlySet<string>,
  closedSessions: ReadonlySet<string>,
  liveOnRemovedPages: ReadonlySet<string>,
): boolean {
  const recPage = rec.pageId ?? "default";
  if (recPage === pageId) return false;
  return (
    removedPageIds.has(recPage) ||
    closedSessions.has(rec.claudeSessionId) ||
    liveOnRemovedPages.has(rec.terminalId)
  );
}

/** What {@link readOrphanAdoption} found. */
export type OrphanAdoptionRead =
  | { kind: "read"; ids: string[] }
  | { kind: "unreadable"; candidates: string[] | null };

/**
 * How many times in a row a removal's orphan-adoption pass may fail to read
 * before it stops and says so: 7 — with the guard's backoff (15 s doubling to
 * the 120 s cap) the last attempt falls 585 s after the first failure, inside
 * the 10-minute horizon the guard holds a page's auto-save gate for
 * (`GUARD_GATE_HOLD_MS`). A registry or terminal list unreadable for that long
 * is not a blip; retrying beyond it would only hide the loss.
 */
export const ORPHAN_PASS_MAX_ATTEMPTS = 7;

/** A removal's orphans that its adoption pass gave up on, shown to the operator. */
export interface UnadoptedOrphans {
  removedPageIds: readonly string[];
  /** The orphans' session ids, or `null` when the registry was never read. */
  sessionIds: readonly string[] | null;
}

/**
 * What an ADOPTING restore run (one holding the orphan-adoption claim) leaves
 * of the unadopted-orphans notice: it read `read` from a readable registry and
 * restored or held the orphans in `handled`. A run that reached every record
 * (`complete`) adopted every orphan there was, so nothing is left unadopted.
 * Otherwise an orphan it handled is adopted, and one it no longer read has
 * closed. Returns `list` itself when nothing changed. Exported for tests.
 */
export function pruneUnadoptedOrphans(
  list: readonly UnadoptedOrphans[],
  read: ReadonlySet<string>,
  handled: ReadonlySet<string>,
  complete: boolean,
): readonly UnadoptedOrphans[] {
  if (list.length === 0) return list;
  if (complete) return [];
  let changed = false;
  const next = list.flatMap((u) => {
    if (u.sessionIds === null) return [u];
    const left = u.sessionIds.filter((id) => read.has(id) && !handled.has(id));
    if (left.length === u.sessionIds.length) return [u];
    changed = true;
    return left.length > 0 ? [{ ...u, sessionIds: left }] : [];
  });
  return changed ? next : list;
}

/** `pageId` when it exists in the page list, else the list's first page. */
function existingPageFor(pageId: string): string {
  const known = loadKnownPageIds();
  return known.includes(pageId) ? pageId : known[0];
}

/** The operator's notice for {@link UnadoptedOrphans}. */
export function unadoptedOrphansNotice(u: UnadoptedOrphans): string {
  const which =
    u.sessionIds === null
      ? "orphaned sessions it could not name (the session registry could not be read)"
      : `orphaned session${u.sessionIds.length === 1 ? "" : "s"} ${u.sessionIds.join(", ")}`;
  return (
    `Closing ${u.removedPageIds.length === 1 ? "page" : "pages"} ${u.removedPageIds.join(", ")} ` +
    `left ${which}, and no page could adopt them. ` +
    `They are not restored; resume them from Previous Sessions.`
  );
}

/**
 * The orphan-adoption claim while a removal's adoption pass is owed: held by
 * no page, so no run that starts meanwhile adopts an orphan the pass is about
 * to. The pass hands it to the page it adopts onto. No page has this id.
 */
export const ORPHAN_CLAIM_IN_PASS = "\u0000orphan-adoption-pass";

/** One restore run of a page, by its run token (`RestoreGuardRetry.begin`). */
export interface RestoreRunEntry {
  pageId: string;
  /** Its `finally` ran. */
  ended: boolean;
  /** The orphans it restored or held: they close with its page. */
  orphansHandled: Set<string>;
}

/** The runs of `pageId` that its removal stopped, with their tokens. */
function stoppedRunsOf(
  runs: ReadonlyMap<number, RestoreRunEntry>,
  pageId: string,
  retry: RestoreGuardRetry,
): Array<[number, RestoreRunEntry]> {
  return [...runs].filter(([token, run]) => run.pageId === pageId && retry.removed(pageId, token));
}

/** The `RerunTimers` key of the orphan-adoption pass's retry; no page has this id. */
const ORPHAN_PASS_TIMER_KEY = "\u0000orphan-adoption-retry";

/** Close the pane a stopped restore run spawned. */
function closeLateSpawnPane(tabId: string): void {
  invoke("terminal_close", { terminalId: tabId }).catch((err) => {
    console.warn(`[TerminalPage] closing ${tabId}, spawned for a removed page, failed:`, err);
  });
}

/**
 * Where a session goes whose pane a STOPPED restore run (its page,
 * `stoppedPageId`, was removed mid-spawn) had just spawned — decided by the
 * record's page AS IT IS NOW, never by the run that spawned it:
 * - `restore`: the page exists (it may have been re-created) — that page
 *   restores it, through its re-run queue (or its own first restore);
 * - `adopt`: the page is gone and the record was an ORPHAN the run had
 *   adopted — the removal's orphan-adoption pass adopts it onto the page on
 *   screen;
 * - `close`: the page is gone and the record was the removed page's own — it
 *   closed with its page.
 * The pane itself is closed in every case: it is bound to the removed page.
 * Exported for tests.
 */
export function routeLateSpawn(
  rec: TerminalSessionRecord,
  stoppedPageId: string,
  knownPageIds: readonly string[],
  w: RestoreRerunWiring,
): "restore" | "adopt" | "close" {
  const recPage = rec.pageId ?? "default";
  if (knownPageIds.includes(recPage)) {
    scheduleRestoreRerun(w, recPage, [rec.claudeSessionId]);
    return "restore";
  }
  return recPage === stoppedPageId ? "close" : "adopt";
}

/**
 * The ids of the live terminals bound to `pageId`, or `null` when the terminal
 * list cannot be read — UNKNOWN, on which the adoption pass adopts nothing
 * rather than risk taking over a closed page's pane, and tries again later.
 */
async function liveTerminalIdsOnPages(pageIds: ReadonlySet<string>): Promise<Set<string> | null> {
  try {
    const result = await invoke<{
      success: boolean;
      data?: { terminals: Array<{ id: string; pageId?: string; page_id?: string }> };
    }>("terminal_list");
    if (!result?.success || !result.data) return null;
    return new Set(
      result.data.terminals
        .filter((t) => pageIds.has(t.pageId || t.page_id || "default"))
        .map((t) => t.id),
    );
  } catch (err) {
    console.warn("[TerminalPage] terminal_list unreadable for the orphan-adoption pass:", err);
    return null;
  }
}

/** The restore set for a page, or the coord device drain's deferral of it. */
export interface RestoreSet {
  records: TerminalSessionRecord[];
  /**
   * Non-null when coord's device drain DEFERRED the restore (plan
   * `2026-09-13-drained-runner-never-reaches-idle`, Phase 3): restoring tabs
   * respawns sessions autonomously, so the backend withheld the set. This is
   * NOT "no sessions" — the records are untouched and the restore must re-run
   * once autonomous spawns are allowed again.
   */
  deferredByDrain: string | null;
  /**
   * True when the registry could not be read (the command failed, or answered
   * without a session list): `records` is then empty because NOTHING is known,
   * not because there are no sessions.
   */
  readFailed: boolean;
}

/**
 * {@link fetchOpenRecords}, keeping the drain deferral and a failed read
 * distinguishable from an empty restore set. Exported for unit tests.
 */
export async function fetchRestoreSet(
  pageId: string,
  opts?: { knownPageIds?: readonly string[]; adoptOrphans?: boolean },
): Promise<RestoreSet> {
  let resp: CommandResponse | null;
  try {
    // `purpose: "restore"` marks THIS as the boot restore's own read: only it
    // may tell the backend's lifecycle poll that the restore was withheld or
    // has run (plan `2026-10-01-drain-deferred-restore-is-swept-as-orphans`).
    // The other readers of this command must not.
    resp = await invoke<CommandResponse>("terminal_session_list_open", { purpose: "restore" });
  } catch (err) {
    console.warn("[TerminalPage] terminal_session_list_open failed:", err);
    return { records: [], deferredByDrain: null, readFailed: true };
  }
  const data = resp?.data as
    | { sessions?: TerminalSessionRecord[]; deferredByDrain?: { reason?: unknown } | null }
    | undefined;
  if (data?.deferredByDrain && typeof data.deferredByDrain === "object") {
    const reason = data.deferredByDrain.reason;
    return {
      records: [],
      deferredByDrain:
        typeof reason === "string" && reason.length > 0
          ? reason
          : "deferred by the coord device drain",
      readFailed: false,
    };
  }
  const sessions = data?.sessions;
  if (!resp?.success || !Array.isArray(sessions)) {
    return { records: [], deferredByDrain: null, readFailed: true };
  }
  const byId = new Map<string, TerminalSessionRecord>();
  for (const rec of sessions) {
    if (!rec || typeof rec.claudeSessionId !== "string") continue;
    if (
      !recordBelongsToRestore(rec, pageId, opts?.knownPageIds ?? [], opts?.adoptOrphans ?? false)
    ) {
      continue;
    }
    if (!byId.has(rec.claudeSessionId)) byId.set(rec.claudeSessionId, rec);
  }
  return { records: [...byId.values()], deferredByDrain: null, readFailed: false };
}

/**
 * Does `rec` belong to THIS page's restore run?
 *
 * - Its `pageId` matches ⇒ yes (the normal case).
 * - Its `pageId` names a page that EXISTS in the persisted layout
 *   (`knownPageIds`) ⇒ no — that page's own restore run owns it.
 * - Its `pageId` names NO live page (an ORPHAN — e.g. the hook-confirm path
 *   wrote its hardcoded "default" onto a layout whose default page was
 *   replaced) ⇒ adopted by the FIRST page to restore (`adoptOrphans`), but
 *   ONLY when the record classifies `auto-resume` or `needs-account` (a
 *   confirmed, resumable provider session someone would actually miss — the
 *   latter waiting only for its account). Unconfirmed shells and
 *   quarantine-tier guesses are never adopted — restoring a stale agent
 *   shell into the operator's page would be noise, not recovery.
 *
 * Without adoption, an orphan record is never restored on any page and the
 * no-terminal sweep closes it ~3 min after boot — permanent, silent session
 * loss (observed live 2026-07-13: two confirmed gmail sessions stranded on
 * "default"). Pure + exported for unit tests.
 */
export function recordBelongsToRestore(
  rec: TerminalSessionRecord,
  pageId: string,
  knownPageIds: readonly string[],
  adoptOrphans: boolean,
): boolean {
  const recPage = rec.pageId ?? "default";
  if (recPage === pageId) return true;
  if (!adoptOrphans) return false;
  if (knownPageIds.includes(recPage)) return false;
  // A confirmed resumable session whose ACCOUNT is unknown is adopted too: it
  // is just as missed, and adopting it is what surfaces it for an account
  // choice instead of leaving it on no page at all. The default home only
  // decides between those two (an unknown home can make a dir needs-account),
  // and both are adopted, so it is not needed here.
  const action = classifyRestoreAction(rec, null);
  return action === "auto-resume" || action === "needs-account";
}

/** The gate a resumed continuation reports to (from its durable record). */
export interface ResumeGateIdentity {
  gateId?: string;
  gateConsumingDeviceId?: string;
}

const UUID_RE = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/**
 * `QONTINUI_GATE_ID` / `QONTINUI_GATE_DEVICE_ID` for a resumed continuation —
 * BOTH or neither (coord accepts neither half alone), and only when both are
 * well-formed UUIDs, since they are interpolated into a shell command.
 */
export function resumeGateEnv(gate: ResumeGateIdentity | undefined): Array<[string, string]> {
  const gateId = gate?.gateId?.trim();
  const deviceId = gate?.gateConsumingDeviceId?.trim();
  if (!gateId || !deviceId || !UUID_RE.test(gateId) || !UUID_RE.test(deviceId)) return [];
  return [
    ["QONTINUI_GATE_ID", gateId],
    ["QONTINUI_GATE_DEVICE_ID", deviceId],
  ];
}

/**
 * Build a `claude --resume <id>` command, optionally prefixed with
 * CLAUDE_CONFIG_DIR and (under the default full-resume policy) with the CLI's
 * resume-size thresholds raised so the interactive "Resume from summary?"
 * picker never shows — an unattended restore can't answer it, and a wedged
 * picker reads as a failed resume (#548 item 3). No suppression flag exists;
 * the CLI consults `CLAUDE_CODE_RESUME_TOKEN_THRESHOLD` (default 100000
 * estimated tokens) and `CLAUDE_CODE_RESUME_THRESHOLD_MINUTES` (default 70)
 * before showing it, and skipping it resumes the full session as-is. Scoped
 * to the typed command (per-process) deliberately: seeding the CLI's global
 * "Don't ask again" key (`resumeReturnDismissed` in `.claude.json`) would
 * also kill the picker for the operator's own interactive resumes.
 *
 * Under the opt-in `"summary"` policy the thresholds are left alone so the
 * picker appears and the verification loop answers it
 * (`buildPickerAnswer`). Exported for unit tests.
 *
 * Every resume, for every provider, is RENDERED BY THE BACKEND
 * (`build_ai_resume_command`), never assembled here. The backend owns the two decisions a typed resume must not
 * get wrong: which `CLAUDE_CONFIG_DIR` the session runs under (a declared model
 * gateway replaces the recorded subscription dir, so a subscription token can
 * never ride a gateway-routed session) and the permission posture (bypass, or
 * the operator's allow-list). The resume-summary env thresholds ride along
 * under the default `"full"` policy.
 *
 * A non-Claude provider is rendered there too (review N7): the backend
 * applies the gateway's provider refusal and validates/quotes the session id.
 */
export async function buildResumeCmd(
  sessionId: string,
  configDir: string | undefined,
  policy: ResumeSummaryPolicy = getResumeSummaryPolicy(),
  provider?: string,
  gate?: ResumeGateIdentity,
): Promise<string> {
  // Every provider is rendered by the backend (review N7): a provider that
  // cannot route through a declared model gateway is refused there, and the
  // session id is validated/quoted there — never assembled here.
  const resumeProvider = provider ?? "claude";
  // A resumed gate continuation must still be able to report its own work
  // outcome, so its gate identity rides the typed resume exactly as it rode
  // the original spawn's env. Sent only as a well-formed pair; the backend
  // validates it again before rendering it into the shell command.
  const gateEnv = Object.fromEntries(resumeGateEnv(gate));
  const gateArgs =
    "QONTINUI_GATE_ID" in gateEnv
      ? { gateId: gateEnv.QONTINUI_GATE_ID, gateDeviceId: gateEnv.QONTINUI_GATE_DEVICE_ID }
      : {};
  const isWindows =
    typeof navigator !== "undefined" && (navigator.platform ?? "").startsWith("Win");
  const resp = await invoke<CommandResponse>("build_ai_resume_command", {
    sessionId,
    provider: resumeProvider,
    configDir: configDir ?? null,
    isWindows,
    fullResume: policy === "full",
    ...gateArgs,
  });
  const command = (resp?.data as { command?: string } | null | undefined)?.command;
  if (!resp?.success || !command) {
    throw new Error(resp?.message ?? "build_ai_resume_command returned no command");
  }
  return `${command}\r`;
}

/**
 * Type the resume command for a restored tab and VERIFY the Claude UI
 * handshake actually appeared (Phase 3, issue #548): on first failure the
 * same command is retyped once; on persistent failure the tab is parked in an
 * explicit `resumeFailed` state (operator-clickable retry via
 * `ResumeFailedBanner`) and the durable restore-pending marker is left SET so
 * the backend liveness poll keeps protecting the `open` record. Only a
 * verified handshake clears the marker and the reconnecting affordance.
 *
 * Used by the boot-restore drain below AND by the operator retry path in
 * `TerminalPage`. Exported (with injectable verify options) so the
 * verified/failed state machine is unit-testable without booting React.
 */
export async function runVerifiedResume(params: {
  terminalRefs: TerminalRefsMap;
  tabId: string;
  claudeSessionId: string;
  configDir?: string;
  /** Provider owning the session — selects the adapter resume + handshake. */
  provider?: string;
  /** Gate identity of a resumed continuation — re-injected into the resume. */
  gate?: ResumeGateIdentity;
  updateTab: (
    id: string,
    updates: Partial<{ isReconnecting?: boolean; resumeFailed?: boolean }>,
  ) => void;
  /**
   * Registry re-assert payload for the VERIFIED branch (boot-restore item 3):
   * the OPEN record is re-recorded under the freshly created terminal id
   * ONLY after the resume handshake verified. Re-asserting at tab-creation
   * time refreshed `lastSeenAt` on rows whose resume then failed — resetting
   * both the prune clock and the recency cap, making ghost rows immortal. On
   * `failed` the original row is left untouched (restore-pending already
   * protects it from the liveness poll). Omitted = no re-assert (operator
   * retry without zone context, tests).
   */
  recordOpen?: SessionOpenArgs;
  verifyOptions?: TypeAndVerifyOptions;
  /** Injectable for tests: the default Claude home (see `defaultConfigHome.ts`). */
  defaultConfigHome?: () => Promise<string | null>;
}): Promise<ResumeOutcome> {
  const {
    terminalRefs,
    tabId,
    claudeSessionId,
    configDir,
    provider,
    gate,
    updateTab,
    recordOpen,
    verifyOptions,
  } = params;
  const policy = getResumeSummaryPolicy();
  // THE choke point every typed resume goes through: the default home is never
  // typed as `CLAUDE_CONFIG_DIR` (it would swap `~/.claude.json` for
  // `~/.claude/.claude.json`). Records keep the explicit path.
  const home = await (params.defaultConfigHome ?? loadDefaultConfigHome)();
  let resumeCmd: string;
  try {
    resumeCmd = await buildResumeCmd(
      claudeSessionId,
      typedConfigDir(configDir, home),
      policy,
      provider,
      gate,
    );
  } catch (err) {
    // The backend refused to render a resume (e.g. an unresolved model
    // gateway). Type nothing; park the tab for an explicit retry and keep the
    // restore-pending marker, exactly like an unverified resume.
    console.warn(`[TerminalPage] resume not rendered for ${claudeSessionId}:`, err);
    updateTab(tabId, { isReconnecting: false, resumeFailed: true });
    return "failed";
  }
  // Per-adapter handshake patterns (Phase 4): the verification loop matches the
  // resume success/failure against the descriptor's patterns instead of the
  // Claude-hardcoded sets, so a future Gemini resume verifies against Gemini's
  // banners. Claude's descriptor mirrors the live `resumeVerification.ts` sets.
  const handshakePatterns = providerDescriptorFor(provider).handshakePatterns();
  const outcome = await typeResumeAndVerify(terminalRefs, tabId, resumeCmd, {
    // Fallback picker answerer (#548 item 3): under the default "full" policy
    // the env thresholds in `buildResumeCmd` already suppress the picker;
    // this catches CLI version drift and the opt-in "summary" policy.
    pickerAnswer: buildPickerAnswer(policy),
    handshakePatterns,
    sessionId: claudeSessionId,
    ...verifyOptions,
  });
  if (outcome === "verified") {
    updateTab(tabId, { isReconnecting: false, resumeFailed: false });
    // Verified handshake — NOW re-assert the OPEN record under the live
    // terminal id so the registry tracks this tab (the next restart
    // reconnect-matches on it). Deliberately after verification, never
    // before: see `recordOpen` docs. No `origin` is sent, so the
    // backend preserves the existing origin (unasserted re-record).
    if (recordOpen) {
      invoke("terminal_session_record_open", { ...recordOpen }).catch((err) => {
        console.warn(`[TerminalPage] re-record open failed for ${claudeSessionId}:`, err);
      });
      // This payload was frozen at restore time, so its `zoneIndex` is the one
      // the RECORD carried before the restore — including a stale `-1` for a
      // session that has since been auto-filled into a real zone. Rather than
      // resolve the zone here (the drain has no live view of the assignments),
      // tell the page's recorded-zone ledger what we just wrote: clearing
      // `isReconnecting` below mutates `tabs`, which re-runs the re-resolution
      // backstop in `TerminalPage`, which sees the disagreement and corrects
      // the record. Ordering-safe in both directions — whichever of the two
      // writes lands last, the ledger disagrees with reality until a correction
      // is emitted.
      noteRecordedZone(
        recordedZoneLedgerFor(recordOpen.pageId),
        claudeSessionId,
        recordOpen.zoneIndex,
      );
    }
    // Release the liveness-poll restore guard so normal classification
    // resumes for this session.
    invoke("terminal_session_clear_restore_pending", { claudeSessionId }).catch((err) => {
      // Best-effort: the poll self-heals a stale marker on the next
      // confident-alive tick.
      console.warn(`[TerminalPage] clear restore-pending failed for ${claudeSessionId}:`, err);
    });
  } else {
    // Resume never landed — or was not typed because the pane could not be
    // checked or already runs a claude not provably this session (see
    // `ResumeOutcome`). Surface an explicit retry affordance and KEEP the
    // restore-pending marker — the open record must survive for the retry.
    console.warn(
      `[TerminalPage] resume verification failed for ${tabId} (session ${claudeSessionId})`,
    );
    updateTab(tabId, { isReconnecting: false, resumeFailed: true });
  }
  return outcome;
}

/**
 * Report a mount of the terminal page tree to the backend
 * (`terminal_report_tree_reset`) so a tree reset is durably observable — P0
 * tree-reset observability. Until now the only trace of a remount (top-level
 * state flip → restore respawns `claude --resume`) was a webview
 * `console.warn` nothing captured.
 *
 * Collects the payload best-effort:
 * - `navigationType` — `PerformanceNavigationTiming.type`, the
 *   reload-vs-in-app-remount discriminator (a fresh "reload"/"navigate" entry
 *   means a full webview load; a remount without one is an in-app tree reset);
 * - `openRecordCount` — size of the restorable set the restore is about to
 *   consider, via the same read-only `terminal_session_list_open` query the
 *   restore itself uses.
 *
 * Fire-and-forget and observability-only: never throws, never blocks or
 * affects initialization. Exported for unit tests.
 */
export async function reportTreeReset(params: {
  mountNumber: number;
  authenticated?: boolean;
  pageIds: string[];
  clientTs?: number;
}): Promise<void> {
  try {
    let navigationType: string | undefined;
    try {
      const nav = performance.getEntriesByType("navigation")[0] as
        | PerformanceNavigationTiming
        | undefined;
      navigationType = nav?.type;
    } catch {
      // No navigation timing surface (tests) — report without it.
    }
    let openRecordCount: number | undefined;
    try {
      const resp = await invoke<CommandResponse>("terminal_session_list_open");
      const data = resp?.data as { sessions?: unknown[]; deferredByDrain?: unknown } | undefined;
      // While coord's device drain withholds the restore set, `sessions` is
      // empty by deferral, not by count — report the count as unknown.
      if (!data?.deferredByDrain && Array.isArray(data?.sessions)) {
        openRecordCount = data.sessions.length;
      }
    } catch {
      // Count unavailable — a partial report is still worth a row.
    }
    await invoke("terminal_report_tree_reset", {
      mountNumber: params.mountNumber,
      authenticated: params.authenticated,
      navigationType,
      pageIds: params.pageIds,
      openRecordCount,
      // Same-document proof: rows sharing a timeOrigin belong to ONE webview
      // document, so a second row with the same timeOrigin is a tree reset by
      // construction (navigationType alone is document-scoped and cannot
      // distinguish an in-app remount from the original load).
      timeOrigin: performance.timeOrigin,
      clientTs: params.clientTs,
    });
  } catch (err) {
    console.warn("[TerminalPage] tree-reset report failed:", err);
  }
}

/**
 * Document-scoped mount counter for the tree-reset report. Deliberately MODULE
 * state, not a `useRef`: a genuine tree reset destroys the fiber (and any ref
 * with it), so a ref-scoped counter reports every real remount as mount #1 —
 * indistinguishable from a fresh boot. Module state survives React remounts
 * within the same JS realm and resets only on a real document load, which is
 * exactly the remount-vs-fresh-load line the diagnostic must draw. (Dev-only
 * StrictMode double-mount writes a spurious mount #2 row on boot; exe-mode
 * runners don't run StrictMode, so a `mountNumber > 1` row in prod IS a tree
 * reset.)
 */
let documentMountCount = 0;

/**
 * Pure guard for the once-per-pageId init backstop (Phase 3 mount-hydration
 * lift). With the session provider lifted above the page, the single
 * `useTerminalInitialization` instance persists across terminal-page switches,
 * so the convergence backstop (reconnect + durable-record restore + resume
 * drain) must run exactly ONCE per pageId — not once per mount. Returns `true`
 * (and records `pageId` as seen) the FIRST time a page is initialized;
 * `false` on every subsequent call for that page.
 *
 * Mutating the passed Set keeps the call-site a one-liner and lets the hook
 * hold the Set in a ref. Exported so the per-pageId semantics can be unit-
 * tested without booting React (vitest `environment: "node"`).
 */
/**
 * May a restored record claim its RECORDED zone? On a first restore, always
 * (records own their zones — see the binding loop). On a restore RE-RUN after a
 * coord device drain lifted, the operator may have opened terminals in the
 * meantime, so a record claims its zone only when that zone is empty or already
 * holds this very tab; otherwise the tab is left unassigned and auto-fill places
 * it in the first free zone. Exported for tests.
 */
export function mayClaimRecordedZone(
  isDrainRerun: boolean,
  zoneIndex: number,
  tabId: string,
  assignments: Record<number, string>,
  zoneCount: number,
): boolean {
  if (zoneIndex < 0) return false;
  if (!isDrainRerun) return true;
  // The grid may have shrunk since the record was written. A tab assigned past
  // the last zone renders nowhere, and it is not "unassigned" either, so nothing
  // reports it. Auto-fill gives it a zone that exists.
  if (zoneIndex >= zoneCount) return false;
  const holder = assignments[zoneIndex];
  return holder === undefined || holder === tabId;
}

export function claimInitForPage(seen: Set<string>, pageId: string): boolean {
  if (seen.has(pageId)) return false;
  seen.add(pageId);
  return true;
}

/**
 * Decide what the restore path does with the `reconnectToExistingSessions`
 * result (P1 restore idempotence, defect (a)).
 *
 * The reconnect returns `null` ONLY when the live-terminal list could not be
 * read (invoke failed / unsuccessful / malformed response) — an INDETERMINATE
 * outcome, distinct from `[]` ("definitely nothing to reconnect", the normal
 * cold start). The old call site coerced `null` to `[]`, which sent EVERY
 * open record down the cold `claude --resume` respawn path while its previous
 * terminal generation was still alive — each restore pass orphaned another
 * generation (measured: one page leaked ~330 orphan PTYs across ~7 passes).
 *
 * On `"abort-restore"` the caller leaves the page exactly as-is — nothing
 * spawned, no record touched — and releases the once-per-pageId init guard so
 * the next activation of the page retries the whole restore.
 *
 * Pure + exported so the null → abort branch is directly unit-testable
 * without booting React.
 */
export type ReconnectDecision =
  | { kind: "abort-restore" }
  | { kind: "proceed"; reconnectedTabIds: readonly string[] };

export function decideReconnectOutcome(
  reconnectedTabIds: readonly string[] | null,
): ReconnectDecision {
  return reconnectedTabIds === null
    ? { kind: "abort-restore" }
    : { kind: "proceed", reconnectedTabIds };
}

/**
 * Decide whether a cold-restored `auto-resume` record may actually respawn
 * `claude --resume <id>` (P1 restore idempotence, defect (b)).
 *
 * Even after a successful reconnect, a record's session can be alive under a
 * process the reconnect did not return — an orphaned previous terminal
 * generation, a PTY on another page, or a process outside the runner
 * entirely. Respawning it forks the live session: two processes on one
 * transcript.
 *
 * - `"respawn"` — the id is definitively not hosted by any live process; the
 *   normal cold-restore path may proceed.
 * - `"skip-alive"` — some live Claude process already hosts this id: NEVER
 *   respawn. The runner owns no reconnectable PTY for that process, so there
 *   is no tab to rebind it to either — the record is left untouched (it stays
 *   restorable/manually resumable); the reconnected-PTY branch remains the
 *   only rebind mechanism.
 * - `"skip-unknown"` — the live registry could not be read (`liveSessionIds
 *   === null`): FAIL CLOSED toward not-respawning. A missing tab is
 *   recoverable; a duplicate live session corrupts the transcript.
 *
 * Pure + exported for direct unit tests.
 */
export type ColdResumeDecision = "respawn" | "skip-alive" | "skip-unknown";

export function decideColdResume(
  liveSessionIds: ReadonlySet<string> | null,
  claudeSessionId: string,
): ColdResumeDecision {
  if (liveSessionIds === null) return "skip-unknown";
  return liveSessionIds.has(claudeSessionId) ? "skip-alive" : "respawn";
}

/**
 * The live reconnected tab a record re-attaches to on a React remount (restore
 * branch (a)), or `null` when it has none.
 *
 * A record held for an account choice (`awaitingAccountSince` set) has NO pane
 * by design: the backend detaches its terminal when it holds it, because its
 * old terminal id may already host another session. Matching it by
 * `terminalId` would stamp that live tab with the held session's id, so a held
 * record never reconnects — whatever `terminalId` it carries. A blank
 * `terminalId` (unbound) names no tab either.
 *
 * Pure + exported for direct unit tests.
 */
export function reconnectedTabFor(
  rec: Pick<TerminalSessionRecord, "terminalId" | "awaitingAccountSince">,
  reconnectedTabIds: ReadonlySet<string>,
): string | null {
  if (rec.awaitingAccountSince != null) return null;
  if (!rec.terminalId.trim()) return null;
  return reconnectedTabIds.has(rec.terminalId) ? rec.terminalId : null;
}

/**
 * What a DRAIN-TIME skip must do to the record + tab it is walking away from.
 *
 * ## THE DEFECT (the loop's oldest, and the reason this is a pure function)
 *
 * The classify loop stamps a durable `restore_pending_at` marker on every
 * `auto-resume` record before the drain runs. At drain time
 * `fetchLiveClaudeSessionIds()` returns `null` on ANY IPC failure, which
 * {@link decideColdResume} maps to `"skip-unknown"` — and the drain simply
 * `continue`d. Nothing was typed, `resumeFailed` was never set (so
 * `ResumeFailedBanner` never rendered), and the record kept its DEAD
 * pre-restart `terminal_id`.
 *
 * That last part is what made it permanent. The backend poll matches records
 * to live terminals by `terminal_id`; a dead one matches nothing, so
 * `live_is_alive` is `None`, so `classify`
 * (`session_lifecycle_store.rs`) can never reach `KeepAlive` — and the
 * restore-pending guard arm turns every other outcome into `Skip`. The
 * KeepAlive branch is the ONLY place the poll clears a stale marker, so the
 * row sat at `tier=failed, pendingAt=set` forever, invisible in the UI.
 *
 * The in-code claim that "the restore-pending marker self-heals via the
 * liveness poll" was therefore FALSE for exactly the rows that reach here, and
 * has been removed rather than left standing as a documented invariant.
 *
 * ## The disposition
 *
 * Both skips REBIND the record to the tab the restore just created. That is
 * what un-freezes the poll: the record points at a terminal that exists, so
 * classification produces a real verdict instead of `Skip`-forever. `rebind`
 * never refreshes `last_seen_at` (unlike a `record_open` re-assert), so it
 * cannot resurrect a ghost row — the reason the auto-resume track defers its
 * re-assert does not apply to it. Rebinding also stamps the honest
 * `terminal-only` restore tier: the terminal came back, the conversation did
 * not.
 *
 * They then diverge on what the operator is told, because the two skips mean
 * different things:
 *
 * - `skip-alive` — a live process already hosts this conversation. Typing
 *   `--resume` would FORK its transcript, so there is nothing to retry: the
 *   honest surface is the informational "terminal restored — fresh
 *   conversation" note, and the marker is CLEARED because no resume is or will
 *   be pending.
 * - `skip-unknown` — the live registry was unreadable, so we failed closed and
 *   did not resume. The conversation may well still be resumable, so this gets
 *   the actionable `resumeFailed` banner whose Retry re-runs
 *   {@link runVerifiedResume}. The marker is cleared here too: leaving it set
 *   is precisely the stranding this fixes, and the rebind has already given
 *   the poll a live terminal to classify against.
 *
 * Pure + exported so the disposition is unit-testable; {@link applyDrainSkip}
 * applies it.
 */
export interface DrainSkipDisposition {
  /** Tab-state patch — always clears the spinner, never leaves it spinning. */
  tabUpdates: { isReconnecting: boolean; resumeFailed?: boolean; restoreTerminalOnly?: boolean };
  /** Re-point the record at the tab the restore created (also stamps the tier). */
  rebindToRestoredTab: boolean;
  /** Clear `restore_pending_at` — nothing else is scheduled to. */
  clearRestorePending: boolean;
}

export function decideDrainSkipDisposition(
  decision: Exclude<ColdResumeDecision, "respawn">,
): DrainSkipDisposition {
  return {
    tabUpdates:
      decision === "skip-alive"
        ? { isReconnecting: false, restoreTerminalOnly: true }
        : { isReconnecting: false, resumeFailed: true },
    rebindToRestoredTab: true,
    clearRestorePending: true,
  };
}

/**
 * Apply a {@link decideDrainSkipDisposition} verdict: patch the tab, then
 * rebind + clear the durable marker.
 *
 * The two IPC calls are ORDERED, not fire-and-forget-in-parallel:
 * `terminal_session_rebind_terminal` stamps the `terminal-only` tier, and
 * `terminal_session_clear_restore_pending` promotes a `failed` tier to
 * `resumed` — running them the other way round would label a skipped restore
 * as a successful resume. Awaiting the rebind first makes that impossible.
 *
 * Never throws: a failed IPC is logged and the remaining steps still run. The
 * worst case is the pre-fix behaviour for one record, and the operator can
 * still see the banner.
 *
 * `invokeFn` is injectable so the wiring — not just the decision — is testable
 * without booting React or Tauri.
 */
export async function applyDrainSkip(params: {
  decision: Exclude<ColdResumeDecision, "respawn">;
  tabId: string;
  claudeSessionId: string;
  zoneIndex: number;
  updateTab: (
    id: string,
    updates: Partial<{
      isReconnecting?: boolean;
      resumeFailed?: boolean;
      restoreTerminalOnly?: boolean;
    }>,
  ) => void;
  invokeFn?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
}): Promise<DrainSkipDisposition> {
  const { decision, tabId, claudeSessionId, zoneIndex, updateTab } = params;
  const call = params.invokeFn ?? ((cmd, args) => invoke(cmd, args));
  const disposition = decideDrainSkipDisposition(decision);
  updateTab(tabId, disposition.tabUpdates);
  if (disposition.rebindToRestoredTab) {
    try {
      await call("terminal_session_rebind_terminal", {
        claudeSessionId,
        terminalId: tabId,
        zoneIndex,
      });
    } catch (err) {
      console.warn(`[TerminalPage] drain-skip rebind failed for ${claudeSessionId}:`, err);
    }
  }
  if (disposition.clearRestorePending) {
    try {
      await call("terminal_session_clear_restore_pending", { claudeSessionId });
    } catch (err) {
      console.warn(
        `[TerminalPage] drain-skip clear restore-pending failed for ${claudeSessionId}:`,
        err,
      );
    }
  }
  return disposition;
}

/** Validate session IDs before interpolating into shell commands. */
const SESSION_ID_RE = /^[a-zA-Z0-9_-]+$/;
function isValidSessionId(id: string): boolean {
  return SESSION_ID_RE.test(id);
}

/**
 * What the cold-restore path does with a registry record (Phase 4 —
 * autonomous restore).
 *
 * - `"auto-resume"`: the record is `origin === "authoritative"` OR
 *   `origin === "observed"`, AND CONFIRMED (`confirmedAt` set), AND its
 *   transcript was not probed-absent (`transcriptExists !== false`), AND its
 *   provider's `restoreTier()` is `"full"`. For `authoritative` the runner KNOWS
 *   the id (it pre-pinned `--session-id`) AND a provider's SessionStart hook (or
 *   the process-start-anchored reconcile backstop) observed a REAL provider
 *   session start here. `observed` is the same-strength proof arrived at from the
 *   other direction — a claude-process-start-anchored, uniquely-correlated
 *   transcript bind — so a CONFIRMED observed record is treated IDENTICALLY to a
 *   confirmed authoritative one: the resume command is typed unattended, with NO
 *   operator click. This removes the old `guessed→quarantine` gate for both
 *   authoritative and observed records.
 * - `"terminal-only"`: restore terminal+cwd+launch-command, but type NO resume
 *   and show NO operator-confirm banner — there is no conversation to bring
 *   back, and the UI is HONEST about it ("fresh conversation"). Three distinct
 *   sources land here:
 *     1. an authoritative/observed-but-PROVISIONAL record — the spawn-time pin
 *        wrote a provisional record for EVERY terminal, including plain shells
 *        that never ran a provider (a "phantom shell"); auto-`--resume`-ing it
 *        would manufacture a failed resume against an unused uuid. The backend
 *        reconcile normally PRUNES these before the frontend ever classifies
 *        (it confirms the ones with a real transcript, drops the rest); this
 *        branch is the frontend's defense-in-depth for a cold-boot phantom the
 *        reconcile couldn't reach (no live PTY).
 *     2. a CONFIRMED authoritative/observed record whose provider declares
 *        `restoreTier() === "terminal-only"` (Phase 5 honest tiers) — the
 *        provider can re-open the terminal at the right cwd/launch-command but
 *        CANNOT deterministically resume the conversation by id. The terminal
 *        is restored; the UI notes the conversation is fresh.
 *     3. a CONFIRMED record the backend probed as having NO transcript on disk
 *        (`transcriptExists === false`) — the session STARTED (that is all
 *        `confirmedAt` attests) but never accumulated messages, so the provider
 *        never wrote a transcript and `--resume` can only fail. Distinct from
 *        (1): the record is genuinely confirmed, so no reconcile prune covers
 *        it, and it survives every boot until the row is closed.
 *   A `"reconciled"` (or pre-field / any other) origin — a backstop-recovered
 *   id that can name a FOREIGN session — lands here too: it is NOT proof
 *   strong enough to type a resume or to ask the operator to confirm a guess.
 *   The tab is created so layout is preserved, but nothing is done with the
 *   guessed id beyond that — an honest, click-free "fresh conversation"
 *   restore, identical to any other unresumable row.
 * - `"skip-invalid"`: the recorded id fails shell-safety validation — never
 *   typed, nothing actionable.
 * - `"needs-account"`: a Claude record that would otherwise auto-resume, but
 *   whose ACCOUNT cannot be established (see {@link resolveRestoreAccount}):
 *   its recorded `configDir` was rejected by `sanitizeConfigDir`, or may be
 *   the default home while that home is unreadable, or it has none and zero /
 *   several config dirs hold its transcript. NOT auto-resumed
 *   — a `--resume` under a guessed (default) account fails with "No
 *   conversation found", which reads exactly like a session that never
 *   existed. The hook surfaces it instead (`needsAccountRecords`) for the
 *   operator to resume under a chosen account (plan
 *   `2026-10-04-runner-session-roster-restore-picker`, Phase 3).
 *
 * The auto-resume GATE lives here on the frontend as
 * `(origin === "authoritative" || origin === "observed") && confirmed &&
 * transcriptExists !== false` (a record is "confirmed" when its `confirmedAt` is
 * set; `transcriptExists` is `undefined` when the backend attached no probe, and
 * unknown must not downgrade). The backend reconcile is the
 * PRIMARY phantom defense (it prunes/flags provisional records for live PTYs
 * before this runs), but the classifier keeps the `confirmed` check so a phantom
 * the reconcile didn't see
 * (cold boot, no live process) still can't be auto-resumed. Pure + exported so
 * the gate is unit-testable without React.
 */
export type RestoreAction = "auto-resume" | "terminal-only" | "skip-invalid" | "needs-account";

/** Why a record's account could not be established. */
export type NeedsAccountReason =
  /** The recorded (or the transcript-located) config dir failed `sanitizeConfigDir`. */
  | "config-dir-rejected"
  /** The default home is unreadable and the recorded dir may be it — never typed on a guess. */
  | "default-home-unknown"
  /** No recorded dir, and no config dir holds the transcript. */
  | "no-transcript-holder"
  /** No recorded dir, and SEVERAL config dirs hold the transcript. */
  | "ambiguous-transcript"
  /** No recorded dir, and the backend did not probe where the transcript lives. */
  | "transcript-unprobed";

/**
 * The account a restore resumes under, resolved from evidence:
 * - `resolved` — the recorded `configDir`, else the UNIQUE config dir holding
 *   the transcript. `recordDir` is the explicit path the REGISTRY record keeps
 *   (`undefined` only when no path is known at all); `typedDir` is what the
 *   resume types as `CLAUDE_CONFIG_DIR` and what the tab carries — `undefined`
 *   for the default home (`~/.claude`), see `resolveAccountDir`;
 * - `unknown` — the account cannot be named: the evidence says so, or there is
 *   none at all (no recorded dir AND no transcript probe). A resume under the
 *   default account in that case is a guess, and a wrong guess fails as "No
 *   conversation found" — so it is asked, never assumed.
 */
export type RestoreAccount =
  | { kind: "resolved"; recordDir: string | undefined; typedDir: string | undefined }
  | { kind: "unknown"; reason: NeedsAccountReason };

type RestoreAccountFields = Pick<
  TerminalSessionRecord,
  "configDir" | "transcriptConfigDir" | "transcriptConfigDirCount" | "transcriptConfigDirIsDefault"
>;

/**
 * Resolve a record's account: recorded `configDir` first, then a unique
 * `transcriptConfigDir`. The same order as the Rust
 * `session_ledger::resolve_config_dir`, which builds every copy-able resume
 * line. A recorded dir goes through `resolveAccountDir` against the default
 * home `home` (`null` = unreadable). Pure + exported for unit tests.
 */
export function resolveRestoreAccount(
  rec: RestoreAccountFields,
  home: string | null,
): RestoreAccount {
  const recorded = rec.configDir?.trim();
  if (recorded) {
    const dir = resolveAccountDir(recorded, home);
    return dir.kind === "resolved"
      ? { kind: "resolved", recordDir: dir.recordDir, typedDir: dir.typedDir }
      : { kind: "unknown", reason: dir.reason };
  }
  const holders = rec.transcriptConfigDirCount;
  if (holders === undefined) return { kind: "unknown", reason: "transcript-unprobed" };
  if (holders === 0) return { kind: "unknown", reason: "no-transcript-holder" };
  if (holders > 1) return { kind: "unknown", reason: "ambiguous-transcript" };
  const transcriptDir = rec.transcriptConfigDir?.trim() || undefined;
  // The backend judged the holder to be the default home: never typed, so it
  // needs no shell-safety check.
  if (rec.transcriptConfigDirIsDefault === true) {
    return { kind: "resolved", recordDir: transcriptDir, typedDir: undefined };
  }
  const located = sanitizeConfigDir(transcriptDir);
  return located
    ? { kind: "resolved", recordDir: located, typedDir: located }
    : { kind: "unknown", reason: "config-dir-rejected" };
}

/**
 * The restored tab's name: THE display-name rule (`displayNameOf`, mirroring
 * Rust `session_ledger::display_name`) — an operator-chosen `/rename` name
 * wins, a `derived` auto-name falls back behind the tab title. Not the bare
 * `title`, which is just `"claude"` for about half of all rows.
 */
export function restoreTabName(
  rec: Pick<TerminalSessionRecord, "claudeSessionId" | "sessionName" | "nameSource" | "title">,
): string {
  return displayNameOf({
    claudeSessionId: rec.claudeSessionId,
    sessionName: rec.sessionName ?? null,
    nameSource: rec.nameSource ?? null,
    title: rec.title ?? null,
  });
}

/** A restore candidate the boot restore did NOT resume because its account is unknown. */
export interface NeedsAccountRecord {
  record: TerminalSessionRecord;
  reason: NeedsAccountReason;
}

/**
 * Hold a `needs-account` record for an account choice — but NEVER a session a
 * live Claude process already hosts. Holding marks the row awaiting-account,
 * which DETACHES it from its terminal; doing that to a live session would cut
 * it off its own pane. So the same liveness gate the cold auto-resume path
 * uses runs first, and only a `"respawn"` verdict (registry read, id not live)
 * lets the hold through. `"skip-alive"` leaves the row untouched and surfaces
 * nothing. `"skip-unknown"` (registry unreadable) still SURFACES the record for
 * an account choice but does not re-mark it: the boot hold may already have
 * held it, and hiding it would leave a held row with no chooser until the hold
 * ages out. Not marking can detach nothing; its cost is that a row no earlier
 * hold covers may be closed `no-terminal` by the liveness poll and so miss the
 * NEXT boot's restore set (this boot's chooser still lists it).
 *
 * Returns the liveness decision. Pure wiring with injected effects, exported
 * so the gate — not just `decideColdResume` — is unit-testable.
 */
export async function holdNeedsAccountIfNotLive(params: {
  entry: NeedsAccountRecord;
  getLiveSessionIds: () => Promise<ReadonlySet<string> | null>;
  noteNeedsAccount: (entry: NeedsAccountRecord, opts: { mark: boolean }) => void;
}): Promise<ColdResumeDecision> {
  const { entry } = params;
  const decision = decideColdResume(await params.getLiveSessionIds(), entry.record.claudeSessionId);
  if (decision === "skip-alive") {
    console.warn(
      `[TerminalPage] not holding needs-account session ${entry.record.claudeSessionId}: ` +
        "a live Claude process already hosts this id",
    );
    return decision;
  }
  if (decision === "skip-unknown") {
    console.warn(
      `[TerminalPage] live-session registry unreadable — surfacing needs-account session ` +
        `${entry.record.claudeSessionId} without re-marking it`,
    );
    params.noteNeedsAccount(entry, { mark: false });
    return decision;
  }
  params.noteNeedsAccount(entry, { mark: true });
  return decision;
}

/**
 * Record a `needs-account` entry for the account chooser and, only when
 * `mark` is set, ask the backend to hold its registry row awaiting an account
 * (`terminal_session_mark_awaiting_account`: stamps the hold and DETACHES the
 * row from its terminal, keeping it open and on the roster instead of being
 * closed `no-terminal` by the liveness poll; the backend declines a row bound
 * to a live PTY, ignores a closed or finished row, and keeps a running
 * hold's stamp on a re-mark (an expired stamp is re-stamped)). With
 * `mark: false` the row is left untouched. Effects
 * injected so the suppression is unit-testable.
 */
export function applyNeedsAccountNote(
  entry: NeedsAccountRecord,
  opts: { mark: boolean },
  effects: {
    setRecords: (update: (prev: NeedsAccountRecord[]) => NeedsAccountRecord[]) => void;
    invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  },
): void {
  effects.setRecords((prev) => [
    ...prev.filter((e) => e.record.claudeSessionId !== entry.record.claudeSessionId),
    entry,
  ]);
  if (!opts.mark) return;
  effects
    .invoke("terminal_session_mark_awaiting_account", {
      claudeSessionId: entry.record.claudeSessionId,
    })
    .catch((err) =>
      console.warn(
        `[TerminalPage] could not hold needs-account session ${entry.record.claudeSessionId}:`,
        err,
      ),
    );
}

export function classifyRestoreAction(
  rec: Pick<
    TerminalSessionRecord,
    "claudeSessionId" | "origin" | "confirmedAt" | "provider" | "transcriptExists"
  > &
    RestoreAccountFields,
  home: string | null,
): RestoreAction {
  if (!isValidSessionId(rec.claudeSessionId)) return "skip-invalid";
  if (rec.origin === "authoritative" || rec.origin === "observed") {
    // Authoritative/observed-but-unconfirmed ⇒ phantom shell — restore the
    // terminal only, never auto-resume a session that may never have existed.
    // (`observed` is the claude-process-start-anchored, uniquely-correlated
    // transcript bind; a CONFIRMED observed record is as strong a proof that the
    // session exists as a confirmed authoritative one, so it takes the same
    // path.)
    if (rec.confirmedAt == null) return "terminal-only";
    // CONFIRMED but probed as having NO transcript ⇒ no conversation to bring
    // back, so typing `--resume` can only manufacture a "No conversation found"
    // failure. Restore the terminal honestly instead — the same landing the
    // provisional branch above takes. Strictly `=== false`: `undefined` is NOT
    // PROBED, and unknown must never downgrade. See `TerminalSessionRecord.
    // transcriptExists` for why `confirmedAt` is not proof of resumability.
    if (rec.transcriptExists === false) return "terminal-only";
    // Confirmed authoritative/observed ⇒ auto-resume (no operator click) — but
    // ONLY when the provider can deterministically resume the FULL conversation
    // by id (`restoreTier() === "full"`). A `terminal-only`-tier provider can
    // re-open the terminal but not the chat, so a CONFIRMED row of that provider
    // restores terminal-only (honest "fresh conversation"), never a resume typed
    // against an id the provider can't resume by `--resume`.
    const descriptor = providerDescriptorFor(rec.provider);
    if (descriptor.restoreTier() !== "full") return "terminal-only";
    // The account gate (Claude only — the account is Claude's
    // `CLAUDE_CONFIG_DIR`): a resume under a guessed account is refused.
    if (descriptor.provider === "claude" && resolveRestoreAccount(rec, home).kind === "unknown") {
      return "needs-account";
    }
    return "auto-resume";
  }
  // Reconciled / pre-field origin: a backstop guess that can name a foreign
  // session — not strong enough to act on. Restore the terminal only, same as
  // any other unresumable row; do nothing further with the guessed id.
  return "terminal-only";
}

/**
 * Is this durable record a Conductor worker's? The marker is `taskRunId`
 * (set only by `dispatch_subtask`), NOT `terminalId === claudeSessionId` —
 * a coincidence a hand-pinned `--session-id` shell could also produce. Pure.
 */
export function isWorkerRecord(rec: Pick<TerminalSessionRecord, "taskRunId">): boolean {
  return typeof rec.taskRunId === "string" && rec.taskRunId.length > 0;
}

interface UseTerminalInitializationParams {
  /** Which terminal page this restore runs for ("default" when unset). */
  pageId: string;
  /**
   * `authStatus?.authenticated` at reset time, forwarded into the tree-reset
   * report (`undefined` = auth state unknown/not yet resolved). Optional so
   * non-App harnesses need not thread auth through.
   */
  authenticated?: boolean;
  tabs: TerminalTab[];
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  reconnectToExistingSessions: () => Promise<string[] | null>;
  /**
   * The cold-restore loop passes a `ResourceGuardSource` ("session restore",
   * with the records still ahead of it), so a resource-guard refusal mid-restore
   * says what is asking and how many more may follow.
   */
  createTerminal: (
    title?: string,
    workingDir?: string,
    tenantId?: string,
    spawnSource?: ResourceGuardSource,
  ) => Promise<string | null>;
  createPlanTab: (filePath: string) => string | null;
  /**
   * Add the grid tab for a Conductor worker's durable record (`taskRunId`
   * set). Such a record names NO terminal process — its `terminalId` is the
   * task run id — so the cold-restore path must adopt it as a
   * `sessionBacked` tab rather than `terminal_create` a shell that would
   * sit empty beside the real worker. See `useTerminalManager.adoptWorkerTab`.
   */
  adoptWorkerTab: (rec: TerminalSessionRecord) => string | null;
  setInitialized: (v: boolean) => void;
  updateTab: (
    id: string,
    updates: Partial<{
      claudeSessionId?: string;
      claudeConfigDir?: string;
      claudeRecordConfigDir?: string;
      isReconnecting?: boolean;
      resumeFailed?: boolean;
      restoreTerminalOnly?: boolean;
    }>,
  ) => void;
  zoneLayout: {
    layoutId: string;
    setLayoutId: (id: string, request: { source: LayoutRequestSource }) => void;
    /**
     * Place a restored session at its recorded GRID zone — into the grid in
     * grid mode, into the grid snapshot in tmux mode (never onto the screen).
     */
    placeRestoredTab: (zoneIdx: number, tabId: string) => void;
    setFocusedZone: (zoneIdx: number) => void;
    /**
     * The grid, read live from however long ago this object was captured — the
     * snapshot's in tmux mode (`useZoneLayout().readGridView`).
     */
    readGridView: () => { layoutId: string; assignments: Record<number, string> };
    /**
     * Hold (false) or release (true) the page's auto-trim around a restore
     * run (`useZoneLayout().setRestoreSettled`): while a run drains, a falling
     * tab count is the drain, not a close. Mirrors the auto-save gate.
     */
    setRestoreSettled: (settled: boolean) => void;
  };
  labelsAndTags: {
    setZoneLabel: (zoneIdx: number, label: string) => void;
    setZoneNote: (zoneIdx: number, note: string) => void;
    setPinnedZones: React.Dispatch<React.SetStateAction<Set<number>>>;
  };
  sessionPersistence: {
    saveSessionLayout: (params: SaveSessionLayoutParams) => void;
    saveScrollbackBuffers: (tabs: Array<{ id: string }>) => Promise<Record<string, string>>;
    updateScrollbackPaths: (
      pathMap: Record<string, string>,
      tabIdToSessionIndex: Record<string, number>,
    ) => void;
    getSavedLayout: () => {
      layoutId: string;
      focusedZone: number;
      sessions: Array<{
        zoneIndex: number;
        title: string;
        workingDir?: string;
        type?: "terminal" | "plan";
        planFilePath?: string;
        scrollbackPath?: string;
        isClaudeSession?: boolean;
        claudeSessionId?: string;
        claudeConfigDir?: string;
        label?: string;
        notes?: string;
        pinned?: boolean;
      }>;
    } | null;
    clearSavedLayout: () => void;
    hasSavedLayout: () => boolean;
  };
  /** The grid as persisted: `useZoneLayout().gridView` plus the zone cosmetics. */
  layoutState: {
    layoutId: string;
    assignments: Record<number, string>;
    zoneLabels: Record<number, string>;
    zoneNotes: Record<number, string>;
    pinnedZones: Set<number>;
    focusedZone: number;
  };
}

export function useTerminalInitialization({
  pageId,
  authenticated,
  tabs,
  terminalRefs,
  reconnectToExistingSessions,
  createTerminal,
  createPlanTab,
  adoptWorkerTab,
  setInitialized,
  updateTab,
  zoneLayout,
  labelsAndTags,
  sessionPersistence,
  layoutState,
}: UseTerminalInitializationParams) {
  // Phase 3 (mount-hydration lift): with the session provider lifted above the
  // page and per-page scopes always mounted, a terminal-PAGE switch no longer
  // unmounts/remounts this hook — so the old "REMOUNTED … tabs were lost" /
  // "UNMOUNTED … will be destroyed" warnings (which fired on every page
  // switch) are gone. A genuine unmount of the whole tree (auth change) is
  // still surfaced once.
  const mountCountRef = useRef(0);
  useEffect(() => {
    mountCountRef.current += 1;
    const mountNum = mountCountRef.current;
    if (mountNum > 1) {
      console.warn(
        `[TerminalPage] REMOUNTED (mount #${mountNum}) — the terminal page tree remounted. ` +
          `Page state is preserved by the lifted session provider; this usually means the ` +
          `parent app tree unmounted (e.g., auth state change).`,
      );
    }
    // P0 tree-reset observability: durably report EVERY mount (mount #1 is a
    // data point too — consumers filter on mountNumber). Uses the module-scope
    // counter, NOT mountCountRef: the ref dies with the fiber on a genuine
    // remount and would report every reset as mount #1. Fire-and-forget; the
    // report must never affect the initialization flow.
    documentMountCount += 1;
    reportTreeReset({
      mountNumber: documentMountCount,
      authenticated,
      pageIds: loadKnownPageIds(),
      // Stamped at mount, before the awaited open-record fetch inside
      // reportTreeReset, so the row orders correctly against restore logs.
      clientTs: Date.now(),
    }).catch(() => {
      // reportTreeReset already swallows internally; this is belt-and-braces.
    });
    // Mount-time report only: re-running on `authenticated` flips would skew
    // the mount counter semantics.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Init / restore guards are keyed by pageId, NOT per hook mount. The single
  // TerminalPage instance now persists across terminal-page switches (no
  // remount), so the convergence backstop (reconnect + durable-record restore
  // + resume-drain) must run exactly ONCE per pageId — the first time each page
  // becomes active — rather than once per mount.
  const didInitPages = useRef<Set<string>>(new Set());

  // Orphan-record adoption is OWNED by the first page to restore after mount
  // (one adopter per boot — two pages both adopting would double-restore the
  // same orphan: a duplicate PTY, or a forked `--resume`). See
  // `recordBelongsToRestore`. `null` = unclaimed. The owner keeps adopting on
  // its own later runs — a guard retry must still reach the orphans its
  // refused run never did — and no other page ever does while it lives. It is
  // handed back in exactly two cases:
  // - the run that TOOK it was deferred by a coord drain before restoring
  //   anything (a later drain-deferred run of the owner restored orphans
  //   earlier, so it keeps the claim — handing it back would let another page
  //   adopt those again);
  // - the owner page is removed, whatever it still owed: its unreached
  //   orphans would otherwise sit on no page until the no-terminal sweep
  //   closed them. Only this window's `removePage` removes one
  //   (`terminalPageRemoval`; windows do not sync page lists): the removal's
  //   adoption pass holds the claim (`ORPHAN_CLAIM_IN_PASS`) until it has
  //   adopted what is left onto the page on screen, which then takes it. The
  //   records the owner restored are on live tabs and are no longer orphans.
  const orphanAdoptionOwner = useRef<string | null>(null);

  // Gate for the debounced auto-save effect. The restore path recreates
  // plain shells and only *asynchronously* types `claude --resume <id>` /
  // re-attaches `claudeSessionId`. If the debounced auto-save fires while
  // those tabs are still plain (no claudeSessionId yet), it persists the
  // degraded layout and clobbers the good saved Claude layout — leaving
  // nothing to resume on the next reopen. We keep auto-save suppressed
  // until restore has fully drained (resume commands issued / ids merged),
  // then open the gate exactly once. The flag is opened in a `finally` so
  // it ALWAYS flips — even when there were no saved sessions or restore
  // threw — so brand-new sessions still persist normally.
  //
  // Keyed per pageId (Phase 3): each page's restore drains independently, and
  // auto-save runs in the single page instance for whichever page is active.
  const restoreCompletePages = useRef<Set<string>>(new Set());

  // Restore candidates NOT resumed because their account is unknown
  // (`needs-account`), keyed by `claudeSessionId` so a page's re-run never
  // lists one twice. Returned from the hook for the "Since restart" panel to
  // offer an account chooser. See `applyNeedsAccountNote` for what happens to
  // the registry row on each path.
  const [needsAccountRecords, setNeedsAccountRecords] = useState<NeedsAccountRecord[]>([]);
  const noteNeedsAccount = useCallback(
    (entry: NeedsAccountRecord, opts: { mark: boolean }) =>
      applyNeedsAccountNote(entry, opts, { setRecords: setNeedsAccountRecords, invoke }),
    [],
  );

  // Coord device drain (plan `2026-09-13-drained-runner-never-reaches-idle`,
  // Phase 3): bumped each time autonomous spawns become allowed again. The init
  // effect below depends on it, so a restore the drain deferred (which released
  // its page guard) re-runs; a page whose restore already ran stays a no-op via
  // `claimInitForPage`.
  //
  // The re-run is LAZY and PER PAGE: only the page this hook currently serves
  // (the active page) re-runs on the epoch bump. Every other page whose restore
  // was deferred had its guard released too, so it restores the next time it
  // becomes active — nothing is lost, it simply waits for the operator to look.
  //
  // Pages whose restore the drain deferred, so their re-run merges into the
  // tabs that exist by then instead of re-laying the page out.
  const drainDeferredPages = useRef<Set<string>>(new Set());
  // Pages whose last run was deferred by the drain, so their next run does not
  // close the trim hold until it knows it is not deferred again. A deferred run
  // opens the hold, and that state change re-renders the page's scope, which
  // re-runs the restore effect. Closing the hold at the start of every retry
  // turned that into a loop that lasted as long as the drain.
  const deferredRetryPages = useRef<Set<string>>(new Set());
  // Each page's newest restore run. A re-run can start before an earlier run's
  // drain timer fires, and that timer must not release the hold the newer run
  // still needs.
  const newestRestoreRun = useRef<Map<string, number>>(new Map());

  // The two page sets above, PUBLISHED as state for the "Since restart" strip
  // (plan `2026-10-04-runner-session-roster-restore-picker`, Phase 4): it
  // speaks only once every page holding a prior-ledger row has settled, which
  // is this per-page completion — never the boot census latch, which runs
  // before any restore. A new Set identity is published only when a page's
  // status actually moves.
  const [publishedCompletePages, setPublishedCompletePages] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [publishedDeferredPages, setPublishedDeferredPages] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const publishRestorePages = useCallback(() => {
    setPublishedCompletePages(new Set(restoreCompletePages.current));
    setPublishedDeferredPages(new Set(drainDeferredPages.current));
  }, []);
  // Live tabs, for the re-run's "already open" check (the init effect does not
  // re-run on every tab change, so it cannot close over `tabs`).
  const liveTabsRef = useRef(tabs);
  useEffect(() => {
    liveTabsRef.current = tabs;
  }, [tabs]);
  const [drainResumeEpoch, setDrainResumeEpoch] = useState(0);
  // Sessions a restore refused as "live" BEFORE the PTY-holder boot sweep
  // settled, which the sweep's settle then showed to be resumable (see
  // `recheckRefusedAfterSweep`), per page, and the page runs in flight. A
  // re-run triggered for them (the same epoch bump as a drain re-run)
  // restores exactly these records — one run per page at a time.
  // Sessions a run is creating a tab for right now.
  const coldResumeGuard = useRef(new ColdResumeGuard());
  // Re-run `forPage`'s restore: merging into its live tabs (`merge`, like a
  // drain re-run), or — retrying an aborted first restore — as that restore.
  const triggerRerun = useCallback((forPage: string, merge = true) => {
    if (merge) drainDeferredPages.current.add(forPage);
    didInitPages.current.delete(forPage);
    setDrainResumeEpoch((n) => n + 1);
  }, []);
  // The re-run queue, an aborted restore's backoff retries (review V1) and
  // how they trigger a re-run. Stable for the hook's lifetime.
  const [rerun] = useState<RestoreRerunWiring>(() => ({
    queue: new RestoreRerunQueue(),
    timers: new RerunTimers(),
    markMerging: (forPage) => drainDeferredPages.current.add(forPage),
    trigger: triggerRerun,
  }));
  // A restore the resource guard refused, retried as a later merging run
  // (see `RestoreGuardRetry`). Stable for the hook's lifetime.
  // When a page's gate-hold runs out, its auto-save gate opens.
  // The one door that opens a page's restore gates — auto-save AND the trim
  // hold — for its newest run (`openRestoreGate`), so the gate-hold's expiry
  // opens both, as a run's own end does.
  const restoreGateDoors = useRef(new Map<string, () => void>());
  const releaseRestoreGate = useCallback(
    (page: string) => {
      const door = restoreGateDoors.current.get(page);
      if (door) door();
      else restoreCompletePages.current.add(page);
      publishRestorePages();
    },
    [publishRestorePages],
  );
  const [guardRetry] = useState(() => new RestoreGuardRetry());
  useEffect(() => {
    guardRetry.setGateRelease(releaseRestoreGate);
  }, [guardRetry, releaseRestoreGate]);
  // Active while mounted (re-armed on a StrictMode re-mount); unmounting
  // clears the timers — a guard retry's among them — and stops a run that
  // ends later from scheduling one.
  useEffect(() => {
    rerun.timers.setActive(true);
    return () => {
      rerun.timers.setActive(false);
      guardRetry.withdrawAll();
    };
  }, [rerun, guardRetry]);
  // A page removed in this window (`terminalPageRemoval`) takes everything
  // this hook holds for it: its guard retry (timer, wait, auto-save hold), its
  // restore bookkeeping — so a page that exists under that id again restores
  // like any page — and, when it owned the orphan-adoption claim, the claim.
  // Its unreached orphans are then adopted by the page on screen
  // (`scheduleOrphanAdoption`) rather than left for the no-terminal sweep: no
  // other page's first restore may ever run to pick them up. The pass waits
  // for an effect, after the removal has rendered, so the persisted page list
  // no longer names the removed page.
  //
  // ONE rule for a run of the removed page still in flight — a STOPPED run
  // (`guardRetry.removed`): it restores nothing more, touches no page state
  // when it ends, and hands every session it was spawning when the page went
  // to whoever owns that session NOW (`routeLateSpawn`). The pass waits for
  // every stopped run of the page to end (`restoreRuns`), because only then
  // are its notes complete — the orphans its runs restored or held, which
  // closed with the page — and its `ColdResumeGuard` claims released. It stays
  // owed until it completes with a readable registry and terminal list onto a
  // page that still exists, retrying with the guard's backoff (`guardWaitMs`)
  // — a read that fails or throws alike — and gives up, visibly
  // (`unadoptedOrphans`), after `ORPHAN_PASS_MAX_ATTEMPTS`. One pass runs at a
  // time.
  const orphanPassesOwed = useRef(new Set<string>());
  const orphanPassAttempts = useRef(0);
  const orphanPassBusy = useRef(false);
  const [orphanPassTick, setOrphanPassTick] = useState(0);
  const [unadoptedOrphans, setUnadoptedOrphans] = useState<readonly UnadoptedOrphans[]>([]);
  // What closed with a removed page, for good: its id (its own records are
  // never adopted — unless a page of that id exists again, whose records they
  // then are) and the orphans its runs had restored or held. Every adopting
  // run excludes them, the pass's or not, so a claim handed on — or released
  // by a pass that gave up — never brings a closed page's sessions back.
  const closedPages = useRef(new Set<string>());
  const closedSessions = useRef(new Set<string>());
  // Every restore run by its token (`RestoreGuardRetry.begin`), until it ends,
  // and after it while it holds orphans its page would take with it.
  const restoreRuns = useRef(new Map<number, RestoreRunEntry>());
  useEffect(
    () =>
      subscribeTerminalPageRemoved((removed) => {
        guardRetry.forget(rerun, removed);
        rerun.queue.forget(removed);
        didInitPages.current.delete(removed);
        restoreCompletePages.current.delete(removed);
        drainDeferredPages.current.delete(removed);
        deferredRetryPages.current.delete(removed);
        restoreGateDoors.current.delete(removed);
        publishRestorePages();
        closedPages.current.add(removed);
        // Only the claim's owner leaves orphans to adopt: a page removed while
        // a pass holds the claim never adopted any, and its own records are
        // excluded by `closedPages` like every removed page's.
        if (orphanAdoptionOwner.current !== removed) return;
        orphanAdoptionOwner.current = ORPHAN_CLAIM_IN_PASS;
        orphanPassesOwed.current.add(removed);
        setOrphanPassTick((n) => n + 1);
      }),
    [guardRetry, rerun, publishRestorePages],
  );
  useEffect(() => {
    if (orphanPassBusy.current || rerun.timers.has(ORPHAN_PASS_TIMER_KEY)) return;
    const removed = new Set(orphanPassesOwed.current);
    if (removed.size === 0) return;
    const stopped = [...removed].flatMap((r) => stoppedRunsOf(restoreRuns.current, r, guardRetry));
    if (stopped.some(([, run]) => !run.ended)) return;
    const handled = new Set(stopped.flatMap(([, run]) => [...run.orphansHandled]));
    // Everything that closed with a removed page, then.
    const closeWithStopped = () => {
      for (const id of handled) closedSessions.current.add(id);
      for (const [token] of stopped) restoreRuns.current.delete(token);
    };
    orphanPassBusy.current = true;
    void (async () => {
      // Every outcome but `done` is a failed attempt: retried after the
      // guard's backoff, and bounded.
      let outcome: "done" | "failed" = "failed";
      let candidates: string[] | null = null;
      try {
        // Onto the page that holds the claim, or — while the pass holds it —
        // a page that EXISTS: the one on screen, else the first one (the page
        // on screen can be the unsaved "default" fallback right after the
        // page on screen was removed). That page takes the claim once the
        // pass completes.
        const owner = orphanAdoptionOwner.current;
        const target =
          owner === null || owner === ORPHAN_CLAIM_IN_PASS ? existingPageFor(pageId) : owner;
        const read = await readOrphanAdoption(
          target,
          new Set([...closedPages.current, ...removed]),
          new Set([...closedSessions.current, ...handled]),
        );
        if (read.kind === "unreadable") {
          candidates = read.candidates;
        } else if (!loadKnownPageIds().includes(target)) {
          // The target was removed during the reads: its queue is gone, and
          // so is any page to hand the claim to. The next attempt picks again.
          candidates = read.ids;
        } else {
          scheduleRestoreRerun(rerun, target, read.ids);
          const now = orphanAdoptionOwner.current;
          if (now === null || now === ORPHAN_CLAIM_IN_PASS) orphanAdoptionOwner.current = target;
          outcome = "done";
        }
      } catch (err) {
        console.warn("[TerminalPage] the orphan-adoption pass failed:", err);
      } finally {
        orphanPassBusy.current = false;
        if (outcome === "done") {
          // A page removed during the pass stays owed: the next pass covers it.
          for (const r of removed) orphanPassesOwed.current.delete(r);
          orphanPassAttempts.current = 0;
          closeWithStopped();
        } else {
          orphanPassAttempts.current += 1;
          const attempt = orphanPassAttempts.current;
          if (attempt >= ORPHAN_PASS_MAX_ATTEMPTS) {
            // Given up: the claim is released, so the next restore of any page
            // adopts the orphans (never what closed with a removed page), and
            // the operator is told which sessions no page adopted meanwhile.
            for (const r of removed) orphanPassesOwed.current.delete(r);
            orphanPassAttempts.current = 0;
            closeWithStopped();
            if (orphanAdoptionOwner.current === ORPHAN_CLAIM_IN_PASS) {
              orphanAdoptionOwner.current = null;
            }
            console.error(
              `[TerminalPage] gave up adopting the orphans of removed page(s) ` +
                `${[...removed].join(", ")} after ${attempt} failed attempts: ` +
                `${candidates?.join(", ") ?? "the registry was never read"}`,
            );
            setUnadoptedOrphans((prev) => [
              ...prev,
              { removedPageIds: [...removed], sessionIds: candidates },
            ]);
          } else {
            rerun.timers.set(ORPHAN_PASS_TIMER_KEY, guardWaitMs(attempt), () =>
              setOrphanPassTick((n) => n + 1),
            );
          }
        }
        setOrphanPassTick((n) => n + 1);
      }
    })();
  }, [pageId, orphanPassTick, rerun, guardRetry]);
  // Re-run the restore of `forPage` (merging, like a drain re-run) for
  // exactly `sessionIds` — after a late sweep settle, or a session a dead
  // holder handed back. The active page re-runs now (or when its run in
  // flight ends); another page when it is next activated.
  const scheduleRecheckRerun = useCallback(
    (forPage: string, sessionIds: string[]) => {
      scheduleRestoreRerun(rerun, forPage, sessionIds);
    },
    [rerun],
  );
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen<HolderSessionReleased>(HOLDER_SESSION_RELEASED_EVENT, (event) => {
      // Broadcast to every window: only the one that restores the page acts.
      const releasedPage = event.payload.pageId ?? "default";
      if (!ownsReleasedPage(releasedPage, readPinnedPageId(), getPageBindings())) return;
      console.warn(
        `[TerminalPage] a PTY holder died before adoption; re-deciding session ${event.payload.sessionId}`,
      );
      void onHolderSessionReleased(event.payload, scheduleRecheckRerun);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [scheduleRecheckRerun]);
  useEffect(
    () =>
      subscribeCoordDrainState(
        autonomousResumeDetector(
          () => setDrainResumeEpoch((n) => n + 1),
          // Seed the edge detector from OUR OWN deferral record rather than
          // from whichever snapshot happens to arrive first (review N1): this
          // effect runs after the deferred restore returned, so the first
          // snapshot can already read `allowed`, and a stream-seeded detector
          // would never see the rising edge that re-runs the restore.
          () => drainDeferredPages.current.size > 0,
        ),
      ),
    [],
  );

  useEffect(() => {
    if (!claimInitForPage(didInitPages.current, pageId)) return;
    const initPageId = pageId;
    // A re-run after a drain deferral merges with what the operator opened
    // meanwhile (see `drainDeferredPages`). Its auto-save gate is closed for the
    // duration of the restore, exactly like a first restore, and reopened by the
    // same finally / drain timer.
    //
    // A page the resource guard still owes a retry (`RestoreGuardRetry`) runs
    // as that retry — merging and IN FULL — whatever started it: the run that
    // was refused stopped part-way, so the records after the refusal are owed
    // too, and a recheck-only run (a released holder, a late sweep settle)
    // that cancelled the retry and restored only its own ids would strand them.
    const guardOwed = guardRetry.owes(initPageId);
    const isDrainRerun = drainDeferredPages.current.delete(initPageId) || guardOwed;
    if (isDrainRerun) restoreCompletePages.current.delete(initPageId);
    // The page's auto-trim waits for this run exactly as auto-save does: both
    // are released together by `openRestoreGate`. The setter is the page's
    // own (captured with this run), so a drain timer firing after a page
    // switch still releases the right page.
    const setRestoreSettled = zoneLayout.setRestoreSettled;
    const retryingDeferral = deferredRetryPages.current.delete(initPageId);
    if (!retryingDeferral) setRestoreSettled(false);
    const thisRun = (newestRestoreRun.current.get(initPageId) ?? 0) + 1;
    newestRestoreRun.current.set(initPageId, thisRun);
    const openRestoreGate = () => {
      restoreCompletePages.current.add(initPageId);
      if (newestRestoreRun.current.get(initPageId) === thisRun) setRestoreSettled(true);
    };
    restoreGateDoors.current.set(initPageId, openRestoreGate);
    // What a merging run may place into: its own page's grid as it is now. The
    // captured `zoneLayout` is the page's own, so this holds through a page
    // switch, and it reads live because a deferred retry starts with the hold
    // still open, so a trim can land mid-run. A flow-grid sizes itself to its
    // highest occupied zone, so it has no limit, and every other layout's zone
    // count ignores the tab count.
    const runGridView = zoneLayout.readGridView;
    const gridZoneCount = () => {
      const { layoutId } = runGridView();
      return layoutId === FLOW_GRID_ID
        ? Number.POSITIVE_INFINITY
        : resolveLayout(layoutId, 0).zones.length;
    };
    // A re-run for the holder sweep's late settle restores only these. Ids
    // scheduled while this run is in flight wait for its end. A full run
    // covers them, so they are not restricted to while the guard is owed.
    const recheckIds = beginRestoreRun(rerun, initPageId);
    const recheckOnly = guardOwed ? undefined : recheckIds;
    const runToken = guardRetry.begin(initPageId);
    // Stopped once its page is removed (see `restoreRuns`).
    const stopped = () => guardRetry.removed(initPageId, runToken);
    const runEntry: RestoreRunEntry = {
      pageId: initPageId,
      ended: false,
      orphansHandled: new Set(),
    };
    restoreRuns.current.set(runToken, runEntry);
    // An ORPHAN this run restored or held (a record of another page it
    // adopted): if this page is removed, that session closed with it and the
    // removal's adoption pass must not bring it back.
    const noteOrphanHandled = (rec: TerminalSessionRecord) => {
      if ((rec.pageId ?? "default") !== initPageId)
        runEntry.orphansHandled.add(rec.claudeSessionId);
    };
    // Sessions this run had spawned a pane for when it was stopped: routed by
    // `routeLateSpawn` once its claims are released.
    const lateSpawns: TerminalSessionRecord[] = [];
    // The sessions an ADOPTING run read from a readable registry: what it
    // tells the unadopted-orphans notice (`pruneUnadoptedOrphans`).
    let adoptingRead: ReadonlySet<string> | null = null;
    // Set when the resource guard refused a cold restore: this run stops there
    // and settles as deferred; its `finally` schedules the merging retry.
    let guardDeferral: ResourceGuardDeferral | null = null;
    // Sessions this run claimed for a cold resume; released when it ends.
    const coldClaims: string[] = [];
    // The drain withheld the restore set: this run restored nothing.
    let restoreDeferred = false;
    // Sessions THIS run refuses as live while the sweep is unsettled, and
    // sessions it holds back because their pane is still being adopted.
    const refusedWhileSweeping: string[] = [];
    const heldBackWhileSweeping: string[] = [];

    // Per-page restore queue (Phase 3): local to this page's init run so a
    // second page initializing before this one's drain timer fires can't see
    // (or clear) the other page's pending restores.
    const pendingRestores: Array<{
      tabId: string;
      scrollbackPath?: string;
      isClaudeSession?: boolean;
      claudeSessionId?: string;
      claudeConfigDir?: string;
      /** Which provider owns this session — drives the adapter resume. */
      provider?: string;
      /** Gate identity of a continuation record — re-injected on resume. */
      gate?: ResumeGateIdentity;
      /** Deferred registry re-assert — applied only on VERIFIED resume. */
      recordOpen?: SessionOpenArgs;
    }> = [];

    (async () => {
      // A drain re-run moved this page out of both sets above.
      if (isDrainRerun) publishRestorePages();
      // True once a deferred resume/scrollback drain timer is scheduled. When
      // set, that timer owns flipping the per-page restore-complete gate (after
      // it issues the resume commands); the `finally` below only opens the gate
      // when NO drain was scheduled, so the flag always flips exactly once.
      let drainScheduled = false;
      // True when the reconnect result was INDETERMINATE (`null`) and restore
      // was aborted: the once-per-pageId guard is released so the next
      // activation retries, and the auto-save gate is left CLOSED so the
      // retried restore still runs with saves suppressed (an open gate would
      // let the debounced save clobber the cosmetic snapshot mid-retry).
      let restoreAborted = false;
      try {
        // 1) Reconnect to live PTYs that survived a React remount. These tabs
        //    are plain (no claudeSessionId on the wire) but their ids are the
        //    SAME stable terminal ids the registry recorded under `terminalId`.
        //
        //    P1 defect (a): `null` means the live-terminal list could not be
        //    read — NOT "nothing reconnected". Coercing it to [] used to send
        //    every open record down the cold `claude --resume` path while its
        //    previous terminal generation was still alive, orphaning a PTY
        //    generation per pass. Abort instead: leave the page as-is and let
        //    the next activation retry. (Same null-awareness as the plan-tab
        //    cold-start check in step 4 below.)
        // Holder panes that survived the last runner exit are adopted by the
        // runner's boot sweep. Learn its census first (seconds, not the whole
        // sweep): every record whose pane is NOT held is restored now; the
        // held ones wait for the sweep (review round 4, M1). Without a census
        // — or on a re-run for its late settle — fall back to the full wait.
        const sweepStatus = recheckOnly
          ? { settled: await waitForHolderSweep(), held: new Set<string>() }
          : await fetchHolderSweepStatus();
        const holderSweepSettled =
          sweepStatus.held === null ? await waitForHolderSweep() : sweepStatus.settled;
        const reconnectDecision = decideReconnectOutcome(await reconnectToExistingSessions());
        if (reconnectDecision.kind === "abort-restore") {
          restoreAborted = true;
          // A deferral retry reaches here with the hold still open.
          setRestoreSettled(false);
          // Release the guard so the retry can run — this abort must not
          // consume the pageId.
          didInitPages.current.delete(initPageId);
          console.warn(
            `[TerminalPage] reconnect indeterminate for page "${initPageId}" — aborting restore ` +
              `(nothing spawned, records untouched); the next activation retries. Cold-respawning ` +
              `here could fork sessions whose processes are still alive.`,
          );
          // Nothing was spawned or consumed. The `finally` below flips
          // `initialized`, so the page renders as-is rather than hanging on a
          // permanent loading spinner.
          return;
        }
        const { reconnectedTabIds } = reconnectDecision;
        const reconnectedSet = new Set(reconnectedTabIds);

        // 2) The durable backend session registry is the SOURCE OF TRUTH for
        //    which Claude sessions exist and their zones. The localStorage
        //    snapshot is demoted to cosmetics only (layout / labels / notes /
        //    pins / focusedZone / scrollback) — matched by zoneIndex.
        const knownPageIds = loadKnownPageIds();
        // The claim's owner is a page this window holds: its own page state is
        // the authority, and only this window's `removePage` removes one.
        // Windows do not sync page lists, so a page another window removed —
        // or left out of the persisted list it wrote — still exists here.
        const tookOrphanOwnership = orphanAdoptionOwner.current === null;
        const adoptOrphans = tookOrphanOwnership || orphanAdoptionOwner.current === initPageId;
        if (adoptOrphans) orphanAdoptionOwner.current = initPageId;
        const restoreSet = await fetchRestoreSet(pageId, {
          knownPageIds,
          adoptOrphans,
        });
        if (restoreSet.deferredByDrain !== null) {
          // Coord device drain (plan `2026-09-13-drained-runner-never-reaches-idle`,
          // Phase 3): the backend withheld the restore set because restoring
          // tabs respawns sessions autonomously. Nothing spawned, records
          // untouched. Unlike the indeterminate-reconnect abort above, the
          // auto-save gate is OPENED (by the finally below): a drain can last
          // hours, and terminals the operator opens meanwhile must persist. The
          // once-per-page guard and the orphan-adoption claim are released so
          // the restore re-runs — merging, not re-laying out — when
          // `drainResumeEpoch` moves.
          restoreDeferred = true;
          deferredRetryPages.current.add(initPageId);
          drainDeferredPages.current.add(initPageId);
          didInitPages.current.delete(initPageId);
          if (tookOrphanOwnership) orphanAdoptionOwner.current = null;
          console.warn(
            `[TerminalPage] restore for page "${initPageId}" deferred by the coord device ` +
              `drain: ${restoreSet.deferredByDrain} — it re-runs when the drain lifts.`,
          );
          return;
        }
        // Not deferred after all, so this retry is a real restore: hold the trim.
        setRestoreSettled(false);
        // An adopting run never takes what closed with a removed page — the
        // same test as the adoption pass's. A terminal list it cannot read
        // leaves it adopting no orphan, rather than guess, and hands them back
        // to the adoption pass, which holds the claim and retries with its
        // bounded backoff until it reads (or gives up, visibly).
        let liveOnRemoved: ReadonlySet<string> | null = new Set<string>();
        if (adoptOrphans && closedPages.current.size > 0) {
          liveOnRemoved = await liveTerminalIdsOnPages(closedPages.current);
          if (liveOnRemoved === null && !stopped()) {
            if (orphanAdoptionOwner.current === initPageId) {
              orphanAdoptionOwner.current = ORPHAN_CLAIM_IN_PASS;
            }
            for (const page of closedPages.current) orphanPassesOwed.current.add(page);
            setOrphanPassTick((n) => n + 1);
          }
        }
        const openRecords = adoptOrphans
          ? restoreSet.records.filter((rec) =>
              liveOnRemoved === null
                ? (rec.pageId ?? "default") === initPageId
                : !closedWithRemovedPage(
                    rec,
                    initPageId,
                    closedPages.current,
                    closedSessions.current,
                    liveOnRemoved,
                  ),
            )
          : restoreSet.records;
        if (adoptOrphans && !restoreSet.readFailed && liveOnRemoved !== null) {
          adoptingRead = new Set(openRecords.map((rec) => rec.claudeSessionId));
        }
        // The default Claude home, for the account rule (`resolveAccountDir`):
        // `null` = unreadable, which makes a dir that may be it needs-account.
        const defaultHome = await loadDefaultConfigHome();

        // P1 defect (b): before ANY cold `claude --resume`, read which session
        // ids are ALREADY hosted by a live Claude process (Claude Code's own
        // per-process registry, PID-verified on the Rust side). Fetched LAZILY
        // (memoized) the first time a record actually reaches the cold
        // auto-resume path, so the fetch condition can never drift from the
        // loop's real gate. `undefined` = not fetched yet; `null` = fetched
        // and INDETERMINATE (registry unreadable) = every such respawn fails
        // CLOSED (see `decideColdResume`).
        let liveSessionIdsCache: ReadonlySet<string> | null | undefined;
        const getLiveSessionIds = async (): Promise<ReadonlySet<string> | null> => {
          if (liveSessionIdsCache === undefined) {
            liveSessionIdsCache = await fetchLiveClaudeSessionIds();
          }
          return liveSessionIdsCache;
        };

        // Cosmetic snapshot — never the resumable Claude set / zone binding.
        const saved = sessionPersistence.hasSavedLayout()
          ? sessionPersistence.getSavedLayout()
          : null;

        // Per-zone cosmetics lookups from the snapshot (matched by zoneIndex).
        const cosmeticsByZone = new Map<
          number,
          { label?: string; notes?: string; pinned?: boolean; scrollbackPath?: string }
        >();
        if (saved) {
          for (const s of saved.sessions) {
            if (s.zoneIndex < 0) continue;
            cosmeticsByZone.set(s.zoneIndex, {
              label: s.label,
              notes: s.notes,
              pinned: s.pinned,
              scrollbackPath: s.scrollbackPath,
            });
          }
        }

        const applyZoneCosmetics = (zoneIndex: number) => {
          if (zoneIndex < 0) return;
          const c = cosmeticsByZone.get(zoneIndex);
          if (!c) return;
          if (c.label) labelsAndTags.setZoneLabel(zoneIndex, c.label);
          if (c.notes) labelsAndTags.setZoneNote(zoneIndex, c.notes);
          if (c.pinned) labelsAndTags.setPinnedZones((prev) => new Set([...prev, zoneIndex]));
        };

        // Restore the layout preset from the cosmetic snapshot if it differs.
        // The restored layout is a starting point only — auto-grow may expand
        // it later so every live session keeps a visible zone.
        // Not on a drain re-run: the operator's current layout wins.
        if (!isDrainRerun && saved && saved.layoutId !== zoneLayout.layoutId) {
          // Accept a persisted `flow-grid` id (synthesized, not in the preset
          // table) alongside the static presets — else restoring into flow mode
          // silently drops back to the default and re-hides sessions past the 9th.
          if (
            saved.layoutId === FLOW_GRID_ID ||
            LAYOUT_PRESETS.some((p) => p.id === saved.layoutId)
          ) {
            zoneLayout.setLayoutId(saved.layoutId, { source: "restore" });
          }
        }

        // 3) Bind every open Claude record to its RECORDED zone — Claude zones
        //    are claimed from records FIRST so the creation-order auto-fill in
        //    `useZoneLayout` can never steal a zone a record owns (it only fills
        //    zones still empty after this loop runs).
        const { heldBack } = splitRestoreByHold(openRecords, {
          settled: holderSweepSettled,
          held: sweepStatus.held,
        });
        const heldBackIds = new Set(heldBack.map((r) => r.terminalId));
        for (const r of heldBack) heldBackWhileSweeping.push(r.claudeSessionId);
        for (const [recIndex, rec] of openRecords.entries()) {
          // The page was removed while this run was in flight: restore nothing
          // more onto a page that no longer exists, and adopt no orphan the
          // page on screen is about to adopt (`subscribeTerminalPageRemoved`).
          if (stopped()) break;
          // A re-run for the holder sweep's late settle restores ONLY the
          // sessions that settle showed resumable.
          if (recheckOnly && !recheckOnly.has(rec.claudeSessionId)) continue;
          // Its holder pane is still being adopted: it comes back as that
          // tab, or through the re-run once the sweep settles.
          if (heldBackIds.has(rec.terminalId)) continue;
          // A Conductor worker (Phase 2b): no PTY exists or should exist for
          // it. Adopt the record as a `sessionBacked` tab and let
          // `reconcileAssignments` place it — every worker record is written
          // with `zone_index: 0`, so binding its recorded zone would stack
          // all of them onto one zone. Its state, transcript and steering
          // come from the SessionManager through `WorkerSessionCell`.
          if (isWorkerRecord(rec)) {
            adoptWorkerTab(rec);
            noteOrphanHandled(rec);
            continue;
          }
          const account = resolveRestoreAccount(rec, defaultHome);
          // What the tab, the last-known id and the typed resume carry — never
          // the default home's explicit path (that goes to the record only).
          const typedDir = account.kind === "resolved" ? account.typedDir : undefined;
          // The explicit path, for the registry record only (a Retry's too).
          const recordDir = account.kind === "resolved" ? account.recordDir : undefined;
          const restoreAction = classifyRestoreAction(rec, defaultHome);

          // a) A live reconnected PTY is already running this session (React
          //    remount): match by the record's stable terminalId. Just rebind
          //    the zone + re-attach the claudeSessionId; no resume needed. A
          //    record held for an account choice has no pane and never matches.
          const reconnectedTabId = reconnectedTabFor(rec, reconnectedSet);
          if (reconnectedTabId !== null) {
            const tabId = reconnectedTabId;
            if (
              mayClaimRecordedZone(
                isDrainRerun,
                rec.zoneIndex,
                tabId,
                runGridView().assignments,
                gridZoneCount(),
              )
            ) {
              zoneLayout.placeRestoredTab(rec.zoneIndex, tabId);
              applyZoneCosmetics(rec.zoneIndex);
            }
            updateTab(tabId, {
              claudeSessionId: rec.claudeSessionId,
              claudeConfigDir: typedDir,
              claudeRecordConfigDir: recordDir,
            });
            rememberSessionId(tabId, rec.claudeSessionId, typedDir);
            noteOrphanHandled(rec);
            continue;
          }

          // P1 defect (b): an `auto-resume` record whose session is ALREADY
          // hosted by a live process must never be cold-respawned — typing
          // `claude --resume <id>` would fork the live session. There is no
          // reconnectable PTY for it either (the reconnect above didn't
          // return one), so there is nothing to rebind: skip the record
          // entirely, leaving it untouched in the registry. It stays
          // AUTO-restorable only briefly — the lifecycle poll retires
          // unmatched records CloseNoTerminal after 3×45 s (~2¼ min),
          // dropping them from the restorable set — but it remains MANUALLY
          // resumable (`claude --resume <id>`) indefinitely. When liveness is
          // UNKNOWN (registry read failed), fail CLOSED toward not-respawning
          // — a missing tab is recoverable; a duplicate live session corrupts
          // the transcript.
          if (restoreAction === "auto-resume") {
            const resumeDecision = decideColdResume(await getLiveSessionIds(), rec.claudeSessionId);
            if (resumeDecision === "skip-alive" && !holderSweepSettled) {
              refusedWhileSweeping.push(rec.claudeSessionId);
            }
            if (resumeDecision !== "respawn") {
              console.warn(
                `[TerminalPage] skipping cold respawn for session ${rec.claudeSessionId}: ` +
                  (resumeDecision === "skip-alive"
                    ? "a live Claude process already hosts this id (respawning would fork it)"
                    : "live-session registry unreadable — failing closed toward not-respawning"),
              );
              continue;
            }
          }

          // An unknown account is never guessed: no pane, no resume, the
          // registry row untouched (like the skip-alive branch above). The
          // hook surfaces the record for an account choice instead.
          if (restoreAction === "needs-account") {
            if (account.kind === "unknown") {
              console.warn(
                `[TerminalPage] not resuming session ${rec.claudeSessionId}: account unknown ` +
                  `(${account.reason}) — a resume under a guessed account fails as "No conversation found"`,
              );
              // Never hold (and so detach) a session that is alive.
              await holdNeedsAccountIfNotLive({
                entry: { record: rec, reason: account.reason },
                getLiveSessionIds,
                noteNeedsAccount,
              });
              noteOrphanHandled(rec);
            }
            continue;
          }

          // A drain re-run never duplicates a session a live tab already shows.
          if (
            isDrainRerun &&
            liveTabsRef.current.some((t) => t.claudeSessionId === rec.claudeSessionId)
          ) {
            continue;
          }

          // b) Cold restart (no live pty): recreate the tab, bind its recorded
          //    zone, attach the session id, and (for pinned bindings) queue a
          //    `claude --resume` via the existing drain loop. The OPEN record
          //    is re-asserted under the new ephemeral terminal id ONLY after
          //    the resume handshake VERIFIES (item 3 — re-asserting here
          //    refreshed `lastSeenAt` on ghost rows, making them immortal).
          // The source names this restore to the resource guard. `queued`
          // counts the records still ahead, this one included — an upper bound,
          // since reconnected and live records ahead will be skipped.
          // `onRefusal: "defer"`: a restore has nobody to answer the guard's
          // dialog (on a headless box it would block this loop for ever), so a
          // refusal ends this run here and the page is restored again, merging,
          // after a capped backoff (`RestoreGuardRetry`), shown as
          // "waiting: <lane> ceiling" — no dialog, no override, no grant spent.
          // This is the shared cold-restore call, so it covers `auto-resume`
          // rows as well as `terminal-only` ones. That is deliberate until
          // plan D5 deletes this path's typed boot restore of `auto-resume`
          // rows in favour of the backend queue; until then both kinds wait
          // the same way rather than one of them asking a human.
          // Another run in flight is already creating this session's tab.
          if (!coldResumeGuard.current.tryClaim(rec.claudeSessionId)) continue;
          coldClaims.push(rec.claudeSessionId);
          const tabName = restoreTabName(rec);
          let tabId: string | null;
          try {
            tabId = await createTerminal(tabName, rec.workingDir, undefined, {
              label: "session restore",
              queued: openRecords.length - recIndex,
              onRefusal: "defer",
            });
          } catch (err) {
            if (!(err instanceof ResourceGuardDeferral)) throw err;
            guardDeferral = err;
            restoreDeferred = true;
            // The orphan-adoption claim stays with this page (it is page-owned,
            // see `orphanAdoptionOwner`): its retry still adopts the orphans
            // this run never reached, and no other page can adopt the ones it
            // already restored.
            break;
          }
          if (!tabId) continue;
          if (stopped()) {
            // The page was removed while this spawn was in flight. The pane is
            // bound to the removed page, and nothing of the session is in it
            // yet (no resume typed, no record re-pointed at it): close it, and
            // let the session's owner NOW restore it (`routeLateSpawn`).
            closeLateSpawnPane(tabId);
            lateSpawns.push(rec);
            break;
          }
          noteOrphanHandled(rec);
          // A record with no recorded zone (`UNZONED_INDEX`) is NOT force-placed
          // here, and is deliberately not clamped to zone 0: zone 0 belongs to
          // whichever tab actually holds it, and `assignTabToZone` RESERVES the
          // zone it is given, so inventing one would let an unplaced record
          // steal the zone a later record legitimately claims (the exact
          // creation-order bug the reservation machinery exists to prevent).
          // The tab still lands in a zone — `reconcileAssignments` auto-fills
          // every unassigned tab into the first non-reserved empty zone — and
          // the recorded `-1` is then corrected by the re-resolution backstop
          // in `TerminalPage` (auto-resume rows, via the ledger note in
          // `runVerifiedResume`). What is NOT done is re-asserting the record
          // from here: that refreshes `last_seen_at` and is what made ghost
          // rows immortal.
          if (
            mayClaimRecordedZone(
              isDrainRerun,
              rec.zoneIndex,
              tabId,
              runGridView().assignments,
              gridZoneCount(),
            )
          ) {
            zoneLayout.placeRestoredTab(rec.zoneIndex, tabId);
            applyZoneCosmetics(rec.zoneIndex);
          }
          updateTab(tabId, {
            claudeSessionId: rec.claudeSessionId,
            claudeConfigDir: typedDir,
            claudeRecordConfigDir: recordDir,
            // Show a "resuming" affordance until the resume lands; cleared in
            // the drain loop after the resume command is written. Phase 4:
            // CONFIRMED authoritative rows of a FULL-tier provider auto-resume
            // with NO operator click. A `terminal-only` row restores
            // terminal+cwd only — no resume, no confirm banner — and is
            // surfaced HONESTLY (Phase 5) via the `restoreTerminalOnly` flag so
            // the user sees the conversation was NOT restored (phantom shell, a
            // terminal-only-tier provider, or a reconciled/backstop-guessed id
            // that isn't strong enough to act on).
            isReconnecting: restoreAction === "auto-resume",
            // Honest "fresh conversation" note (Phase 5) ONLY for a CONFIRMED
            // terminal-only restore — i.e. a real provider session existed here
            // but its conversation could not be resumed by id: either the
            // provider is terminal-only tier, or the session has no transcript
            // on disk (the transcript gate above). The banner copy names the
            // OUTCOME rather than either cause, since both land here. An
            // UNCONFIRMED terminal-only row is a phantom shell that never hosted
            // a conversation, so restoring it as a plain terminal (no note) is
            // the honest, clutter-free outcome.
            restoreTerminalOnly: restoreAction === "terminal-only" && rec.confirmedAt != null,
          });
          rememberSessionId(tabId, rec.claudeSessionId, typedDir);

          // Re-point the record at the tab we JUST created for it.
          //
          // Only the auto-resume track re-asserts `terminalId` (deferred to
          // `runVerifiedResume` via `recordOpen`, on a VERIFIED handshake).
          // Every `terminal-only` record therefore kept pointing
          // at the dead pre-restart terminal id — so the NEXT restore pass
          // couldn't see the tab it had already made for that record, took the
          // cold path again, and spawned another PTY. Each pass leaked one
          // orphan shell per stale record, unbounded (observed 2026-07-22: 48
          // stale records on one page → ~330 orphan PTYs over ~7 passes,
          // 621 live shells under the runner process).
          //
          // `rebind` updates the binding ONLY — it never refreshes
          // `lastSeenAt`, so this cannot resurrect a ghost row the way a
          // `record_open` re-assert would (the reason the re-assert was gated
          // to verified resumes in the first place). Fire-and-forget: a failed
          // rebind costs us the dedupe on the next pass, nothing more.
          if (restoreAction !== "auto-resume") {
            invoke("terminal_session_rebind_terminal", {
              claudeSessionId: rec.claudeSessionId,
              terminalId: tabId,
              zoneIndex: rec.zoneIndex,
            }).catch((err) => {
              console.warn(
                `[TerminalPage] rebind failed for ${rec.claudeSessionId} → ${tabId}:`,
                err,
              );
            });
          }

          // Restore-pending marker only protects rows whose resume the drain
          // will actually attempt (auto-resume). A `terminal-only` phantom has
          // no resume to verify, so marking it would leave the marker
          // permanently SET — skip it.
          if (restoreAction === "auto-resume") {
            // Durable, backend-owned restore-pending marker (Phase 3, #548):
            // from here until the resume handshake is VERIFIED the liveness
            // poll must never flip this record `poll-dead` — a failed restore
            // leaves the `open` record intact for the next attempt. Cleared in
            // `runVerifiedResume` on verified handshake (and self-healed by
            // the poll once it sees the session confidently alive).
            invoke("terminal_session_mark_restore_pending", {
              claudeSessionId: rec.claudeSessionId,
              boot: true,
            }).catch((err) => {
              console.warn(
                `[TerminalPage] mark restore-pending failed for ${rec.claudeSessionId}:`,
                err,
              );
            });
          }

          if (restoreAction !== "skip-invalid") {
            // auto-resume AND terminal-only tabs get their saved scrollback
            // replayed by the drain; ONLY auto-resume entries type a resume
            // (`isClaudeSession` gates the typing in the drain loop).
            pendingRestores.push({
              tabId,
              scrollbackPath:
                rec.zoneIndex >= 0 ? cosmeticsByZone.get(rec.zoneIndex)?.scrollbackPath : undefined,
              isClaudeSession: restoreAction === "auto-resume",
              claudeSessionId: rec.claudeSessionId,
              claudeConfigDir: typedDir,
              // Provider drives the adapter-supplied resume command + handshake
              // patterns (Phase 4) — defaults to "claude" on pre-provider rows.
              provider: rec.provider,
              // A gate continuation's identity, so the resumed session can
              // still report its work outcome (plan 2026-10-03 D3).
              gate: { gateId: rec.gateId, gateConsumingDeviceId: rec.gateConsumingDeviceId },
              // Deferred re-assert payload: applied by `runVerifiedResume`
              // on VERIFIED handshake only. No `origin` — the backend
              // preserves the existing (authoritative) origin on unasserted writes.
              recordOpen:
                restoreAction === "auto-resume"
                  ? {
                      claudeSessionId: rec.claudeSessionId,
                      // The RESOLVED account's explicit path (the default
                      // home's too), so a record whose dir came from
                      // transcript evidence carries it from now on.
                      configDir: recordDir,
                      workingDir: rec.workingDir,
                      pageId,
                      zoneIndex: rec.zoneIndex,
                      // Re-assert the name the tab now shows, not the bare
                      // `"claude"` label the row may carry.
                      title: tabName,
                      terminalId: tabId,
                    }
                  : undefined,
            });
          }
        }

        // A stopped run lays out nothing more: no plan tab, and no resume
        // drain, whose cleanup would delete the saved scrollback of the
        // sessions it never reached, which another page restores now.
        if (stopped()) return;

        // 4) Plan tabs are cosmetic-only state held in the snapshot (no PTY, no
        //    registry record) — recreate them so the markdown viewers survive a
        //    cold restart. Skip on a React remount: live plan tabs are gone with
        //    the unmounted tree but their snapshot entry still re-creates them
        //    only when we cold-started (no reconnected pty tabs). Cold start is
        //    now signalled by an EMPTY reconnect list — the `null` case aborted
        //    the whole restore above (P1 defect (a)).
        if (!isDrainRerun && saved && reconnectedTabIds.length === 0) {
          for (const session of saved.sessions) {
            if (session.type !== "plan" || !session.planFilePath) continue;
            const tabId = createPlanTab(session.planFilePath);
            if (tabId && session.zoneIndex >= 0) {
              zoneLayout.placeRestoredTab(session.zoneIndex, tabId);
              applyZoneCosmetics(session.zoneIndex);
            }
          }
        }

        // 5) Focused zone from the cosmetic snapshot.
        if (!isDrainRerun && saved && saved.focusedZone >= 0) {
          zoneLayout.setFocusedZone(saved.focusedZone);
        }

        // 6) Drain: restore scrollback then issue `claude --resume` for every
        //    cold-created Claude tab. Identical mechanism to the prior restore;
        //    the per-page gate flips in the drain's finally.
        if (pendingRestores.length > 0) {
          drainScheduled = true;
          setTimeout(async () => {
            try {
              for (const restore of pendingRestores) {
                const ref = terminalRefs.current.get(restore.tabId);
                const handle = ref?.current;

                if (restore.scrollbackPath && handle) {
                  try {
                    const result = await invoke<CommandResponse>("terminal_get_saved_scrollback", {
                      filePath: restore.scrollbackPath,
                    });
                    if (result.success && result.data) {
                      const encoded = (result.data as { data: string }).data;
                      if (encoded) {
                        const raw = atob(encoded);
                        const decoded = new TextDecoder().decode(
                          Uint8Array.from(raw, (c) => c.charCodeAt(0)),
                        );
                        handle.writeToDisplay(decoded);
                      }
                    }
                  } catch (err) {
                    console.warn(
                      `[TerminalPage] Failed to restore scrollback for ${restore.tabId}:`,
                      err,
                    );
                  }
                }

                if (
                  restore.isClaudeSession &&
                  restore.claudeSessionId &&
                  isValidSessionId(restore.claudeSessionId)
                ) {
                  await new Promise((r) => setTimeout(r, 500));
                  // P1 defect (b), drain-time re-check: the classify-time
                  // liveness snapshot is up to ~25 s stale by the time the
                  // drain reaches a tab on a large page — an id that became
                  // live in that window (another page's restore, an operator
                  // resume, an external process) would be forked by typing
                  // `--resume` now. Re-read the registry immediately before
                  // typing EACH resume (drains are already spaced ~500 ms, so
                  // one IPC read per resume is cheap), with the same
                  // fail-closed polarity as classify time: `null` =
                  // indeterminate = skip. A skipped record gets the same
                  // treatment as classify-time skip-alive: no resume typed and
                  // no `record_open` re-assert. What it must NOT get is the
                  // old bare `continue` — that stranded the durable
                  // restore-pending marker on a record still pointing at its
                  // dead pre-restart terminal, which the liveness poll can
                  // never clear (it only self-heals a marker on the KeepAlive
                  // branch, which an unmatched record cannot reach). See
                  // `decideDrainSkipDisposition` for the full mechanism; the
                  // skip now rebinds, clears the marker, and surfaces an
                  // honest banner. The decision (`decideColdResume`) and the
                  // disposition + its IPC wiring (`applyDrainSkip`) are both
                  // covered by pure tests; only this timer closure is not.
                  const liveNow = await fetchLiveClaudeSessionIds();
                  const drainDecision = decideColdResume(liveNow, restore.claudeSessionId);
                  if (drainDecision !== "respawn") {
                    console.warn(
                      `[TerminalPage] drain-time skip for session ${restore.claudeSessionId}: ` +
                        (drainDecision === "skip-alive"
                          ? "a live Claude process now hosts this id (typing --resume would fork it)"
                          : "live-session registry unreadable at drain time — failing closed toward not-respawning"),
                    );
                    await applyDrainSkip({
                      decision: drainDecision,
                      tabId: restore.tabId,
                      claudeSessionId: restore.claudeSessionId,
                      zoneIndex: restore.recordOpen?.zoneIndex ?? -1,
                      updateTab,
                    });
                    continue;
                  }
                  // Type the resume and VERIFY the handshake (retry once on
                  // failure). Fire-and-forget per tab so one slow/failed
                  // verification (up to ~30s) doesn't serialize the drain;
                  // `runVerifiedResume` owns clearing `isReconnecting` /
                  // setting `resumeFailed` and never throws.
                  void runVerifiedResume({
                    terminalRefs: terminalRefs.current,
                    tabId: restore.tabId,
                    claudeSessionId: restore.claudeSessionId,
                    configDir: restore.claudeConfigDir,
                    provider: restore.provider,
                    gate: restore.gate,
                    updateTab,
                    recordOpen: restore.recordOpen,
                    // The pane was created as a plain shell just above, so no
                    // claude can be in it before the first type.
                    verifyOptions: { skipFirstProbe: true },
                  });
                }
              }

              // Not after a guard deferral: the cleanup deletes EVERY saved
              // scrollback file, and the records this run did not reach still
              // need theirs when the retry restores them.
              if (!guardDeferral) {
                try {
                  await invoke("terminal_cleanup_scrollback");
                } catch (err) {
                  console.warn("[TerminalPage] Failed to cleanup scrollback files:", err);
                }
              }

              pendingRestores.length = 0;
            } finally {
              // Restored tabs now carry their claudeSessionId / scrollback —
              // safe to let the debounced auto-save persist this page's layout.
              // Not after a guard deferral, for the same reason as the cleanup:
              // a save now would write a layout missing the unreached records'
              // labels, notes and pins. The retry run opens the gate.
              if (!guardDeferral || guardRetry.gateReleased(initPageId)) {
                openRestoreGate();
              }
              publishRestorePages();
            }
          }, 1500);
        }

        // NOTE: deliberately do NOT clearSavedLayout(). The cosmetic snapshot
        // must survive until the debounced auto-save overwrites it (after the
        // drain has run and tabs hold their ids). The resumable Claude set now
        // comes from the registry, so a failed/never-firing resume no longer
        // risks losing sessions — the registry record persists regardless.
        // No default terminal — start empty so users can launch AI sessions via the Launch Menu.
      } catch (err) {
        // Restore is best-effort. Whatever bound before the throw stays bound;
        // the page renders in whatever state it reached (see the `finally`).
        console.error(`[TerminalPage] restore failed for page ${initPageId}:`, err);
      } finally {
        // The run's own bookkeeping first, so nothing below can skip it: its
        // claims are released and it is no longer in flight.
        for (const id of coldClaims) coldResumeGuard.current.release(id);
        runEntry.ended = true;
        if (runEntry.orphansHandled.size === 0) restoreRuns.current.delete(runToken);
        if (stopped()) {
          // A stopped run touches no page state: the page it ran for is gone,
          // and one that exists under its id again is another page's run's.
          setInitialized(true);
          for (const rec of lateSpawns) {
            if (routeLateSpawn(rec, initPageId, loadKnownPageIds(), rerun) === "adopt") {
              orphanPassesOwed.current.add(initPageId);
            }
          }
          // A removal's orphan-adoption pass waited for this run to end.
          setOrphanPassTick((n) => n + 1);
        } else {
          // Always open the auto-save gate. If a drain timer was scheduled it
          // owns the flip (after resume commands are issued); otherwise — no
          // saved sessions, nothing to restore, or restore threw — open it now
          // so brand-new sessions still persist. Never leave it permanently
          // closed (that would silently disable persistence).
          //
          // Exception (P1 abort-restore): when the reconnect was indeterminate
          // the whole restore was aborted for a retry on the next activation —
          // the gate must stay CLOSED so the retried restore still runs with
          // auto-save suppressed. Not permanent: the retry flips it via this
          // same finally / its drain timer.
          // A guard deferral keeps it closed too, until its retry run restores
          // the records this run did not reach (see the drain timer above).
          // …unless its gate-hold already ran out (`GUARD_GATE_HOLD_MS`).
          if (
            !drainScheduled &&
            ((!restoreAborted && !guardDeferral) || guardRetry.gateReleased(initPageId))
          ) {
            openRestoreGate();
          }
          // Publishes the deferral too (set before the early return above).
          publishRestorePages();
          // ALWAYS leave the loading state, even if restore threw. `initialized`
          // gates the whole page behind a "Loading terminals..." spinner, and the
          // once-per-pageId guard above has already consumed `initPageId` — so a
          // throw anywhere in the block above used to strand the page on that
          // spinner for the lifetime of the window, with no retry and no visible
          // error. Flipping it here degrades a failed restore to "a page with
          // fewer tabs than expected" (recoverable, and the console carries the
          // cause) instead of a permanently blank window.
          setInitialized(true);
          // The PTY-holder boot sweep had not settled, and this run refused
          // some sessions as live or held some back for an adoption in flight:
          // once it settles, re-run the restore (merging, like a drain re-run)
          // for the held-back ones and the refused ones no longer hosted live.
          if (refusedWhileSweeping.length > 0 || heldBackWhileSweeping.length > 0) {
            void recheckRefusedAfterSweep(
              refusedWhileSweeping,
              fetchLiveClaudeSessionIds,
              heldBackWhileSweeping,
            ).then((resumable) => {
              if (resumable.length === 0) return;
              console.warn(
                `[TerminalPage] the holder sweep settled late; resuming ${resumable.length} ` +
                  `session(s) it had kept from a cold resume`,
              );
              scheduleRecheckRerun(initPageId, resumable);
            });
          }
          // An adopting run settles the unadopted-orphans notice for what it
          // restored, held, or no longer found.
          if (adoptingRead !== null) {
            const read = adoptingRead;
            const complete = !restoreAborted && !restoreDeferred && recheckOnly === undefined;
            const handledNow = runEntry.orphansHandled;
            setUnadoptedOrphans((prev) => pruneUnadoptedOrphans(prev, read, handledNow, complete));
          }
          // Re-runs scheduled while this run was in flight: one, now — or,
          // after an abort, a bounded retry for the ids it still owes.
          settleRestoreRun(
            rerun,
            initPageId,
            restoreAborted ? "aborted" : restoreDeferred ? "deferred" : "restored",
            isDrainRerun,
          );
          // After the settle, so the guard retry is the page's one timer: a
          // deferred run schedules none of its own.
          if (guardDeferral) {
            console.warn(
              `[TerminalPage] the resource guard refused a restore on page ${initPageId}; ` +
                `retrying the page after a backoff: ${guardDeferral.refusal.message}`,
            );
            guardRetry.defer(rerun, initPageId, guardDeferral, "session restore", runToken);
          } else if (restoreAborted && guardRetry.owes(initPageId)) {
            // A retry run that aborted restored nothing: the page is still owed.
            guardRetry.rearm(rerun, initPageId, runToken);
          } else if (!restoreAborted && !restoreDeferred) {
            guardRetry.restored(initPageId);
          }
        }
      }
    })().catch((err) => {
      // A throw in the `finally` above: the run's bookkeeping ran before it.
      console.error(`[TerminalPage] settling the restore of page ${initPageId} failed:`, err);
    });
  }, [
    pageId,
    drainResumeEpoch,
    scheduleRecheckRerun,
    rerun,
    reconnectToExistingSessions,
    createTerminal,
    adoptWorkerTab,
    createPlanTab,
    setInitialized,
    sessionPersistence,
    zoneLayout,
    labelsAndTags,
    updateTab,
    terminalRefs,
    noteNeedsAccount,
    publishRestorePages,
    guardRetry,
  ]);

  // Auto-save session layout for persistence across app restarts
  useEffect(() => {
    if (tabs.length === 0) return;
    // Suppress auto-save until THIS page's restore has fully completed.
    // Otherwise the debounced save can fire while the restore path still holds
    // plain shells (no claudeSessionId yet) and clobber the good saved Claude
    // layout. Once the gate opens, the `updateTab` calls that attach
    // claudeSessionId mutate `tabs`, re-running this effect — so no save is
    // permanently lost.
    if (!restoreCompletePages.current.has(pageId)) return;
    sessionPersistence.saveSessionLayout({
      layoutId: layoutState.layoutId,
      tabs,
      assignments: layoutState.assignments,
      zoneLabels: layoutState.zoneLabels,
      zoneNotes: layoutState.zoneNotes,
      pinnedZones: layoutState.pinnedZones,
      focusedZone: layoutState.focusedZone,
    });
  }, [
    pageId,
    tabs,
    layoutState.assignments,
    layoutState.layoutId,
    layoutState.focusedZone,
    layoutState.zoneLabels,
    layoutState.zoneNotes,
    layoutState.pinnedZones,
    sessionPersistence,
  ]);

  // Refs for unmount/close handlers that need latest values
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);
  const layoutStateRef = useRef(layoutState);
  useEffect(() => {
    layoutStateRef.current = layoutState;
  }, [layoutState]);

  // Immediate save on unmount (page switch) — the debounced auto-save may not
  // have flushed, so we save synchronously to avoid losing state. Not while a
  // guard retry is owed inside its gate-hold, for the reason the auto-save gate
  // is closed then: the layout is missing the records the retry will restore,
  // and saving it would drop their cosmetics (`RestoreGuardRetry`).
  const pageIdRef = useRef(pageId);
  useEffect(() => {
    pageIdRef.current = pageId;
  }, [pageId]);
  useEffect(() => {
    return () => {
      const ls = layoutStateRef.current;
      const currentTabs = tabsRef.current;
      if (currentTabs.length > 0 && !guardRetry.holdsSave(pageIdRef.current)) {
        sessionPersistence.saveSessionLayout({
          layoutId: ls.layoutId,
          tabs: currentTabs,
          assignments: ls.assignments,
          zoneLabels: ls.zoneLabels,
          zoneNotes: ls.zoneNotes,
          pinnedZones: ls.pinnedZones,
          focusedZone: ls.focusedZone,
        });
      }
    };
  }, [sessionPersistence, guardRetry]);

  // Save scrollback buffers to disk when the window is about to close.
  //
  // NOTE: we deliberately do NOT record session CLOSEs to the durable registry
  // here. Window teardown is ambiguous with a supervisor restart — the same
  // `onCloseRequested` fires whether the user is quitting for good or the
  // supervisor is bouncing the app. Recording closes on window teardown would
  // mark still-live Claude sessions as closed and drop them from the next
  // restore. Closes are recorded only on EXPLICIT tab close (useTerminalManager
  // `closeTerminal`) and on pty-exit (TerminalPage `handleExit`).
  const handleWindowClose = useCallback(async () => {
    const currentTabs = tabsRef.current;
    if (currentTabs.length === 0) return;

    try {
      const terminalTabs = currentTabs.filter((t) => t.type !== "plan");
      const pathMap = await sessionPersistence.saveScrollbackBuffers(terminalTabs);
      const tabIdToSessionIndex: Record<string, number> = {};
      // The same assignments the layout save walks, so the indices line up.
      const currentAssignments = layoutStateRef.current.assignments;
      const assignedTabIds = new Set(Object.values(currentAssignments));
      let idx = 0;
      for (const [, tabId] of Object.entries(currentAssignments)) {
        if (currentTabs.some((t) => t.id === tabId)) {
          tabIdToSessionIndex[tabId] = idx++;
        }
      }
      for (const tab of currentTabs) {
        if (!assignedTabIds.has(tab.id)) {
          tabIdToSessionIndex[tab.id] = idx++;
        }
      }
      sessionPersistence.updateScrollbackPaths(pathMap, tabIdToSessionIndex);
    } catch (err) {
      console.warn("[TerminalPage] Failed to save scrollback on close:", err);
    }
  }, [sessionPersistence]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    getCurrentWindow()
      .onCloseRequested(async () => {
        await handleWindowClose();
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => {
      unlisten?.();
    };
  }, [handleWindowClose]);

  return {
    /**
     * Orphans a removed page left that no page has adopted: the adoption pass
     * failed for its whole bound and gave up. Empty again once an adopting
     * restore has dealt with them.
     */
    unadoptedOrphans,
    /**
     * Boot-restore candidates NOT resumed because their account is unknown
     * (`needs-account`), each with why. The record is left in the registry;
     * resume it under an operator-chosen account.
     */
    needsAccountRecords,
    /**
     * Pages whose boot restore has fully drained (resume commands issued). A
     * page restores lazily, the first time it is opened. A drain-DEFERRED page
     * is in here too (its auto-save gate opened) — read it together with
     * {@link restoreDeferredPages}.
     */
    restoreCompletePages: publishedCompletePages,
    /** Pages whose restore the coord device drain deferred; they re-run when it lifts. */
    restoreDeferredPages: publishedDeferredPages,
  };
}
