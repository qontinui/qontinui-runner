/**
 * Tests for the FINISHED band on a terminal zone: when it draws (the pure
 * `zoneFinishedBand` predicate — the JSX it gates is verified through the UI
 * Bridge), and the geometry of the chequered `border-image` it draws.
 */

import { describe, it, expect } from "vitest";
import { zoneFinishedBand } from "./utils";
import { FINISHED_BAND_PX, FINISHED_CHECK_COLOR, finishedBorderImage } from "./constants";

const SESSION = "8553fb2f-dee1-49e2-9438-c29932317500";
const finished = { [SESSION]: { verdict: "finished" as const, source: "coord" as const } };

describe("zoneFinishedBand", () => {
  it("draws the band for a session marked finished", () => {
    expect(zoneFinishedBand({ claudeSessionId: SESSION, finishedStates: finished })).toEqual({
      verdict: "finished",
      showBand: true,
    });
  });

  it("does not draw it for a session that is not finished, or not known to be", () => {
    expect(
      zoneFinishedBand({
        claudeSessionId: SESSION,
        finishedStates: { [SESSION]: { verdict: "not_finished" } },
      }),
    ).toEqual({ verdict: "not_finished", showBand: false });
    // Not polled yet / coord unread: UNKNOWN, ordinary border.
    expect(zoneFinishedBand({ claudeSessionId: SESSION, finishedStates: {} })).toEqual({
      verdict: "unknown",
      showBand: false,
    });
  });

  it("has no verdict for a tab with no Claude session", () => {
    expect(zoneFinishedBand({ finishedStates: finished })).toEqual({
      verdict: undefined,
      showBand: false,
    });
  });

  it("yields to the operator's in-progress gestures but keeps the verdict", () => {
    for (const gesture of ["isDropTarget", "isSwapSource", "isSelected", "searchMatch"]) {
      expect(
        zoneFinishedBand({ claudeSessionId: SESSION, finishedStates: finished, [gesture]: true }),
      ).toEqual({ verdict: "finished", showBand: false });
    }
  });
});

describe("finishedBorderImage", () => {
  const value = finishedBorderImage();
  const svg = decodeURIComponent(/data:image\/svg\+xml,([^")]+)/.exec(value)![1]);

  it("slices and sizes the image at the band width, repeating with `round`", () => {
    expect(value.endsWith(`${FINISHED_BAND_PX} / ${FINISHED_BAND_PX}px round`)).toBe(true);
  });

  it("is a chequerboard of half-band squares in the check colour", () => {
    const square = FINISHED_BAND_PX / 2;
    expect(svg).toContain(`width="${FINISHED_BAND_PX * 3}"`);
    expect(svg).toContain(`<pattern id="c" width="${FINISHED_BAND_PX}"`);
    // Two filled squares per 2x2 cell, on the diagonal — the alternation.
    expect(svg).toContain(
      `<rect width="${square}" height="${square}" fill="${FINISHED_CHECK_COLOR}"/>`,
    );
    expect(svg).toContain(
      `<rect x="${square}" y="${square}" width="${square}" height="${square}" fill="${FINISHED_CHECK_COLOR}"/>`,
    );
  });

  it("encodes the colour so a `#` cannot end the data URI early", () => {
    expect(value).not.toContain("#");
    expect(finishedBorderImage("#123456")).toContain(encodeURIComponent("#123456"));
  });
});
