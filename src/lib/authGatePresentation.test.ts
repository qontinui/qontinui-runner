import { describe, expect, it } from "vitest";

import type { AuthGate } from "./authGate";
import { resolveAuthGatePresentation } from "./authGatePresentation";

/**
 * P2 (auth-gate overlay): the load-bearing invariant is that NO auth verdict
 * that leaves a renderable tree underneath may unmount the app tree.
 * `"loading"` and `"login"` must therefore present as overlays (tree stays
 * mounted). Exactly ONE early return remains: `"tier-unknown"` (the
 * NO-DOWNGRADE hold — the tier is unreadable, so nothing tier-derived may
 * render underneath).
 *
 * `"wizard"` used to be the second early return. It is not a gate surface at
 * all any more: `App` mounts the wizard on `setupCompleted === false` alone, so
 * no auth verdict can destroy the operator's wizard progress. That reasoning
 * error ("nothing to keep mounted") is what made every Tier-step click reset
 * the wizard to step 0.
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

  it("presents 'tier-unknown' as an early return (NO-DOWNGRADE hold: no tier-derived tree may render underneath)", () => {
    expect(resolveAuthGatePresentation("tier-unknown")).toEqual({
      mode: "early-return",
      surface: "tier-unknown",
    });
  });

  it("presents 'app' with no gate surface", () => {
    expect(resolveAuthGatePresentation("app")).toEqual({ mode: "none" });
  });

  // R2 — a `loading → wizard-visible` transition must leave the wizard mounted.
  // The wizard is rendered on `setupCompleted === false`, which is not an input
  // to this function at all; the strongest statement available here is that no
  // gate reachable while setup is incomplete presents as an early return.
  it("no gate reachable during first-run setup replaces the tree", () => {
    // `resolveAuthGate` can only return these two while setupCompleted is
    // false ("login" is suppressed, "tier-unknown" holds by design).
    for (const gate of ["loading", "app"] as const) {
      expect(resolveAuthGatePresentation(gate).mode).not.toBe("early-return");
    }
  });

  it("never early-returns for a gate that has a live tree underneath (exhaustive)", () => {
    // Record<AuthGate, true> forces this list to grow with the type — a new
    // AuthGate variant is a compile error here, not a silently untested gate.
    const allGates: Record<AuthGate, true> = {
      loading: true,
      login: true,
      "tier-unknown": true,
      app: true,
    };
    const gates = Object.keys(allGates) as AuthGate[];
    const earlyReturnGates = new Set<AuthGate>(["tier-unknown"]);
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
