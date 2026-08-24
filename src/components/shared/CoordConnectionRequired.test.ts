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

import { coordDisabledCopy, runCoordDisabledAction } from "./CoordConnectionRequired";
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

describe("coordDisabledCopy — hasControls", () => {
  it("names the surface's controls by default", () => {
    const copy = coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Fleet health");
    expect(copy.body).toContain("nothing for these controls to act on");
  });

  it("does not invent controls for a read-only section", () => {
    // The File Activity live heatmap renders a list and owns no controls;
    // telling its reader there is "nothing for these controls to act on"
    // names widgets that are not on screen.
    const copy = coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Live worktree heatmap", {
      hasControls: false,
    });
    expect(copy.body).toContain("nothing for it to show");
    expect(copy.body).not.toContain("these controls");
  });

  it("is irrelevant to the settings-unreadable arm, which names neither", () => {
    for (const hasControls of [true, false]) {
      const copy = coordDisabledCopy(COORD_SOURCE_SETTINGS_UNREADABLE, "Fleet health", {
        hasControls,
      });
      expect(copy.body).not.toContain("these controls");
      expect(copy.body).not.toContain("nothing for it to show");
    }
  });
});

describe("coordDisabledCopy — tooltip agrees with the body", () => {
  // The defect this pins: a hard-coded "connect an account" tooltip 8px from
  // a body saying "this is NOT a missing account" puts two contradictory
  // diagnoses on one screen and the operator follows the wrong one. Both
  // strings come from one call, so they cannot disagree — assert the pairing
  // rather than the exact prose.
  it("blames the account only when the body does", () => {
    const noAccount = coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Fleet health");
    expect(noAccount.tooltip).toContain("no qontinui account is connected");
    expect(noAccount.tooltip).not.toContain("settings.json");
  });

  it("blames settings.json only when the body does", () => {
    const unreadable = coordDisabledCopy(COORD_SOURCE_SETTINGS_UNREADABLE, "Fleet health");
    expect(unreadable.tooltip).toContain("settings.json");
    expect(unreadable.tooltip).toContain("will not fix it");
    expect(unreadable.tooltip).not.toContain("no qontinui account is connected");
  });
});

describe("runCoordDisabledAction", () => {
  it("dismisses the host BEFORE navigating", () => {
    // Ordering is the whole contract: the action's host may be a
    // `fixed inset-0` overlay (SpawnFromPlanModal), so navigating first
    // switches a tab that stays hidden behind it — the exact "clicked and
    // nothing happened" failure this notice exists to remove.
    const calls: string[] = [];
    runCoordDisabledAction({
      copy: coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Spawn from Plan"),
      onDismiss: () => calls.push("dismiss"),
      dispatch: (tab) => calls.push(`dispatch:${tab}`),
    });
    expect(calls).toEqual(["dismiss", "dispatch:settings-account"]);
  });

  it("navigates when the host supplies no dismiss handler", () => {
    const dispatched: string[] = [];
    runCoordDisabledAction({
      copy: coordDisabledCopy(COORD_SOURCE_NO_ACCOUNT, "Fleet health"),
      dispatch: (tab) => dispatched.push(tab),
    });
    expect(dispatched).toEqual(["settings-account"]);
  });

  it("does nothing at all when the copy has no in-app action", () => {
    // The settings-unreadable repair happens on disk. Dismissing the host
    // for an action that then cannot run would close a modal for nothing.
    const calls: string[] = [];
    runCoordDisabledAction({
      copy: coordDisabledCopy(COORD_SOURCE_SETTINGS_UNREADABLE, "Fleet health"),
      onDismiss: () => calls.push("dismiss"),
      dispatch: (tab) => calls.push(`dispatch:${tab}`),
    });
    expect(calls).toEqual([]);
  });
});
