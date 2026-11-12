/**
 * `mapSdkElement`'s interactivity, which the state explorer
 * (`useStateExploration.ts`) and the AI context (`lib/ui-bridge/generateAIContext.ts`)
 * read. Since `@qontinui/ui-bridge` 0.46 a media row advertises the verbs every
 * element takes (`EVERY_ELEMENT` in `core/element-kind.ts`), so "has actions"
 * would make every image a control the explorer clicks.
 */

import { describe, expect, it } from "vitest";

import {
  advertisesExplorationClick,
  isContentOrMediaRow,
  isInteractiveElementRow,
  mapSdkElement,
} from "./utils";

/** `EVERY_ELEMENT` at ui-bridge `9f373a12` — served on every kind, media included. */
const EVERY_ELEMENT = [
  "click",
  "hoverClick",
  "doubleClick",
  "rightClick",
  "middleClick",
  "sendKeys",
  "focus",
  "blur",
  "hover",
  "unhover",
  "scroll",
  "scrollIntoView",
  "drag",
];

describe("isInteractiveElementRow", () => {
  it("a media row is not interactive however many verbs it advertises", () => {
    for (const type of ["image", "video", "canvas", "svg", "picture"]) {
      const row = { id: "m", type, category: "media", actions: EVERY_ELEMENT };
      expect(isContentOrMediaRow(row)).toBe(true);
      expect(isInteractiveElementRow(row)).toBe(false);
      expect(mapSdkElement(row).is_interactive).toBe(false);
    }
  });

  it("a content row is not interactive", () => {
    expect(
      isInteractiveElementRow({ id: "c", type: "generic", kind: "content", actions: [] }),
    ).toBe(false);
    expect(
      isInteractiveElementRow({
        id: "c",
        type: "heading",
        category: "content",
        actions: ["click"],
      }),
    ).toBe(false);
  });

  it("an interactive row is interactive by its actions or by its type", () => {
    expect(
      isInteractiveElementRow({
        id: "b",
        type: "generic",
        category: "interactive",
        actions: ["click"],
      }),
    ).toBe(true);
    // A registration that declared `actions: []` is still a control of its kind
    // (the SDK's `INTERACTIVE_ELEMENT_TYPES` arm).
    expect(
      isInteractiveElementRow({ id: "t", type: "textarea", category: "interactive", actions: [] }),
    ).toBe(true);
    expect(isInteractiveElementRow({ id: "g", type: "generic", actions: [] })).toBe(false);
  });

  it("mapSdkElement carries the row's category", () => {
    expect(mapSdkElement({ id: "m", type: "image", category: "media" }).category).toBe("media");
    expect(mapSdkElement({ id: "x", type: "button", category: "bogus" }).category).toBeUndefined();
  });
});

describe("advertisesExplorationClick", () => {
  it("queues a control that advertises click", () => {
    expect(
      advertisesExplorationClick({
        type: "button",
        category: "interactive",
        actions: EVERY_ELEMENT,
      }),
    ).toBe(true);
  });

  it("negative controls: a declared `actions: []` control and a media row are not queued", () => {
    // The terminal proxy pane registers `type: "textarea", actions: []`: a
    // control for display (`isInteractiveElementRow`), but the runner's door
    // refuses a click on it.
    const proxy = { type: "textarea", category: "interactive", actions: [] };
    expect(isInteractiveElementRow(proxy)).toBe(true);
    expect(advertisesExplorationClick(proxy)).toBe(false);
    expect(
      advertisesExplorationClick({ type: "image", category: "media", actions: EVERY_ELEMENT }),
    ).toBe(false);
  });
});
