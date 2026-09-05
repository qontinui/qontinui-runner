/**
 * Pure decision core behind the `terminal-page` maximize/restore UI Bridge
 * actions (plan `2026-08-19-session-info-dropdown-mount-gaps-remediation`, D2).
 *
 * ## Why these actions exist at all
 *
 * `ZoneGrid` has a second `SessionInfoDropdown` mount site in the maximized
 * view. It could not be exercised: the only ways to maximize a zone were a
 * hover button that is `opacity-0 pointer-events-none` until `group-hover`, a
 * keyboard shortcut, and an internal slash command — none of them addressable
 * from the UI Bridge. The original attempt drove the hover button by element id
 * and was rejected pre-IPC (`rejecting misleading recovered:true,
 * commandChosen:null … ACTION_NOT_SUPPORTED`), which is the correct answer to
 * clicking something that cannot be clicked. Giving the button an id would not
 * have helped; the fix is a named component action with a result envelope.
 *
 * ## Why the decision is pure, and separate from the React component
 *
 * The envelope is the point. A caller must be able to *prove* the maximized
 * state changed rather than infer it from a subsequent snapshot, and an
 * out-of-range zone must be a named error rather than a silent no-op — a
 * no-op is exactly the "answer that looks like success" this plan's parent
 * loop exists to eliminate. Both properties are decided here, so they can be
 * asserted without a DOM, a React tree or a UI Bridge.
 */

/** What a maximize/restore request resolves to, before anything is mutated. */
export type MaximizePlan = { ok: true; next: number | null } | { ok: false; error: string };

/** The envelope every maximize/restore action returns on success. */
export interface MaximizeResult {
  /** The maximized zone AFTER the action. `null` means restored/tiled. */
  maximizedZone: number | null;
  /** The maximized zone BEFORE it. Lets a caller prove a change happened. */
  previousMaximizedZone: number | null;
  /** How many zones the current layout has — the bound `zoneIndex` is checked against. */
  zoneCount: number;
  /**
   * `false` when the requested state was ALREADY the current state. Not an
   * error (the request is satisfied), but a caller asserting "my action did
   * something" needs to be able to tell the two apart.
   */
  changed: boolean;
}

/**
 * Validate a `zoneIndex` parameter arriving off the wire.
 *
 * Deliberately strict about the *type* as well as the range: the UI Bridge
 * hands handlers an untyped `unknown`, and a `"1"` that silently coerced to
 * zone 1 would make a caller's typo indistinguishable from a correct call.
 */
export function parseZoneIndex(raw: unknown, zoneCount: number): MaximizePlan {
  if (typeof raw !== "number" || !Number.isInteger(raw)) {
    return {
      ok: false,
      error: `zoneIndex must be an integer, got ${JSON.stringify(raw)}`,
    };
  }
  if (raw < 0 || raw >= zoneCount) {
    return {
      ok: false,
      error: `zoneIndex ${raw} is out of range — this layout has ${zoneCount} zone(s), so valid values are 0..${zoneCount - 1}`,
    };
  }
  return { ok: true, next: raw };
}

/** `maximize-zone`: pin one zone to the whole page. */
export function planMaximizeZone(raw: unknown, zoneCount: number): MaximizePlan {
  return parseZoneIndex(raw, zoneCount);
}

/**
 * `toggle-maximize-zone`: maximize the zone, or restore if it is already the
 * maximized one. Mirrors `useZoneLayout`'s own `toggleMaximize` so the action
 * and the keyboard shortcut cannot drift apart.
 */
export function planToggleMaximizeZone(
  raw: unknown,
  zoneCount: number,
  current: number | null,
): MaximizePlan {
  const parsed = parseZoneIndex(raw, zoneCount);
  if (!parsed.ok) return parsed;
  return { ok: true, next: current === parsed.next ? null : parsed.next };
}

/** Build the success envelope. `next` is what the plan resolved to. */
export function buildMaximizeResult(
  previous: number | null,
  next: number | null,
  zoneCount: number,
): MaximizeResult {
  return {
    maximizedZone: next,
    previousMaximizedZone: previous,
    zoneCount,
    changed: previous !== next,
  };
}
