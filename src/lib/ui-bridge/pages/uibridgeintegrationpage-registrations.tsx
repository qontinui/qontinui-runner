import { useUIComponent, useUIElement } from "@qontinui/ui-bridge";

/**
 * UI Bridge element registrations for UIBridgeIntegrationPage (/ui-bridge-integration).
 *
 * Only registers elements rendered directly by UIBridgeIntegrationPage.
 * Child components (ProjectCoordinator, SourceIntegrationPanel, DiscoveryPanel)
 * own their own registrations.
 */
export function useUIBridgeIntegrationPageRegistrations() {
  useUIComponent({
    id: "ui-bridge-integration-page",
    name: "UI Bridge Integration",
    description:
      "Configure UI Bridge for a target project: pick a project for one-click integration, or open Advanced for per-stage controls (analyze, install SDK, discover pages, generate registrations/specs/tutorials/videos).",
    actions: [
      {
        id: "open-advanced-controls",
        label: "Open Advanced per-stage controls",
        handler: () => {
          const summary = document.querySelector<HTMLElement>(
            '[data-ui-bridge-id="ui-bridge-advanced-disclosure"]',
          );
          summary?.click();
        },
      },
      {
        id: "scroll-to-top",
        label: "Scroll to the primary integration panel",
        handler: () => {
          window.scrollTo({ top: 0, behavior: "smooth" });
        },
      },
      {
        id: "get-pipeline-phase",
        label: "Read the current ProjectCoordinator pipeline phase",
        handler: () => {
          // ProjectCoordinator marks its root with `data-pipeline-phase={phase}`,
          // so callers can introspect progress without scraping button labels.
          // Phase values: idle | analyzing | integrating | discovering | no-pages
          //             | generating | generated | applied | failed
          const root = document.querySelector<HTMLElement>("[data-pipeline-phase]");
          return { phase: root?.dataset.pipelinePhase ?? "unknown" };
        },
      },
      {
        id: "get-last-prompt",
        label: "Read the most recently constructed registration prompt(s) from the integration pipeline",
        handler: () => {
          // Pipeline writes to window.__qontinuiPreviewPrompts (most-recent-first, capped at 20 entries).
          // Each entry: { pageRoute, pageName, prompt, timestamp }.
          const w = window as unknown as {
            __qontinuiPreviewPrompts?: Array<{
              pageRoute: string;
              pageName: string;
              prompt: string;
              timestamp: number;
            }>;
          };
          const prompts = w.__qontinuiPreviewPrompts ?? [];
          return {
            count: prompts.length,
            prompts: prompts.map((p) => ({
              pageRoute: p.pageRoute,
              pageName: p.pageName,
              prompt: p.prompt,
              promptLength: p.prompt.length,
              timestamp: p.timestamp,
            })),
          };
        },
      },
    ],
  });

  const advancedDisclosure = useUIElement({
    id: "ui-bridge-advanced-disclosure",
    type: "disclosure",
    label:
      "Advanced: per-stage controls — pick specific pages, toggle specs / tutorials / videos, inspect each stage",
    actions: ["click"],
  });

  return {
    advancedDisclosureRef: advancedDisclosure.ref,
  };
}
