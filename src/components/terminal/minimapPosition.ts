/**
 * Zone minimap position — pure helpers shared by `useUIState` (persistence)
 * and `ZoneMinimap` (dragging and render-time clamping).
 *
 * The position is an offset from the container's TOP-RIGHT corner, not an
 * absolute `left`/`top`. The default placement is top-right, so anchoring to
 * the same corner means a window resize keeps a moved minimap where the
 * operator put it relative to that corner, instead of drifting toward the
 * left edge as the grid widens.
 *
 * `null` means "the original location" — the `top-7 right-2` default in
 * `ZoneMinimap`. Resetting removes the stored key rather than writing the
 * default offset, so a future change to the default reaches operators who
 * never moved the minimap AND those who moved it back.
 */

export const MINIMAP_POSITION_KEY = "zone-minimap-position";

/** Default offset — matches the `top-7 right-2` placement (28px / 8px). */
export const DEFAULT_MINIMAP_OFFSET: MinimapOffset = { top: 28, right: 8 };

export interface MinimapOffset {
  /** px from the container's top edge to the minimap's top edge. */
  top: number;
  /** px from the container's right edge to the minimap's right edge. */
  right: number;
}

export interface Size {
  width: number;
  height: number;
}

/**
 * Parse a stored position. Anything that is not an object with two finite
 * numbers is treated as absent (default position) rather than trusted — a
 * corrupt value must not be able to put the minimap off-screen.
 */
export function parseMinimapOffset(raw: string | null): MinimapOffset | null {
  if (!raw) return null;
  try {
    const v: unknown = JSON.parse(raw);
    if (typeof v !== "object" || v === null) return null;
    const { top, right } = v as Record<string, unknown>;
    if (typeof top !== "number" || typeof right !== "number") return null;
    if (!Number.isFinite(top) || !Number.isFinite(right)) return null;
    return { top, right };
  } catch {
    return null;
  }
}

/**
 * Gap kept from the top and right edges. The dismiss and reset buttons
 * overhang the box by 6px (`-top-1.5`, `-right-1.5`) and the grid container
 * is `overflow-hidden`, so a flush offset would clip them.
 */
export const MINIMAP_EDGE_MARGIN = 6;

/**
 * Keep the minimap fully inside its container, with MINIMAP_EDGE_MARGIN on
 * the sides its buttons overhang. When the container is smaller than the
 * minimap the offset pins to the margin at the top-right corner, which keeps
 * the dismiss and reset buttons reachable.
 */
export function clampMinimapOffset(
  offset: MinimapOffset,
  container: Size,
  box: Size,
): MinimapOffset {
  const min = MINIMAP_EDGE_MARGIN;
  const maxTop = Math.max(min, container.height - box.height);
  const maxRight = Math.max(min, container.width - box.width);
  return {
    top: Math.round(Math.min(Math.max(min, offset.top), maxTop)),
    right: Math.round(Math.min(Math.max(min, offset.right), maxRight)),
  };
}
