/**
 * Fan-out strip — counts, labels and visibility as pure rules (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
 * Phase 7). The `sessionCounts.ts` precedent: the strip imports ONE rule
 * instead of deriving counts inline, and the rule is testable without React.
 *
 * The governing invariant is the same one `sessionCounts.ts` states: a count a
 * control claims must be the count that control can reach. `N running` is the
 * run's ADMITTED members (the ones Release-slot acts on), `M queued` its
 * QUEUED ones, `K refused` its REFUSED ones — and Cancel-queued acts on both
 * of the latter, exactly as the runner's `POST /fanout/{id}/cancel` does.
 */

import {
  FANOUT_LEDGER_NOT_LOADED,
  type FanoutMemberView,
  type FanoutResult,
  type FanoutRunView,
} from "./fanoutApi";

// ---------------------------------------------------------------------------
// Read state
// ---------------------------------------------------------------------------

/** What the strip knows about the scheduler. */
export type FanoutReadState =
  /** No read has completed yet. */
  | { kind: "loading" }
  /**
   * The runner answered 503 `FANOUT_LEDGER_NOT_LOADED`: its boot settle has
   * not run the first ledger load yet. Its runs are not known YET — expected
   * for the first ~45 s after a start, and nothing has failed — so this is a
   * quiet state that never forces the strip visible on its own.
   */
  | { kind: "settling"; reason: string }
  /**
   * The last read failed: the scheduler's state is UNKNOWN, not empty. This
   * includes a runner whose ledger load FAILED (503
   * `FANOUT_LEDGER_LOAD_FAILED`, PostgreSQL unreadable) — `code` carries the
   * server's word when it sent one.
   */
  | { kind: "unknown"; error: string; status: number | null; code?: string }
  | { kind: "ok"; runs: FanoutRunView[] };

/** Fold a `GET /fanout` result into the read state. */
export function readStateFromResult(result: FanoutResult<FanoutRunView[]>): FanoutReadState {
  if (result.ok) return { kind: "ok", runs: result.data };
  if (result.code === FANOUT_LEDGER_NOT_LOADED) return { kind: "settling", reason: result.error };
  return {
    kind: "unknown",
    error: result.error,
    status: result.status,
    ...(result.code !== undefined ? { code: result.code } : {}),
  };
}

/**
 * Merge one changed run (a `fanout-changed` payload, or a mutation's answer)
 * into a known list: replace by id, or add a run the list has not seen yet at
 * the front (the route lists newest first). An UNKNOWN or loading state is
 * left alone — one run's update does not make the whole scheduler known.
 */
export function mergeRunUpdate(state: FanoutReadState, run: FanoutRunView): FanoutReadState {
  if (state.kind !== "ok") return state;
  const idx = state.runs.findIndex((r) => r.id === run.id);
  if (idx < 0) return { kind: "ok", runs: [run, ...state.runs] };
  const runs = state.runs.slice();
  runs[idx] = run;
  return { kind: "ok", runs };
}

/**
 * Merge one changed run and, when the state actually changed, mark the gate
 * — synchronously, at the moment the change is decided. Marking from inside
 * a React `setState` updater would run it late (deferred to render) and twice
 * (StrictMode double-invokes updaters), so a poll that started between the
 * event and the render could still overwrite the newer run.
 */
export function applyRunUpdateGated(
  prev: FanoutReadState,
  run: FanoutRunView,
  gate: FreshnessGate,
): FanoutReadState {
  const next = mergeRunUpdate(prev, run);
  if (next !== prev) gate.markApplied();
  return next;
}

/** Runs the strip shows: active ones only. A completed run has nothing to control. */
export function activeFanoutRuns(runs: readonly FanoutRunView[]): FanoutRunView[] {
  return runs.filter((r) => r.state === "active");
}

/**
 * Whether the fan-out state, on its own, makes the strip show. Not while the
 * first read is in flight, not during the runner's boot settle (`settling`),
 * and not with no active runs; ALWAYS when the read failed, so an unreadable
 * route is visible as UNKNOWN rather than as an absent strip.
 */
export function fanoutStripVisible(state: FanoutReadState): boolean {
  if (state.kind === "loading" || state.kind === "settling") return false;
  if (state.kind === "unknown") return true;
  return activeFanoutRuns(state.runs).length > 0;
}

/**
 * Orders the two sources of the run list — the poll and the per-run events
 * (and mutation answers) — so a slow poll cannot overwrite newer state.
 *
 * Every poll takes a stamp when it STARTS; every applied event or mutation
 * marks the state with a fresh stamp. A poll's answer is applied only when it
 * started after everything already applied — otherwise it was read before a
 * change the state already shows, and is dropped (the next poll is newer).
 */
export interface FreshnessGate {
  /** Stamp a poll as it starts. */
  begin(): number;
  /** Whether a poll that started at `stamp` may be applied now; marks it applied if so. */
  acceptPoll(stamp: number): boolean;
  /** Record that an event or mutation answer was applied. */
  markApplied(): void;
}

export function createFreshnessGate(): FreshnessGate {
  let clock = 0;
  let applied = 0;
  return {
    begin: () => ++clock,
    acceptPoll: (stamp) => {
      if (stamp <= applied) return false;
      applied = stamp;
      return true;
    },
    markApplied: () => {
      applied = ++clock;
    },
  };
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/** Stable wire reasons (`fanout/model.rs` `reason`) in operator words. */
const REASON_LABELS: Record<string, string> = {
  terminal_exit: "terminal exited",
  finished: "finished",
  operator_release: "released",
  runner_restarted: "runner restarted",
  runner_draining: "runner draining",
  fanout_bound_occupied: "fan-out bound full",
  store_unavailable: "ledger unavailable",
  cancelled: "cancelled",
  spawn_unconfirmed: "spawn unconfirmed (runner restarted)",
  stale_after_restart: "stale after restart",
};

const MAX_REASON_CHARS = 60;

function truncate(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/** A reason in operator words; a free-text spawn error is shown, truncated. */
export function reasonLabel(reason: string | null | undefined): string | null {
  if (!reason) return null;
  return REASON_LABELS[reason] ?? truncate(reason, MAX_REASON_CHARS);
}

/**
 * The strip text for an unreadable scheduler: UNKNOWN plus the error, kept to
 * one strip-width (the full error belongs in the tooltip).
 */
export function unknownStripText(error: string): string {
  return `fan-out UNKNOWN — ${truncate(error, MAX_REASON_CHARS)}`;
}

/**
 * The reasons carried by members in `state`, in operator words: one reason
 * verbatim, several as the first plus `+N more`. `null` when none carries one.
 */
export function summarizeReasons(
  members: readonly FanoutMemberView[],
  state: FanoutMemberView["state"],
): string | null {
  const distinct: string[] = [];
  for (const m of members) {
    if (m.state !== state) continue;
    const label = reasonLabel(m.reason);
    if (label && !distinct.includes(label)) distinct.push(label);
  }
  if (distinct.length === 0) return null;
  if (distinct.length === 1) return distinct[0];
  return `${distinct[0]} +${distinct.length - 1} more`;
}

/** The name a run is shown under: its template slug, else a short id. */
export function runName(run: FanoutRunView): string {
  return run.templateSlug && run.templateSlug.trim().length > 0
    ? run.templateSlug
    : run.id.slice(0, 8);
}

/**
 * `run <slug> — 2 running · 5 queued (runner draining) · 1 refused (low memory)`.
 *
 * Running and queued are always shown (a zero is information on an active
 * run); refused only when non-zero. A queued or refused reason is appended in
 * parentheses when the members carry one.
 */
export function fanoutRunSummary(run: FanoutRunView): string {
  const parts = [`${run.counts.admitted} running`];
  const queuedReason = summarizeReasons(run.members, "queued");
  parts.push(`${run.counts.queued} queued${queuedReason ? ` (${queuedReason})` : ""}`);
  if (run.counts.refused > 0) {
    const refusedReason = summarizeReasons(run.members, "refused");
    parts.push(`${run.counts.refused} refused${refusedReason ? ` (${refusedReason})` : ""}`);
  }
  return `run ${runName(run)} — ${parts.join(" · ")}`;
}

/**
 * The `#n` a member is shown under: its row number in the PREVIEW it was
 * created from, so unticked rows do not shift the numbers between the preview
 * and the strip. Falls back to the posted position for a run created without
 * preview indices (or by a runner build that does not echo them).
 */
export function memberNumber(m: Pick<FanoutMemberView, "index" | "previewIndex">): number {
  return (m.previewIndex ?? m.index) + 1;
}

/** `45s`, `12m`, `3h`, `2d` — the coarsest unit that is at least 1. */
function shortDuration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/**
 * When a refused member returns to the queue: `retry in 40s`, `retrying` once
 * its backoff has elapsed, `null` when it is not waiting on one.
 */
export function retryLabel(m: FanoutMemberView, nowMs: number): string | null {
  if (m.state !== "refused" || !m.nextRetryAt) return null;
  const at = Date.parse(m.nextRetryAt);
  if (Number.isNaN(at)) return null;
  const wait = at - nowMs;
  const n = m.refusals && m.refusals > 1 ? ` (refusal ${m.refusals})` : "";
  return wait > 0 ? `retry in ${shortDuration(wait)}${n}` : `retrying${n}`;
}

/** One member row's state text: the state, its reason in operator words, and any retry. */
export function memberStateLabel(m: FanoutMemberView, nowMs: number = Date.now()): string {
  const parts = [reasonLabel(m.reason), retryLabel(m, nowMs)].filter(
    (p): p is string => p !== null,
  );
  return parts.length > 0 ? `${m.state} (${parts.join(" · ")})` : m.state;
}

/** How long ago a run was created: `started 3h ago`. `null` for an unparseable stamp. */
export function runAgeLabel(run: Pick<FanoutRunView, "createdAt">, nowMs: number): string | null {
  const at = Date.parse(run.createdAt);
  if (Number.isNaN(at)) return null;
  return `started ${shortDuration(nowMs - at)} ago`;
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/** Release-slot applies only to an ADMITTED member (the route 409s otherwise). */
export function canReleaseMember(m: FanoutMemberView): boolean {
  return m.state === "admitted";
}

/** Cancel-queued cancels QUEUED and REFUSED members — what the route cancels. */
export function cancellableCount(run: FanoutRunView): number {
  return run.counts.queued + run.counts.refused;
}

/**
 * The note after Cancel-queued, counted from the server's answer (cancelled
 * after minus before) — not from the count shown when the button was clicked,
 * since a member admitted in between was not cancelled.
 */
export function cancelledNote(before: FanoutRunView, after: FanoutRunView): string {
  const n = Math.max(0, after.counts.cancelled - before.counts.cancelled);
  return `cancelled ${n} waiting member${n === 1 ? "" : "s"}`;
}

/**
 * The cap a ± click requests. Never below 1; the upper side is the server's
 * to clamp (the `parallel_fanout` bound), and the strip shows the clamp.
 */
export function nextCap(current: number, delta: number): number {
  return Math.max(1, current + delta);
}

/** The strip's one-line note after a PATCH: the clamp, when the server applied one. */
export function capClampNote(
  requested: number,
  outcome: { run: { maxConcurrent: number }; fanoutBound: number; clampedFrom: number | null },
): string | null {
  if (outcome.clampedFrom === null) return null;
  return (
    `asked for ${requested}, clamped to ${outcome.run.maxConcurrent} ` +
    `(fan-out bound ${outcome.fanoutBound})`
  );
}
