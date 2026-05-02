import { useUIElement } from "@qontinui/ui-bridge";

/**
 * UI Bridge element registrations for SourceIntegrationPanel
 * (rendered inside the page-config-ui-bridge Advanced disclosure).
 *
 * Registers stable ids the prompt-home planner can target via direct
 * id routing (`element <id>` form in NLActionExecutor), bypassing the
 * fuzzy text matcher that previously failed to find these controls.
 */
export function useSourceIntegrationPanelRegistrations() {
  const projectPathInput = useUIElement({
    id: "ui-bridge-project-path-input",
    type: "input",
    label: "project path input",
    actions: ["type"],
  });

  const analyzeButton = useUIElement({
    id: "ui-bridge-analyze-button",
    type: "button",
    label: "Analyze project source",
    actions: ["click"],
  });

  return {
    projectPathInputRef: projectPathInput.ref,
    analyzeButtonRef: analyzeButton.ref,
  };
}
