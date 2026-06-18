/**
 * Unit coverage for the NL planner's component-action retarget fallback.
 *
 * The homepage NL planner names `terminal-launch-menu` for the AI-spawn flow,
 * but that in-terminal-tree component (and `terminal-page`) are observed NOT to
 * register in the live runner registry. The active-tab `page-terminal`
 * component — registered via TabContent's `PageRegistration` path — DOES
 * reliably register and exposes the same registry-backed spawn actions
 * (`create-best-account`, etc.). `resolveComponentForAction` must retarget the
 * planner-named component to `page-terminal` whenever the named component is
 * absent but `page-terminal` is registered and exposes the requested action.
 *
 * Runner vitest config is `environment: "node"` — these helpers are pure
 * (they read a structurally-typed `bridge.getComponent`), so no React tree is
 * needed.
 */

import { describe, it, expect } from "vitest";
import { resolveComponentForAction, COMPONENT_FALLBACKS } from "./usePromptExecution";

type Comp = { actions?: Array<{ id: string }> };

/** Build a minimal bridge stub from an id → component map. */
function bridgeWith(registry: Record<string, Comp>) {
  return {
    getComponent: (id: string): Comp | undefined => registry[id],
  };
}

describe("resolveComponentForAction", () => {
  it("retargets terminal-launch-menu → page-terminal when only page-terminal is registered", () => {
    const bridge = bridgeWith({
      "page-terminal": { actions: [{ id: "create-best-account" }, { id: "create-plain" }] },
    });
    expect(resolveComponentForAction(bridge, "terminal-launch-menu", "create-best-account")).toBe(
      "page-terminal",
    );
  });

  it("retargets terminal-page → page-terminal when terminal-page is absent", () => {
    const bridge = bridgeWith({
      "page-terminal": { actions: [{ id: "create-best-account" }] },
    });
    expect(resolveComponentForAction(bridge, "terminal-page", "create-best-account")).toBe(
      "page-terminal",
    );
  });

  it("prefers the planner-named component when it is registered and exposes the action", () => {
    const bridge = bridgeWith({
      "terminal-launch-menu": { actions: [{ id: "create-best-account" }] },
      "page-terminal": { actions: [{ id: "create-best-account" }] },
    });
    expect(resolveComponentForAction(bridge, "terminal-launch-menu", "create-best-account")).toBe(
      "terminal-launch-menu",
    );
  });

  it("falls back through the chain to page-terminal when terminal-page lacks the action", () => {
    const bridge = bridgeWith({
      // terminal-page registered but WITHOUT the action; page-terminal has it.
      "terminal-page": { actions: [{ id: "create-plain" }] },
      "page-terminal": { actions: [{ id: "create-best-account" }] },
    });
    expect(resolveComponentForAction(bridge, "terminal-launch-menu", "create-best-account")).toBe(
      "page-terminal",
    );
  });

  it("returns the original component id when nothing better is registered", () => {
    const bridge = bridgeWith({});
    expect(resolveComponentForAction(bridge, "terminal-launch-menu", "create-best-account")).toBe(
      "terminal-launch-menu",
    );
  });

  it("lists page-terminal first in the terminal-launch-menu fallback chain (most reliable first)", () => {
    expect(COMPONENT_FALLBACKS["terminal-launch-menu"][0]).toBe("page-terminal");
    expect(COMPONENT_FALLBACKS["terminal-page"]).toContain("page-terminal");
  });
});
