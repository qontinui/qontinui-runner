/**
 * `generateQuerySuggestions` routes an element to the generator of the field it
 * is. `@qontinui/ui-bridge` 0.46 kinds an ARIA textbox / contenteditable
 * `textbox` and a combobox / listbox `combobox` / `listbox`, which the registry
 * used to report as `input` and `select`; each still gets its suggestion.
 */

import { describe, expect, it } from "vitest";

import type { ExternalElement } from "../../types/ui-bridge-types";
import { generateQuerySuggestions } from "./querySuggestions";

function element(id: string, type: string, label: string): ExternalElement {
  return {
    id,
    tagName: "div",
    type,
    bounds: { x: 0, y: 0, width: 10, height: 10 },
    visible: true,
    enabled: true,
    focused: false,
    actions: ["click"],
    label,
    is_interactive: true,
  };
}

function reasonsFor(el: ExternalElement): string[] {
  return generateQuerySuggestions([el]).map((s) => s.reason);
}

describe("generateQuerySuggestions by element kind", () => {
  it("a combobox and a listbox get the dropdown suggestion a select gets", () => {
    for (const type of ["select", "combobox", "listbox"]) {
      expect(reasonsFor(element(`f-${type}`, type, "Country")), type).toEqual(["Dropdown menu"]);
    }
  });

  it("a textbox gets the field suggestion an input gets", () => {
    const [input] = generateQuerySuggestions([element("f-input", "input", "Notes")]);
    const [textbox] = generateQuerySuggestions([element("f-textbox", "textbox", "Notes")]);
    expect(textbox?.reason).toBe(input?.reason);
    expect(textbox?.action).toBe(input?.action);
  });

  it("negative control: an option row is not a dropdown and gets no suggestion", () => {
    expect(reasonsFor(element("f-option", "option", "Canada"))).toEqual([]);
  });
});
