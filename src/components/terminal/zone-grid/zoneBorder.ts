import type { CSSProperties } from "react";
import type { SessionState } from "../useZoneLayout";
import {
  FINISHED_BAND_PX,
  FINISHED_HELD_OPACITY,
  STATE_BORDER_COLORS,
  STATE_COLORS,
  ZONE_FOCUS_COLOR,
  ZONE_GESTURE_COLORS,
  finishedBandColor,
  finishedBorderImage,
} from "./constants";

/**
 * A held band dims, so it never looks like a confirmed one — except in an
 * attention colour: a permission prompt or an error is a live fact about the
 * session, and must not be shown at reduced strength because the FINISHED
 * half of the band is unconfirmed.
 */
function isAttention(state: SessionState): boolean {
  return state === "needs-input" || state === "error";
}

/** Everything a zone's border reflects. */
export interface ZoneBorderInput {
  state: SessionState;
  isFocused?: boolean;
  isDropTarget?: boolean;
  isSwapSource?: boolean;
  isSelected?: boolean;
  searchMatch?: boolean;
  isStale?: boolean;
  /** Draw the chequered FINISHED band (see `zoneFinishedBand`). */
  finishedBand?: boolean;
  /** The band is held from an earlier read, not confirmed by this one. */
  finishedHeld?: boolean;
}

/**
 * The border of a terminal zone, as inline style.
 *
 * Exactly four keys, the same four in every branch, and none of them a
 * shorthand of another: `borderWidth`, `borderColor` and `borderStyle` carry
 * one value per side (top right bottom left), so the left stripe lives in them
 * rather than in a `borderLeft*` key. Mixing a four-side key with a
 * `borderLeft*` key is unsafe under React, which writes only the keys whose
 * values changed: when only the four-side key changes (a focus toggle) the
 * browser resets the left side too, and React never writes the unchanged
 * `borderLeft*` key back.
 */
export function zoneBorderStyle(i: ZoneBorderInput): CSSProperties {
  if (i.finishedBand) {
    const color = finishedBandColor({
      isDropTarget: i.isDropTarget,
      isSwapSource: i.isSwapSource,
      isSelected: i.isSelected,
      needsInput: i.state === "needs-input",
      isError: i.state === "error",
      isFocused: i.isFocused,
    });
    const dim = i.finishedHeld && !isAttention(i.state);
    return {
      borderWidth: `${FINISHED_BAND_PX}px`,
      borderColor: "transparent",
      borderStyle: "solid",
      borderImage: finishedBorderImage(color, dim ? FINISHED_HELD_OPACITY : 1),
    };
  }

  const stateBorder = STATE_BORDER_COLORS[i.state];
  const color = i.isDropTarget
    ? ZONE_GESTURE_COLORS.dropTarget
    : i.isSwapSource
      ? ZONE_GESTURE_COLORS.swapSource
      : i.isSelected
        ? ZONE_GESTURE_COLORS.selected
        : i.searchMatch
          ? ZONE_GESTURE_COLORS.searchMatch
          : i.isStale
            ? ZONE_GESTURE_COLORS.stale
            : i.isFocused && stateBorder === STATE_BORDER_COLORS.idle
              ? ZONE_FOCUS_COLOR
              : stateBorder;
  const width = i.isFocused || i.isSwapSource || i.isSelected || i.searchMatch ? "2px" : "1px";
  const leftWidth = i.state === "needs-input" ? "3px" : "2px";
  const style =
    i.isSwapSource || (i.isStale && !i.isFocused && !i.searchMatch) ? "dashed" : "solid";
  return {
    borderWidth: `${width} ${width} ${width} ${leftWidth}`,
    borderColor: `${color} ${color} ${color} ${STATE_COLORS[i.state]}`,
    borderStyle: `${style} ${style} ${style} solid`,
    borderImage: "none",
  };
}

/**
 * The maximized view's frame: no border normally, the FINISHED band when the
 * session is finished. Same key set in both branches, for the reason above.
 */
export function maximizedBorderStyle(i: {
  state: SessionState;
  finishedBand?: boolean;
  finishedHeld?: boolean;
}): CSSProperties {
  if (!i.finishedBand) {
    return { borderWidth: 0, borderStyle: "none", borderColor: "transparent", borderImage: "none" };
  }
  const color = finishedBandColor({
    needsInput: i.state === "needs-input",
    isError: i.state === "error",
  });
  const dim = i.finishedHeld && !isAttention(i.state);
  return {
    borderWidth: `${FINISHED_BAND_PX}px`,
    borderStyle: "solid",
    borderColor: "transparent",
    borderImage: finishedBorderImage(color, dim ? FINISHED_HELD_OPACITY : 1),
  };
}
