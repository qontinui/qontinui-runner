import { describe, it, expect } from "vitest";
import {
  promptsPanelAvailable,
  promptsPanelOrientation,
  promptsStripHeight,
  availableStripHeight,
  zoneBodyPadding,
  MIN_TERMINAL_BODY_PX,
  MIN_PROMPTS_STRIP_PX,
  PROMPTS_PANEL_GEOMETRY,
  REVIEW_PANEL_GEOMETRY,
  toggleZonePanel,
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

describe("review panel geometry", () => {
  it("is taller, wider and leaves the strip sooner than the prompts panel", () => {
    expect(REVIEW_PANEL_GEOMETRY.topHeightPx).toBeGreaterThan(PROMPTS_PANEL_GEOMETRY.topHeightPx);
    expect(REVIEW_PANEL_GEOMETRY.rightWidthPx).toBeGreaterThan(PROMPTS_PANEL_GEOMETRY.rightWidthPx);
    expect(REVIEW_PANEL_GEOMETRY.minStripPx).toBeGreaterThan(PROMPTS_PANEL_GEOMETRY.minStripPx);
  });

  it("omitting the geometry is exactly the prompts panel's contract", () => {
    for (const zoneHeightPx of [0, SHORT, TALL]) {
      const opts = {
        zoneHeaderPx: HEADER,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView: false,
        zoneHeightPx,
      };
      expect(zoneBodyPadding(opts)).toEqual(
        zoneBodyPadding({ ...opts, geometry: PROMPTS_PANEL_GEOMETRY }),
      );
    }
  });

  it("sends a review panel to the column in a zone that would still fit a prompts strip", () => {
    // SHORT leaves 172 - 20 - 100 = 52px: enough for a prompts strip (48),
    // not for a review strip (160).
    expect(
      promptsPanelOrientation({ isSingleView: false, zoneHeightPx: SHORT, chromeTopPx: HEADER }),
    ).toBe("top");
    expect(
      promptsPanelOrientation({
        isSingleView: false,
        zoneHeightPx: SHORT,
        chromeTopPx: HEADER,
        geometry: REVIEW_PANEL_GEOMETRY,
      }),
    ).toBe("right");
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView: false,
        zoneHeightPx: SHORT,
        geometry: REVIEW_PANEL_GEOMETRY,
      }),
    ).toEqual({ top: HEADER, right: REVIEW_PANEL_GEOMETRY.rightWidthPx });
  });

  it("clamps a review strip to what the zone can spare and pads the body by the same amount", () => {
    const tall = 600;
    const strip = promptsStripHeight(tall, HEADER, REVIEW_PANEL_GEOMETRY);
    expect(strip).toBe(REVIEW_PANEL_GEOMETRY.topHeightPx);
    const mid = 420; // 420 - 20 - 100 = 300 available ≥ 160 → top, clamped to 260
    expect(promptsStripHeight(mid, HEADER, REVIEW_PANEL_GEOMETRY)).toBe(260);
    const pad = zoneBodyPadding({
      zoneHeaderPx: HEADER,
      filterBarPx: 0,
      promptsOpen: true,
      isSingleView: false,
      zoneHeightPx: 330, // 210 available → top strip of 210
      geometry: REVIEW_PANEL_GEOMETRY,
    });
    expect(pad).toEqual({ top: HEADER + 210, right: 0 });
  });
});

describe("toggleZonePanel", () => {
  const empty = { prompts: new Set<string>(), review: new Set<string>() };

  it("opens and closes one panel for one tab", () => {
    const opened = toggleZonePanel(empty, "t1", "review");
    expect([...opened.review]).toEqual(["t1"]);
    expect(opened.prompts.size).toBe(0);
    const closed = toggleZonePanel(opened, "t1", "review");
    expect(closed.review.size).toBe(0);
  });

  it("opening one panel closes the other for that tab only", () => {
    const start = { prompts: new Set(["t1", "t2"]), review: new Set<string>() };
    const next = toggleZonePanel(start, "t1", "review");
    expect([...next.review]).toEqual(["t1"]);
    expect([...next.prompts]).toEqual(["t2"]);
    const back = toggleZonePanel(next, "t1", "prompts");
    expect([...back.prompts].sort()).toEqual(["t1", "t2"]);
    expect(back.review.size).toBe(0);
  });

  it("closing a panel leaves the other set untouched (same reference)", () => {
    const start = { prompts: new Set(["t2"]), review: new Set(["t1"]) };
    const next = toggleZonePanel(start, "t1", "review");
    expect(next.prompts).toBe(start.prompts);
  });

  describe("with the global prompts default on", () => {
    it("closing prompts records the tab as an override", () => {
      const next = toggleZonePanel(empty, "t1", "prompts", true);
      expect([...next.prompts]).toEqual(["t1"]);
      const back = toggleZonePanel(next, "t1", "prompts", true);
      expect(back.prompts.size).toBe(0);
    });

    it("opening review closes the default-open prompts panel", () => {
      const next = toggleZonePanel(empty, "t1", "review", true);
      expect([...next.review]).toEqual(["t1"]);
      expect([...next.prompts]).toEqual(["t1"]);
    });

    it("opening review leaves an already-closed prompts panel closed", () => {
      const start = { prompts: new Set(["t1"]), review: new Set<string>() };
      const next = toggleZonePanel(start, "t1", "review", true);
      expect(next.prompts).toBe(start.prompts);
    });

    it("prompts hidden behind review open in one click, closing review", () => {
      // The state a global "show" flip leaves when t1 had review open.
      const start = { prompts: new Set<string>(), review: new Set(["t1"]) };
      const next = toggleZonePanel(start, "t1", "prompts", true);
      expect(next.prompts).toBe(start.prompts);
      expect(next.review.size).toBe(0);
    });

    it("a stale review entry does not hide prompts where review is unavailable", () => {
      const start = { prompts: new Set<string>(), review: new Set(["t1"]) };
      const next = toggleZonePanel(start, "t1", "prompts", true, false);
      expect([...next.prompts]).toEqual(["t1"]);
    });

    it("re-opening prompts closes review", () => {
      const start = { prompts: new Set(["t1"]), review: new Set(["t1"]) };
      const next = toggleZonePanel(start, "t1", "prompts", true);
      expect(next.prompts.size).toBe(0);
      expect(next.review.size).toBe(0);
    });
  });
});
