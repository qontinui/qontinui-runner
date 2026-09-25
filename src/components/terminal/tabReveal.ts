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
 *      reappears among the hidden tabs, exactly as a manual reassignment would.
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
): TabRevealPlan | null {
  if (zoneCount <= 0) return null;

  for (const [idx, id] of Object.entries(assignments)) {
    const zone = Number(idx);
    if (id === tabId && zone < zoneCount) return { zone, assign: false };
  }

  for (let zone = 0; zone < zoneCount; zone++) {
    if (!assignments[zone]) return { zone, assign: true };
  }

  const zone = focusedZone >= 0 && focusedZone < zoneCount ? focusedZone : 0;
  return { zone, assign: true };
}
