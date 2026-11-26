// @vitest-environment jsdom
/**
 * The Natural Language panel's type bonuses match a typed word against an
 * element's type by the SDK's named-type family (`lib/ui-bridge/namedType.ts`):
 * since `@qontinui/ui-bridge` 0.46 an ARIA textbox is kinded `textbox` and a
 * combobox `combobox`, which the registry used to report as `input` and
 * `select`, so "the notes input" must still find the textbox.
 */

import { describe, expect, it } from "vitest";

import { answersToNamedType, namedTypeFamily } from "@/lib/ui-bridge/namedType";
import type { ExternalElement } from "@/types/ui-bridge-types";

import { findMatchingElements } from "./NaturalLanguagePanel";

/** An element whose id, label and text share nothing with the queries below. */
function element(id: string, type: string): ExternalElement {
  return {
    id,
    tagName: "div",
    type,
    bounds: { x: 0, y: 0, width: 10, height: 10 },
    visible: true,
    enabled: true,
    focused: false,
    actions: [],
  };
}

function reasonFor(target: string, el: ExternalElement): string | undefined {
  return findMatchingElements(target, [el]).find((m) => m.element.id === el.id)?.matchReason;
}

describe("namedTypeFamily / answersToNamedType", () => {
  it("names the whole family for a family member, and only itself otherwise", () => {
    expect(namedTypeFamily("Input")).toEqual(["input", "textbox"]);
    expect(namedTypeFamily("listbox")).toEqual(["select", "combobox", "listbox"]);
    expect(namedTypeFamily("Button")).toEqual(["button"]);
    expect(answersToNamedType("Combobox", "select")).toBe(true);
    expect(answersToNamedType("checkbox", "input")).toBe(false);
    expect(answersToNamedType(undefined, "input")).toBe(false);
  });
});

describe("NaturalLanguagePanel type bonuses", () => {
  it("positive control: a textbox answers to a typed 'input', a combobox to 'select'", () => {
    const textbox = reasonFor("input", element("el-7", "textbox"));
    expect(textbox).toContain("Type matches");
    expect(textbox).toContain('Keyword "input" matches type');

    const combobox = reasonFor("select", element("el-8", "combobox"));
    expect(combobox).toContain("Type matches");
    expect(combobox).toContain('Keyword "select" matches type');
  });

  it("the precise kind still matches its own name", () => {
    expect(reasonFor("input", element("el-9", "input"))).toContain("Type matches");
  });

  it("negative control: a kind outside the family earns no type bonus", () => {
    // `checkbox` and `option` are not in the input / select families, so they
    // score nothing here and are not offered at all.
    expect(reasonFor("input", element("el-10", "checkbox"))).toBeUndefined();
    expect(reasonFor("select", element("el-11", "option"))).toBeUndefined();
  });
});
