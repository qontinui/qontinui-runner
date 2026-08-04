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
 * Claim a WebGL slot, evicting the least valuable holder when the cap is
 * already reached. Always grants: the caller is opening a pane now, and an
 * already-open pane can fall back to Canvas without losing content.
 *
 * @param downgrade invoked when this slot is later evicted.
 */
export function acquireWebglSlot(
  key: string,
  downgrade: () => void,
  visible = true,
): WebglSlotHandle {
  while (slots.length >= MAX_WEBGL_PANES) {
    const victimIdx = pickVictimIndex();
    if (victimIdx < 0) break;
    const [victim] = slots.splice(victimIdx, 1);
    try {
      victim.downgrade();
    } catch (e) {
      console.warn(`[webglContextLru] downgrade of "${victim.key}" failed:`, e);
    }
  }
  const slot: Slot = { key, downgrade, visible };
  slots.push(slot);

  let released = false;
  return {
    release() {
      if (released) return;
      released = true;
      const idx = slots.indexOf(slot);
      if (idx >= 0) slots.splice(idx, 1);
    },
  };
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
 */
export function setWebglSlotVisible(key: string, visible: boolean): void {
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

/** Test-only. */
export function __resetWebglLruForTest(): void {
  slots.length = 0;
}
