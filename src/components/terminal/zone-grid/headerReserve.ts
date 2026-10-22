/**
 * Room the zone header keeps clear at its right end while the cell is hovered.
 *
 * `ZoneHoverActions` reveals its button cluster over the cell's top-right
 * corner on hover, which is the same row where `ZoneLabel` right-aligns the cwd
 * label (`ml-auto`). Padding the header by the cluster's width while the cell is
 * hovered pushes the label left of the cluster instead of under it.
 *
 * 148 px = the six-button cluster's span (`hoverActionsSpanPx(6)` = 144) plus a
 * 4 px gap. The class is a literal so Tailwind's scanner emits it. The
 * `ZoneHoverActions` reserve test counts the rendered buttons and fails when
 * the cluster outgrows this.
 */
export const ZONE_HEADER_HOVER_RESERVE_PX = 148;
export const ZONE_HEADER_HOVER_RESERVE_CLASS = "group-hover:pr-[148px]";
