/**
 * Tab-routing invariants (manual-test-loop iter 2).
 *
 * Iteration 1 replaced TabContent's `default: return null` with a visible
 * "This page could not be displayed" fallback — which immediately exposed 10
 * `MainTabId` members that no `TabContent` case rendered. Five of them
 * (`settings-notifications`, `-otel`, `-containers`, `-ci-runner`,
 * `-lock-yield`) were reachable in two clicks from the Settings sub-nav and
 * stranded the user, because the fallback replaces the Settings shell —
 * sub-nav included.
 *
 * These tests lock the invariant that produced that bug:
 *   R1 — every `MainTabId` has a renderer.
 *   R3 — an id from OUTSIDE the type system can never become the active tab.
 *
 * `TabContent.tsx` is read as SOURCE rather than imported: the vitest env is
 * `node`, and importing it would drag in React + @tauri-apps + a hundred lazy
 * page modules. The check we care about is exactly a source-level one — "does
 * a `case` exist for this id" — so scanning the switch is the honest assertion,
 * not a proxy for one.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { TAB_LIST, isValidTabId, migrateTabId, resolveExternalTabId } from "../tab-types";
import type { MainTabId } from "../tab-types";
import {
  SETTINGS_TABS,
  isSettingsTabId,
  settingsSubTabFor,
  VALID_SETTINGS_TABS,
} from "@/components/settings/settings-tabs";

const ALL_TAB_IDS: MainTabId[] = TAB_LIST.map((t) => t.id);

function tabContentCases(): Set<string> {
  const source = readFileSync(resolve(__dirname, "../TabContent.tsx"), "utf8");
  return new Set([...source.matchAll(/^\s*case "([a-z0-9-]+)":/gm)].map((m) => m[1]));
}

describe("R1: every MainTabId has a renderer", () => {
  it("no tab id falls through to UnknownTabFallback", () => {
    const cases = tabContentCases();
    const unrendered = ALL_TAB_IDS.filter((id) => !isSettingsTabId(id) && !cases.has(id));
    expect(unrendered).toEqual([]);
  });

  it("routes the whole settings-* family through the settings registry", () => {
    const settingsIds = ALL_TAB_IDS.filter(isSettingsTabId);
    // 25 `settings-*` panels + the bare `settings` root.
    expect(settingsIds.length).toBeGreaterThan(20);
    for (const id of settingsIds) {
      expect(VALID_SETTINGS_TABS).toContain(settingsSubTabFor(id));
    }
  });

  it("every settings sub-nav button lands on a routable MainTabId", () => {
    // The two-click path that broke: sub-nav click → `ui-bridge-set-tab` with a
    // `settings-*` MainTabId → TabContent. Each hop must resolve.
    for (const tab of SETTINGS_TABS) {
      const mainTabId = resolveExternalTabId(`settings-${tab.id}`) ?? resolveExternalTabId(tab.id);
      // `advanced` → `settings-debug` and `devenv-enroll` → backend-connection
      // are the two id-stem outliers; they have no `settings-<id>` MainTabId.
      if (tab.id === "advanced" || tab.id === "devenv-enroll") continue;
      expect(mainTabId, `settings sub-tab "${tab.id}" has no MainTabId`).not.toBeNull();
      expect(isSettingsTabId(mainTabId as MainTabId)).toBe(true);
    }
  });

  it("retired ids are gone from the union but still migrate", () => {
    // Removed in R1 — nothing renders them, so nothing may route to them…
    for (const dead of [
      "run-summary",
      "monitor-summary",
      "monitor-issues",
      "monitor-learnings",
      "monitor-discoveries",
    ]) {
      expect(isValidTabId(dead)).toBe(false);
      // …but a value persisted by an older build still lands on a real page.
      expect(migrateTabId(dead)).not.toBe("prompt-home");
      expect(isValidTabId(migrateTabId(dead))).toBe(true);
    }
  });
});

describe("R3: external tab ids are resolved, never cast", () => {
  it("accepts a live tab id verbatim", () => {
    expect(resolveExternalTabId("memory-federation")).toBe("memory-federation");
    expect(resolveExternalTabId("settings-notifications")).toBe("settings-notifications");
    // A live id wins over its legacy-alias entry: an external caller asking for
    // `check-builder` means the Check Builder page, not the alias target.
    expect(resolveExternalTabId("check-builder")).toBe("check-builder");
  });

  it("resolves a retired id through the legacy alias table", () => {
    expect(resolveExternalTabId("monitor-issues")).toBe("run-findings");
    expect(resolveExternalTabId("run-summary")).toBe("run-recap");
  });

  it("rejects a bogus id instead of navigating to a broken page", () => {
    for (const bogus of ["page-zzz", "", "   ", "settings-does-not-exist", null, undefined]) {
      expect(resolveExternalTabId(bogus)).toBeNull();
    }
  });

  it("trims whitespace like the Rust gate does", () => {
    expect(resolveExternalTabId("  specs  ")).toBe("specs");
  });
});
