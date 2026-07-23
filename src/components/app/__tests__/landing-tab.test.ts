/**
 * Where the runner opens.
 *
 * The Terminal-first IA has a trap that a "default tab" constant alone does not
 * close: `DEFAULT_TAB_ID` is only consulted when the persisted value is ABSENT
 * or unresolvable, and every existing install has a real one persisted — very
 * often `prompt-home`, which was the landing tab until this change and is now
 * behind the "advanced" disclosure. Landing there again would both miss the goal
 * and strand the user on a page their sidebar no longer lists.
 *
 * These tests pin the rule that closes it, and — just as importantly — its
 * limits: a page that is simply not a sidebar item must NOT be bounced, or the
 * fix would throw users off every legitimately-deep destination they left open.
 */
import { describe, expect, it } from "vitest";

import { getProductMode, getShowHiddenItems, setProductMode, setShowHiddenItems } from "@qontinui/navigation";

import { resolveLandingTab, type LandingContext } from "../landing-tab";
import { DEFAULT_TAB_ID } from "../tab-types";

// NB `productMode` is the EFFECTIVE mode, so it is always "ai" while the visual
// disclosure is off — `ProductModeContext` pins it there. Pairing `visual: true`
// with `productMode: "visual"` is the only combination that models a user who
// opted in AND switched.
const NEITHER: LandingContext = { advanced: false, visual: false, productMode: "ai" };
const ADVANCED: LandingContext = { advanced: true, visual: false, productMode: "ai" };
const VISUAL: LandingContext = { advanced: false, visual: true, productMode: "visual" };
const VISUAL_IN_AI_MODE: LandingContext = {
  advanced: false,
  visual: true,
  productMode: "ai",
};

describe("resolveLandingTab", () => {
  it("opens on the Terminal for a cold start", () => {
    expect(resolveLandingTab(null, NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("a-tab-that-never-existed", NEITHER)).toBe(DEFAULT_TAB_ID);
  });

  it("bounces a persisted tab that is now behind a disclosure", () => {
    // THE upgrade case: an existing runner persisted `prompt-home`, which is
    // now `hidden: ["runner"]`. Without this rule the runner still opens on
    // Home and nothing in the sidebar is highlighted.
    expect(resolveLandingTab("prompt-home", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("active", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("run-image", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("unified-workflow-builder", NEITHER)).toBe(DEFAULT_TAB_ID);
  });

  it("honours those same tabs once their disclosure is on", () => {
    for (const tab of ["prompt-home", "active", "run-image", "unified-workflow-builder"]) {
      expect(resolveLandingTab(tab, ADVANCED)).toBe(tab);
    }
  });

  it("honours a visual-mode page only when the sidebar is actually in Visual mode", () => {
    // With `visual` off the product mode is pinned to "ai", so a visual-only
    // item is not in the sidebar and must not be opened on.
    expect(resolveLandingTab("gui-automation", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("gui-automation", VISUAL)).toBe("gui-automation");

    // The disclosure alone is not enough: a user who enabled visual automation
    // but is sitting in AI Dev mode gets a sidebar with no `gui-automation`
    // item, so landing there would open a page the sidebar does not list. This
    // is why the landing decision takes the EFFECTIVE mode rather than dropping
    // the product-mode filter whenever the disclosure is on.
    expect(resolveLandingTab("gui-automation", VISUAL_IN_AI_MODE)).toBe(DEFAULT_TAB_ID);
  });

  it("bounces a gated settings panel that has no nav item of its own", () => {
    // `settings-execution-variables` is gated by `settings-tabs.ts` but has no
    // entry in the nav package's SETTINGS_ITEMS, so a rule keyed on nav-registry
    // membership would wave it straight through and cold-start on a panel the
    // sidebar cannot highlight.
    expect(resolveLandingTab("settings-execution-variables", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("settings-execution-variables", ADVANCED)).toBe(
      "settings-execution-variables",
    );
  });

  it("bounces a nav id that has no runner tab, in every disclosure state", () => {
    // `vga` and `visual-dashboard` are qontinui-web routes with no runner tab
    // (that is why @qontinui/navigation 0.3.1 gates them `platforms: ["web"]`).
    // `migrateTabId` therefore cannot resolve them, and the landing tab must
    // not be a page the runner is unable to render.
    for (const enabled of [NEITHER, ADVANCED, VISUAL]) {
      expect(resolveLandingTab("vga", enabled)).toBe(DEFAULT_TAB_ID);
      expect(resolveLandingTab("visual-dashboard", enabled)).toBe(DEFAULT_TAB_ID);
    }
  });

  it("never bounces a page that is not a sidebar item at all", () => {
    // These are reachable only by deep-link or an in-app hand-off. They were
    // never in the nav, so their absence from it says nothing about intent —
    // bouncing them would be the fix causing its own stranding.
    // NB `check-builder` is deliberately NOT in this list: it is also a key in
    // LEGACY_TAB_MIGRATIONS, and `migrateTabId` is alias-first, so it resolves
    // to the (gated) `step-builders` and is correctly bounced. That asymmetry
    // with `resolveExternalTabId` — live id wins there, alias wins here — is
    // pre-existing and documented on both functions.
    for (const tab of ["workflow-queue", "memory-search", "capture", "run-recap"]) {
      expect(resolveLandingTab(tab, NEITHER)).toBe(tab);
    }
  });

  it("keeps the default-visible pages", () => {
    for (const tab of ["terminal", "productivity", "runs", "observations", "help"]) {
      expect(resolveLandingTab(tab, NEITHER)).toBe(tab);
    }
  });

  it("bounces a gated SETTINGS panel but keeps an ungated one", () => {
    // Settings panels are gated by `settings-tabs.ts`, not by the nav package,
    // so this asserts the two gates are both consulted.
    expect(resolveLandingTab("settings-playwright", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("settings-playwright", ADVANCED)).toBe("settings-playwright");
    expect(resolveLandingTab("settings-mobile", VISUAL)).toBe("settings-mobile");
    expect(resolveLandingTab("settings-general", NEITHER)).toBe("settings-general");
  });

  it("resolves a legacy alias before judging it", () => {
    // `run-plan` aliases to `terminal`; `ai-builder` to the (gated) builder.
    expect(resolveLandingTab("run-plan", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("ai-builder", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("ai-builder", ADVANCED)).toBe("unified-workflow-builder");
  });

  it("leaves the shared registry's global filter state exactly as it found it", () => {
    // `resolveLandingTab` mutates the nav package's module-global product-mode
    // and hidden-item flags to answer its question. The Sidebar reads those SAME
    // globals, so anything left set here leaks into whatever renders next.
    // Assert the actual globals, not just that repeated calls agree — the latter
    // passes vacuously because every call re-sets them on entry.
    setProductMode("visual");
    setShowHiddenItems(true);

    resolveLandingTab("prompt-home", NEITHER);

    expect(getProductMode()).toBe("visual");
    expect(getShowHiddenItems()).toBe(true);
  });
});
