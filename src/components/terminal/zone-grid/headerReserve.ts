/**
 * Room the zone header keeps clear at its right end while the cell is hovered,
 * and the zone width below which the hover cluster collapses to one button.
 *
 * `ZoneHoverActions` reveals its button cluster over the cell's top-right
 * corner on hover, which is the same row where `ZoneLabel` right-aligns the cwd
 * label (`ml-auto`). Padding the header by the cluster's width while the cell is
 * hovered pushes the label left of the cluster instead of under it.
 *
 * 148 px = the six-button cluster's span (`hoverActionsSpanPx(6)` = 144) plus a
 * 4 px gap. It takes effect on the cluster's own 150 ms reveal delay, so a
 * pointer passing over a grid does not reflow every header it crosses. The
 * class is a literal so Tailwind's scanner emits it. The `ZoneHoverActions`
 * reserve test counts the rendered buttons and fails when the cluster
 * outgrows this.
 *
 * Narrow zones. The header's controls that cannot truncate (state dot, label
 * chip, switch-session chevron and the icon toggles) need
 * `headerControlsMinPx()` = 162 px, and the row cannot clip them (its dropdowns
 * open below it). Beside a 148 px reserve that only fits a zone at least
 * `162 + 16 (px-2) + 148` = 326 px wide; in a narrower one the toggles would
 * sit under the z-30 cluster and could not be clicked. So the cell is a CSS
 * container (`@container/zone`), and below 326 px the cluster collapses to ONE
 * overflow button whose menu holds all six actions, and the reserve shrinks to
 * that button's span: `hoverActionsSpanPx(1)` = 34 plus the same 4 px gap = 38.
 * All of it is CSS — no ResizeObserver — and the class strings are literals so
 * Tailwind's scanner emits them; `ZoneHoverActions.inline.test.tsx` ties every
 * number here to the arithmetic it comes from.
 *
 * Known limit: a remote tab's "End on remote session" button stays beside the
 * overflow button (it owns a confirm dialog), widening the collapsed cluster by
 * one button that the 38 px reserve does not cover — the same seventh button
 * the 148 px reserve does not cover either.
 */
export const ZONE_HEADER_HOVER_RESERVE_PX = 148;
/** The collapsed cluster's reserve: `hoverActionsSpanPx(1)` + a 4 px gap. */
export const ZONE_HEADER_COLLAPSED_RESERVE_PX = 38;
/** Zone width (CSS px) below which the hover cluster collapses: 162 + 16 + 148. */
export const ZONE_HOVER_COLLAPSE_BELOW_PX = 326;

/** Makes the zone cell the named query container the variants below read. */
export const ZONE_CELL_CONTAINER_CLASS = "@container/zone";

export const ZONE_HEADER_HOVER_RESERVE_CLASS =
  "group-hover:pr-[148px] @max-[326px]/zone:group-hover:pr-[38px] transition-[padding] duration-200 group-hover:delay-150";

/** On each of the six full-cluster actions: gone below the collapse width. */
export const ZONE_HOVER_FULL_ONLY_CLASS = "@max-[326px]/zone:hidden";
/** On the overflow button's wrapper: shown only below the collapse width. */
export const ZONE_HOVER_COLLAPSED_ONLY_CLASS = "hidden @max-[326px]/zone:flex";
