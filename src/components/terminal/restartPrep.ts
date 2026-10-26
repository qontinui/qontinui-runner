/**
 * The operator's "prepare for restart" — the frontend half (plan
 * `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 5).
 *
 * The backend (`src-tauri/src/restart_prep/`) holds spawns, tells live sessions
 * to save and pause, freezes account moves and records every session. This
 * module is what the Terminal page reads it through:
 *
 * - the WIRE types of the prepare record (`restart_prep_current` /
 *   `restart_prep_prepare` / `restart_prep_cancel` / `restart_prep_set_held`
 *   and the `restart-prep-changed` event payload), snake_case as Rust sends
 *   them — the cohort's `session` is the ledger entry, camelCase;
 * - PURE readings of a record: the headline, a row's state chip, its effort
 *   badge. Pure so the `environment: "node"` vitest suite pins them;
 * - ONE store per webview ({@link restartPrepStore}). The header button, the
 *   panel and the `/prepare-restart` command all drive it, so a prepare started
 *   from the command bar and one started from the button are the same prepare,
 *   and a second click while one is in flight joins it instead of sending
 *   another.
 *
 * The store LISTENS to `restart-prep-changed` (emitted by the backend on every
 * state change) instead of polling; it reads `restart_prep_current` once when the
 * first consumer mounts, because an event only says what changed after it.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import { describeThrown } from "@/lib/utils";
import { restoreCohortStore } from "./restorePlaceholders";

// ── Wire types ────────────────────────────────────────────────────────

/** `restart_prep::PrepState`. */
export type PrepState = "preparing" | "ready" | "restoring" | "done" | "cancelled";

/** `restart_prep::HoldStatus` (internally tagged on `status`). */
export type HoldStatus =
  | { status: "not_attempted" }
  | { status: "confirmed" }
  | { status: "unconfirmed"; cause: string }
  | { status: "local" }
  | { status: "local_secondary" }
  | { status: "release_sent"; outcome: string };

/** `restart_prep::effort::Effort`. */
export interface Effort {
  level: string | null;
  source: "screen" | "account_default" | "unknown";
}

/** `restart_prep::effort::Baseline`. */
export interface Baseline {
  baseline: string | null;
  default_source: "settings" | "cli_default" | "unknown";
}

/** `restart_prep::cohort::PauseState`. */
export type PauseState =
  | "working"
  | "notified"
  | "paused"
  | "finished"
  | "closed"
  | "limit_hit"
  | "unknown";

/** The fields of the ledger entry (`session_ledger::LedgerEntry`) the panel reads. */
export interface CohortSession {
  claudeSessionId: string;
  terminalId: string;
  pageId: string;
  zoneIndex: number;
  title: string | null;
  sessionName: string | null;
  accountLabel: string | null;
  configDir: string | null;
}

/** `restart_prep::restore::RowRestore`. */
export interface RowRestore {
  at_ms: number;
  at: string;
  verdict: string;
  effort_applied: string | null;
  effort: string | null;
}

/** `restart_prep::cohort::CohortRow` — the fields the panel reads. */
export interface CohortRow {
  session: CohortSession;
  effort: Effort;
  baseline: Baseline;
  model: string | null;
  state_at_prepare: string;
  held: boolean;
  held_by_operator?: boolean;
  notified_at: number | null;
  limit_reset_at: number | null;
  finished: boolean | null;
  pause_state?: PauseState | null;
  pause_reason?: string | null;
  has_live_children?: boolean | null;
  notice_skipped?: string | null;
  restore?: RowRestore | null;
}

/** `restart_prep::notice::Blocker`. */
export interface Blocker {
  session: string | null;
  display_name: string | null;
  reason: string;
}

/** `restart_prep::notice::PrepVerdict`. */
export interface PrepVerdict {
  prepare_id: string;
  state: PrepState;
  hold: string;
  ready_to_restart: boolean;
  blockers: Blocker[];
  cohort_captured_at_ms: number;
  evaluated_at_ms: number;
  evaluated_at: string;
}

/** `restart_prep::PrepareRecord` — the fields the panel reads. */
export interface PrepareRecord {
  prepare_id: string;
  started_at_ms: number;
  started_at: string;
  prepared_build: string;
  state: PrepState;
  hold: HoldStatus;
  cohort: { captured_at_ms: number; captured_at: string; reason: string; rows: CohortRow[] };
  reason: string;
  hold_until_ms: number | null;
  hold_until: string | null;
  verdict: PrepVerdict | null;
  /**
   * The hold's release, once one was sent (`ReleaseReport`): `outcome` is
   * coord's answer verbatim, `read_back` the device drain read after it
   * (`clear`, `drained (<reason>)`, `unreadable: …`; null when no coord
   * release was sent).
   */
  release?: { outcome: string; read_back: string | null } | null;
}

/** `restart_prep::prepare::PrepView`. */
export interface PrepView {
  hold: string;
  active: boolean;
  accounts_frozen: boolean;
  local_hold: boolean;
  same_build_hold: boolean;
  hold_expired_at: string | null;
  release_failed: string | null;
  /**
   * The record's verdict was evaluated by THIS runner process. After a
   * restart it is false until the notice loop's first reading here: the
   * persisted verdict and row states describe the previous process's
   * sessions. Absent (an older backend): not known to be current.
   */
  verdict_current?: boolean;
}

/** `restart_prep::prepare::PrepChanged` — the event payload and the action replies. */
export interface PrepChanged {
  record: PrepareRecord;
  view: PrepView;
}

/** The Tauri event the backend emits on every prepare state change. */
export const RESTART_PREP_CHANGED_EVENT = "restart-prep-changed";

/**
 * Narrow an untrusted value to a {@link PrepareRecord}. A shape surprise is
 * `null` — a failed read the panel renders as UNKNOWN — never a half-read
 * record whose missing fields would render as defaults.
 */
export function asPrepareRecord(value: unknown): PrepareRecord | null {
  if (typeof value !== "object" || value === null) return null;
  const r = value as Partial<PrepareRecord>;
  if (typeof r.prepare_id !== "string" || typeof r.state !== "string") return null;
  if (typeof r.hold !== "object" || r.hold === null) return null;
  if (typeof r.cohort !== "object" || r.cohort === null || !Array.isArray(r.cohort.rows)) {
    return null;
  }
  return r as PrepareRecord;
}

/** Narrow an untrusted value to a {@link PrepChanged}; `null` on any surprise. */
export function asPrepChanged(value: unknown): PrepChanged | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as { record?: unknown; view?: unknown };
  const record = asPrepareRecord(v.record);
  const view = v.view as Partial<PrepView> | undefined;
  if (!record || typeof view !== "object" || view === null || typeof view.hold !== "string") {
    return null;
  }
  return { record, view: view as PrepView };
}

// ── Pure readings ─────────────────────────────────────────────────────

/** A prepare in its restart window — the one the panel's headline is about. */
export function inWindow(record: PrepareRecord | null): boolean {
  return record?.state === "preparing" || record?.state === "ready";
}

/** The name the panel shows: the session name, else the tab title, else the id. */
export function rowName(row: CohortRow): string {
  return (
    row.session.sessionName?.trim() || row.session.title?.trim() || row.session.claudeSessionId
  );
}

/** The headline's three — and only three — readings. */
export type Headline =
  | { kind: "ready"; text: string }
  | { kind: "waiting"; text: string; blockers: Blocker[] }
  /** `blockers`: every blocker the verdict named, so a prepare-wide UNKNOWN
   * never hides which sessions are still working (empty when none was read). */
  | { kind: "unknown"; text: string; blockers: Blocker[] };

export const READY_HEADLINE = "Safe to restart — rebuild and restart the runner now";

/** What the headline is computed from. */
export interface HeadlineInput {
  record: PrepareRecord | null;
  /** The last read of the record failed. */
  readError: string | null;
  /** At least one read (or event) answered. */
  loaded: boolean;
  /**
   * The record's CURRENT view. The verdict is the notice loop's, evaluated
   * at its last tick; a hold that changed since (an Extend that coord
   * refused, a hold that expired) makes that verdict stale. Omitted: not
   * compared.
   */
  view?: PrepView | null;
}

/**
 * The panel's headline: EXACTLY one of "Safe to restart — …", "Waiting on N
 * sessions" (each blocker named) or "UNKNOWN: <input>". `null` only when there
 * is no prepare in its window to headline (none, or one that already ended —
 * the panel says which instead).
 *
 * Read from the backend's own verdict (`record.verdict`, Phase 4), never
 * re-derived here: the verdict is the one place "ready" is decided, and a
 * second derivation would be a second answer.
 *
 * - An unread record, or a verdict that is absent or about another prepare, is
 *   UNKNOWN — never a default of either answer.
 * - A PREPARE-WIDE blocker (no session: the hold, the census, the re-capture)
 *   is an input the safety claim rests on, so it makes the headline UNKNOWN
 *   naming it; "Waiting on N sessions" is said only of sessions. The UNKNOWN
 *   still carries every blocker, so the session list stays on screen.
 * - A verdict that contradicts itself (ready with blockers, not ready with
 *   none), or that was evaluated under a hold that has changed since, is
 *   UNKNOWN too.
 */
export function restartPrepHeadline({
  record,
  readError,
  loaded,
  view,
}: HeadlineInput): Headline | null {
  if (readError !== null) {
    return {
      kind: "unknown",
      text: `UNKNOWN: the restart-prep record (${readError})`,
      blockers: [],
    };
  }
  if (!loaded) {
    return {
      kind: "unknown",
      text: "UNKNOWN: the restart-prep record (not read yet)",
      blockers: [],
    };
  }
  if (!record || !inWindow(record)) return null;
  const verdict = record.verdict;
  if (!verdict || verdict.prepare_id !== record.prepare_id) {
    return {
      kind: "unknown",
      text: "UNKNOWN: the verdict (the runner has not evaluated this prepare yet)",
      blockers: [],
    };
  }
  if (!Array.isArray(verdict.blockers)) {
    return { kind: "unknown", text: "UNKNOWN: the verdict's blockers (unreadable)", blockers: [] };
  }
  const blockers = verdict.blockers;
  if (view && view.verdict_current !== true) {
    return {
      kind: "unknown",
      text: "UNKNOWN: the verdict (evaluated before this runner restarted — waiting for its first reading here)",
      blockers,
    };
  }
  // Both strings are the backend's one hold label (`HoldStatus::label`, or
  // "hold expired at …"), so they compare directly.
  if (view && verdict.hold !== view.hold) {
    return {
      kind: "unknown",
      text: `UNKNOWN: the verdict (evaluated under hold "${verdict.hold}"; the hold now reads "${view.hold}")`,
      blockers,
    };
  }
  const prepareWide = blockers.filter((b) => !b.session);
  if (prepareWide.length > 0) {
    return {
      kind: "unknown",
      text: `UNKNOWN: ${prepareWide.map((b) => b.reason).join("; ")}`,
      blockers,
    };
  }
  if (verdict.ready_to_restart && blockers.length === 0) {
    return { kind: "ready", text: READY_HEADLINE };
  }
  if (!verdict.ready_to_restart && blockers.length > 0) {
    const n = new Set(blockers.map((b) => b.session)).size;
    return {
      kind: "waiting",
      text: `Waiting on ${n} session${n === 1 ? "" : "s"}`,
      blockers,
    };
  }
  return {
    kind: "unknown",
    text: verdict.ready_to_restart
      ? "UNKNOWN: the verdict (ready, yet it names blockers)"
      : "UNKNOWN: the verdict (not ready, and it names no blocker)",
    blockers,
  };
}

/** A blocker's line: who, then why. */
export function blockerLine(b: Blocker): string {
  return `${b.display_name?.trim() || b.session || "prepare"} — ${b.reason}`;
}

/**
 * The text and tone of a row's state chip, from what the backend observed.
 *
 * `recordState` is the prepare's state: the notice loop reads `pause_state`
 * only while the prepare is in its window, so outside it (after a restart, or
 * a cancel) that value is the LAST reading, not a current one, and is shown
 * as such — a dead session never reads "paused" in green.
 */
export function rowStateChip(
  row: CohortRow,
  recordState?: PrepState,
  /** `PrepView.verdict_current`: the row states were read by this process. */
  current?: boolean,
): { text: string; tone: "ok" | "wait" | "unknown" } {
  if (row.restore) {
    const v = row.restore.verdict;
    const tone =
      v === "resumed" || v === "running" ? "ok" : v.startsWith("failed") ? "unknown" : "wait";
    return { text: `restore: ${v}`, tone };
  }
  const s = row.pause_state;
  if (recordState === "restoring") {
    return {
      text: `waiting to be restored${s ? ` (was ${s.replace(/_/g, " ")})` : ""}`,
      tone: "wait",
    };
  }
  if (recordState === "cancelled" || recordState === "done") {
    return { text: s ? `last read: ${s.replace(/_/g, " ")}` : "not read", tone: "unknown" };
  }
  if (!s) return { text: "unknown: not read yet", tone: "unknown" };
  if (current === false) {
    // In its window, but read by the process before a restart.
    return { text: `last read: ${s.replace(/_/g, " ")}`, tone: "unknown" };
  }
  if (s === "unknown") return { text: "unknown", tone: "unknown" };
  const text = s.replace(/_/g, " ");
  const tone = s === "paused" || s === "finished" || s === "closed" ? "ok" : "wait";
  return { text, tone };
}

/** The effort badge: the level and where it was read, or "effort unknown". */
export function effortBadge(row: CohortRow): string {
  const { level, source } = row.effort ?? { level: null, source: "unknown" };
  if (!level || source === "unknown") return "effort unknown";
  return `${level} · ${source === "screen" ? "screen" : "account default"}`;
}

// ── The store ─────────────────────────────────────────────────────────

/**
 * An operator action's acknowledgement (`an-action-must-acknowledge-itself`).
 * The click's receipt IS the `in-flight` render: the control turns into a
 * disabled spinner in the same frame as the click, before anything is sent,
 * and then reaches a terminal `done` or `failed` that says what happened.
 */
export type ActionPhase = "idle" | "in-flight" | "done" | "failed";

export interface ActionState {
  phase: ActionPhase;
  /** The terminal state's one line: what happened, or why it failed. */
  message?: string;
}

const IDLE: ActionState = { phase: "idle" };

const MALFORMED_EVENT = "a restart-prep-changed event came in an unrecognized shape";

/** The hold labels (`HoldStatus::label`) under which spawns are held as asked. */
const HELD_LABELS = new Set(["confirmed", "local", "local (secondary)"]);

/** The record-wide operator actions, each with its own acknowledgement. */
type PrepAction = "prepare" | "cancel" | "extend";

export interface RestartPrepSnapshot {
  record: PrepareRecord | null;
  view: PrepView | null;
  readError: string | null;
  loaded: boolean;
  panelOpen: boolean;
  prepare: ActionState;
  cancel: ActionState;
  extend: ActionState;
  dismiss: ActionState;
  /**
   * Why the `restart-prep-changed` listener could not be set up, or `null`.
   * Without it the panel shows only what each read and action returned, and
   * says so rather than going quietly stale.
   */
  eventsError: string | null;
  /** Per Claude session id: the held toggle's acknowledgement. */
  held: Readonly<Record<string, ActionState>>;
}

/** What the store reaches the backend through; injected so tests need no Tauri. */
export interface RestartPrepDeps {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  listen: (event: string, onPayload: (payload: unknown) => void) => Promise<() => void>;
  /**
   * The cohort `prepareId` was dismissed here: end it in the webview through
   * the one end every dismiss shares (the cohort store's `endCohort` — the
   * queue and the placeholder tabs follow it), so the panel's Dismiss stops
   * the restore queue exactly as the strip's does, event channel or not.
   */
  onDismissed?: (prepareId: string) => void;
}

export interface RestartPrepStore {
  getSnapshot(): RestartPrepSnapshot;
  subscribe(listener: () => void): () => void;
  /**
   * Start listening to `restart-prep-changed` and read the record once.
   * Reference-counted: the last `release` stops listening.
   */
  retain(): () => void;
  /** Re-read the record (`restart_prep_current`). */
  refresh(): Promise<void>;
  /**
   * Prepare for a restart. A call while one is in flight JOINS it (the same
   * promise; nothing is sent twice). Resolves to the reply, or throws the
   * backend's refusal.
   */
  prepare(reason?: string): Promise<PrepChanged>;
  /** Cancel the prepare (also the "Release hold" of a same-build restart). */
  cancel(): Promise<PrepChanged>;
  /** Extend the spawn hold's `until` by 6 h (re-sent to coord as the same hold). */
  extend(): Promise<PrepChanged>;
  /**
   * Dismiss a rebuilt runner's restore of the cohort (`restoring` → `done`):
   * its waiting rows stop being placeholders and go to Past Sessions. What
   * ends a restore the operator will not finish, and frees the next prepare.
   */
  dismiss(): Promise<PrepareRecord>;
  /** The held toggle: hold a row for a hand restore, or release it. */
  setHeld(claudeSessionId: string, held: boolean): Promise<PrepareRecord>;
  openPanel(): { changed: boolean };
  closePanel(): void;
}

export function createRestartPrepStore(deps: RestartPrepDeps): RestartPrepStore {
  let snap: RestartPrepSnapshot = {
    record: null,
    view: null,
    readError: null,
    loaded: false,
    panelOpen: false,
    prepare: IDLE,
    cancel: IDLE,
    extend: IDLE,
    dismiss: IDLE,
    eventsError: null,
    held: {},
  };
  const listeners = new Set<() => void>();
  let refs = 0;
  let stopListening: (() => void) | null = null;
  let listening = false;
  const inflight: Partial<Record<PrepAction, Promise<PrepChanged>>> = {};
  /**
   * Bumped by every event and every applied action reply, so neither an
   * action reply nor a read that started before a newer change lands over it.
   */
  let eventSeq = 0;
  /** Bumped by every read started; see `refresh`. */
  let readSeq = 0;

  const set = (patch: Partial<RestartPrepSnapshot>): void => {
    snap = { ...snap, ...patch };
    for (const l of listeners) l();
  };

  const applyChanged = (changed: PrepChanged): void => {
    set({ record: changed.record, view: changed.view, readError: null, loaded: true });
  };

  /**
   * Apply an action's reply unless something newer landed while it was in
   * flight. An applied reply counts as newer than any read still out.
   */
  const applyReply = (changed: PrepChanged, seqAtStart: number): void => {
    if (eventSeq !== seqAtStart) return;
    eventSeq += 1;
    applyChanged(changed);
  };

  const onEvent = (payload: unknown): void => {
    const changed = asPrepChanged(payload);
    if (!changed) {
      // Never applied half-read — and never silently: the panel would keep
      // showing the old state as if it were live.
      set({ eventsError: MALFORMED_EVENT });
      return;
    }
    // A good event after a malformed one: the channel is live again.
    if (snap.eventsError === MALFORMED_EVENT) set({ eventsError: null });
    eventSeq += 1;
    applyChanged(changed);
  };

  const refresh = async (): Promise<void> => {
    const seq = eventSeq;
    // Reads can overlap (mount, the re-read once the listener is up, a panel
    // open): only the newest one started may land.
    const read = ++readSeq;
    try {
      // `restart_prep_current`: the record and view as the runner holds them
      // in memory — exactly what an event carries — with no live cohort
      // capture. `null` is "no readable record", not proof none ever ran.
      const current = await deps.invoke("restart_prep_current");
      // An event or applied reply that landed while this read was out is
      // newer, and so is any read started after this one.
      if (eventSeq !== seq || read !== readSeq) return;
      if (current === null || current === undefined) {
        set({ record: null, view: null, readError: null, loaded: true });
        return;
      }
      const changed = asPrepChanged(current);
      if (!changed) {
        set({ readError: "the record came back in an unrecognized shape", loaded: true });
        return;
      }
      set({ record: changed.record, view: changed.view, readError: null, loaded: true });
    } catch (err) {
      if (eventSeq !== seq || read !== readSeq) return;
      set({
        readError: describeThrown(err, "could not read the restart-prep record"),
        loaded: true,
      });
    }
  };

  /**
   * One `listen` at a time: a release while it is still being set up stops it
   * as soon as it answers, and a retain in the meantime reuses it.
   */
  const startListening = (): void => {
    if (listening) return;
    listening = true;
    deps
      .listen(RESTART_PREP_CHANGED_EVENT, onEvent)
      .then((stop) => {
        if (snap.eventsError !== null) set({ eventsError: null });
        if (refs === 0) {
          stop();
          listening = false;
        } else {
          stopListening = stop;
          // Read again now that nothing can slip past: a change emitted
          // between the first read and this registration was announced to
          // nobody, and a settled prepare may not announce again.
          void refresh();
        }
      })
      .catch((err) => {
        // No event channel: the panel still reads on mount and applies each
        // action's own reply, and says that it is not live.
        listening = false;
        set({ eventsError: describeThrown(err, "the restart-prep event listener failed") });
      });
  };

  const retain = (): (() => void) => {
    refs += 1;
    if (refs === 1) {
      startListening();
      void refresh();
    }
    let released = false;
    return () => {
      if (released) return;
      released = true;
      refs -= 1;
      if (refs === 0 && stopListening) {
        stopListening();
        stopListening = null;
        listening = false;
      }
    };
  };

  /**
   * One prepare / cancel / extend: in flight at once per action — a second
   * call JOINS the first (same promise, nothing sent twice; a joining
   * prepare's reason is the first one's) — then a terminal phase naming the
   * hold the backend read back, or its refusal.
   */
  const runAction = (
    action: PrepAction,
    cmd: string,
    args?: Record<string, unknown>,
  ): Promise<PrepChanged> => {
    const joined = inflight[action];
    if (joined) return joined;
    set({ [action]: { phase: "in-flight" } });
    const seq = eventSeq;
    const run = (async (): Promise<PrepChanged> => {
      try {
        const reply = asPrepChanged(await deps.invoke(cmd, args));
        if (!reply) throw new Error(`the ${action} reply came back in an unrecognized shape`);
        applyReply(reply, seq);
        if (action === "extend" && !HELD_LABELS.has(reply.view.hold)) {
          // The backend extended only the runner-local backstop: coord did
          // not take the new deadline, so its drain still ends where it did.
          // "Did not confirm", never "did not extend": a read-back miss or a
          // timeout can follow a write that landed. What IS known: the
          // runner-local backstop holds spawns to the new expiry.
          throw new Error(
            `coord did not confirm the extension (hold ${reply.view.hold}); the runner-local ` +
              `hold keeps spawns held to ${reply.record.hold_until ?? "?"} — Extend again to retry`,
          );
        }
        const expiry =
          action === "extend" && reply.record.hold_until
            ? `; expires ${reply.record.hold_until}`
            : "";
        set({ [action]: { phase: "done", message: `hold ${reply.view.hold}${expiry}` } });
        return reply;
      } catch (err) {
        const message = describeThrown(err, `the ${action} failed`);
        set({ [action]: { phase: "failed", message } });
        throw new Error(message, { cause: err });
      } finally {
        delete inflight[action];
      }
    })();
    inflight[action] = run;
    return run;
  };

  const dismiss = async (): Promise<PrepareRecord> => {
    const record = snap.record;
    if (!record || record.state !== "restoring") {
      const message = "no restore is waiting — nothing to dismiss";
      set({ dismiss: { phase: "failed", message } });
      throw new Error(message);
    }
    if (snap.dismiss.phase === "in-flight") throw new Error("the dismiss is already in flight");
    set({ dismiss: { phase: "in-flight" } });
    const seq = eventSeq;
    try {
      const reply = asPrepareRecord(
        await deps.invoke("restart_prep_dismiss", { prepareId: record.prepare_id }),
      );
      if (!reply) throw new Error("the dismiss reply came back in an unrecognized shape");
      if (eventSeq === seq && snap.record?.prepare_id === reply.prepare_id) {
        eventSeq += 1;
        set({ record: reply });
      }
      if (reply.state !== "done") throw new Error(`the record reads ${reply.state}, not done`);
      deps.onDismissed?.(reply.prepare_id);
      set({ dismiss: { phase: "done", message: "dismissed" } });
      return reply;
    } catch (err) {
      const message = describeThrown(err, "the dismiss failed");
      set({ dismiss: { phase: "failed", message } });
      throw new Error(message, { cause: err });
    }
  };

  const setHeld = async (claudeSessionId: string, held: boolean): Promise<PrepareRecord> => {
    const record = snap.record;
    const mark = (s: ActionState): void => set({ held: { ...snap.held, [claudeSessionId]: s } });
    if (!record) {
      mark({ phase: "failed", message: "no restart prepare is recorded" });
      throw new Error("no restart prepare is recorded");
    }
    mark({ phase: "in-flight" });
    const seq = eventSeq;
    try {
      const reply = asPrepareRecord(
        await deps.invoke("restart_prep_set_held", {
          prepareId: record.prepare_id,
          claudeSessionId,
          held,
        }),
      );
      if (!reply) throw new Error("the reply came back in an unrecognized shape");
      // The backend announces the change too; applying the reply keeps the
      // toggle honest where the event channel is down — but never over an
      // event that landed meanwhile (a notice tick queued behind the toggle
      // runs, and announces, the moment the toggle lets go).
      if (eventSeq === seq && snap.record?.prepare_id === reply.prepare_id) {
        eventSeq += 1;
        set({ record: reply });
      }
      const row = reply.cohort.rows.find((r) => r.session.claudeSessionId === claudeSessionId);
      if (!row || row.held !== held) {
        throw new Error("the record does not show the change");
      }
      mark({ phase: "done", message: held ? "held" : "released" });
      return reply;
    } catch (err) {
      const message = describeThrown(err, "could not change the hold");
      mark({ phase: "failed", message });
      throw new Error(message, { cause: err });
    }
  };

  return {
    getSnapshot: () => snap,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    retain,
    refresh,
    prepare: (reason = "") => runAction("prepare", "restart_prep_prepare", { reason }),
    cancel: () => runAction("cancel", "restart_prep_cancel"),
    extend: () => runAction("extend", "restart_prep_extend"),
    dismiss,
    setHeld,
    openPanel() {
      const changed = !snap.panelOpen;
      if (changed) set({ panelOpen: true });
      return { changed };
    },
    closePanel() {
      if (snap.panelOpen) set({ panelOpen: false });
    },
  };
}

/** The webview's one store. Loads `listen` lazily so a node test never touches it. */
export const restartPrepStore: RestartPrepStore = createRestartPrepStore({
  invoke: (cmd, args) => tauriInvoke(cmd, args),
  onDismissed: (prepareId) => restoreCohortStore.endCohort(prepareId),
  listen: async (event, onPayload) => {
    const { listen } = await import("@tauri-apps/api/event");
    return listen(event, (e) => onPayload(e.payload));
  },
});
