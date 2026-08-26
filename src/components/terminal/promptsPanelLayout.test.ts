import { describe, it, expect } from "vitest";
import {
  promptsPanelAvailable,
  promptsPanelOrientation,
  zoneBodyPadding,
} from "./promptsPanelLayout";
import { PROMPTS_PANEL_TOP_HEIGHT_PX, PROMPTS_PANEL_RIGHT_WIDTH_PX } from "./ZonePromptsPanel";

const HEADER = 20;
const FILTER = 26;

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
    expect(promptsPanelOrientation(true)).toBe("right");
  });

  it("gives a tiled zone the top strip", () => {
    expect(promptsPanelOrientation(false)).toBe("top");
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
      }),
    ).toEqual({ top: HEADER + FILTER, right: 0 });
  });

  it("reserves zero when the zone renders no chrome at all", () => {
    expect(
      zoneBodyPadding({ zoneHeaderPx: 0, filterBarPx: 0, promptsOpen: false, isSingleView: false }),
    ).toEqual({ top: 0, right: 0 });
  });

  it("adds the prompts strip below existing chrome in a tiled zone", () => {
    expect(
      zoneBodyPadding({
        zoneHeaderPx: HEADER,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView: false,
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
      }),
    ).toEqual({ top: HEADER, right: PROMPTS_PANEL_RIGHT_WIDTH_PX });
  });

  it("never reserves both axes at once", () => {
    for (const isSingleView of [true, false]) {
      const pad = zoneBodyPadding({
        zoneHeaderPx: 0,
        filterBarPx: 0,
        promptsOpen: true,
        isSingleView,
      });
      expect(pad.top === 0 || pad.right === 0).toBe(true);
    }
  });
});
