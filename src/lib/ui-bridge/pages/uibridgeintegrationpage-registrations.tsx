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
