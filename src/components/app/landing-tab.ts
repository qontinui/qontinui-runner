/**
 * landing-tab.ts
 *
 * Which tab the runner opens on, given what the last session persisted.
 *
 * `migrateTabId` alone is not enough. It answers "is this a tab that still
 * exists?", and every existing install has a real, existing tab persisted —
 * very often `prompt-home`, because that was the landing tab until the
 * Terminal-first IA. Landing there again would defeat the change AND strand the
 * user: the Home item is now behind the "advanced" disclosure, so nothing in
 * the sidebar would be highlighted and there would be no way back to the page
 * after navigating away.
 *
 * So the landing decision asks a second question: is the persisted tab still
 * REACHABLE from the sidebar under the user's current disclosures? A tab that
 * the registry lists but the disclosures currently hide is not somewhere to
 * open on; anything else — including the many pages that are legitimately not
 * sidebar items at all (`workflow-queue`, `check-builder`, `memory-search`, the
 * run detail tabs) — is a deliberate destination and is honoured.
 *
 * Note the asymmetry with the LIVE case, which is deliberate. Navigating TO a
 * gated page mid-session keeps you there and adds its entry to the sidebar (the
 * "visible UNION active" rule) — you asked for it. Only the cold-start default
 * bounces, because nobody asked for it.
 */

import {
  getAllItems,
  getChildrenForPlatform,
  getNavigationGroups,
  getProductMode,
  getShowHiddenItems,
  setProductMode,
  setShowHiddenItems,
} from "@qontinui/navigation";
import type { FeatureDisclosure } from "@/contexts/FeatureDisclosureContext";
import type { ProductMode } from "@/contexts/ProductModeContext";
import { isSettingsNavItemVisible } from "@/components/settings/settings-tabs";
import { DEFAULT_TAB_ID, migrateTabId, type MainTabId } from "./tab-types";

/**
 * What the landing decision needs to know about the user's current UI.
 *
 * `productMode` is the EFFECTIVE mode (`ProductModeContext` already pins it to
 * `"ai"` while the visual disclosure is off), not the stored preference —
 * the point is to model the sidebar that will actually render.
 */
export interface LandingContext {
  advanced: boolean;
  visual: boolean;
  productMode: ProductMode;
}

/** Every id the shared registry knows about, ignoring all filtering. */
function allRegisteredNavIds(): Set<string> {
  const ids = new Set<string>();
  for (const item of getAllItems()) ids.add(item.id);
  return ids;
}

/**
 * Ids the runner sidebar would render right now, for the given disclosures.
 *
 * `setProductMode` / `setShowHiddenItems` mutate synchronous module-global state
 * in the registry, which the Sidebar reads too. We save, set, derive and RESTORE
 * within one tick (no `await` between) so this is net-pure and cannot leak this
 * call's filters into whatever renders next. `try`/`finally` so a throw mid-walk
 * still restores.
 */
function visibleNavIds(ctx: LandingContext): Set<string> {
  const isEnabled = (d: FeatureDisclosure) => ctx[d];
  const prevMode = getProductMode();
  const prevShowHidden = getShowHiddenItems();
  const ids = new Set<string>();
  try {
    // The EFFECTIVE mode, matching `Sidebar`'s own filter. Passing `null`
    // (no product-mode filtering) would model a WIDER sidebar than renders and
    // let the cold start open on a page the sidebar then does not list —
    // leaning on the live anti-stranding rule that this function exists to
    // avoid needing.
    setProductMode(ctx.productMode);
    setShowHiddenItems(ctx.advanced);
    for (const group of getNavigationGroups("runner")) {
      for (const item of group.items) {
        ids.add(item.id);
        if (!item.hasChildren) continue;
        for (const child of getChildrenForPlatform(item.id, "runner")) {
          if (isSettingsNavItemVisible(child.id, isEnabled)) ids.add(child.id);
        }
      }
    }
  } finally {
    setProductMode(prevMode);
    setShowHiddenItems(prevShowHidden);
  }
  return ids;
}

/**
 * Resolve the tab to open on. `stored` is the raw persisted value (or null).
 */
export function resolveLandingTab(stored: string | null, ctx: LandingContext): MainTabId {
  const migrated = migrateTabId(stored);
  if (migrated === DEFAULT_TAB_ID) return migrated;

  const isEnabled = (d: FeatureDisclosure) => ctx[d];

  // Settings panels are gated by `settings-tabs.ts`, NOT by the nav registry,
  // and the two sets do not coincide: `settings-execution-variables` is gated
  // but has no `SETTINGS_ITEMS` entry, so the registry-membership check below
  // would wave it straight through. Ask the gate that actually governs it.
  if (migrated.startsWith("settings-")) {
    return isSettingsNavItemVisible(migrated, isEnabled) ? migrated : DEFAULT_TAB_ID;
  }

  // Not a sidebar item at all → a deliberate destination, honour it.
  if (!allRegisteredNavIds().has(migrated)) return migrated;

  return visibleNavIds(ctx).has(migrated) ? migrated : DEFAULT_TAB_ID;
}
