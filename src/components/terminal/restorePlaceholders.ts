/**
 * The restart cohort's placeholders, as the webview knows them (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 7,
 * D6).
 *
 * After a PREPARED restart the backend does not hand the cohort's sessions to
 * the boot auto-restore. `terminal_session_list_open` with `placeholders: true`
 * returns them under `placeholders` instead, and `restart_prep_status` carries
 * what the prepare captured about each (effort, held, state at prepare, the
 * account's usage reset). This module joins the two into one
 * {@link RestorePlaceholder} per session and keeps them in one app-wide store,
 * so the placeholder panes, the per-page "Restore all on this page" button and
 * the global strip all read the same rows.
 *
 * Nothing here spawns anything. Restores go through the one queue
 * (`useRestoreQueue.ts`), which calls the backend's operator twin.
 *
 * UNKNOWN is never rendered as a default: an effort the prepare did not read
 * is "effort unknown", and a row whose `held` flag has not been read yet is
 * treated as held by the bulk buttons (they skip it and say so) — a bulk
 * restore must never bring back a session the operator meant to restore by
 * hand just because the status read had not landed.
 */

import { invoke } from "@tauri-apps/api/core";
import { displayNameOf } from "@/lib/session-ledger";
import type { TerminalSessionRecord } from "./types";

/** `state_at_prepare` as the backend spells it (`restart_prep::cohort::StateAtPrepare`). */
export type StateAtPrepare = "working" | "idle" | "paused" | "limit_hit" | "finished" | "unknown";

/** One placeholder record of the restore set: the lifecycle record, plus why it may not restore yet. */
export interface PlaceholderRecord extends TerminalSessionRecord {
  /** Set by the restore set when a live PTY holder may still run the session. */
  blocker?: string;
  /** Unix millis the session was declared finished (the work axis). */
  finishedAt?: number;
  /** Marks a record that came from `placeholders`, never from `sessions`. */
  restorePlaceholder: true;
}

/** The restore set's `restartPrep` block: which prepare's cohort is restoring. */
export interface RestartPrepRef {
  prepareId: string;
  /** The PTY holder census is not in yet: a placeholder may still be adopted. */
  holderSweepPending: boolean;
}

/** One cohort row of `restart_prep_status` — only the fields this UI reads. */
export interface CohortRowWire {
  session: {
    claudeSessionId: string;
    pageId?: string;
    zoneIndex?: number;
    title?: string | null;
    sessionName?: string | null;
    nameSource?: string | null;
    accountLabel?: string | null;
    workingDir?: string | null;
    planSlug?: string | null;
  };
  effort?: { level?: string | null; source?: string } | null;
  held?: boolean;
  state_at_prepare?: StateAtPrepare;
  limit_reset_at?: number | null;
}

/** What one placeholder pane, and the bulk buttons, know about one session. */
export interface RestorePlaceholder {
  claudeSessionId: string;
  prepareId: string;
  pageId: string;
  zoneIndex: number;
  /** THE display-name rule (`displayNameOf`), as the restored tab is named. */
  name: string;
  /** `null` = the prepare did not record one (UNKNOWN, not "no account"). */
  accountLabel: string | null;
  configDir: string | null;
  workingDir: string | null;
  planSlug: string | null;
  /** `null` = not read. */
  effortLevel: string | null;
  /** `screen` | `account_default` | `unknown`; `null` before the status read. */
  effortSource: string | null;
  /** `null` = not read yet. */
  held: boolean | null;
  finished: boolean;
  /** `null` = not read yet. */
  stateAtPrepare: StateAtPrepare | null;
  limitResetAt: number | null;
  blocker: string | null;
}

/**
 * The Tauri command that sets a cohort row's `held` flag. Phase 5 owns the
 * backend command; this is the ONE place its name is spelled, so integration
 * aligns it here.
 */
export const RESTART_PREP_SET_HELD_COMMAND = "restart_prep_set_held";

/** The tab id a placeholder occupies. Stable per session, so creation is idempotent. */
export function restorePlaceholderTabId(claudeSessionId: string): string {
  return `restore-placeholder-${claudeSessionId}`;
}

/**
 * PURE: whether a placeholder may take its recorded zone — only when the zone
 * is empty or already holds it; never over a tab the operator has there.
 */
export function placeholderMayTakeZone(
  zoneIndex: number,
  tabId: string,
  assignments: Record<number, string>,
): boolean {
  if (zoneIndex < 0) return false;
  const holder = assignments[zoneIndex];
  return holder === undefined || holder === tabId;
}

/** The session a placeholder tab id names, or `null` for any other tab. */
export function placeholderSessionIdOf(tabId: string): string | null {
  const prefix = "restore-placeholder-";
  return tabId.startsWith(prefix) && tabId.length > prefix.length
    ? tabId.slice(prefix.length)
    : null;
}

/** One read of the restore set's placeholder half. */
export interface PlaceholderSet {
  placeholders: PlaceholderRecord[];
  restartPrep: RestartPrepRef | null;
}

/**
 * The restore set's `placeholders` and `restartPrep`, tolerant of an older
 * backend. `null` when `data` is not a restore set at all: that is an
 * UNREADABLE answer, never "no cohort" — publishing it would wipe every
 * page's placeholders.
 */
export function parsePlaceholderSet(data: unknown): PlaceholderSet | null {
  if (!data || typeof data !== "object" || Array.isArray(data)) return null;
  const d = data as { placeholders?: unknown; restartPrep?: unknown };
  const prep = d.restartPrep as { prepareId?: unknown; holderSweepPending?: unknown } | undefined;
  const restartPrep =
    prep && typeof prep.prepareId === "string" && prep.prepareId.length > 0
      ? { prepareId: prep.prepareId, holderSweepPending: prep.holderSweepPending === true }
      : null;
  if (!restartPrep || !Array.isArray(d.placeholders)) return { placeholders: [], restartPrep };
  const placeholders: PlaceholderRecord[] = [];
  for (const raw of d.placeholders) {
    const rec = raw as Partial<TerminalSessionRecord> | null;
    if (!rec || typeof rec.claudeSessionId !== "string" || rec.claudeSessionId.length === 0) {
      continue;
    }
    placeholders.push({ ...(rec as TerminalSessionRecord), restorePlaceholder: true });
  }
  return { placeholders, restartPrep };
}

/** PURE: join one placeholder record with its cohort row (absent before the status read). */
export function toPlaceholder(
  prepareId: string,
  rec: PlaceholderRecord,
  row: CohortRowWire | undefined,
): RestorePlaceholder {
  const blank = (s: string | null | undefined): string | null => {
    const t = s?.trim();
    return t ? t : null;
  };
  const effortSource = row?.effort?.source ?? null;
  return {
    claudeSessionId: rec.claudeSessionId,
    prepareId,
    pageId: rec.pageId || "default",
    zoneIndex: typeof rec.zoneIndex === "number" ? rec.zoneIndex : -1,
    name: displayNameOf({
      claudeSessionId: rec.claudeSessionId,
      sessionName: rec.sessionName ?? null,
      nameSource: rec.nameSource ?? null,
      title: rec.title ?? null,
    }),
    accountLabel: blank(row?.session.accountLabel),
    configDir: blank(rec.configDir),
    workingDir: blank(rec.workingDir) ?? blank(row?.session.workingDir),
    planSlug: blank(row?.session.planSlug),
    effortLevel: effortSource && effortSource !== "unknown" ? blank(row?.effort?.level) : null,
    effortSource,
    held: row ? row.held === true : null,
    // The lifecycle record is the live truth: a finish (or an un-finish) after
    // the prepare is there, and the cohort's capture-time flag is not.
    finished: typeof rec.finishedAt === "number",
    stateAtPrepare: row?.state_at_prepare ?? null,
    limitResetAt: typeof row?.limit_reset_at === "number" ? row.limit_reset_at : null,
    blocker: blank(rec.blocker),
  };
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

/** The effort badge: the level, "held for hand restore" when held, or UNKNOWN. */
export function effortBadgeText(p: RestorePlaceholder): string {
  const level = p.effortLevel ? `effort ${p.effortLevel}` : "effort unknown";
  if (p.held === true) return `${level} · held for hand restore`;
  if (p.held === null) return `${level} · hold not read yet`;
  return level;
}

function formatTime(ms: number): string {
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? String(ms) : d.toLocaleString();
}

/** The state at prepare, with the account's reset time for a usage-limit hit. */
export function stateAtPrepareText(p: RestorePlaceholder): string {
  switch (p.stateAtPrepare) {
    case null:
      return "state at prepare: not read yet";
    case "limit_hit":
      return p.limitResetAt !== null
        ? `usage limit hit · account resets at ${formatTime(p.limitResetAt)}`
        : "usage limit hit · account reset time unknown";
    case "unknown":
      return "state at prepare: unknown";
    default:
      return `was ${p.stateAtPrepare.replace("_", " ")} at prepare`;
  }
}

/**
 * Whether a bulk button restores this row: unheld (read, not unknown),
 * unfinished, and not blocked — a blocked row's restore can only be refused.
 */
export function bulkEligible(p: RestorePlaceholder): boolean {
  return p.held === false && !p.finished && p.blocker === null;
}

/**
 * The strip's counts, each row in exactly one: finished, else blocked, else
 * held (a hold not read yet counts as held), else waiting.
 */
export interface CohortCounts {
  waiting: number;
  held: number;
  finished: number;
  blocked: number;
}

/** PURE: which count a row falls in (see {@link CohortCounts}). */
export function cohortBucket(p: RestorePlaceholder): keyof CohortCounts {
  if (p.finished) return "finished";
  if (p.blocker !== null) return "blocked";
  if (p.held !== false) return "held";
  return "waiting";
}

export function cohortCounts(rows: readonly RestorePlaceholder[]): CohortCounts {
  const c: CohortCounts = { waiting: 0, held: 0, finished: 0, blocked: 0 };
  for (const r of rows) c[cohortBucket(r)]++;
  return c;
}

/**
 * The global strip's text, or `null` when no cohort is restoring (the strip
 * renders nothing). A restoring cohort with no row left to show still gets a
 * strip, so its Dismiss stays reachable: a cohort must never stay `restoring`
 * with no way to end it.
 */
export function cohortStripText(
  rows: readonly RestorePlaceholder[],
  startedAt: string | null,
  prepareId: string | null = rows[0]?.prepareId ?? null,
): string | null {
  if (prepareId === null) return null;
  const when = startedAt ? new Date(startedAt) : null;
  const from = when && !Number.isNaN(when.getTime()) ? when.toLocaleString() : "an earlier prepare";
  if (rows.length === 0) return `Restart cohort from ${from}: nothing waiting to restore`;
  const c = cohortCounts(rows);
  const parts = [`${c.waiting} waiting`, `${c.held} held`];
  if (c.blocked > 0) parts.push(`${c.blocked} blocked`);
  if (c.finished > 0) parts.push(`${c.finished} finished`);
  return `Restart cohort from ${from}: ${parts.join(" · ")}`;
}

// ---------------------------------------------------------------------------
// The app-wide store
// ---------------------------------------------------------------------------

export interface RestoreCohortSnapshot {
  prepareId: string | null;
  /**
   * The last restore-set read could not be read, so whether a cohort is
   * restoring is UNKNOWN: keep re-reading until one read lands.
   */
  unread: boolean;
  /** The PTY holder census was not in at the last read: re-read soon. */
  holderSweepPending: boolean;
  /** The prepare's `started_at`, once the status was read. */
  startedAt: string | null;
  /** The waiting placeholders, every page, ordered by page then zone. */
  rows: readonly RestorePlaceholder[];
  /** Whether `restart_prep_status` has been read for `prepareId`. */
  statusLoaded: boolean;
}

/** What the store needs from the outside, injectable for tests. */
export interface RestoreCohortIo {
  readStatus: () => Promise<{ startedAt: string | null; rows: CohortRowWire[] } | null>;
  /** A fresh restore-set read; `null` when it could not be read. */
  readSet: () => Promise<PlaceholderSet | null>;
}

/**
 * Re-read the restore set for its placeholders: `placeholders: true` so the
 * backend judges and settles the cohort rows exactly as the boot restore's
 * read does, and NO `purpose` — this read restores nothing, so it must not
 * move the lifecycle poll's boot-restore latch. Its `sessions` are discarded:
 * nothing auto-resumes from it. A row it is the FIRST to judge live is settled
 * on the boot restore — which is right, since a running session is no
 * placeholder, and such a row leaves the cohort once the lifecycle poll sees
 * it running (`restart_prep::restore::note_running`).
 */
export async function readPlaceholderSet(): Promise<PlaceholderSet | null> {
  try {
    const resp = await invoke<{ success?: boolean; data?: unknown }>("terminal_session_list_open", {
      placeholders: true,
    });
    return resp?.success === false ? null : parsePlaceholderSet(resp?.data);
  } catch (err) {
    console.warn("[RestoreCohort] restore-set re-read failed:", err);
    return null;
  }
}

/** Read `restart_prep_status` for the record's start time and cohort rows. */
export async function readRestartPrepStatus(): Promise<{
  startedAt: string | null;
  rows: CohortRowWire[];
} | null> {
  try {
    const status = await invoke<{
      record?: {
        started_at?: string;
        cohort?: { rows?: CohortRowWire[] };
      } | null;
    }>("restart_prep_status");
    const rec = status?.record;
    if (!rec) return null;
    const rows = Array.isArray(rec.cohort?.rows) ? rec.cohort!.rows! : [];
    return { startedAt: rec.started_at ?? null, rows };
  } catch (err) {
    console.warn("[RestoreCohort] restart_prep_status failed:", err);
    return null;
  }
}

export class RestoreCohortStore {
  private prepareId: string | null = null;
  private startedAt: string | null = null;
  private records = new Map<string, PlaceholderRecord>();
  private cohortRows = new Map<string, CohortRowWire>();
  private statusLoaded = false;
  private holderSweepPending = false;
  private unread = false;
  private setRead: Promise<void> | null = null;
  /**
   * A forced re-read asked for while a read was in flight: the ONE read
   * chained after it. The in-flight read may have been issued before the
   * change that forced it (the record moving to `restoring`), so it cannot
   * answer it.
   */
  private rereadAfter: Promise<void> | null = null;
  /** Bumped by every `markBack`: a read that started before one is stale. */
  private backEpoch = 0;
  private snapshot: RestoreCohortSnapshot = {
    prepareId: null,
    unread: false,
    holderSweepPending: false,
    startedAt: null,
    rows: [],
    statusLoaded: false,
  };
  private listeners = new Set<() => void>();
  private statusRead: Promise<void> | null = null;

  constructor(private io: RestoreCohortIo) {}

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  getSnapshot = (): RestoreCohortSnapshot => this.snapshot;

  /** A row by session id, while it is still waiting. */
  row(claudeSessionId: string): RestorePlaceholder | undefined {
    return this.snapshot.rows.find((r) => r.claudeSessionId === claudeSessionId);
  }

  /**
   * Fold a restore set read in. Every page's read carries every page's
   * placeholders, so the latest read replaces the list. A read with no
   * `restartPrep` means no cohort is restoring: the store empties.
   */
  publish(set: PlaceholderSet): void {
    this.unread = false;
    const prepareId = set.restartPrep?.prepareId ?? null;
    this.holderSweepPending = set.restartPrep?.holderSweepPending === true;
    if (prepareId !== this.prepareId) {
      this.prepareId = prepareId;
      this.startedAt = null;
      this.cohortRows = new Map();
      this.statusLoaded = false;
      this.statusRead = null;
    }
    this.records = new Map(set.placeholders.map((r) => [r.claudeSessionId, r]));
    this.rebuild();
    if (prepareId !== null && !this.statusLoaded) void this.refreshStatus();
  }

  /**
   * Re-read the restore set (one read at a time) while a cohort is restoring,
   * so a session that came back by another path (a PTY holder's adoption, a
   * restore elsewhere) stops being a placeholder, and a cohort the backend
   * finished empties the store. An unreadable read changes nothing.
   *
   * With no cohort known (and the last read readable) the periodic re-read
   * does nothing; `force` reads anyway — for a change that announces a cohort
   * this store has not seen yet ({@link onPrepChanged}). A forced read never
   * joins a read already in flight (issued before the change, it may not see
   * it): exactly one more read is chained after it, however many forced
   * re-reads ask meanwhile.
   */
  refreshSet(force = false): Promise<void> {
    if (!force && this.prepareId === null && !this.unread) return Promise.resolve();
    if (this.setRead) {
      if (!force) return this.setRead;
      // Chained on settle, not success: a failed in-flight read must still
      // be followed by the forced one, and must not leave this slot stuck.
      const reread = () => {
        this.rereadAfter = null;
        return this.refreshSet(true);
      };
      this.rereadAfter ??= this.setRead.then(reread, reread);
      return this.rereadAfter;
    }
    const epoch = this.backEpoch;
    this.setRead = (async () => {
      try {
        const set = await this.io.readSet();
        // A restore landed while this read was in flight: the read may still
        // list that session. The next read is current; this one is dropped.
        if (set && epoch === this.backEpoch) this.publish(set);
      } finally {
        this.setRead = null;
      }
    })();
    return this.setRead;
  }

  /** Re-read `restart_prep_status` (one read at a time). */
  refreshStatus(): Promise<void> {
    if (this.prepareId === null) return Promise.resolve();
    if (this.statusRead) return this.statusRead;
    const forPrepare = this.prepareId;
    this.statusRead = (async () => {
      try {
        const status = await this.io.readStatus();
        if (this.prepareId !== forPrepare || !status) return;
        this.startedAt = status.startedAt;
        this.cohortRows = new Map(status.rows.map((r) => [r.session.claudeSessionId, r]));
        this.statusLoaded = true;
        this.rebuild();
      } finally {
        this.statusRead = null;
      }
    })();
    return this.statusRead;
  }

  /**
   * A read could not be read: whether a cohort is restoring is UNKNOWN, so
   * the store keeps what it has and asks to be re-read (`refreshSet`).
   */
  markUnread(): void {
    if (this.unread) return;
    this.unread = true;
    this.rebuild();
  }

  /** The session is back (resumed): it is no longer a placeholder. */
  markBack(claudeSessionId: string): void {
    this.backEpoch++;
    if (!this.records.delete(claudeSessionId)) return;
    this.rebuild();
  }

  /** The session was marked finished (or unmarked) on the work axis. */
  markFinished(claudeSessionId: string, finished: boolean): void {
    const rec = this.records.get(claudeSessionId);
    if (!rec) return;
    const next: PlaceholderRecord = { ...rec };
    if (finished) next.finishedAt = Date.now();
    else delete next.finishedAt;
    this.records.set(claudeSessionId, next);
    this.rebuild();
  }

  /** The row's hold was set (or released). */
  markHeld(claudeSessionId: string, held: boolean): void {
    const row = this.cohortRows.get(claudeSessionId);
    if (!row) return;
    this.cohortRows.set(claudeSessionId, { ...row, held });
    this.rebuild();
  }

  /**
   * A `restart-prep-changed` event: the prepare record changed. Two moves
   * follow from its payload:
   *
   * - **a cohort this store has not seen began restoring** (Release hold on a
   *   restarted runner, a rebuilt runner's boot step). The store may have
   *   read "no cohort" before — at boot, before the record reached
   *   `restoring` — and it re-reads only while it knows a cohort, so the set
   *   is re-read regardless, or the placeholders and the strip never appear;
   * - **the cohort this store holds left `restoring`** (dismissed from any
   *   surface, or every row back): it ends here at once
   *   ({@link endCohort}), whichever surface or path ended it.
   *
   * Anything else re-reads as before. An unrecognized payload re-reads too:
   * it may well be the restoring cohort.
   */
  onPrepChanged(payload: unknown): Promise<void> {
    const record = (payload as { record?: { prepare_id?: unknown; state?: unknown } } | null)
      ?.record;
    const prepareId = typeof record?.prepare_id === "string" ? record.prepare_id : null;
    const state = typeof record?.state === "string" ? record.state : null;
    if (prepareId !== null && prepareId === this.prepareId && state !== "restoring") {
      this.endCohort(prepareId);
    }
    const announcesRestoring = state === "restoring" || prepareId === null;
    void this.refreshStatus();
    return this.refreshSet(announcesRestoring);
  }

  /**
   * The cohort `prepareId` ended — dismissed (from the panel, the strip or
   * any other door) or every row back. The ONE end of a cohort in the
   * webview: the store empties, which drops every queued restore of it
   * (`restoreQueue` follows the store) and every page's placeholder tabs. A
   * no-op for any other prepare.
   */
  endCohort(prepareId: string): void {
    if (this.prepareId !== prepareId) return;
    this.clear();
    emitPlaceholderEvent({ kind: "dismissed" });
  }

  /** The cohort was dismissed: nothing is a placeholder any more. */
  clear(): void {
    this.prepareId = null;
    this.holderSweepPending = false;
    this.startedAt = null;
    this.records = new Map();
    this.cohortRows = new Map();
    this.statusLoaded = false;
    this.statusRead = null;
    this.rebuild();
  }

  private rebuild(): void {
    const prepareId = this.prepareId;
    const rows =
      prepareId === null
        ? []
        : [...this.records.values()]
            .map((rec) => toPlaceholder(prepareId, rec, this.cohortRows.get(rec.claudeSessionId)))
            .sort(
              (a, b) =>
                a.pageId.localeCompare(b.pageId) ||
                a.zoneIndex - b.zoneIndex ||
                a.claudeSessionId.localeCompare(b.claudeSessionId),
            );
    this.snapshot = {
      prepareId,
      unread: this.unread,
      holderSweepPending: this.holderSweepPending,
      startedAt: this.startedAt,
      rows,
      statusLoaded: this.statusLoaded,
    };
    for (const fn of this.listeners) fn();
  }
}

/** The app's one cohort store. */
export const restoreCohortStore = new RestoreCohortStore({
  readStatus: readRestartPrepStatus,
  readSet: readPlaceholderSet,
});

/** How often a restoring cohort's placeholders are re-read. */
export const COHORT_REFRESH_MS = 15_000;
/** ...and while the PTY holder census is not in yet. */
export const COHORT_REFRESH_SWEEP_PENDING_MS = 3_000;

/**
 * PURE: the re-read interval for `snap`, or `null` when nothing is restoring.
 * A store whose last read was unreadable keeps re-reading: that is UNKNOWN,
 * not "no cohort".
 */
export function cohortRefreshInterval(snap: RestoreCohortSnapshot): number | null {
  if (snap.prepareId === null) return snap.unread ? COHORT_REFRESH_MS : null;
  return snap.holderSweepPending ? COHORT_REFRESH_SWEEP_PENDING_MS : COHORT_REFRESH_MS;
}

// ---------------------------------------------------------------------------
// Row actions (each answers `null` on success, else what went wrong)
// ---------------------------------------------------------------------------

/**
 * Finish (or un-finish) a placeholder's session on the work axis —
 * `terminal_session_set_finished`, reversible. A finished row leaves the bulk
 * buttons; its own Restore still works (the resume core lifts the mark once
 * the resume is verified).
 */
export async function setPlaceholderFinished(
  claudeSessionId: string,
  finished: boolean,
  store: RestoreCohortStore = restoreCohortStore,
): Promise<string | null> {
  try {
    const resp = await invoke<{ success?: boolean; message?: string | null }>(
      "terminal_session_set_finished",
      {
        claudeSessionId,
        finished,
        reason: finished ? "finished from its restore placeholder" : null,
      },
    );
    if (resp?.success !== true) return resp?.message ?? "the session was not found";
    store.markFinished(claudeSessionId, finished);
    return null;
  } catch (err) {
    return String(err);
  }
}

/**
 * Hold a row for a hand restore, or release it, through
 * {@link RESTART_PREP_SET_HELD_COMMAND}. The store changes only once the
 * backend accepted it, so the badge never claims a hold that was not written.
 */
export async function setPlaceholderHeld(
  prepareId: string,
  claudeSessionId: string,
  held: boolean,
  store: RestoreCohortStore = restoreCohortStore,
): Promise<string | null> {
  try {
    await invoke(RESTART_PREP_SET_HELD_COMMAND, { prepareId, claudeSessionId, held });
    store.markHeld(claudeSessionId, held);
    return null;
  } catch (err) {
    return String(err);
  }
}

// ---------------------------------------------------------------------------
// Page events: placeholder replacement and dismissal
// ---------------------------------------------------------------------------

/** What a page does when the queue resolves one of its placeholders. */
export type PlaceholderEvent =
  | {
      kind: "back";
      pageId: string;
      claudeSessionId: string;
      zoneIndex: number;
      /** The live pane the resume core created; `null` when the reply named none. */
      terminalId: string | null;
    }
  | { kind: "dismissed" };

const placeholderListeners = new Set<(e: PlaceholderEvent) => void>();

export function onPlaceholderEvent(fn: (e: PlaceholderEvent) => void): () => void {
  placeholderListeners.add(fn);
  return () => placeholderListeners.delete(fn);
}

export function emitPlaceholderEvent(e: PlaceholderEvent): void {
  for (const fn of placeholderListeners) fn(e);
}

/**
 * Ask the page roster to recreate pages a placeholder names but this window
 * no longer has (a page closed after the prepare). Handled by
 * `useTerminalPages`, which adds without activating.
 */
export const ENSURE_PAGES_EVENT = "terminal-ensure-pages";

export interface EnsurePagesDetail {
  pages: Array<{ id: string; name: string }>;
}

/** PURE: the pages to recreate for `placeholders`, given the pages that exist. */
export function missingPlaceholderPages(
  placeholders: readonly Pick<PlaceholderRecord, "pageId">[],
  knownPageIds: readonly string[],
): Array<{ id: string; name: string }> {
  const known = new Set(knownPageIds);
  const out: Array<{ id: string; name: string }> = [];
  for (const p of placeholders) {
    const id = p.pageId || "default";
    if (known.has(id)) continue;
    known.add(id);
    // The cohort records the page id, not its title: name it for what it is.
    out.push({ id, name: id === "default" ? "Terminal" : `Restored ${id.slice(0, 8)}` });
  }
  return out;
}

export function requestEnsurePages(pages: EnsurePagesDetail["pages"]): void {
  if (pages.length === 0 || typeof window === "undefined") return;
  window.dispatchEvent(
    new CustomEvent<EnsurePagesDetail>(ENSURE_PAGES_EVENT, { detail: { pages } }),
  );
}
