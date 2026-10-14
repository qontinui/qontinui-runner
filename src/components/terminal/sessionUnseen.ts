/**
 * The "unseen activity" dot — Chrome's title-changed dot on a pinned tab,
 * applied to sessions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 7).
 *
 * A session is UNSEEN when, after the operator last focused it, its activity
 * word changed or it produced output. "Last focused" is the organization
 * store's `lastSeenAt` (Phase 2), so it survives a reload and is shared by
 * every window; it is cleared by focusing the session (`markSeen`).
 *
 * Two inputs the store cannot hold are observed here, in-process:
 *
 *  - when the activity word last CHANGED (`activityChangedAt`) — the status
 *    model states what the activity is, not when it became that;
 *  - when this window first observed the session (`firstObservedAt`) — the
 *    baseline for a session the operator has never focused. Without it every
 *    never-focused row would carry a dot on first launch, and a dot on every
 *    row says nothing. A never-focused session is unseen once it does
 *    something after it was first listed.
 *
 * A transition into or out of the `unknown` activity is not a change of the
 * session — it is a change in what the runner could observe — so it neither
 * sets nor moves `activityChangedAt`.
 *
 * Every verdict is three-valued: an unreadable store is `unknown`, never
 * "seen" (served policy `verification-and-evidence`
 * `unknown-must-not-render-as-a-default`).
 *
 * Pure: no React, no Tauri.
 */

import type { Fact } from "./sessionFacts";
import type { SessionActivity } from "./sessionStatusModel";

/** What this window has observed of one session's activity. */
export interface ActivityObservation {
  /** The last KNOWN activity word (an `unknown` reading never replaces it). */
  activity: SessionActivity | null;
  /** Epoch ms this window first listed the session. */
  firstObservedAt: number;
  /** Epoch ms the known activity last changed; `null` = not since observed. */
  activityChangedAt: number | null;
}

export interface ActivityRow {
  /** The organization-store key (`sessionKeyFor`). */
  key: string;
  activity: SessionActivity;
}

/**
 * Fold one listing into the observations. Returns `prev` itself when nothing
 * changed, so a caller holding it in React state does not re-render. A key
 * absent from `rows` is dropped — the map is bounded by the live list.
 */
export function observeActivity(
  prev: ReadonlyMap<string, ActivityObservation>,
  rows: readonly ActivityRow[],
  now: number,
): ReadonlyMap<string, ActivityObservation> {
  let changed = rows.length !== prev.size;
  const next = new Map<string, ActivityObservation>();
  for (const row of rows) {
    const before = prev.get(row.key);
    if (!before) {
      changed = true;
      next.set(row.key, {
        activity: row.activity === "unknown" ? null : row.activity,
        firstObservedAt: now,
        activityChangedAt: null,
      });
      continue;
    }
    if (row.activity === "unknown" || row.activity === before.activity) {
      next.set(row.key, before);
      continue;
    }
    changed = true;
    next.set(row.key, {
      activity: row.activity,
      firstObservedAt: before.firstObservedAt,
      // The first KNOWN reading after an all-unknown start is a first
      // observation, not a change.
      activityChangedAt: before.activity === null ? before.activityChangedAt : now,
    });
  }
  return changed ? next : prev;
}

export type UnseenVerdict =
  | { state: "unseen"; /** Epoch ms of the newest unseen activity. */ since: number }
  | { state: "seen" }
  | { state: "unknown"; reason: string };

export interface UnseenInputs {
  /** The store's last focus; `null` = never focused. */
  lastSeenAt: Fact<number | null>;
  /** Epoch ms of the last output / transcript record; `null` = none known. */
  lastActivityAt: number | null;
  observation: ActivityObservation | undefined;
  /** The session is the one the operator is looking at right now. */
  focused: boolean;
}

/** Whether a session carries the unseen dot. */
export function unseenVerdict(inputs: UnseenInputs): UnseenVerdict {
  if (inputs.focused) return { state: "seen" };
  if (!inputs.lastSeenAt.known) return { state: "unknown", reason: inputs.lastSeenAt.reason };
  const baseline = inputs.lastSeenAt.value ?? inputs.observation?.firstObservedAt ?? null;
  if (baseline === null) return { state: "unknown", reason: "not observed yet" };
  const latest = Math.max(
    inputs.lastActivityAt ?? -Infinity,
    inputs.observation?.activityChangedAt ?? -Infinity,
  );
  return latest > baseline ? { state: "unseen", since: latest } : { state: "seen" };
}

/** The `data-session-unseen` attribute value for a verdict. */
export function unseenAttr(verdict: UnseenVerdict): "true" | "false" | "unknown" {
  return verdict.state === "unseen" ? "true" : verdict.state === "seen" ? "false" : "unknown";
}

/** The row fields {@link focusedSessionKey} reads — `UnifiedSession` satisfies it. */
export interface FocusableRow {
  sessionId: string;
  source: "transcript" | "tab";
  /** The live tab backing a transcript row in this window, if any. */
  zoneTabId: string | null;
}

/**
 * The store key of the session the operator is looking at: the row whose
 * live tab is the focused tab (a transcript row through its `zoneTabId`, a
 * tab-sourced row through its own id). `null` = no tab focused, or the
 * focused tab backs no listed row (a plain shell the list does not carry).
 */
export function focusedSessionKey<R extends FocusableRow>(
  rows: readonly R[],
  focusedTabId: string | null,
  keyOf: (row: R) => string,
): string | null {
  if (focusedTabId === null) return null;
  const row = rows.find(
    (r) => r.zoneTabId === focusedTabId || (r.source === "tab" && r.sessionId === focusedTabId),
  );
  return row ? keyOf(row) : null;
}
