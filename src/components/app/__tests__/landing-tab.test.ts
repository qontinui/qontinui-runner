/**
 * Where the runner opens.
 *
 * A "default tab" constant alone does not decide it: `DEFAULT_TAB_ID` is only
 * consulted when the persisted value is ABSENT or unresolvable, and every
 * existing install has a real one persisted — very often `prompt-home`, which
 * was the landing tab before the Terminal-first IA and is now behind the
 * "advanced" disclosure. Landing there again would both miss the goal and
 * strand the user on a page their sidebar no longer lists.
 *
 * That asymmetry cuts both ways, and it is why moving the default from
 * `terminal` to `projects` is safe: a fresh install has nothing persisted and
 * gets Projects, while an existing one keeps whatever it had — `terminal`
 * included, since it is still a visible nav item and so resolves to itself.
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
  it("opens on the default tab for a cold start", () => {
    expect(resolveLandingTab(null, NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("", NEITHER)).toBe(DEFAULT_TAB_ID);
    expect(resolveLandingTab("a-tab-that-never-existed", NEITHER)).toBe(DEFAULT_TAB_ID);
  });

  it("lands a fresh install on Projects but leaves an existing one on its tab", () => {
    // The two halves of the DEFAULT_TAB_ID change, pinned against the literal
    // ids rather than the constant — a test written in terms of
    // `DEFAULT_TAB_ID` alone passes no matter what the constant is set to, and
    // would not have caught the change flipping the wrong way.
    //
    // Nothing persisted (or nothing recognisable) -> Projects.
    expect(resolveLandingTab(null, NEITHER)).toBe("projects");
    expect(resolveLandingTab("a-tab-that-never-existed", NEITHER)).toBe("projects");

    // Every install that has ever opened a tab has one persisted, and
    // `terminal` is still a visible nav item in every disclosure state, so it
    // resolves to itself and the landing tab does NOT move underneath anyone.
    for (const ctx of [NEITHER, ADVANCED]) {
      expect(resolveLandingTab("terminal", ctx)).toBe("terminal");
    }
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
    //
    // `run-recap` USED to be in this list. As of @qontinui/navigation 0.4.0 it
    // is a registered nav item (in the demoted REVIEW group), so it is no
    // longer "not a sidebar item at all" — it is a GATED one, and belongs to
    // the gated case asserted below.
    for (const tab of ["workflow-queue", "memory-search", "capture"]) {
      expect(resolveLandingTab(tab, NEITHER)).toBe(tab);
    }
  });

  it("keeps the default-visible pages", () => {
    // `runs` and `observations` were in this list under navigation 0.3.1. They
    // are not default-visible any more: 0.3.2 demoted the WHOLE REVIEW group
    // (Runs / Findings / Memory / Knowledge / Helper Tasks) behind "Show
    // advanced automation features", and 0.4.0 carries that. Their new home is
    // the gated assertion below.
    for (const tab of ["terminal", "productivity", "help"]) {
      expect(resolveLandingTab(tab, NEITHER)).toBe(tab);
    }
  });

  it("bounces a REVIEW-group page when the advanced disclosure is off, and keeps it when on", () => {
    // The behavioural half of the navigation 0.4.0 REVIEW demotion, asserted in
    // both directions so a future nav change that silently re-promotes (or
    // fully removes) these fails here rather than in someone's install.
    for (const tab of ["runs", "observations", "run-recap"]) {
      expect(resolveLandingTab(tab, NEITHER)).toBe(DEFAULT_TAB_ID);
      expect(resolveLandingTab(tab, ADVANCED)).toBe(tab);
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
    // The alias target is then judged on its own merits: `terminal` is visible
    // so it is honoured, and the gated builder is bounced to the default. (Both
    // used to land on the default, because `terminal` WAS the default — an
    // equality this assertion was accidentally riding on.)
    expect(resolveLandingTab("run-plan", NEITHER)).toBe("terminal");
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
