/**
 * mountedTerminalViews — a process-wide answer to one question the two halves of
 * the terminal bridge surface could not previously ask each other:
 *
 *     "Does terminal <id> have a LIVE xterm view right now?"
 *
 * ## Why this exists (manual-test-loop iter 24, item 1)
 *
 * `terminal-input-<id>` has two possible owners: the mounted
 * `TerminalInstance`'s real xterm helper textarea
 * (`./bridgeInputRegistration.ts`) and, when nothing is mounted, the hidden 1×1
 * proxy textarea `TerminalBridgeProxies` renders
 * (`./subordinateBridgeRegistration.ts`). The proxy is meant to be strictly
 * subordinate — it claims only while the id is unowned and yields the instant
 * anyone else holds it.
 *
 * That rule is expressed purely in terms of *who currently holds the registry
 * entry*, which is a proxy (in both senses) for the thing that actually
 * matters. Measured on the iteration-23 build: soft-navigate `/terminal` →
 * `/settings` → `/terminal`, and the remounted pane's element label still read
 * `Terminal input (…) [no mounted view — …]` indefinitely. The registry entry
 * was the hidden textarea; the pane was on screen, painted, with a live xterm
 * and a live PTY. Because ownership never came back, every subsequent defect in
 * this cluster — proxy `focus` stealing real focus (item 4), `paste` missing
 * (item 5), bracketed-paste hardcoded off (item 6) — was observable on a pane
 * that had a perfectly good mounted view sitting right there.
 *
 * The old rule cannot detect that state: from inside the proxy, "the id is
 * unowned" and "the id is unowned *and a live xterm exists*" look identical.
 * This module is the missing signal. The proxy consults it and yields on a live
 * view regardless of who holds the entry, and the mounted attachment re-claims
 * the entry — the two fixes are complementary, and either alone would leave the
 * other half of the invariant unproven.
 *
 * ## Liveness, not mounting
 *
 * The predicate is deliberately "a live INPUT ELEMENT exists", not "a component
 * is mounted". A `TerminalInstance` mounts ~200ms before its backend finishes
 * building, and during that window there is genuinely nothing that can serve a
 * write. If the proxy yielded on mere mounting, that window would answer
 * `ELEMENT_NOT_FOUND` — trading iteration 24's defect for iteration 17's. So a
 * view counts only once it can actually do the job.
 *
 * Module-level state rather than React context: the two components are in
 * different subtrees (`TerminalBridgeProxies` hangs off `PageSessionScope`,
 * which is deliberately outside `ZoneGrid`), and threading a context between
 * them would re-couple exactly what iteration 18 decoupled.
 */

/** terminalId → "is there a live xterm input element for this terminal?" */
const liveViews = new Map<string, () => boolean>();

/**
 * Announce that a `TerminalInstance` for `terminalId` is mounted, and how to
 * ask whether its input element is live yet.
 *
 * @returns an INSTANCE-KEYED release. It deletes the entry only if this exact
 * probe is still the registered one — the same rule
 * `bridgeInputRegistration.ts` and `subordinateBridgeRegistration.ts` follow,
 * for the same reason: a pane moving between zones briefly has two instances,
 * and a stale unmount must never erase the live one's record.
 */
export function registerMountedTerminalView(
  terminalId: string,
  hasLiveInput: () => boolean,
): () => void {
  liveViews.set(terminalId, hasLiveInput);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    if (liveViews.get(terminalId) === hasLiveInput) liveViews.delete(terminalId);
  };
}

/**
 * Is there a mounted view with a live input element for `terminalId`?
 *
 * Fails CLOSED (returns `false`) on a missing or throwing probe: an unknown
 * answer must leave the proxy serving the pane, because a proxy-served pane
 * still works and an unserved one does not.
 */
export function hasMountedTerminalView(terminalId: string): boolean {
  const probe = liveViews.get(terminalId);
  if (!probe) return false;
  try {
    return probe() === true;
  } catch {
    return false;
  }
}

/** Test-only: drop every record. Never called by production code. */
export function resetMountedTerminalViews(): void {
  liveViews.clear();
}
