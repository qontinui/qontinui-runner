/**
 * Settings sub-nav disclosure gating.
 *
 * The Terminal-first IA moved the verification-agentic loop-workflow pages and
 * the visual-GUI-automation pages behind two opt-in disclosures. Their SETTINGS
 * panels had to move with them — a sub-nav that still advertised "Playwright"
 * and "Self-Healing" to a user who cannot open a workflow builder is the same
 * clutter the sidebar change removed, one click deeper.
 *
 * The invariant these tests lock is the one that makes the gating safe: it is
 * PRESENTATION-ONLY. Every panel keeps its id, its `MainTabId` mapping and its
 * renderer whatever the disclosures say, so deep-links and UI Bridge
 * `activate-tab` calls still resolve. A gate that also removed the route would
 * turn a hidden panel into a "This page could not be displayed" fallback.
 */
import { describe, expect, it } from "vitest";

import {
  SETTINGS_TABS,
  VALID_SETTINGS_TABS,
  isSettingsNavItemVisible,
  isSettingsTabId,
  settingsSubTabFor,
  settingsTabsFor,
  visibleSettingsTabs,
  type SettingsTab,
} from "../settings-tabs";
import type { FeatureDisclosure } from "@/contexts/FeatureDisclosureContext";

const NONE = () => false;
const ALL = () => true;
const only =
  (...enabled: FeatureDisclosure[]) =>
  (d: FeatureDisclosure) =>
    enabled.includes(d);

const idsOf = (tabs: readonly { id: SettingsTab }[]) => tabs.map((t) => t.id);

describe("settings sub-nav disclosure gating", () => {
  it("shows only the ungated panels by default (both disclosures off)", () => {
    const visible = idsOf(visibleSettingsTabs(NONE));

    // The Terminal-first default: account, connection, AI/MCP, logs, storage,
    // security, updates — nothing that configures a paradigm the user has not
    // opted into.
    expect(visible).toEqual([
      "account",
      "dev-loop",
      "backend-connection",
      "devenv-enroll",
      "ai",
      "mcp",
      "log-sources",
      "notifications",
      "general",
      // Ungated on purpose: the only switch for the markdown-plan tier, which
      // is OFF by default with no fallback — a gate would put it back out of
      // reach one click deeper.
      "paths",
      "storage",
      "backup",
      "instances",
      "otel",
      "containers",
      "ci-runner",
      // Ungated on purpose: the session-protection floors guard the
      // Terminal-first user's own sessions against commit exhaustion, which is
      // the most default-path failure this app has.
      "resource-guard",
      "security",
      "lock-yield",
      "advanced",
      "updates",
    ]);
  });

  it("reveals the loop-workflow panels with the advanced disclosure", () => {
    const gained = idsOf(visibleSettingsTabs(only("advanced"))).filter(
      (id) => !idsOf(visibleSettingsTabs(NONE)).includes(id),
    );
    expect(gained).toEqual([
      "agentic",
      "world-state-verifier",
      "playwright",
      "discovery",
      "execution-variables",
      // Per-app build/start commands for the auto-fresh engine — it configures
      // the fleet-fresh routing machinery, which is advanced-only surface.
      "app-freshness",
    ]);
  });

  it("reveals the visual-GUI panels with the visual disclosure", () => {
    const gained = idsOf(visibleSettingsTabs(only("visual"))).filter(
      (id) => !idsOf(visibleSettingsTabs(NONE)).includes(id),
    );
    expect(gained).toEqual(["self-healing", "world-state-verifier", "mobile"]);
  });

  it("treats a multi-disclosure panel as ANY-of, not ALL-of", () => {
    // The World State Verifier judges GUI actions inside the agentic loop, so
    // it belongs to both paradigms — either opt-in must surface it, and
    // requiring both would hide it from every user who enabled just one.
    const wsv = SETTINGS_TABS.find((t) => t.id === "world-state-verifier");
    expect(wsv?.requires).toEqual(["advanced", "visual"]);
    for (const predicate of [only("advanced"), only("visual"), ALL]) {
      expect(idsOf(visibleSettingsTabs(predicate))).toContain("world-state-verifier");
    }
    expect(idsOf(visibleSettingsTabs(NONE))).not.toContain("world-state-verifier");
  });

  it("shows every panel when both disclosures are on", () => {
    expect(idsOf(visibleSettingsTabs(ALL))).toEqual(idsOf(SETTINGS_TABS));
  });
});

describe("gating is presentation-only", () => {
  it("keeps every gated panel routable by its MainTabId", () => {
    // The registry is unchanged by the disclosures: each gated id still maps to
    // a settings MainTabId that TabContent renders through the settings arm.
    const gated = SETTINGS_TABS.filter((t) => t.requires);
    expect(gated.length).toBeGreaterThan(0);

    for (const tab of gated) {
      expect(VALID_SETTINGS_TABS).toContain(tab.id);
      // `advanced`/`devenv-enroll` are the known id-stem outliers; none of the
      // gated tabs are among them, so `settings-<id>` is their MainTabId.
      const mainTabId = `settings-${tab.id}`;
      expect(isSettingsTabId(mainTabId as never)).toBe(true);
      expect(settingsSubTabFor(mainTabId as never)).toBe(tab.id);
    }
  });

  it("declares only known disclosures", () => {
    const known: FeatureDisclosure[] = ["advanced", "visual"];
    for (const tab of SETTINGS_TABS) {
      for (const requirement of tab.requires ?? []) {
        expect(known).toContain(requirement);
      }
    }
  });
});

describe("settingsTabsFor — the anti-stranding rule", () => {
  it("adds a gated tab back when it is the active one", () => {
    // Reached by deep-link / UI Bridge activate-tab, or left selected when the
    // disclosure was turned off. Omitting it renders the panel with nothing
    // marked active and no way back to it.
    expect(idsOf(settingsTabsFor(NONE, "playwright"))).toContain("playwright");
    expect(idsOf(settingsTabsFor(NONE, null))).not.toContain("playwright");
  });

  it("keeps the restored tab in its canonical position, not appended", () => {
    const ids = idsOf(settingsTabsFor(NONE, "playwright"));
    // Registry order puts playwright before mcp; appending would put it last.
    expect(ids.indexOf("playwright")).toBeLessThan(ids.indexOf("mcp"));
    expect(ids[ids.length - 1]).toBe("updates");
  });

  it("is exactly visibleSettingsTabs when the active tab is not gated", () => {
    for (const predicate of [NONE, only("advanced"), only("visual"), ALL]) {
      expect(idsOf(settingsTabsFor(predicate, "general"))).toEqual(
        idsOf(visibleSettingsTabs(predicate)),
      );
    }
  });
});

describe("the sidebar Settings flyout agrees with the Settings sub-nav", () => {
  // The runner lists its settings panels in TWO places. If they disagree, the
  // flyout offers a panel whose own page has no button for it — the shape that
  // stranded users before. `isSettingsNavItemVisible` is what keeps the flyout
  // reading its answer off the same registry.
  const flyoutVisible = (predicate: (d: FeatureDisclosure) => boolean) =>
    SETTINGS_TABS.map((t) => `settings-${t.id}`).filter((navId) =>
      isSettingsNavItemVisible(navId, predicate),
    );

  it("hides exactly the sub-tabs the Settings page hides", () => {
    for (const predicate of [NONE, only("advanced"), only("visual"), ALL]) {
      expect(flyoutVisible(predicate)).toEqual(
        idsOf(visibleSettingsTabs(predicate)).map((id) => `settings-${id}`),
      );
    }
  });

  it("never hides a non-settings nav item", () => {
    for (const id of ["terminal", "runs", "productivity", "help", "settings"]) {
      expect(isSettingsNavItemVisible(id, NONE)).toBe(true);
    }
  });

  it("applies the anti-stranding rule identically to the Settings page", () => {
    // The two surfaces must agree in the case that matters: a gated-but-active
    // panel. Before the shared `settingsTabsFor`, the page added it back and the
    // flyout did not — so the flyout hid the very page on screen.
    expect(isSettingsNavItemVisible("settings-playwright", NONE, "settings-playwright")).toBe(true);
    expect(isSettingsNavItemVisible("settings-playwright", NONE, "settings-general")).toBe(false);
    expect(flyoutVisible(NONE)).toEqual(
      idsOf(settingsTabsFor(NONE, null)).map((id) => `settings-${id}`),
    );
  });

  it("keeps an unmapped settings-* id visible rather than silently dropping it", () => {
    // Defensive: a `settings-*` nav id the registry does not know about is a
    // bug to surface elsewhere, not a reason to vanish it from the flyout.
    expect(isSettingsNavItemVisible("settings-does-not-exist", NONE)).toBe(true);
  });
});
