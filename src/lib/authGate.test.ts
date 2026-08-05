import { describe, it, expect } from "vitest";
import { resolveAuthGate, type AuthGateInput } from "./authGate";

/** A signed-in Tier-2 runner with everything settled. */
const signedIn: AuthGateInput = {
  authLoading: false,
  authResolving: false,
  devAutoLoginPending: false,
  apiReady: true,
  setupCompleted: true,
  isTier2: true,
  authenticated: true,
  tierUnknown: false,
};

describe("resolveAuthGate", () => {
  it("renders the app for a settled, signed-in Tier-2 runner", () => {
    expect(resolveAuthGate(signedIn)).toBe("app");
  });

  it("renders LoginScreen for a real, explicit sign-out", () => {
    expect(resolveAuthGate({ ...signedIn, authenticated: false })).toBe("login");
  });

  // THE regression this function exists for. While the first auth probe is
  // still being retried, `authStatus` is null — indistinguishable at the gate
  // from a real sign-out. Showing LoginScreen there tells a signed-in operator
  // they are signed out, and they may act on it (the retry chain runs for up to
  // ~a minute).
  it("NEVER renders LoginScreen while the first probe is unresolved", () => {
    expect(
      resolveAuthGate({
        ...signedIn,
        authResolving: true,
        authenticated: false, // authStatus === null reads as not-authenticated
      }),
    ).toBe("loading");
  });

  // `loading` is force-cleared 3s after every transition (so a wedged IPC
  // channel can't hang the app), so the retry chain CANNOT hold the shell with
  // it. This is exactly the case that made the LoginScreen visible for ~55 of
  // the chain's ~67 seconds when only `loading` was used.
  it("keeps the loading shell on an unresolved probe even once `loading` was force-cleared", () => {
    expect(
      resolveAuthGate({
        ...signedIn,
        authLoading: false, // failsafe already forced this false
        authResolving: true,
        authenticated: false,
      }),
    ).toBe("loading");
  });

  // …but it must terminate. Once the chain gives up, the operator has to be
  // able to act; AuthProvider keeps re-probing in the background.
  it("falls through to LoginScreen once the retry chain gives up", () => {
    expect(
      resolveAuthGate({
        ...signedIn,
        authResolving: false,
        authenticated: false,
      }),
    ).toBe("login");
  });

  it("shows the loading shell while auth work is in flight", () => {
    expect(resolveAuthGate({ ...signedIn, authLoading: true })).toBe("loading");
    expect(resolveAuthGate({ ...signedIn, devAutoLoginPending: true })).toBe("loading");
  });

  it("shows the loading shell until the local API server is up", () => {
    expect(resolveAuthGate({ ...signedIn, apiReady: false })).toBe("loading");
  });

  // The wizard is NOT a gate surface any more (see the note on `AuthGate`).
  // `App` mounts it on `setupCompleted === false` alone, so the only thing this
  // function still owes the wizard is: never divert a mid-setup runner to the
  // sign-in screen.
  it("suppresses the sign-in gate while first-run setup is incomplete", () => {
    expect(resolveAuthGate({ ...signedIn, setupCompleted: false, authenticated: false })).toBe(
      "app",
    );
  });

  it("never reports 'wizard' — the wizard is not an auth-gate surface", () => {
    const gates = [
      resolveAuthGate({ ...signedIn, setupCompleted: false }),
      resolveAuthGate({ ...signedIn, setupCompleted: false, authenticated: false }),
      resolveAuthGate({ ...signedIn, setupCompleted: false, isTier2: false }),
    ];
    for (const g of gates) {
      expect(g).not.toBe("wizard");
    }
  });

  // R1 — the P0 this file's wizard cases were rewritten for. A tier RE-read
  // must not move the gate: it used to raise `AuthProvider.loading`, flip the
  // gate off `"wizard"`, and unmount `<SetupWizard>` mid-flight, resetting the
  // operator to step 0 every time they picked a tier. `useRunnerTier` now keeps
  // re-reads on a separate `refreshing` flag that feeds NOTHING here, so with
  // `setupCompleted === false` the verdict is stable.
  it("stays on the wizard's surface while a tier RE-READ is in flight", () => {
    const midSetup: AuthGateInput = { ...signedIn, setupCompleted: false, authenticated: false };
    // A re-read no longer raises authLoading, so the gate does not move…
    expect(resolveAuthGate(midSetup)).toBe("app");
    // …and even if some other transient did raise it, the surface is an
    // OVERLAY, never an early return — the wizard stays mounted underneath.
    expect(resolveAuthGate({ ...midSetup, authLoading: true })).toBe("loading");
  });

  it("does not gate Tier 0/1 on authentication", () => {
    // Tier 0/1 get a synthesized local-guest auth, but even a falsy value must
    // not divert them to a sign-in screen they have no account for.
    expect(resolveAuthGate({ ...signedIn, isTier2: false, authenticated: false })).toBe("app");
  });

  // NO-DOWNGRADE: an unreadable settings.json leaves the tier UNKNOWN. The gate
  // must HOLD on a dedicated screen — never the local-guest app shell (which
  // would silently strip cloud features from a Tier-2 runner) and never
  // LoginScreen (which reads as a sign-out).
  describe("tier-unknown (settings.json unreadable)", () => {
    it("holds on the tier-unknown screen when the tier could not be determined", () => {
      expect(resolveAuthGate({ ...signedIn, tierUnknown: true })).toBe("tier-unknown");
    });

    it("does NOT fall through to the app shell when the tier is unknown", () => {
      // The exact silent downgrade: an unknown-tier runner with a placeholder
      // (non-Tier-2) tier must not render as a local-guest app.
      expect(
        resolveAuthGate({
          ...signedIn,
          tierUnknown: true,
          isTier2: false,
          authenticated: false,
        }),
      ).toBe("tier-unknown");
    });

    it("does NOT show LoginScreen when the tier is unknown, even for a Tier-2 look", () => {
      expect(resolveAuthGate({ ...signedIn, tierUnknown: true, authenticated: false })).toBe(
        "tier-unknown",
      );
    });

    it("holds even when setup is incomplete (never guess a surface from an unknown tier)", () => {
      expect(resolveAuthGate({ ...signedIn, tierUnknown: true, setupCompleted: false })).toBe(
        "tier-unknown",
      );
    });

    it("still shows the loading shell before the tier settles (unknown never wins over loading)", () => {
      // `tierUnknown` is only ever true after the read retries are exhausted;
      // while auth/tier work is in flight the loading shell must win.
      expect(resolveAuthGate({ ...signedIn, tierUnknown: true, authLoading: true })).toBe(
        "loading",
      );
      expect(resolveAuthGate({ ...signedIn, tierUnknown: true, apiReady: false })).toBe("loading");
    });

    it("leaves the tier-unknown screen once the tier resolves", () => {
      // The LEAVE path: a re-read lands a known tier → tierUnknown clears → the
      // normal gate resumes (app for a settled signed-in runner).
      expect(resolveAuthGate({ ...signedIn, tierUnknown: false })).toBe("app");
    });
  });

  it("treats a still-unread setup flag as not-yet-wizard", () => {
    expect(resolveAuthGate({ ...signedIn, setupCompleted: null })).toBe("app");
  });
});
