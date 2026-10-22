/**
 * Room the zone header keeps clear at its right end while the cell is hovered.
 *
 * `ZoneHoverActions` reveals its button cluster over the cell's top-right
 * corner on hover, which is the same row where `ZoneLabel` right-aligns the cwd
 * label (`ml-auto`). Padding the header by the cluster's width while the cell is
 * hovered pushes the label left of the cluster instead of under it.
 *
 * 148 px = the six-button cluster's span (`hoverActionsSpanPx(6)` = 144) plus a
 * 4 px gap. It takes effect on the cluster's own 150 ms reveal delay, so a pointer
 * passing over a grid does not reflow every header it crosses. The class is a literal so Tailwind's scanner emits it. The
 * `ZoneHoverActions` reserve test counts the rendered buttons and fails when
 * the cluster outgrows this.
 *
 * Known limit: the header's other chips are `shrink-0` and the row does not
 * clip (its dropdowns open below it), so in a very narrow zone those chips can
 * still run into the reserved strip. The cwd label is the one this protects.
 */
export const ZONE_HEADER_HOVER_RESERVE_PX = 148;
export const ZONE_HEADER_HOVER_RESERVE_CLASS =
  "group-hover:pr-[148px] transition-[padding] duration-200 group-hover:delay-150";
