/**
 * Tests for the FINISHED band on a terminal zone: when it draws (the pure
 * `zoneFinishedBand` predicate), what it looks like (`zoneBorderStyle` and the
 * chequered `border-image`), and the property that matters most for a live
 * terminal — nothing but a real verdict change alters the border WIDTH, because
 * a width change resizes the zone's PTY.
 */

import { describe, it, expect } from "vitest";
import { zoneFinishedBand } from "./utils";
import {
  FINISHED_BAND_PX,
  FINISHED_CHECK_COLOR,
  FINISHED_HELD_OPACITY,
  STATE_COLORS,
  ZONE_FOCUS_COLOR,
  ZONE_GESTURE_COLORS,
  finishedBandColor,
  finishedBorderImage,
} from "./constants";
import { maximizedBorderStyle, zoneBorderStyle, type ZoneBorderInput } from "./zoneBorder";
import type { SessionState } from "../useZoneLayout";

const SESSION = "8553fb2f-dee1-49e2-9438-c29932317500";

describe("zoneFinishedBand", () => {
  it("draws the band for a session marked finished", () => {
    expect(zoneFinishedBand({ claudeSessionId: SESSION, verdict: "finished" })).toEqual({
      verdict: "finished",
      showBand: true,
      held: false,
    });
  });

  it("does not draw it for a session that is not finished, or not known to be", () => {
    expect(zoneFinishedBand({ claudeSessionId: SESSION, verdict: "not_finished" })).toEqual({
      verdict: "not_finished",
      showBand: false,
      held: false,
    });
    // Not polled yet: UNKNOWN, ordinary border.
    expect(zoneFinishedBand({ claudeSessionId: SESSION })).toEqual({
      verdict: "unknown",
      showBand: false,
      held: false,
    });
  });

  it("keeps a held band while reporting the current verdict as unknown", () => {
    expect(zoneFinishedBand({ claudeSessionId: SESSION, verdict: "unknown", held: true })).toEqual({
      verdict: "unknown",
      showBand: true,
      held: true,
    });
  });

  it("has no verdict for a tab with no Claude session", () => {
    expect(zoneFinishedBand({ verdict: "finished" })).toEqual({
      verdict: undefined,
      showBand: false,
      held: false,
    });
  });
});

describe("finishedBandColor", () => {
  it("is the finished green when nothing else is going on", () => {
    expect(finishedBandColor({})).toBe(FINISHED_CHECK_COLOR);
  });

  it("carries what the ordinary border would have said, most urgent first", () => {
    expect(finishedBandColor({ isDropTarget: true, isFocused: true })).toBe(
      ZONE_GESTURE_COLORS.dropTarget,
    );
    expect(finishedBandColor({ isSwapSource: true })).toBe(ZONE_GESTURE_COLORS.swapSource);
    expect(finishedBandColor({ isSelected: true, needsInput: true })).toBe(
      ZONE_GESTURE_COLORS.selected,
    );
    expect(finishedBandColor({ needsInput: true, isFocused: true })).toBe(
      STATE_COLORS["needs-input"],
    );
    expect(finishedBandColor({ isError: true })).toBe(STATE_COLORS.error);
    expect(finishedBandColor({ isFocused: true })).toBe(ZONE_FOCUS_COLOR);
  });
});

describe("zoneBorderStyle", () => {
  const base: ZoneBorderInput = { state: "idle" };
  const gestures: Partial<ZoneBorderInput>[] = [
    {},
    { isFocused: true },
    { isDropTarget: true },
    { isSwapSource: true },
    { isSelected: true },
    { searchMatch: true },
    { isStale: true },
    { finishedHeld: true },
  ];
  const states: SessionState[] = ["idle", "working", "needs-input", "completed", "error"];

  it("never changes the band's width, so only a real verdict change can resize the PTY", () => {
    for (const state of states) {
      for (const g of gestures) {
        const style = zoneBorderStyle({ ...base, ...g, state, finishedBand: true });
        expect(style.borderWidth).toBe(`${FINISHED_BAND_PX}px`);
      }
    }
  });

  it("uses the same four keys in every branch, none a shorthand of another", () => {
    // React writes only CHANGED keys, so a four-side key beside a per-side
    // `borderLeft*` key lets a focus toggle wipe the left stripe for good.
    const keys = ["borderColor", "borderImage", "borderStyle", "borderWidth"];
    for (const state of states) {
      for (const g of gestures) {
        for (const finishedBand of [true, false]) {
          const style = zoneBorderStyle({ ...base, ...g, state, finishedBand });
          expect(Object.keys(style).sort()).toEqual(keys);
        }
      }
    }
  });

  it("dims a held band in every colour but an attention one", () => {
    expect(zoneBorderStyle({ ...base, finishedBand: true, finishedHeld: true }).borderImage).toBe(
      finishedBorderImage(FINISHED_CHECK_COLOR, FINISHED_HELD_OPACITY),
    );
    // The zone the operator is looking at must not pass a held band off as
    // confirmed.
    const focused = zoneBorderStyle({
      ...base,
      isFocused: true,
      finishedBand: true,
      finishedHeld: true,
    });
    expect(focused.borderImage).toBe(finishedBorderImage(ZONE_FOCUS_COLOR, FINISHED_HELD_OPACITY));
    // A permission prompt on a held zone is a live fact: full strength.
    const prompt = zoneBorderStyle({
      state: "needs-input",
      finishedBand: true,
      finishedHeld: true,
    });
    expect(prompt.borderImage).toBe(finishedBorderImage(STATE_COLORS["needs-input"], 1));
  });

  it("keeps the ordinary border as it was when there is no band", () => {
    const amber = STATE_COLORS["needs-input"];
    expect(zoneBorderStyle({ state: "needs-input", isFocused: true })).toEqual({
      borderWidth: "2px 2px 2px 3px",
      borderColor: `${amber} ${amber} ${amber} ${amber}`,
      borderStyle: "solid solid solid solid",
      borderImage: "none",
    });
    // Focus on an idle zone shows the focus colour on three sides; the left
    // stripe keeps the state colour.
    const focusBlue = ZONE_FOCUS_COLOR;
    expect(zoneBorderStyle({ state: "idle", isFocused: true }).borderColor).toBe(
      `${focusBlue} ${focusBlue} ${focusBlue} ${STATE_COLORS.idle}`,
    );
    // The stale dash leaves the left stripe solid, as before.
    expect(zoneBorderStyle({ state: "working", isStale: true }).borderStyle).toBe(
      "dashed dashed dashed solid",
    );
  });

  it("frames the maximized view only when finished, with one key set", () => {
    const off = maximizedBorderStyle({ state: "idle" });
    const on = maximizedBorderStyle({ state: "idle", finishedBand: true });
    expect(off.borderWidth).toBe(0);
    expect(on.borderWidth).toBe(`${FINISHED_BAND_PX}px`);
    expect(Object.keys(on).sort()).toEqual(Object.keys(off).sort());
  });
});

describe("finishedBorderImage", () => {
  const value = finishedBorderImage();
  const svg = decodeURIComponent(/url\("data:image\/svg\+xml,([^"]+)"\)/.exec(value)![1]);

  it("slices and sizes the image at the band width, repeating with `round`", () => {
    expect(value.endsWith(`${FINISHED_BAND_PX} / ${FINISHED_BAND_PX}px round`)).toBe(true);
  });

  it("is a crisp chequerboard of half-band squares, painted over the whole image", () => {
    const square = FINISHED_BAND_PX / 2;
    expect(svg).toContain(`width="${FINISHED_BAND_PX * 3}"`);
    expect(svg).toContain('shape-rendering="crispEdges"');
    expect(svg).toContain(`<pattern id="c" width="${FINISHED_BAND_PX}"`);
    // Two filled squares per 2x2 cell, on the diagonal: the alternation.
    expect(svg).toContain(
      `<rect width="${square}" height="${square}" fill="${FINISHED_CHECK_COLOR}"/>`,
    );
    expect(svg).toContain(
      `<rect x="${square}" y="${square}" width="${square}" height="${square}" fill="${FINISHED_CHECK_COLOR}"/>`,
    );
    // The pattern is actually painted.
    expect(svg).toContain('fill="url(#c)" fill-opacity="1"');
  });

  it("encodes the colour so a `#` cannot end the data URI early", () => {
    expect(value).not.toContain("#");
    expect(finishedBorderImage("#123456")).toContain(encodeURIComponent("#123456"));
  });
});
