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

  it("runs the setup wizard before any auth gate", () => {
    expect(resolveAuthGate({ ...signedIn, setupCompleted: false, authenticated: false })).toBe(
      "wizard",
    );
    // …but never before auth has settled (the wizard must not flash either).
    expect(resolveAuthGate({ ...signedIn, setupCompleted: false, authResolving: true })).toBe(
      "loading",
    );
  });

  it("does not gate Tier 0/1 on authentication", () => {
    // Tier 0/1 get a synthesized local-guest auth, but even a falsy value must
    // not divert them to a sign-in screen they have no account for.
    expect(resolveAuthGate({ ...signedIn, isTier2: false, authenticated: false })).toBe("app");
  });

  it("treats a still-unread setup flag as not-yet-wizard", () => {
    expect(resolveAuthGate({ ...signedIn, setupCompleted: null })).toBe("app");
  });
});
