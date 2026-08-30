/**
 * Pure builders for the durable terminal-session registry `invoke` payloads.
 *
 * Kept free of React / Tauri imports so the resolution logic (which zone does a
 * tab belong to, which working dir / title to record) can be unit-tested in the
 * runner's `node` vitest environment without booting the component tree.
 *
 * The frontend records OPEN/CLOSE through three Tauri commands:
 *   - `terminal_session_record_open`  (args = {@link SessionOpenArgs})
 *   - `terminal_session_record_close` (args = { claudeSessionId, terminalId, reason })
 *   - `terminal_session_list_open`    (restore — see useTerminalInitialization)
 */

/** A tab shape the open-record builder needs (subset of `TerminalTab`). */
export interface OpenRecordTab {
  id: string;
  title?: string;
  workingDir?: string;
}

/**
 * How a tab learned its `claudeSessionId`: `"authoritative"` (the runner KNOWS
 * the id exactly — `--session-id`/`--resume`/a provider hook) vs `"reconciled"`
 * (recovered by a freshest-transcript/process-anchored backstop, may be
 * foreign). Omitted = unknown; the backend then preserves any existing origin.
 *
 * (Migrated from the previous `pinned`/`guessed` vocabulary in the
 * session-restore-redesign Phase 1.)
 */
export type SessionOrigin = "authoritative" | "reconciled";

/** Args for the `terminal_session_record_open` Tauri command. */
export interface SessionOpenArgs {
  claudeSessionId: string;
  configDir?: string;
  workingDir?: string;
  pageId: string;
  zoneIndex: number;
  title?: string;
  terminalId: string;
  origin?: SessionOrigin;
  /** Which provider owns the session. Defaults to `"claude"` backend-side. */
  provider?: string;
}

/**
 * The `zoneIndex` sentinel for "this tab is in no zone".
 *
 * A LEGITIMATE steady state, not an error: the preset layouts cap at 9 zones
 * (`full-grid`) and auto-grow stops there, so every live tab past the ceiling
 * is genuinely unassigned (surfaced by `UnzonedChip` / the zone control
 * panel's Unassigned list). The recorded zone must be allowed to BE -1 — it is
 * never clamped to 0, because zone 0 belongs to whichever tab actually holds
 * it.
 */
export const UNZONED_INDEX = -1;

/**
 * Resolve the zone a tab is assigned to by reverse-lookup over the live
 * `zoneIndex → tabId` assignments. Returns {@link UNZONED_INDEX}
 * (unassigned/hidden) when the tab is in no zone — the same sentinel the
 * backend record uses.
 */
export function resolveZoneIndex(assignments: Record<number, string>, tabId: string): number {
  return Number(Object.entries(assignments).find(([, id]) => id === tabId)?.[0] ?? UNZONED_INDEX);
}

/** One tab's CURRENTLY resolved placement, as the zone backstop observes it. */
export interface ZoneObservation {
  claudeSessionId: string;
  tabId: string;
  /** Zone the tab resolves to right now ({@link UNZONED_INDEX} = no zone). */
  zoneIndex: number;
}

/**
 * Ledger of "what `zoneIndex` does the DURABLE record currently hold for this
 * session" — keyed by `claudeSessionId`, owned by the zone backstop effect.
 *
 * The distinction that matters is ledger-of-WRITES vs ledger-of-OBSERVATIONS.
 * It used to be the latter: the backstop seeded each session from the first
 * zone it happened to SEE and only re-emitted on a subsequent change. But the
 * OPEN record is written the instant a tab binds its `claudeSessionId`, which
 * is typically BEFORE `reconcileAssignments` has auto-filled that tab into a
 * zone — so the record was written with `-1` while the backstop, debounced
 * until the layout settled, first observed the tab already sitting in its real
 * zone. Seeing no *change*, it emitted nothing, and the `-1` stood forever.
 * That is the self-perpetuating `zoneIndex: -1` this ledger fixes: seed from
 * what was WRITTEN ({@link noteRecordedZone}) and the discrepancy is visible
 * on the very first observation.
 */
export type RecordedZoneLedger = Map<string, number>;

/**
 * One ledger per `pageId`, module-scoped.
 *
 * The frontend has FOUR writers of `terminal_session_record_open` and they do
 * not share a React owner: the id-bind recorder and the backstop live in
 * `TerminalPage`, the transcript-panel resume lives in `useShellIntegration`
 * (mounted from `TerminalSessionContext`), and the profile resume lives in
 * `TerminalSessionContext` itself. Prop-drilling one ref through all of them
 * would be strictly worse than a keyed registry, and the alternative — every
 * writer silently disagreeing with the ledger — is the bug being fixed:
 * `useShellIntegration`'s resume path writes `zoneIndex: -1` with a comment
 * saying "the zone-move backstop refreshes it", which was not true.
 *
 * Keyed by page so `planZoneReemits`' prune (which drops sessions absent from
 * the observation set) can never delete another page's entries.
 */
const LEDGERS = new Map<string, RecordedZoneLedger>();

/** The recorded-zone ledger for `pageId`, created on first use. */
export function recordedZoneLedgerFor(pageId: string): RecordedZoneLedger {
  let ledger = LEDGERS.get(pageId);
  if (!ledger) {
    ledger = new Map();
    LEDGERS.set(pageId, ledger);
  }
  return ledger;
}

/** Drop every ledger. Test-only — keeps cases from leaking into each other. */
export function resetRecordedZoneLedgers(): void {
  LEDGERS.clear();
}

/**
 * Record that the durable OPEN record for `claudeSessionId` now carries
 * `zoneIndex`. Called by every frontend writer of `terminal_session_record_open`
 * on this page, with the zone it actually sent.
 */
export function noteRecordedZone(
  ledger: RecordedZoneLedger,
  claudeSessionId: string,
  zoneIndex: number,
): void {
  ledger.set(claudeSessionId, zoneIndex);
}

/**
 * Decide which observed placements disagree with the durable record and must
 * be re-emitted, updating `ledger` to match and pruning sessions that no
 * longer exist (so a reused `claudeSessionId` re-seeds cleanly).
 *
 * Three cases, in order:
 *
 *  1. **Known recorded zone, unchanged** → nothing. A re-render that moved
 *     nothing emits nothing.
 *  2. **Known recorded zone, different** → re-emit. Covers both directions:
 *     an operator drag into a zone, AND a drag that leaves the tab unassigned
 *     (`-1` is a real recorded value, so the record follows it down too).
 *  3. **No recorded zone for this session** → seed silently, no emit. This is
 *     a record THIS page never wrote — i.e. one the boot-restore path owns.
 *     Re-asserting `terminal_session_record_open` for it would refresh
 *     `last_seen_at` on rows the restore deliberately left alone, which is
 *     precisely how ghost records were made immortal (see the
 *     `terminal_session_rebind_terminal` doc comment). Restore-owned records
 *     therefore keep their recorded placement until a live writer moves them.
 *
 * Pure apart from the two explicit mutations of `ledger`, so the whole
 * re-resolution contract is unit-testable without React or Tauri.
 */
export function planZoneReemits(
  ledger: RecordedZoneLedger,
  observed: readonly ZoneObservation[],
): ZoneObservation[] {
  const emits: ZoneObservation[] = [];
  const seen = new Set<string>();

  for (const obs of observed) {
    seen.add(obs.claudeSessionId);
    const recorded = ledger.get(obs.claudeSessionId);
    if (recorded === undefined) {
      ledger.set(obs.claudeSessionId, obs.zoneIndex);
      continue;
    }
    if (recorded === obs.zoneIndex) continue;
    ledger.set(obs.claudeSessionId, obs.zoneIndex);
    emits.push(obs);
  }

  for (const sid of [...ledger.keys()]) {
    if (!seen.has(sid)) ledger.delete(sid);
  }

  return emits;
}

/**
 * Build the `terminal_session_record_open` payload for a tab that just bound a
 * `claudeSessionId`. Resolves the tab's current zone and pulls workingDir/title
 * from the live tab list.
 */
export function buildSessionOpenArgs(params: {
  assignments: Record<number, string>;
  tabs: OpenRecordTab[];
  tabId: string;
  claudeSessionId: string;
  configDir: string | undefined;
  pageId: string;
  origin?: SessionOrigin;
}): SessionOpenArgs {
  const { assignments, tabs, tabId, claudeSessionId, configDir, pageId, origin } = params;
  const tab = tabs.find((t) => t.id === tabId);
  return {
    claudeSessionId,
    configDir,
    workingDir: tab?.workingDir,
    pageId,
    zoneIndex: resolveZoneIndex(assignments, tabId),
    title: tab?.title,
    terminalId: tabId,
    ...(origin ? { origin } : {}),
  };
}
