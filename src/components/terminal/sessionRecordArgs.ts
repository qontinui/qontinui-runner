/**
 * Pure builders for the durable terminal-session registry `invoke` payloads.
 *
 * Kept free of React / Tauri imports so the resolution logic (which zone does a
 * tab belong to, which working dir / title to record) can be unit-tested in the
 * runner's `node` vitest environment without booting the component tree.
 *
 * The frontend records OPEN/CLOSE through three Tauri commands:
 *   - `terminal_session_record_open`  (args = {@link SessionOpenArgs})
 *   - `terminal_session_record_close` (args = {@link SessionCloseArgs})
 *   - `terminal_session_list_open`    (restore — see useTerminalInitialization)
 */

/** A tab shape the open-record builder needs (subset of `TerminalTab`). */
export interface OpenRecordTab {
  id: string;
  title?: string;
  workingDir?: string;
}

/**
 * How a tab learned its `claudeSessionId` — the evidence grade the durable
 * record carries. THREE values, matching the Rust store's `ORIGIN_*` constants
 * one-for-one:
 *
 *  - `"authoritative"` — the runner KNOWS the id exactly (`--session-id` /
 *    `--resume` / a provider hook self-report).
 *  - `"observed"` — the runner SAW the id (a live-registry / process read of
 *    a session that is genuinely running), but did not itself name it. Weaker
 *    than authoritative, stronger than a transcript guess. This value is live
 *    in the Rust store and the restore classifier already branches on it; it
 *    was simply unspellable from TypeScript until now, so no frontend writer
 *    could record an honest observation and had to over- or under-claim.
 *  - `"reconciled"` — recovered by a freshest-transcript / process-anchored
 *    backstop, and may be foreign.
 *
 * Omitted = unknown; the backend then preserves any existing origin.
 *
 * (Migrated from the previous `pinned`/`guessed` vocabulary in the
 * session-restore-redesign Phase 1.)
 */
export type SessionOrigin = "authoritative" | "observed" | "reconciled";

/**
 * Args for the `terminal_session_record_open` Tauri command.
 *
 * A `type` alias, not an `interface`, and that is load-bearing: Tauri's
 * `invoke` takes `InvokeArgs = Record<string, unknown>`, and an `interface`
 * has no implicit index signature, so a named interface cannot be passed
 * straight to `invoke` while an object type alias can. Naming the payload is
 * pointless if the name forces every call site back to an untyped literal.
 */
export type SessionOpenArgs = {
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
};

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

/**
 * Durable-close reasons a FRONTEND caller may record. Mirrors
 * `useTerminalManager`'s `FrontendCloseReason` — both arms have a live tab, so
 * a frontend writer can only ever be recording one of these two. The backend
 * additionally mints reasons no frontend can (`poll-dead`, `never-started`,
 * `no-terminal`, `migrated`, `superseded-terminal-reuse`); they are
 * deliberately NOT in this union.
 */
export type FrontendSessionCloseReason = "explicit" | "pty-exit";

/**
 * Args for the `terminal_session_record_close` Tauri command.
 *
 * BOTH halves of the key, and that is the contract, not redundancy. The
 * `claudeSessionId` alone is not a safe close target: it is a real, correctly
 * minted id, but nothing guarantees it keys the record for *this* terminal —
 * a provisional spawn-seam id, a restored id whose pty was respawned under a
 * fresh `--session-id`, or a `reconciled` freshest-mtime bind that "may be
 * foreign" all look identical here. The backend cross-checks the pair
 * (`commands::terminal::terminal_session_record_close` →
 * `SessionLifecycleStore::record_close_checked`) and closes the record the
 * terminal actually owns, reporting a typed `CloseOutcome` rather than
 * silently closing the wrong row.
 */
export type SessionCloseArgs = {
  /** The DURABLE registry key — one per provider session, survives restore. */
  claudeSessionId: string;
  /** The EPHEMERAL PTY id — one per terminal, minted fresh on every respawn. */
  terminalId: string;
  /** Why it closed. */
  reason: FrontendSessionCloseReason;
};

/**
 * Build the `terminal_session_record_close` payload for a tab that is closing.
 *
 * The typed way to construct the close args, sibling of
 * {@link buildSessionOpenArgs}. `useTerminalManager`'s
 * `buildSessionCloseRecord` resolves the pair off the live tab list (and
 * returns `null` for a plain shell with nothing to record); this takes the
 * already-resolved pair and gives the wire payload a name and a type, so a
 * hand-built object cannot drift from the command's signature.
 *
 * Does NOT change the wire shape — the three keys, spelled exactly as the
 * command reads them.
 */
export function buildSessionCloseArgs(params: {
  claudeSessionId: string;
  terminalId: string;
  reason?: FrontendSessionCloseReason;
}): SessionCloseArgs {
  const { claudeSessionId, terminalId, reason = "explicit" } = params;
  return { claudeSessionId, terminalId, reason };
}

/**
 * The payload `terminal_session_record_open` answers with — the Rust
 * `record_open_confirmation_report`.
 *
 * The command writes a PROVISIONAL row (`confirmed_at` unset) and
 * `terminal_list` deliberately refuses to surface provisional rows, so
 * "recorded" and "bound" are different facts. The backend now reports both;
 * this is the frontend half that reads them.
 */
export interface RecordOpenReport {
  /**
   * The row is IN THE STORE, read back after the write — not an assertion that
   * the command was entered. `record_open` returns early without writing when
   * the map lock is poisoned, and the read-back sees the same poison, so a
   * resolved call CAN report `false`. That is the one outcome for which
   * confirming is not the remedy.
   */
  recorded: boolean;
  /**
   * `confirmed_at.is_some()` READ BACK FROM THE STORE — whether
   * `terminal_list`'s `sessionIdsByTerminal` map will carry this session. A
   * re-record of an already-confirmed session reports `true`.
   */
  confirmed: boolean;
  /** The door that flips a provisional row — `POST /control/session-open`. */
  confirmBy: string;
}

/**
 * Narrow a `terminal_session_record_open` response to its confirmation report,
 * or `null` when there isn't one.
 *
 * `null` is a real answer, not a failure: a runner built before the report
 * existed resolves with `data: null`, and the honest reading of that is
 * "this build does not say", never "not confirmed". Everything downstream
 * therefore has to distinguish the three cases rather than collapsing the
 * absent one into `confirmed: false`.
 */
export function readRecordOpenReport(response: unknown): RecordOpenReport | null {
  if (typeof response !== "object" || response === null) return null;
  const data = (response as { data?: unknown }).data;
  if (typeof data !== "object" || data === null) return null;
  const { recorded, confirmed, confirmBy } = data as Record<string, unknown>;
  if (typeof recorded !== "boolean" || typeof confirmed !== "boolean") return null;
  return {
    recorded,
    confirmed,
    confirmBy: typeof confirmBy === "string" ? confirmBy : "",
  };
}

/**
 * The one line worth logging about a `terminal_session_record_open` that
 * resolved.
 *
 * Written is not bound, and until this existed nothing anywhere read the
 * difference: every frontend writer discarded the resolved value and kept only
 * a `.catch`, so a session that recorded fine and never confirmed looked
 * exactly like one that bound — which is what cost a manual test run most of
 * its wall clock. The line goes to `console.debug`, which the SDK's
 * `ConsoleCapture` buffer keeps, so it is reachable by a UI-Bridge driver and
 * not only by a human with DevTools open.
 *
 * Deliberately NOT wired into the two RE-ASSERT writers (the zone-re-resolution
 * backstop, and the post-handshake re-record): they refresh a row that was
 * already reported when it was first written, and the backstop fires on every
 * layout change — the same line, repeated, saying nothing new.
 */
export function describeRecordOpenOutcome(params: {
  claudeSessionId: string;
  terminalId: string;
  response: unknown;
}): string {
  const { claudeSessionId, terminalId, response } = params;
  const report = readRecordOpenReport(response);
  if (!report) {
    return (
      `session ${claudeSessionId} recorded on ${terminalId}; ` +
      `this runner build returned no confirmation report, so bound-ness is UNKNOWN`
    );
  }
  const door = report.confirmBy || "POST /control/session-open";
  if (!report.recorded) {
    // Distinct from PROVISIONAL, and the distinction is the point: a
    // provisional row exists and is waiting for a door; this one is not there
    // at all, so pointing the reader at ${door} would be advice that cannot
    // work. The command resolves rather than rejects in this case (the store
    // write is infallible by signature), which is exactly why the payload has
    // to carry it.
    return (
      `session ${claudeSessionId} NOT recorded on ${terminalId} — the write did not ` +
      `land in the lifecycle store, so confirming it will not help; terminal_list ` +
      `cannot surface a row that is not there`
    );
  }
  if (report.confirmed) {
    return `session ${claudeSessionId} recorded and BOUND on ${terminalId} — terminal_list will surface it`;
  }
  return (
    `session ${claudeSessionId} recorded but PROVISIONAL on ${terminalId} — ` +
    `terminal_list will not surface it until ${door} confirms it`
  );
}
