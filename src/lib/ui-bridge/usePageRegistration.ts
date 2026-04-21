/**
 * usePageRegistration — Register a runner page with UI Bridge.
 *
 * Drop-in hook that registers the current page/tab as a UI Bridge component
 * with its name, description, and any page-level actions. This enables the
 * AI to discover what page is active and what operations are available.
 *
 * Usage:
 *   usePageRegistration("settings", "Settings", "Application settings and configuration");
 */

import { useUIComponent } from "@qontinui/ui-bridge";

interface PageAction {
  id: string;
  label: string;
  handler: () => Promise<void> | void;
}

export function usePageRegistration(
  id: string,
  name: string,
  description: string,
  actions?: PageAction[],
) {
  useUIComponent({
    id: `page-${id}`,
    name,
    description,
    actions: actions || [],
  });
}
