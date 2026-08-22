/**
 * CoordModeContext tests — plan
 * `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` §6.4.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom — see
 * `FleetHealthPanel.test.tsx` for the same constraint), so these exercise the
 * pure derivations and the shared fetch rather than a rendered tree. Those
 * are the load-bearing parts: every gated surface branches on
 * `deriveCoordGating`'s output, and the panels' own derivations
 * (`deriveFleetView`, `deriveSpawnFormState`) are tested beside them.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import {
  COORD_SOURCE_NO_ACCOUNT,
  COORD_SOURCE_SETTINGS_UNREADABLE,
  deriveCoordAvailability,
  deriveCoordGating,
  fetchCoordModeOnce,
  resetCoordModeCache,
  type CoordMode,
} from "../CoordModeContext";

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const CONNECTED: CoordMode = {
  mode: "connected",
  base: "https://coord.qontinui.io",
  source: "tier_default",
};

const ISOLATED: CoordMode = {
  mode: "isolated",
  base: null,
  source: COORD_SOURCE_NO_ACCOUNT,
};

const ISOLATED_UNREADABLE: CoordMode = {
  mode: "isolated",
  base: null,
  source: COORD_SOURCE_SETTINGS_UNREADABLE,
};

beforeEach(() => {
  mockInvoke.mockReset();
  resetCoordModeCache();
});

describe("deriveCoordAvailability", () => {
  it("reports the mode the backend resolved", () => {
    expect(deriveCoordAvailability(CONNECTED)).toBe("connected");
    expect(deriveCoordAvailability(ISOLATED)).toBe("isolated");
  });

  it("reports UNKNOWN while loading and after an invoke failure", () => {
    expect(deriveCoordAvailability(null)).toBe("unknown");
    expect(deriveCoordAvailability(null)).toBe("unknown");
  });
});

describe("deriveCoordGating", () => {
  it("leaves coord-backed surfaces live on a connected runner", () => {
    const gating = deriveCoordGating(CONNECTED);
    expect(gating).toEqual({
      enabled: true,
      isolated: false,
      availability: "connected",
      source: "tier_default",
    });
  });

  it("closes the gate on a positive isolated mode and carries the source through", () => {
    const gating = deriveCoordGating(ISOLATED);
    expect(gating.enabled).toBe(false);
    expect(gating.isolated).toBe(true);
    expect(gating.source).toBe(COORD_SOURCE_NO_ACCOUNT);
  });

  it("distinguishes the unreadable-settings isolated arm by source", () => {
    const gating = deriveCoordGating(ISOLATED_UNREADABLE);
    expect(gating.isolated).toBe(true);
    expect(gating.source).toBe(COORD_SOURCE_SETTINGS_UNREADABLE);
  });

  it("FAILS OPEN when the invoke rejected — unknown never disables a surface", () => {
    // A runner build predating the backend half of §6.4 rejects
    // `get_coord_mode`. A wrongly-disabled panel on a connected runner
    // removes a working feature; a wrongly-enabled one on an isolated
    // runner just shows the panel's normal error. So unknown stays live.
    const gating = deriveCoordGating(null);
    expect(gating.enabled).toBe(true);
    expect(gating.isolated).toBe(false);
    expect(gating.availability).toBe("unknown");
    expect(gating.source).toBeNull();
  });

  it("FAILS OPEN while the first resolution is still in flight", () => {
    const gating = deriveCoordGating(null);
    expect(gating.enabled).toBe(true);
    expect(gating.isolated).toBe(false);
  });
});

describe("fetchCoordModeOnce", () => {
  it("invokes get_coord_mode exactly once no matter how many consumers ask", async () => {
    mockInvoke.mockResolvedValue(ISOLATED);

    const [a, b, c] = await Promise.all([
      fetchCoordModeOnce(),
      fetchCoordModeOnce(),
      fetchCoordModeOnce(),
    ]);
    // A late consumer mounting after the first settle must also reuse it.
    const d = await fetchCoordModeOnce();

    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("get_coord_mode");
    expect(a).toBe(b);
    expect(b).toBe(c);
    expect(c).toBe(d);
  });

  it("does not cache a rejection, so an explicit refresh can retry", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("Command get_coord_mode not found"));
    await expect(fetchCoordModeOnce()).rejects.toThrow("Command get_coord_mode not found");

    mockInvoke.mockResolvedValueOnce(CONNECTED);
    await expect(fetchCoordModeOnce()).resolves.toEqual(CONNECTED);
    expect(mockInvoke).toHaveBeenCalledTimes(2);
  });

  it("re-invokes after an explicit cache reset (the refresh path)", async () => {
    mockInvoke.mockResolvedValue(CONNECTED);
    await fetchCoordModeOnce();
    resetCoordModeCache();
    await fetchCoordModeOnce();
    expect(mockInvoke).toHaveBeenCalledTimes(2);
  });
});
