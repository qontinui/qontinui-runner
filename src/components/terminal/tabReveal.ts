/**
 * Where to put a tab so the operator can see it — the "take me to this
 * session" decision behind a Session Manager card click.
 *
 * The terminal page derives the active tab FROM the focused zone (the
 * `focusedTabId → activeId` sync in `TerminalSessionContext`), so a bare
 * `setActiveId(tabId)` is reverted on the next render whenever the tab is not
 * the focused zone's occupant. Revealing a tab therefore means moving zone
 * focus, and — when the tab is in no zone at all (one of the "N more" hidden
 * tabs) — placing it in one first.
 *
 * Preference order, so a click disturbs as little of the layout as possible:
 *   1. the zone that already shows the tab — focus only;
 *   2. an empty zone — assign there, nothing is displaced;
 *   3. the focused zone — assign there; the displaced tab stays alive and
 *      reappears among the hidden tabs, exactly as a manual reassignment would;
 *   4. (only when the focused zone is itself reserved) the first zone that
 *      isn't reserved, occupied or not.
 *
 * A zone RESERVED for a session record still being restored (see
 * `useZoneLayout`'s `reservedZonesRef`) is skipped in steps 2 AND 3 exactly as
 * `reconcileAssignments`' auto-fill skips it — a reveal click racing the async
 * restore window must not hand a hidden tab the zone a Claude session record
 * has already claimed, or the restore lands that session in an unassigned
 * slot when its reserved zone turns out occupied. Step 3 needs its own check
 * because the focused zone defaults to 0, the same zone a restoring session
 * typically reserves first — so the empty-zone search in step 2 finding
 * nothing free would otherwise fall straight through onto the reservation.
 */
export interface TabRevealPlan {
  zone: number;
  /** True when the tab must be assigned into `zone` before focusing it. */
  assign: boolean;
}

export function planTabReveal(
  assignments: Record<number, string>,
  zoneCount: number,
  focusedZone: number,
  tabId: string,
  reservedZones: ReadonlySet<number> = new Set(),
): TabRevealPlan | null {
  if (zoneCount <= 0) return null;

  for (const [idx, id] of Object.entries(assignments)) {
    const zone = Number(idx);
    if (id === tabId && zone < zoneCount) return { zone, assign: false };
  }

  for (let zone = 0; zone < zoneCount; zone++) {
    if (!assignments[zone] && !reservedZones.has(zone)) return { zone, assign: true };
  }

  const preferred = focusedZone >= 0 && focusedZone < zoneCount ? focusedZone : 0;
  if (!reservedZones.has(preferred)) return { zone: preferred, assign: true };

  // The focused zone is itself reserved for an in-flight restore — displacing
  // it would fight that restore instead of the click. Every zone here is
  // either occupied or reserved (step 2 already ruled out an empty,
  // unreserved one), so take the first zone that is at least not reserved.
  for (let zone = 0; zone < zoneCount; zone++) {
    if (!reservedZones.has(zone)) return { zone, assign: true };
  }

  // Pathological: every zone in the layout is reserved. Nothing safe to
  // pick — fall back to the focused zone anyway.
  return { zone: preferred, assign: true };
}
