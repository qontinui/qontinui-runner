import { useUIElement } from "@qontinui/ui-bridge";

/**
 * UI Bridge element registrations for PageSelectionPanel
 * (rendered inside the page-config-ui-bridge Advanced disclosure
 * once the SDK is at least partially integrated).
 *
 * Each generation-options checkbox gets a stable id so the prompt-home
 * planner can drive it via the direct `element <id>` routing path
 * (NLActionExecutor → `/control/element/<id>/action {action: "check"|"uncheck"}`).
 * Without these registrations the planner's fuzzy matcher mis-selected
 * the wrapping <label> instead of the underlying <input>, and
 * performCheck/performUncheck rejected.
 */
export function usePageSelectionPanelRegistrations() {
  const generateRegistrations = useUIElement({
    id: "ui-bridge-generate-registrations-checkbox",
    type: "checkbox",
    label: "Generate Registrations",
    actions: ["check", "uncheck"],
  });

  const generatePageIds = useUIElement({
    id: "ui-bridge-generate-page-ids-checkbox",
    type: "checkbox",
    label: "Generate Page IDs",
    actions: ["check", "uncheck"],
  });

  const generateSpecs = useUIElement({
    id: "ui-bridge-generate-specs-checkbox",
    type: "checkbox",
    label: "Generate Page Specs",
    actions: ["check", "uncheck"],
  });

  const generateTutorials = useUIElement({
    id: "ui-bridge-generate-tutorials-checkbox",
    type: "checkbox",
    label: "Generate Tutorials",
    actions: ["check", "uncheck"],
  });

  const generateArchitectureDiagrams = useUIElement({
    id: "ui-bridge-generate-architecture-diagrams-checkbox",
    type: "checkbox",
    label: "Generate Architecture Diagrams",
    actions: ["check", "uncheck"],
  });

  const generateDemoVideos = useUIElement({
    id: "ui-bridge-generate-demo-videos-checkbox",
    type: "checkbox",
    label: "Generate Demo Videos",
    actions: ["check", "uncheck"],
  });

  const generateProductTours = useUIElement({
    id: "ui-bridge-generate-product-tours-checkbox",
    type: "checkbox",
    label: "Generate Product Tours",
    actions: ["check", "uncheck"],
  });

  const generateProjectExplainer = useUIElement({
    id: "ui-bridge-generate-project-explainer-checkbox",
    type: "checkbox",
    label: "Generate Project Explainer",
    actions: ["check", "uncheck"],
  });

  const generateButton = useUIElement({
    id: "ui-bridge-generate-button",
    type: "button",
    label: "Generate selected artifacts for selected pages",
    actions: ["click"],
  });

  return {
    generateRegistrationsRef: generateRegistrations.ref,
    generatePageIdsRef: generatePageIds.ref,
    generateSpecsRef: generateSpecs.ref,
    generateTutorialsRef: generateTutorials.ref,
    generateArchitectureDiagramsRef: generateArchitectureDiagrams.ref,
    generateDemoVideosRef: generateDemoVideos.ref,
    generateProductToursRef: generateProductTours.ref,
    generateProjectExplainerRef: generateProjectExplainer.ref,
    generateButtonRef: generateButton.ref,
  };
}
