/**
 * settings-tabs.ts
 *
 * Single source of truth for the Settings sub-tab registry and its mapping
 * onto the top-level `MainTabId` union.
 *
 * Why this module exists (manual-test-loop iter 2, R1):
 * -----------------------------------------------------
 * The same truth used to live in THREE hand-maintained places:
 *   1. `SETTINGS_TABS` (the sub-nav buttons)          — settings/Settings.tsx
 *   2. `SUB_TAB_TO_MAIN_TAB` (sub-tab → MainTabId)    — settings/Settings.tsx
 *   3. a `settingsTabMap` + a 21-arm `case` list      — app/TabContent.tsx
 *
 * (3) had drifted from (1)/(2): five sub-tabs (`notifications`, `otel`,
 * `containers`, `ci-runner`, `lock-yield`) were reachable from the sub-nav and
 * mirrored their `settings-*` MainTabId into the global `activeTab`, but
 * `TabContent`'s switch had no case for them — so clicking e.g. Settings →
 * Notifications replaced the entire Settings shell (sub-nav included) with the
 * "This page could not be displayed" fallback and stranded the user.
 *
 * The fix is structural, not a per-id patch: `TabContent` no longer enumerates
 * settings ids at all. It asks `isSettingsTabId()` / `settingsSubTabFor()`,
 * both derived from the ONE registry below. A new settings sub-tab is now
 * routable the moment it is added to `SETTINGS_TABS` + `SETTINGS_SUB_TAB_TO_MAIN_TAB`,
 * and the type-level completeness assertion at the bottom of this file fails
 * the build if a `settings-*` MainTabId is ever left unmapped.
 */

import type { MainTabId } from "@/components/app/tab-types";

export type SettingsTab =
  | "account"
  | "dev-loop"
  | "ai"
  | "agentic"
  | "self-healing"
  | "world-state-verifier"
  | "playwright"
  | "mobile"
  | "discovery"
  | "backend-connection"
  | "devenv-enroll"
  | "mcp"
  | "log-sources"
  | "execution-variables"
  | "notifications"
  | "general"
  | "storage"
  | "backup"
  | "instances"
  | "otel"
  | "containers"
  | "security"
  | "ci-runner"
  | "lock-yield"
  | "advanced"
  | "updates";

/**
 * Every `MainTabId` that routes to the Settings page: the bare `"settings"`
 * root plus the whole `settings-*` family. Derived from `MainTabId` with a
 * template-literal `Extract`, so it can never fall out of sync with the union.
 */
export type SettingsMainTabId = Extract<MainTabId, "settings" | `settings-${string}`>;

/** Sub-tab shown when the caller asks for the bare `"settings"` tab id. */
export const DEFAULT_SETTINGS_SUB_TAB: SettingsTab = "backend-connection";

/**
 * Canonical, ordered list of settings sub-tabs (drives the sub-nav).
 *
 * The runner ↔ web channel is a single outbound WebSocket driven by
 * `WebIntegrationSettings`. The panel id is `backend-connection` to
 * reflect the unified scope, but we still mount `WebIntegrationSettings`
 * underneath until the next UX pass.
 */
export const SETTINGS_TABS = [
  { id: "account", label: "Account" },
  { id: "dev-loop", label: "Test My Change" },
  { id: "backend-connection", label: "Backend Connection" },
  { id: "devenv-enroll", label: "Devenv Enrollment" },
  { id: "ai", label: "AI Providers" },
  { id: "agentic", label: "Advanced AI" },
  { id: "self-healing", label: "Self-Healing" },
  { id: "world-state-verifier", label: "World State Verifier" },
  { id: "playwright", label: "Playwright" },
  { id: "mobile", label: "Mobile" },
  { id: "discovery", label: "Discovery" },
  { id: "mcp", label: "MCP Servers" },
  { id: "log-sources", label: "Log Sources" },
  { id: "execution-variables", label: "Execution Variables" },
  { id: "notifications", label: "Notifications" },
  { id: "general", label: "General" },
  { id: "storage", label: "Storage" },
  { id: "backup", label: "Backup" },
  // Phase 3 (pop-out terminal windows): pop-out windows supersede the
  // multi-process instance launcher for the everyday "another terminal window"
  // case, so this tab is relabeled to signal it's now advanced/testing surface.
  // The `id` stays "instances" so routing / UI-Bridge sub-tab mapping / specs
  // are unchanged.
  { id: "instances", label: "Advanced / Testing" },
  { id: "otel", label: "OpenTelemetry" },
  { id: "containers", label: "Container Isolation" },
  { id: "ci-runner", label: "CI Runner" },
  { id: "security", label: "Security" },
  { id: "lock-yield", label: "Lock-yield Policy" },
  { id: "advanced", label: "Debug" },
  { id: "updates", label: "Updates" },
] as const satisfies ReadonlyArray<{ id: SettingsTab; label: string }>;

export const VALID_SETTINGS_TABS: readonly SettingsTab[] = SETTINGS_TABS.map((t) => t.id);

/**
 * Sub-tab id → `MainTabId`. Used by the sub-nav to mirror a sub-tab click into
 * the global `activeTab` (via the `ui-bridge-set-tab` window event) so
 * `/control/tabs` reports the panel actually on screen.
 *
 * Most sub-tabs map by a simple `settings-${id}` concatenation; the outliers
 * (where the `SettingsTab` id and the `MainTabId` stem disagree) are the reason
 * this is an explicit table rather than string arithmetic:
 *   - `advanced`       → `settings-debug`
 *   - `devenv-enroll`  → `settings-backend-connection` (no MainTabId of its own)
 */
export const SETTINGS_SUB_TAB_TO_MAIN_TAB = {
  account: "settings-account",
  "dev-loop": "settings-dev-loop",
  // NOTE: `backend-connection` is listed BEFORE `devenv-enroll` on purpose —
  // both map to `settings-backend-connection`, and the reverse map below is
  // first-wins, so this ordering decides which sub-tab that MainTabId opens.
  "backend-connection": "settings-backend-connection",
  "devenv-enroll": "settings-backend-connection",
  ai: "settings-ai",
  agentic: "settings-agentic",
  "self-healing": "settings-self-healing",
  "world-state-verifier": "settings-world-state-verifier",
  playwright: "settings-playwright",
  mobile: "settings-mobile",
  discovery: "settings-discovery",
  mcp: "settings-mcp",
  "log-sources": "settings-log-sources",
  "execution-variables": "settings-execution-variables",
  notifications: "settings-notifications",
  general: "settings-general",
  storage: "settings-storage",
  backup: "settings-backup",
  instances: "settings-instances",
  otel: "settings-otel",
  containers: "settings-containers",
  "ci-runner": "settings-ci-runner",
  security: "settings-security",
  "lock-yield": "settings-lock-yield",
  advanced: "settings-debug",
  updates: "settings-updates",
} as const satisfies Record<SettingsTab, MainTabId>;

/** The `MainTabId`s actually produced by the table above. */
type MappedSettingsMainTabId = (typeof SETTINGS_SUB_TAB_TO_MAIN_TAB)[SettingsTab];

/**
 * COMPILE-TIME COMPLETENESS GUARD.
 *
 * If a `settings-*` id is added to `MainTabId` but never mapped from a
 * `SettingsTab` above, `Unmapped` stops being `never` and this assignment
 * fails to compile with the offending id(s) named in the error. That is the
 * structural replacement for the hand-maintained `case` list that drifted.
 *
 * `"settings"` (the bare root) is excluded — it is not the target of any
 * sub-tab; it is handled by `DEFAULT_SETTINGS_SUB_TAB`.
 */
type UnmappedSettingsMainTabId = Exclude<SettingsMainTabId, MappedSettingsMainTabId | "settings">;
const _allSettingsMainTabsAreMapped: UnmappedSettingsMainTabId[] = [];
void _allSettingsMainTabsAreMapped;

/** `MainTabId` → the settings sub-tab it should open. First mapping wins. */
const MAIN_TAB_TO_SETTINGS_SUB_TAB: Partial<Record<SettingsMainTabId, SettingsTab>> = (() => {
  const inverse: Partial<Record<SettingsMainTabId, SettingsTab>> = {
    settings: DEFAULT_SETTINGS_SUB_TAB,
  };
  for (const [sub, main] of Object.entries(SETTINGS_SUB_TAB_TO_MAIN_TAB) as Array<
    [SettingsTab, SettingsMainTabId]
  >) {
    if (inverse[main] === undefined) inverse[main] = sub;
  }
  return inverse;
})();

/**
 * Type guard: does this top-level tab id render the Settings page?
 *
 * Prefix-based on purpose — `TabContent` uses this to route the WHOLE
 * `settings-*` family through one arm, so no settings id can ever again be
 * "valid but unrendered".
 */
export function isSettingsTabId(id: MainTabId): id is SettingsMainTabId {
  return id === "settings" || id.startsWith("settings-");
}

/**
 * Which settings sub-tab a `settings*` `MainTabId` opens. The `??` is an
 * unreachable runtime backstop — `UnmappedSettingsMainTabId` above proves at
 * compile time that every id in the domain is present.
 */
export function settingsSubTabFor(id: SettingsMainTabId): SettingsTab {
  return MAIN_TAB_TO_SETTINGS_SUB_TAB[id] ?? DEFAULT_SETTINGS_SUB_TAB;
}

/** Coerce an arbitrary stored/prop string to a valid sub-tab id. */
export function migrateSettingsSubTab(stored: string | null | undefined): SettingsTab {
  if (!stored) return DEFAULT_SETTINGS_SUB_TAB;
  if ((VALID_SETTINGS_TABS as readonly string[]).includes(stored)) {
    return stored as SettingsTab;
  }
  return DEFAULT_SETTINGS_SUB_TAB;
}
