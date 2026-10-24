/**
 * `useSessionUnseen` — the unseen-activity dot's React half (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 7). The decision is `sessionUnseen.ts`; this hook only:
 *
 *  - folds each listing into the in-process activity observations;
 *  - writes `markSeen` to the organization store when focus ARRIVES at a
 *    session and when it LEAVES one — the second write is what makes output
 *    produced while the operator was watching count as seen;
 *  - answers `verdictOf(key)` for the card, the rail and `is:unseen`.
 */

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { known, unknownFact, type Fact } from "./sessionFacts";
import type { SessionOrgOp } from "./sessionOrgTypes";
import {
  observeActivity,
  unseenVerdict,
  type ActivityObservation,
  type ActivityRow,
  type UnseenVerdict,
} from "./sessionUnseen";
import type { SessionOrgView } from "./useSessionOrg";

export interface UnseenRow extends ActivityRow {
  /** Epoch ms of the last output / transcript record; `null` = none known. */
  lastActivityAt: number | null;
}

export interface UseSessionUnseenParams {
  rows: readonly UnseenRow[];
  org: SessionOrgView;
  apply: (ops: SessionOrgOp[]) => unknown;
  /** The store key of the session the operator is looking at; `null` = none. */
  focusedKey: string | null;
}

export interface SessionUnseenState {
  verdictOf: (key: string) => UnseenVerdict;
  /** Record a focus now — the card's open click, for a row with no tab here. */
  markSeen: (key: string) => void;
}

/** The store's `lastSeenAt` for one key, as a fact. */
export function lastSeenFact(org: SessionOrgView, key: string): Fact<number | null> {
  if (org.phase === "loading") return unknownFact("organization store not read yet");
  if (org.phase === "unknown") return unknownFact(`organization store unknown: ${org.reason}`);
  return known(org.org.members[key]?.lastSeenAt ?? null);
}

/** A per-panel store of {@link ActivityObservation}s, fed by each listing. */
export function createActivityObserver() {
  let observations: ReadonlyMap<string, ActivityObservation> = new Map();
  const listeners = new Set<() => void>();
  return {
    get: () => observations,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    observe: (rows: readonly ActivityRow[], now: number) => {
      const next = observeActivity(observations, rows, now);
      if (next === observations) return;
      observations = next;
      for (const l of listeners) l();
    },
  };
}

export function useSessionUnseen({
  rows,
  org,
  apply,
  focusedKey,
}: UseSessionUnseenParams): SessionUnseenState {
  // The observations live in a small external store the effect feeds — the
  // stamps need the wall clock, which a render may not read.
  const [observer] = useState(createActivityObserver);
  useEffect(() => observer.observe(rows, Date.now()), [observer, rows]);
  const observations = useSyncExternalStore(observer.subscribe, observer.get, observer.get);

  const orgKnown = org.phase === "known";
  const markSeen = useCallback(
    (key: string) => {
      // A write into an org the runner could not read would be refused; the
      // dot then stays `unknown`, which is what the store can support.
      if (orgKnown) void apply([{ op: "markSeen", sessionKey: key, at: Date.now() }]);
    },
    [apply, orgKnown],
  );

  const lastFocusedRef = useRef<string | null>(null);
  useEffect(() => {
    const left = lastFocusedRef.current;
    if (left === focusedKey) return;
    lastFocusedRef.current = focusedKey;
    if (left !== null) markSeen(left);
    if (focusedKey !== null) markSeen(focusedKey);
  }, [focusedKey, markSeen]);

  const lastActivityByKey = useMemo(
    () => new Map(rows.map((r) => [r.key, r.lastActivityAt])),
    [rows],
  );

  const verdictOf = useCallback(
    (key: string): UnseenVerdict =>
      unseenVerdict({
        lastSeenAt: lastSeenFact(org, key),
        lastActivityAt: lastActivityByKey.get(key) ?? null,
        observation: observations.get(key),
        focused: key === focusedKey,
      }),
    [org, lastActivityByKey, observations, focusedKey],
  );

  return { verdictOf, markSeen };
}
