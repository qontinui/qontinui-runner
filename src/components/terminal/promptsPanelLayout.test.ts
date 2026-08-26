import { describe, it, expect } from "vitest";
import {
  promptsPanelAvailable,
  promptsPanelOrientation,
  promptsStripHeight,
  availableStripHeight,
  zoneBodyPadding,
  MIN_TERMINAL_BODY_PX,
  MIN_PROMPTS_STRIP_PX,
} from "./promptsPanelLayout";
import { PROMPTS_PANEL_TOP_HEIGHT_PX, PROMPTS_PANEL_RIGHT_WIDTH_PX } from "./ZonePromptsPanel";

const HEADER = 20;
const FILTER = 26;
/** A comfortably tall tiled zone (a 2x2 cell on a 1440p display). */
const TALL = 350;
/** A single-row cell in the 4-row `command-center` layout on a 900px display. */
const SHORT = 172;

describe("promptsPanelAvailable", () => {
  it("is true for a tab bound to a Claude session", () => {
    expect(promptsPanelAvailable({ claudeSessionId: "abc", showCompactCard: false })).toBe(true);
  });

  it("is false for a plain shell tab with no session", () => {
    expect(promptsPanelAvailable({ showCompactCard: false })).toBe(false);
    expect(promptsPanelAvailable({ claudeSessionId: "", showCompactCard: false })).toBe(false);
  });

  it("is false while the zone renders a compact card", () => {
    expect(promptsPanelAvailable({ claudeSessionId: "abc", showCompactCard: true })).toBe(false);
  });
});

describe("promptsPanelOrientation", () => {
  it("gives a full-page zone the right-hand column", () => {
    expect(
      promptsPanelOrientation({ isSingleView: true, zoneHeightPx: TALL, chromeTopPx: HEADER }),
    ).toBe("right");
  });

  it("gives a roomy tiled zone the top strip", () => {
    expect(
      promptsPanelOrientation({ isSingleView: false, zoneHeightPx: TALL, chromeTopPx: HEADER }),
    ).toBe("top");
  });

  it("falls back to the column when a tiled zone is too short for a usable strip", () => {
    // 172 - 20 - 100 = 52px... still above the floor.
    expect(
      promptsPanelOrientation({ isSingleView: false, zoneHeightPx: SHORT, chromeTopPx: HEADER }),
    ).toBe("top");
    // Open the filter bar too and there is no longer room: 172 - 46 - 100 = 26.
    expect(
      promptsPanelOrientation({
        isSingleView: false,
        zoneHeightPx: SHORT,
        chromeTopPx: HEADER + FILTER,
      }),
    ).toBe("right");
  });

  it("treats an unmeasured zone as roomy rather than flashing the column", () => {
    expect(
      promptsPanelOrientation({ isSingleView: false, zoneHeightPx: 0, chromeTopPx: HEADER }),
    ).toBe("top");
  });
});

describe("availableStripHeight / promptsStripHeight", () => {
  it("never lets the terminal body drop below its floor", () => {
    for (const zoneH of [120, 150, 172, 200, 260, 350, 700]) {
      const strip = promptsStripHeight(zoneH, HEADER);
      expect(zoneH - HEADER - strip).toBeGreaterThanOrEqual(MIN_TERMINAL_BODY_PX);
    }
  });

  it("clamps to zero when the zone cannot even hold the floor", () => {
    expect(availableStripHeight(80, HEADER)).toBe(0);
    expect(promptsStripHeight(80, HEADER)).toBe(0);
  });

  it("caps at the natural strip height once there is room to spare", () => {
    expect(promptsStripHeight(2000, HEADER)).toBe(PROMPTS_PANEL_TOP_HEIGHT_PX);
  });

  it("shrinks the strip rather than the terminal on a short zone", () => {
    const strip = promptsStripHeight(SHORT, HEADER);
    expect(strip).toBeLessThan(PROMPTS_PANEL_TOP_HEIGHT_PX);
    expect(strip).toBeGreaterThanOrEqual(MIN_PROMPTS_STRIP_PX);
  });
});

describe("zoneBodyPadding", () => {
  it("reserves only the title bar when nothing else is open", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: 0,
        promptsOpen: false,
        isSingleView: false,
        zoneHeightPx: TALL,
      }),
    ).toEqual({ top: HEADER, right: 0 });
  });

  it("stacks the filter bar under the title bar", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: FILTER,
        promptsOpen: false,
        isSingleView: false,
        zoneHeightPx: TALL,
      }),
    ).toEqual({ top: HEADER + FILTER, right: 0 });
  });

  it("reserves zero when the zone renders no chrome at all", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: 0,
        filterBarPx: 0,
        promptsOpen: false,
        isSingleView: false,
        zoneHeightPx: TALL,
      }),
    ).toEqual({ top: 0, right: 0 });
  });

  it("adds the prompts strip below existing chrome in a tiled zone", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView: false,
        zoneHeightPx: TALL,
      }),
    ).toEqual({ top: HEADER + PROMPTS_PANEL_TOP_HEIGHT_PX, right: 0 });
  });

  it("stacks title bar + filter bar + prompts strip together", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: FILTER,
        promptsOpen: true,
        isSingleView: false,
        zoneHeightPx: TALL,
      }),
    ).toEqual({ top: HEADER + FILTER + PROMPTS_PANEL_TOP_HEIGHT_PX, right: 0 });
  });

  it("reserves width instead of height for a full-page zone", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView: true,
        zoneHeightPx: TALL,
      }),
    ).toEqual({ top: HEADER, right: PROMPTS_PANEL_RIGHT_WIDTH_PX });
  });

  it("reserves width, not height, when a tiled zone is too short for a strip", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: FILTER,
        promptsOpen: true,
        isSingleView: false,
        zoneHeightPx: SHORT,
      }),
    ).toEqual({ top: HEADER + FILTER, right: PROMPTS_PANEL_RIGHT_WIDTH_PX });
  });

  it("never reserves both axes at once", () => {
    for (const isSingleView of [true, false]) {
      const pad = zoneBodyPadding({
        zoneHeaderPx: 0,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView,
        zoneHeightPx: TALL,
      });
      expect(pad.top === 0 || pad.right === 0).toBe(true);
    }
  });
});
