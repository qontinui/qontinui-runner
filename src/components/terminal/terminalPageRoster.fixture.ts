/**
 * The terminal-page roster measured on merytshost on 2026-11-05 (coord finding
 * `8c9e1419`, re-read for plan
 * `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`
 * §Why). Shared by the Phase 5 naming tests and the Phase 6 prune/repair tests,
 * so it lives in its own module rather than in one test file.
 *
 * MEASURED (carried literally):
 * - 40 persisted pages; the backend reports 22 occupied pages.
 * - Two pages named "Terminal": `default`, and `3e86a8ae` (empty, created
 *   2026-10-06T23:39Z by the no-visible-page fallback).
 * - "Page 20", "Page 21", "Page 22" twice each; "Page 23" three times.
 * - The operator renamed six pages: "2", "resumes", "projects", "tmux",
 *   "agentic", "clone factory".
 * - "Page 19" to "Page 27" were synthesized in one second,
 *   2026-11-04T20:51:51Z, by reconcile for the page ids the 20:51Z restart
 *   restored. They and the pages after them are populated.
 *
 * RECONSTRUCTED (the finding does not list them; chosen to be consistent with
 * the measured facts and the old count rule `Page ${nonDefaultCount + 1}`):
 * - The 18 pre-restart non-default pages are `3e86a8ae`, the six operator
 *   pages, the October "Page 20/21/22/23/23", and six more October "Page N"
 *   with N < 19. 18 non-default pages is exactly what makes the old rule name
 *   the 20:51Z batch "Page 19" onward.
 * - After that batch (27 non-default) the old rule synthesized "Page 28" to
 *   "Page 39", reaching the measured 40.
 * - Occupancy: `default`, the 20:51Z batch and "Page 28" to "Page 39" — 22,
 *   matching the backend count. That leaves 18 empty pages where §Why says 19
 *   October-era empty pages; §Why's own totals (40 persisted, 22 occupied)
 *   cannot both hold with 19, so the fixture keeps the two totals.
 * - Ids other than `default` and `3e86a8ae` are invented; createdAt values are
 *   ordered as the history above (October < the 20:51Z batch < later).
 */

import type { TerminalPageConfig } from "./useTerminalPages";

const OCT = Date.UTC(2026, 9, 1); // 2026-10-01
const OCT_FALLBACK = Date.UTC(2026, 9, 6, 23, 39); // 3e86a8ae
const BATCH_AT = Date.UTC(2026, 10, 4, 20, 51, 51); // the 20:51:51Z reconcile

/** The six operator-authored page names, measured. */
export const OPERATOR_PAGE_NAMES = [
  "2",
  "resumes",
  "projects",
  "tmux",
  "agentic",
  "clone factory",
] as const;

/** The persisted roster BEFORE the 2026-11-04 20:51Z restart (default + 18). */
export const ROSTER_BEFORE_RESTART: readonly TerminalPageConfig[] = [
  { id: "default", name: "Terminal", createdAt: 0 },
  { id: "oct-page-2", name: "Page 2", createdAt: OCT + 1 },
  { id: "oct-page-3", name: "Page 3", createdAt: OCT + 2 },
  ...OPERATOR_PAGE_NAMES.map((name, i) => ({
    id: `operator-${i + 1}`,
    name,
    createdAt: OCT + 10 + i,
  })),
  { id: "oct-page-9", name: "Page 9", createdAt: OCT + 20 },
  { id: "oct-page-11", name: "Page 11", createdAt: OCT + 21 },
  { id: "3e86a8ae", name: "Terminal", createdAt: OCT_FALLBACK },
  { id: "oct-page-14", name: "Page 14", createdAt: OCT_FALLBACK + 1 },
  { id: "oct-page-17", name: "Page 17", createdAt: OCT_FALLBACK + 2 },
  { id: "oct-page-20", name: "Page 20", createdAt: OCT_FALLBACK + 3 },
  { id: "oct-page-21", name: "Page 21", createdAt: OCT_FALLBACK + 4 },
  { id: "oct-page-22", name: "Page 22", createdAt: OCT_FALLBACK + 5 },
  { id: "oct-page-23a", name: "Page 23", createdAt: OCT_FALLBACK + 6 },
  { id: "oct-page-23b", name: "Page 23", createdAt: OCT_FALLBACK + 7 },
];

/** The nine page ids the 20:51Z restart restored rows onto (one reconcile pass). */
export const RESTART_BATCH_PAGE_IDS: readonly string[] = Array.from(
  { length: 9 },
  (_, i) => `restored-${i + 1}`,
);

/** The twelve page ids synthesized after the batch ("Page 28" … "Page 39"). */
export const LATER_PAGE_IDS: readonly string[] = Array.from(
  { length: 12 },
  (_, i) => `later-${i + 1}`,
);

/** The full 40-page roster as persisted on 2026-11-05, in tab order. */
export const ROSTER_2026_11_05: readonly TerminalPageConfig[] = [
  ...ROSTER_BEFORE_RESTART,
  ...RESTART_BATCH_PAGE_IDS.map((id, i) => ({
    id,
    name: `Page ${19 + i}`,
    createdAt: BATCH_AT,
  })),
  ...LATER_PAGE_IDS.map((id, i) => ({
    id,
    name: `Page ${28 + i}`,
    createdAt: BATCH_AT + 60_000 * (i + 1),
  })),
];

/** The 22 page ids the backend reported occupied on 2026-11-05. */
export const OCCUPIED_2026_11_05: ReadonlySet<string> = new Set([
  "default",
  ...RESTART_BATCH_PAGE_IDS,
  ...LATER_PAGE_IDS,
]);
