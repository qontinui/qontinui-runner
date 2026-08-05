/**
 * element-adapter tests.
 *
 * Covers the §4.6 scrubbed-string branding introduced alongside the
 * ui-bridge forward-compat fix: registeredToDiscovered's accessibleName
 * preference (state over raw aria-label) and externalToDiscovered's
 * label/accessibleName/textContent/value fallback chains. Runs under
 * vitest's default "node" environment — `element` is cast rather than
 * built from a real HTMLElement since only the two accessors the adapter
 * calls (`getAttribute`, `tagName`) are exercised.
 */

import { describe, expect, it } from "vitest";

import type { ElementType, RegisteredElement, StandardAction } from "@qontinui/ui-bridge";
import type { ExternalElement } from "../../types/ui-bridge-types";
import { externalToDiscovered, registeredToDiscovered } from "./element-adapter";

function fakeDomElement(attrs: Record<string, string>, tagName = "BUTTON"): HTMLElement {
  return {
    tagName,
    getAttribute: (name: string) => attrs[name] ?? null,
  } as unknown as HTMLElement;
}

function registeredElement(overrides: Partial<RegisteredElement> = {}): RegisteredElement {
  return {
    id: "el-1",
    type: "button" as ElementType,
    label: "Save",
    element: fakeDomElement({ "aria-label": "aria fallback label" }),
    actions: ["click"] as StandardAction[],
    getState: () => ({
      visible: true,
      enabled: true,
      focused: false,
      rect: { x: 0, y: 0, width: 10, height: 10, top: 0, right: 10, bottom: 10, left: 0 },
    }),
    registeredAt: 0,
    mounted: true,
    ...overrides,
  } as RegisteredElement;
}

describe("registeredToDiscovered", () => {
  it("passes the registration label through as-is", () => {
    const result = registeredToDiscovered(registeredElement({ label: "Save" }));
    expect(result.label).toBe("Save");
  });

  it("prefers the state's computed accessibleName over the raw aria-label attribute", () => {
    const el = registeredElement({
      element: fakeDomElement({ "aria-label": "raw aria-label" }),
      getState: () => ({
        visible: true,
        enabled: true,
        focused: false,
        accessibleName: "computed accessible name",
        rect: { x: 0, y: 0, width: 10, height: 10, top: 0, right: 10, bottom: 10, left: 0 },
      }),
    });
    const result = registeredToDiscovered(el);
    expect(result.accessibleName).toBe("computed accessible name");
  });

  it("falls back to the raw aria-label attribute when the state has no accessibleName", () => {
    const el = registeredElement({
      element: fakeDomElement({ "aria-label": "raw aria-label" }),
      getState: () => ({
        visible: true,
        enabled: true,
        focused: false,
        rect: { x: 0, y: 0, width: 10, height: 10, top: 0, right: 10, bottom: 10, left: 0 },
      }),
    });
    const result = registeredToDiscovered(el);
    expect(result.accessibleName).toBe("raw aria-label");
  });

  it("leaves accessibleName undefined when neither the state nor the attribute has one", () => {
    const el = registeredElement({
      element: fakeDomElement({}),
      getState: () => ({
        visible: true,
        enabled: true,
        focused: false,
        rect: { x: 0, y: 0, width: 10, height: 10, top: 0, right: 10, bottom: 10, left: 0 },
      }),
    });
    const result = registeredToDiscovered(el);
    expect(result.accessibleName).toBeUndefined();
  });

  it("marks the result as registered and copies the actions array by value", () => {
    const actions = ["click", "focus"] as StandardAction[];
    const el = registeredElement({ actions });
    const result = registeredToDiscovered(el);
    expect(result.registered).toBe(true);
    expect(result.actions).toEqual(actions);
    expect(result.actions).not.toBe(actions);
  });
});

describe("externalToDiscovered", () => {
  function externalElement(overrides: Partial<ExternalElement> = {}): ExternalElement {
    return {
      id: "ext-1",
      tagName: "button",
      type: "button",
      bounds: { x: 1, y: 2, width: 3, height: 4 },
      visible: true,
      enabled: true,
      focused: false,
      actions: ["click"],
      ...overrides,
    };
  }

  it("prefers label, then text, then accessibleName", () => {
    expect(externalToDiscovered(externalElement({ label: "L", text: "T" })).label).toBe("L");
    expect(
      externalToDiscovered(externalElement({ text: "T", accessibleName: "A" })).label,
    ).toBe("T");
    expect(externalToDiscovered(externalElement({ accessibleName: "A" })).label).toBe("A");
    expect(externalToDiscovered(externalElement({})).label).toBeUndefined();
  });

  it("prefers accessibleName, then the flattened accessibility.accessibleName", () => {
    const result = externalToDiscovered(
      externalElement({ accessibleName: "top-level", accessibility: { role: "button", accessibleName: "nested" } }),
    );
    expect(result.accessibleName).toBe("top-level");
  });

  it("falls back to accessibility.accessibleName when the top-level field is absent", () => {
    const result = externalToDiscovered(
      externalElement({ accessibility: { role: "button", accessibleName: "nested" } }),
    );
    expect(result.accessibleName).toBe("nested");
  });

  it("derives textContent from text, then accessibleName, then accessibility.accessibleName", () => {
    const result = externalToDiscovered(
      externalElement({ accessibility: { role: "button", accessibleName: "nested" } }),
    );
    expect(result.state.textContent).toBe("nested");
  });

  it("passes value through unchanged", () => {
    const result = externalToDiscovered(externalElement({ value: "typed text" }));
    expect(result.state.value).toBe("typed text");
  });

  it("derives rect edges from bounds when top/right/bottom/left are absent", () => {
    const result = externalToDiscovered(
      externalElement({ bounds: { x: 10, y: 20, width: 30, height: 40 } }),
    );
    expect(result.state.rect).toEqual({
      x: 10,
      y: 20,
      width: 30,
      height: 40,
      top: 20,
      right: 40,
      bottom: 60,
      left: 10,
    });
  });

  it("zeroes the rect when bounds is absent", () => {
    const el = externalElement();
    // @ts-expect-error -- bounds is required on ExternalElement; this covers a
    // defensive runtime path for payloads from older/looser callers.
    delete el.bounds;
    const result = externalToDiscovered(el);
    expect(result.state.rect).toEqual({
      x: 0,
      y: 0,
      width: 0,
      height: 0,
      top: 0,
      right: 0,
      bottom: 0,
      left: 0,
    });
  });

  it("always marks the result as unregistered, regardless of input", () => {
    expect(externalToDiscovered(externalElement()).registered).toBe(false);
  });
});
