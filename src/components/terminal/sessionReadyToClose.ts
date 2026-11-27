/**
 * "Ready to close" — Chrome's Declutter for sessions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5).
 *
 * A row is ready when `is:closable` (`evaluateClosable`, `sessionQuery.ts` —
 * served policy `finished-criteria` with the plan's vet correction) is TRUE
 * and it has a tab here to close. A row whose verdict is UNKNOWN is never
 * offered — it is counted, so the banner can say how many could not be
 * evaluated rather than reading as "nothing else is finished" (served policy
 * `verification-and-evidence` `unknown-must-not-render-as-a-default`).
 *
 * Each ready row carries the reason its verdict is true, worded for the
 * confirmation list (`Finished · PRs green`, `Exited 0`).
 *
 * Pure: no React, no Tauri.
 */

import type { PrFact, SessionFacts } from "./sessionFacts";
import { evaluateClosable } from "./sessionQuery";

export interface ReadyToCloseRow<R> {
  row: R;
  reason: string;
}

export interface ReadyToClose<R> {
  ready: ReadyToCloseRow<R>[];
  /** Rows with a tab here whose closability could not be evaluated. */
  unknownCount: number;
}

const prsDone = (prs: readonly PrFact[]): boolean =>
  prs.every((p) => p.ci === "green" || p.ci === "landed" || p.ci === "closed");

/** Why a closable session is closable, for the confirmation list. */
export function closableReason(f: SessionFacts): string {
  const prs = f.prs.known && f.prs.value.length > 0 && prsDone(f.prs.value) ? " · PRs green" : "";
  if (f.finished.known && f.finished.value) return `Finished${prs}`;
  return `Exited ${f.exitCode ?? 0}${prs}`;
}

/**
 * The ready rows, in `rows` order. `factsOf` answers a row's facts (absent =
 * not hydrated yet, counted unknown); `hasTab` says whether there is a tab
 * here to close.
 */
export function readyToClose<R>(
  rows: readonly R[],
  factsOf: (row: R) => SessionFacts | undefined,
  hasTab: (row: R) => boolean,
): ReadyToClose<R> {
  const ready: ReadyToCloseRow<R>[] = [];
  let unknownCount = 0;
  for (const row of rows) {
    if (!hasTab(row)) continue;
    const f = factsOf(row);
    if (!f) {
      unknownCount += 1;
      continue;
    }
    const verdict = evaluateClosable(f);
    if (verdict.value === true) ready.push({ row, reason: closableReason(f) });
    else if (verdict.value === "unknown") unknownCount += 1;
  }
  return { ready, unknownCount };
}

/** The banner's line: `3 sessions are finished`, plus the unknown count when there is one. */
export function readyToCloseBanner(ready: number, unknown: number): string | null {
  if (ready === 0) return null;
  const head = `${ready} session${ready === 1 ? " is" : "s are"} finished`;
  return unknown > 0 ? `${head} · ${unknown} could not be evaluated` : head;
}
