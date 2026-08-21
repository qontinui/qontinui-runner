/**
 * Copy tests for the isolated-mode disabled affordance — plan
 * `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration` §6.4.
 *
 * The two isolated `source` arms are two different problems with two
 * different fixes, and conflating them is the specific failure these tests
 * pin: telling an operator whose `settings.json` is unreadable to "connect an
 * account" sends them to a screen that cannot help and leaves the real fault
 * undiagnosed.
 *
 * `environment: "node"` (no jsdom), so this exercises the pure copy
 * derivation rather than the rendered notice.
 */

import { describe, expect, it } from "vitest";

import { coordDisabledCopy } from "./CoordConnectionRequired";
import {
  COORD_SOURCE_NO_ACCOUNT,
  COORD_SOURCE_SETTINGS_UNREADABLE,
} from "@/contexts/CoordModeContext";

describe("coordDisabledCopy", () => {
  it("tells an unconfigured runner to connect a qontinui account", () => {
    const copy = coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Fleet health");
    expect(copy.reason).toBe("no-account");
    expect(copy.title).toContain("qontinui account");
    expect(copy.body).toContain("isolated mode");
    expect(copy.body).toContain("Settings → Account");
    // The fix is in-app, so there is a real focusable action.
    expect(copy.actionLabel).toBe("Open Settings → Account");
    expect(copy.actionTab).toBe("settings-account");
    // It must NOT blame the config file — nothing is wrong with it.
    expect(copy.body).not.toContain("settings.json");
  });

  it("tells an unreadable-settings runner to repair settings.json instead", () => {
    const copy = coordDisabledCopy(COORD_SOURCE_SETTINGS_UNREADABLE, "Fleet health");
    expect(copy.reason).toBe("settings-unreadable");
    expect(copy.title).toContain("settings.json");
    expect(copy.body).toContain("settings.json");
    expect(copy.body).toContain("restart the runner");
    // Explicitly disowns the wrong fix.
    expect(copy.body).toContain("NOT a missing account");
    // The repair happens on disk; offering an in-app button that cannot
    // perform it would be the same dishonesty this component removes.
    expect(copy.actionLabel).toBeNull();
    expect(copy.actionTab).toBeNull();
  });

  it("falls back to the no-account message for an unrecognised source", () => {
    for (const source of [null, "", "some_future_arm"]) {
      expect(coordDisabledCopy(source, "Fleet health").reason).toBe("no-account");
    }
  });

  it("names the surface it belongs to so stacked notices stay distinguishable", () => {
    expect(coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Overlapping intents").title).toContain(
      "Overlapping intents",
    );
    expect(coordDisabledCopy(COORD_SOURCE_SETTINGS_UNREADABLE, "Spawn from Plan").title).toContain(
      "Spawn from Plan",
    );
  });
});
