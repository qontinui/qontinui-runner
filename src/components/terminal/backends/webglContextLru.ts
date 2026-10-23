/**
 * Cap on concurrent WebGL-backed terminal panes in this window (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 2, root cause A9).
 *
 * `XtermBackend.open()` loads one `WebglAddon` per pane under
 * `gpuAcceleration: "auto"`. Browsers cap live WebGL contexts at ~8-16 per
 * process and silently kill the oldest when the cap is exceeded; the flow grid
 * keeps ~18-30 panes live (3 columns × 300 px minimum tile), so the cap is
 * routinely blown. The symptom is a storm of context-loss events, each pane
 * rebuilding a Canvas renderer and a fresh glyph atlas mid-stream.
 *
 * This LRU makes the eviction OURS instead of the driver's: at most
 * {@link MAX_WEBGL_PANES} panes hold a context, and granting a new one
 * downgrades the least valuable holder to the Canvas renderer — the exact
 * transition `WebglAddon.onContextLoss` already performs, so it is a proven
 * path rather than a new one.
 *
 * Eviction order: hidden panes before visible ones, least-recently-used first
 * within each group. A pane calls {@link setWebglSlotVisible} when its
 * visibility tier flips, which records the flag and (on becoming visible)
 * promotes the pane to most-recently-used.
 *
 * An eviction is not final for a pane that registered a `restore` path: it
 * goes DORMANT, and once it is visible again ({@link setWebglSlotVisible}) it
 * is re-seated and its `restore` runs to rebuild the WebGL renderer.
 * Keep-mounted hidden panes (tmux work mode parks off-screen sessions that
 * way, plan `2026-10-11-runner-terminal-tmux-work-mode` D2) are exactly the
 * panes the cap downgrades, so without this a swap would routinely land on a
 * Canvas pane that never got its context back.
 *
 * Re-seating takes a FREE slot or a HIDDEN holder's slot — never another
 * visible pane's. Giving one visible pane WebGL by taking it from another
 * visible pane buys nothing the operator can see and costs two renderer
 * rebuilds; in a grid with more than {@link MAX_WEBGL_PANES} visible panes,
 * un-maximizing (which reveals every one of them at once) would cascade into
 * one such displacement per reveal. A visible dormant pane that finds no seat
 * stays on Canvas and WAITS: it is re-seated as soon as a slot frees (a pane
 * is disposed) or a holder hides.
 *
 * Backends with no WebGL never register — `GhosttyBackend` is Canvas 2D, so
 * the cap is a no-op there by construction (plan §4 Q4).
 */

/** Maximum panes allowed to hold a WebGL context at once. */
export const MAX_WEBGL_PANES = 8;

interface Slot {
  /** Pane identity (the terminal id) — how visibility updates address a slot. */
  key: string;
  /** Downgrade this pane to the Canvas renderer (releases its GL context). */
  downgrade: () => void;
  /** Whether the pane is currently on screen. */
  visible: boolean;
  /**
   * Rebuild this pane's WebGL renderer after an eviction. Absent = an eviction
   * is final (the pane stays on Canvas).
   */
  restore?: () => void;
}

/** Handle for the one registration an acquire created. */
export interface WebglSlotHandle {
  /** Give the context back (pane disposed). Idempotent. */
  release(): void;
}

/**
 * Live slots in LRU order, oldest first. An array (not a Map keyed by
 * terminal id) because a tab moving between zones transiently double-mounts
 * the same terminal id, and the departing pane's release must not evict the
 * arriving pane's registration.
 */
const slots: Slot[] = [];

/**
 * Evicted registrations that can come back: they hold no context, count
 * against no cap, and are re-seated on their next reveal. Removed by the
 * pane's `release()` (disposal) like a live slot.
 */
const dormant: Slot[] = [];

/**
 * Claim a WebGL slot, evicting the least valuable holder when the cap is
 * already reached. Always grants: the caller is opening a pane now, and an
 * already-open pane can fall back to Canvas without losing content.
 *
 * @param downgrade invoked when this slot is later evicted.
 * @param restore   invoked when an evicted slot is re-seated on reveal; the
 *                  pane rebuilds its WebGL renderer. Omit to make an eviction
 *                  final. The registration (and so the handle) survives the
 *                  eviction either way until `release()`.
 */
export function acquireWebglSlot(
  key: string,
  downgrade: () => void,
  visible = true,
  restore?: () => void,
): WebglSlotHandle {
  makeRoom();
  const slot: Slot = { key, downgrade, visible, restore };
  slots.push(slot);

  let released = false;
  return {
    release() {
      if (released) return;
      released = true;
      let freedSeat = false;
      for (const list of [slots, dormant]) {
        const idx = list.indexOf(slot);
        if (idx < 0) continue;
        list.splice(idx, 1);
        if (list === slots) freedSeat = true;
      }
      if (freedSeat) reseatWaiting();
    },
  };
}

/** Evict holders until one more slot fits under the cap. */
function makeRoom(): void {
  while (slots.length >= MAX_WEBGL_PANES) {
    const victimIdx = pickVictimIndex();
    if (victimIdx < 0) break;
    const [victim] = slots.splice(victimIdx, 1);
    // Parked BEFORE the downgrade runs, so a downgrade that re-enters the LRU
    // sees the victim where it now belongs.
    if (victim.restore) dormant.push(victim);
    try {
      victim.downgrade();
    } catch (e) {
      console.warn(`[webglContextLru] downgrade of "${victim.key}" failed:`, e);
    }
  }
}

/**
 * Record a pane's visibility. Becoming visible also promotes it to
 * most-recently-used, so the panes the operator is actually watching are the
 * last to be downgraded. No-op for a pane holding no slot.
 *
 * Recency comes from visibility transitions only — nothing promotes on write
 * or focus — so among equally-visible panes the eviction order is "oldest
 * registration first". That is the intended policy: a hidden pane is always a
 * better victim than any visible one, and visible panes are treated as equally
 * worth keeping.
 *
 * Revealing a pane whose slot was evicted with a `restore` path re-seats it as
 * most-recently-used (making room first, which may evict a hidden holder) and
 * runs `restore`, so the pane the operator is now looking at gets WebGL back.
 */
export function setWebglSlotVisible(key: string, visible: boolean): void {
  // A dormant registration remembers its visibility, so a visible one waiting
  // for a seat is re-seated when one frees, and a hidden one is not.
  for (const slot of dormant) if (slot.key === key) slot.visible = visible;
  if (visible) restoreDormant(key);
  // Every registration for the key is updated (a transient double-mount has
  // two, and leaving the departing one marked visible would shield it from
  // eviction), but only the newest is promoted.
  let newest = -1;
  for (let i = 0; i < slots.length; i++) {
    if (slots[i].key !== key) continue;
    slots[i].visible = visible;
    newest = i;
  }
  if (visible && newest >= 0) slots.push(...slots.splice(newest, 1));
  // A holder that just hid is a seat a waiting visible pane can take.
  if (!visible && newest >= 0) reseatWaiting();
}

/** Can a dormant pane be re-seated without displacing a visible pane? */
function hasSeatForReveal(): boolean {
  return slots.length < MAX_WEBGL_PANES || slots.some((s) => !s.visible);
}

/**
 * Re-seat the newest dormant registration for `key`, if any, and run its
 * `restore` — when a seat is free or a hidden holder can give one up. With
 * neither, the registration stays dormant (and visible) until
 * {@link reseatWaiting} finds it a seat. A live registration for the same key
 * (a transient double-mount) does not block it: each registration is its own
 * pane.
 */
function restoreDormant(key: string): void {
  let idx = -1;
  for (let i = 0; i < dormant.length; i++) if (dormant[i].key === key) idx = i;
  if (idx < 0) return;
  reseat(idx);
}

/**
 * Re-seat visible dormant panes, newest first, while a seat is free or a
 * hidden holder can give one up. Runs when a slot frees or a holder hides.
 */
function reseatWaiting(): void {
  for (let i = dormant.length - 1; i >= 0; i--) {
    if (i >= dormant.length || !dormant[i].visible) continue;
    if (!hasSeatForReveal()) return;
    reseat(i);
  }
}

/** Seat `dormant[idx]` if a seat is available without displacing a visible pane. */
function reseat(idx: number): void {
  if (!hasSeatForReveal()) return;
  const [slot] = dormant.splice(idx, 1);
  makeRoom();
  slot.visible = true;
  slots.push(slot);
  try {
    slot.restore?.();
  } catch (e) {
    // The pane stays on Canvas; it must not keep a seat it holds no context in.
    const at = slots.indexOf(slot);
    if (at >= 0) slots.splice(at, 1);
    console.warn(`[webglContextLru] restore of "${slot.key}" failed:`, e);
  }
}

/** Least-recently-used hidden pane, else least-recently-used pane overall. */
function pickVictimIndex(): number {
  const hidden = slots.findIndex((s) => !s.visible);
  if (hidden >= 0) return hidden;
  return slots.length > 0 ? 0 : -1;
}

/** Test/diagnostic: slot keys in LRU order (oldest first). */
export function __webglSlotKeys(): string[] {
  return slots.map((s) => s.key);
}

/** Test/diagnostic: keys of evicted registrations awaiting a reveal. */
export function __webglDormantKeys(): string[] {
  return dormant.map((s) => s.key);
}

/** Test-only. */
export function __resetWebglLruForTest(): void {
  slots.length = 0;
  dormant.length = 0;
}
