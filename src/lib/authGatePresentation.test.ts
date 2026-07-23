import { describe, expect, it } from "vitest";

import type { AuthGate } from "./authGate";
import { resolveAuthGatePresentation } from "./authGatePresentation";

/**
 * P2 (auth-gate overlay): the load-bearing invariant is that NO auth verdict
 * that leaves a renderable tree underneath may unmount the app tree.
 * `"loading"` and `"login"` must therefore present as overlays (tree stays
 * mounted). The two early returns are the surfaces where there IS no tree to
 * keep: `"wizard"` (renders before any terminal tree exists) and
 * `"tier-unknown"` (the NO-DOWNGRADE hold — the tier is unreadable, so nothing
 * tier-derived may render underneath).
 */
describe("resolveAuthGatePresentation", () => {
  it("presents 'loading' as an overlay — an unsettled auth verdict must never unmount the tree", () => {
    expect(resolveAuthGatePresentation("loading")).toEqual({
      mode: "overlay",
      surface: "loading",
    });
  });

  it("presents 'login' as an overlay — a sign-out (real or transient) must never unmount the tree", () => {
    expect(resolveAuthGatePresentation("login")).toEqual({
      mode: "overlay",
      surface: "login",
    });
  });

  it("presents 'wizard' as an early return (pre-setup: no terminal tree exists yet)", () => {
    expect(resolveAuthGatePresentation("wizard")).toEqual({
      mode: "early-return",
      surface: "wizard",
    });
  });

  it("presents 'tier-unknown' as an early return (NO-DOWNGRADE hold: no tier-derived tree may render underneath)", () => {
    expect(resolveAuthGatePresentation("tier-unknown")).toEqual({
      mode: "early-return",
      surface: "tier-unknown",
    });
  });

  it("presents 'app' with no gate surface", () => {
    expect(resolveAuthGatePresentation("app")).toEqual({ mode: "none" });
  });

  it("never early-returns for a gate that has a live tree underneath (exhaustive)", () => {
    // Record<AuthGate, true> forces this list to grow with the type — a new
    // AuthGate variant is a compile error here, not a silently untested gate.
    const allGates: Record<AuthGate, true> = {
      loading: true,
      wizard: true,
      login: true,
      "tier-unknown": true,
      app: true,
    };
    const gates = Object.keys(allGates) as AuthGate[];
    const earlyReturnGates = new Set<AuthGate>(["wizard", "tier-unknown"]);
    for (const gate of gates) {
      const presentation = resolveAuthGatePresentation(gate);
      if (earlyReturnGates.has(gate)) {
        expect(presentation.mode).toBe("early-return");
      } else {
        expect(presentation.mode).not.toBe("early-return");
      }
    }
  });
});
