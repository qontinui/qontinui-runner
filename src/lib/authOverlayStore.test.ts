import { afterEach, describe, expect, it, vi } from "vitest";

import { getSnapshot, setAuthOverlay, subscribe } from "./authOverlayStore";

/**
 * P2 (auth-gate overlay): this store is what makes the fixed-position siblings
 * rendered OUTSIDE `AppContent` — the tutorial/demo/prompt overlays,
 * `BackgroundTaskPill`, `BuildRefreshBanner` — go `inert` in lockstep with the
 * main tree. A missed publish leaves those siblings in the tab order while the
 * login overlay is up (invisible but Enter-activatable), which is the exact
 * leak the store was added to close.
 *
 * vitest runs `environment: "node"` with no React Testing Library, so these
 * drive the store contract (`subscribe`/`getSnapshot`) that `useAuthOverlay`
 * hands to `useSyncExternalStore`, rather than rendering the hook.
 */

// The store is a module-level singleton — reset it between tests.
afterEach(() => {
  setAuthOverlay(null);
});

describe("authOverlayStore", () => {
  it("starts with no overlay", () => {
    expect(getSnapshot()).toBeNull();
  });

  it("publishes a surface and notifies subscribers", () => {
    const listener = vi.fn();
    const unsubscribe = subscribe(listener);

    setAuthOverlay("login");
    expect(getSnapshot()).toBe("login");
    expect(listener).toHaveBeenCalledTimes(1);

    setAuthOverlay("loading");
    expect(getSnapshot()).toBe("loading");
    expect(listener).toHaveBeenCalledTimes(2);

    unsubscribe();
  });

  it("does not notify when the surface is unchanged", () => {
    setAuthOverlay("login");
    const listener = vi.fn();
    const unsubscribe = subscribe(listener);

    setAuthOverlay("login");
    expect(listener).not.toHaveBeenCalled();

    // useSyncExternalStore re-reads getSnapshot on every notify; a redundant
    // publish that still notified would re-render every sibling boundary on
    // each auth probe tick.
    setAuthOverlay(null);
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
  });

  it("notifies every subscriber and stops after unsubscribe", () => {
    const a = vi.fn();
    const b = vi.fn();
    const unsubA = subscribe(a);
    const unsubB = subscribe(b);

    setAuthOverlay("login");
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);

    unsubA();
    setAuthOverlay(null);
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(2);

    unsubB();
  });

  it("survives a listener that unsubscribes another during notification", () => {
    // The store iterates a COPY of the listener set, so a listener that removes
    // itself or another mid-notify must neither throw nor skip the rest —
    // React does exactly this when a component unmounts in response to a store
    // change.
    const later = vi.fn();
    let unsubLater: () => void = () => {};
    const unsubFirst = subscribe(() => {
      unsubLater();
    });
    unsubLater = subscribe(later);

    expect(() => setAuthOverlay("login")).not.toThrow();
    expect(later).toHaveBeenCalledTimes(1);

    unsubFirst();
    unsubLater();
  });

  it("clears back to null so the siblings leave inert when auth settles", () => {
    setAuthOverlay("loading");
    expect(getSnapshot()).toBe("loading");

    setAuthOverlay(null);
    expect(getSnapshot()).toBeNull();
  });
});
