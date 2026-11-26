/**
 * The AI context lists interactive elements only. An element with no
 * `is_interactive` of its own is classified by the fallback, whose "has
 * actions" arm must not count a media row: since `@qontinui/ui-bridge` 0.46 a
 * media row advertises the every-element verbs (`click`, `sendKeys`, …).
 */

import { describe, expect, it } from "vitest";

import type { ExternalElement } from "../../types/ui-bridge-types";
import { generateAIContext } from "./generateAIContext";

function element(overrides: Partial<ExternalElement>): ExternalElement {
  return {
    id: "el",
    tagName: "div",
    type: "generic",
    bounds: { x: 0, y: 0, width: 10, height: 10 },
    visible: true,
    enabled: true,
    focused: false,
    actions: [],
    ...overrides,
  };
}

describe("generateAIContext interactive filter", () => {
  it("lists an element with actions but not a media or content row with the same actions", () => {
    const verbs = ["click", "sendKeys", "scroll"];
    const text = generateAIContext([
      element({ id: "save", tagName: "div", label: "Save", actions: verbs }),
      element({
        id: "logo",
        tagName: "canvas",
        type: "canvas",
        label: "Logo",
        category: "media",
        actions: verbs,
      }),
      element({ id: "intro", label: "Intro", category: "content", actions: verbs }),
    ]);
    expect(text).toContain("Interactive Elements (1):");
    expect(text).toContain('"Save"');
    expect(text).not.toContain("Logo");
    expect(text).not.toContain("Intro");
  });
});
