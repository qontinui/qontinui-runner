import { useMemo } from "react";
import { usePageContext } from "@qontinui/ui-bridge";
import { deriveSection, TAB_LABELS, type MainTabId, type TabSection } from "./tab-types";

/**
 * Human label for each IA section — matches the sidebar group headers in
 * `@qontinui/navigation` `groups.ts`. Used as the first breadcrumb segment.
 */
const SECTION_LABELS: Record<TabSection, string> = {
  workspace: "Workspace",
  review: "Review",
  spend: "Spend",
  automate: "Automate",
  build: "Build",
  insights: "Insights",
  configure: "Configure",
  dev: "Dev",
  system: "System",
};

/**
 * Publishes the active tab's page context to the UI Bridge.
 *
 * Name, section, and breadcrumb are all derived from the single source of
 * truth in `tab-types.ts` (`TAB_LABELS` + `deriveSection`) rather than a
 * hand-maintained per-tab map. This keeps the page context, the
 * `GET /control/tabs` catalog, and the sidebar IA from ever drifting apart.
 */
export function RunnerPageContext({ activeTab }: { activeTab: MainTabId }) {
  const context = useMemo(() => {
    const section = deriveSection(activeTab);
    const name = TAB_LABELS[activeTab] ?? activeTab;
    return { name, section, breadcrumb: [SECTION_LABELS[section], name] };
  }, [activeTab]);

  usePageContext(context);
  return null;
}
