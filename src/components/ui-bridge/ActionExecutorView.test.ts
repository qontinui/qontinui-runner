/**
 * Which verbs the UI Bridge inspector's action executor offers for an element.
 *
 * The served lists come from `servedActions.0.46.json`, which the frontend
 * tests measure against the real SDK, so these assertions follow what
 * `@qontinui/ui-bridge` actually advertises rather than a hand-written copy.
 */

import { describe, expect, it } from "vitest";

import servedActions from "@/lib/ui-bridge/servedActions.0.46.json";

import { availableActionsFor, paramsFor, parseKeys } from "./ActionExecutorView";

const ids = (element: Parameters<typeof availableActionsFor>[0]) =>
  availableActionsFor(element).map((a) => a.id);

describe("availableActionsFor", () => {
  it("offers setValue on an auto-registered text input, through the value field", () => {
    const offered = availableActionsFor({
      type: "input",
      actions: servedActions.autoRegisteredTextInput,
    });
    const setValue = offered.find((a) => a.id === "setValue");
    expect(setValue).toBeDefined();
    expect(setValue?.paramType).toBe("value");
    expect(offered.map((a) => a.id)).toEqual(
      expect.arrayContaining(["click", "type", "clear", "setValue"]),
    );
  });

  it("offers only advertised verbs: a button gets no text or value verbs", () => {
    const offered = ids({ type: "button", actions: servedActions.autoRegisteredButton });
    expect(offered).toEqual([
      "click",
      "doubleClick",
      "focus",
      "blur",
      "hover",
      "hoverClick",
      "rightClick",
      "middleClick",
      "unhover",
      "scrollIntoView",
      "sendKeys",
    ]);
  });

  it("offers the advertised pointer verbs and sendKeys, sendKeys through the keys field", () => {
    const offered = availableActionsFor({
      type: "input",
      actions: servedActions.autoRegisteredTextInput,
    });
    expect(offered.map((a) => a.id)).toEqual(
      expect.arrayContaining([
        "hoverClick",
        "rightClick",
        "middleClick",
        "unhover",
        "scrollIntoView",
        "sendKeys",
      ]),
    );
    const sendKeys = offered.find((a) => a.id === "sendKeys");
    expect(sendKeys?.requiresParams).toBe(true);
    expect(sendKeys?.paramType).toBe("keys");
  });

  it("never guesses the advertised-only verbs from the kind", () => {
    for (const type of ["input", "textarea", "button", "checkbox", "select"]) {
      const offered = ids({ type });
      for (const id of [
        "hoverClick",
        "rightClick",
        "middleClick",
        "unhover",
        "scrollIntoView",
        "sendKeys",
      ]) {
        expect(offered).not.toContain(id);
      }
    }
  });

  it("offers each verb once when an element advertises both select and setValue", () => {
    const offered = ids({ type: "input", actions: servedActions.commandBarInput });
    expect(offered.filter((id) => id === "setValue")).toHaveLength(1);
    expect(offered).toEqual(expect.arrayContaining(["select", "setValue", "toggle"]));
  });

  it("every offered verb is one the element advertises", () => {
    for (const actions of [
      servedActions.autoRegisteredTextInput,
      servedActions.commandBarInput,
      servedActions.autoRegisteredButton,
    ]) {
      for (const id of ids({ type: "input", actions })) {
        expect(actions).toContain(id);
      }
    }
  });

  it("falls back to the kind when nothing is advertised", () => {
    expect(ids({ type: "textarea" })).toEqual(expect.arrayContaining(["type", "setValue"]));
    expect(ids({ type: "button", actions: [] })).not.toContain("setValue");
    expect(ids({ type: "checkbox" })).toEqual(expect.arrayContaining(["check", "toggle"]));
  });
});

describe("parseKeys", () => {
  it("splits on any whitespace and drops empties", () => {
    expect(parseKeys("  ctrl+a   Backspace\tEnter ")).toEqual(["ctrl+a", "Backspace", "Enter"]);
    expect(parseKeys("   ")).toEqual([]);
  });
});

describe("paramsFor", () => {
  const fields = { text: "hello", value: "v1", keys: " ctrl+a  Enter " };

  it("sends each param under the name the SDK reads", () => {
    expect(paramsFor({ paramType: "text" }, fields)).toEqual({ text: "hello" });
    expect(paramsFor({ paramType: "value" }, fields)).toEqual({ value: "v1" });
    expect(paramsFor({ paramType: "keys" }, fields)).toEqual({ keys: ["ctrl+a", "Enter"] });
  });

  it("sends {} for a no-param action or an empty field", () => {
    expect(paramsFor({}, fields)).toEqual({});
    expect(paramsFor({ paramType: "keys" }, { ...fields, keys: "   " })).toEqual({});
    expect(paramsFor({ paramType: "value" }, { ...fields, value: "" })).toEqual({});
  });
});
