/**
 * Which verbs the UI Bridge inspector's action executor offers for an element.
 *
 * The served lists come from `servedActions.0.46.json`, which the frontend
 * tests measure against the real SDK, so these assertions follow what
 * `@qontinui/ui-bridge` actually advertises rather than a hand-written copy.
 */

import { describe, expect, it } from "vitest";

import servedActions from "@/lib/ui-bridge/servedActions.0.46.json";

import { availableActionsFor } from "./ActionExecutorView";

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
    expect(offered).toEqual(["click", "doubleClick", "focus", "blur", "hover"]);
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
