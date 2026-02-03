/**
 * Generate AI Context
 *
 * Generates an AI-friendly text representation of the element tree,
 * similar to what the Accessibility Explorer provides.
 */

import type { ExternalElement } from "../../hooks/useExternalUIBridge";

export interface AIContextOptions {
  /** Page title (optional) */
  pageTitle?: string;
  /** Page URL (optional) */
  pageUrl?: string;
  /** Include element bounds/position (default: false) */
  includeBounds?: boolean;
  /** Include CSS selector (default: false) */
  includeCssSelector?: boolean;
  /** Include XPath (default: false) */
  includeXPath?: boolean;
  /** Filter to only interactive elements (default: true) */
  interactiveOnly?: boolean;
}

/**
 * Format a state list into a human-readable string
 */
function formatStates(element: ExternalElement): string[] {
  const states: string[] = [];

  // Basic states
  if (element.enabled !== undefined) {
    states.push(element.enabled ? "enabled" : "disabled");
  }
  if (element.focused) {
    states.push("focused");
  }
  if (element.visible === false) {
    states.push("hidden");
  }

  // Accessibility states
  if (element.is_required) {
    states.push("required");
  }
  if (element.is_readonly) {
    states.push("readonly");
  }
  if (element.is_expanded !== undefined) {
    states.push(element.is_expanded ? "expanded" : "collapsed");
  }
  if (element.is_pressed !== undefined) {
    states.push(element.is_pressed ? "pressed" : "not-pressed");
  }
  if (element.is_selected) {
    states.push("selected");
  }
  if (element.checked !== undefined) {
    states.push(element.checked ? "checked" : "unchecked");
  }

  // Keyboard accessibility
  if (element.accessibility?.isInTabOrder) {
    states.push("focusable");
  }

  return states;
}

/**
 * Get the display name for an element (accessible name, label, text, or placeholder)
 */
function getDisplayName(element: ExternalElement): string {
  return (
    element.accessibleName ||
    element.accessibility?.accessibleName ||
    element.label ||
    element.text ||
    element.placeholder ||
    ""
  );
}

/**
 * Get the role for an element (explicit or implicit)
 */
function getRole(element: ExternalElement): string {
  return (
    element.role ||
    element.accessibility?.role ||
    element.accessibility?.implicitRole ||
    element.type ||
    element.tagName.toLowerCase()
  );
}

/**
 * Check if an element is interactive
 */
function isInteractive(element: ExternalElement): boolean {
  if (element.is_interactive !== undefined) {
    return element.is_interactive;
  }

  // Fallback: check based on type/role
  const interactiveRoles = [
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "slider",
    "spinbutton",
    "searchbox",
    "switch",
    "tab",
    "menuitem",
    "option",
  ];

  const role = getRole(element).toLowerCase();
  if (interactiveRoles.includes(role)) {
    return true;
  }

  // Check by tag name
  const interactiveTags = ["button", "a", "input", "select", "textarea"];
  if (interactiveTags.includes(element.tagName.toLowerCase())) {
    return true;
  }

  // Check if has actions
  if (element.actions && element.actions.length > 0) {
    return true;
  }

  return false;
}

/**
 * Generate an AI-friendly text representation of the element tree.
 *
 * @param elements Array of ExternalElement objects
 * @param options Configuration options
 * @returns Formatted text representation
 */
export function generateAIContext(
  elements: ExternalElement[],
  options: AIContextOptions = {},
): string {
  const {
    pageTitle,
    pageUrl,
    includeBounds = false,
    includeCssSelector = false,
    includeXPath = false,
    interactiveOnly = true,
  } = options;

  const lines: string[] = [];

  // Header with page info
  if (pageTitle) {
    lines.push(`Page: ${pageTitle}`);
  }
  if (pageUrl) {
    lines.push(`URL: ${pageUrl}`);
  }
  if (pageTitle || pageUrl) {
    lines.push("");
  }

  // Filter elements
  const filteredElements = interactiveOnly ? elements.filter(isInteractive) : elements;

  // Count header
  const label = interactiveOnly ? "Interactive Elements" : "Elements";
  lines.push(`${label} (${filteredElements.length}):`);
  lines.push("");

  // Generate element entries
  for (let i = 0; i < filteredElements.length; i++) {
    const element = filteredElements[i];
    const ref = element.ref || `@e${i + 1}`;
    const role = getRole(element);
    const name = getDisplayName(element);

    // Main line: @e1 [button] "Submit Form"
    const nameDisplay = name ? ` "${name}"` : "";
    lines.push(`${ref} [${role}]${nameDisplay}`);

    // Details
    const details: string[] = [];

    // Role (if different from what's shown in brackets)
    if (element.accessibility?.hasExplicitRole && element.accessibility?.implicitRole) {
      details.push(`    - Role: ${role} (implicit: ${element.accessibility.implicitRole})`);
    }

    // Accessible name (if not already shown)
    if (!name && element.text) {
      details.push(`    - Text: ${element.text}`);
    }

    // States
    const states = formatStates(element);
    if (states.length > 0) {
      details.push(`    - State: ${states.join(", ")}`);
    }

    // Value for inputs
    if (element.value !== undefined && element.value !== "") {
      details.push(`    - Value: "${element.value}"`);
    } else if (
      element.value === "" &&
      ["textbox", "searchbox", "input", "textarea"].includes(role.toLowerCase())
    ) {
      details.push(`    - Value: ""`);
    }

    // Placeholder
    if (element.placeholder && !name.includes(element.placeholder)) {
      details.push(`    - Placeholder: "${element.placeholder}"`);
    }

    // Optional: bounds
    if (includeBounds && element.bounds) {
      const { x, y, width, height } = element.bounds;
      details.push(
        `    - Position: (${Math.round(x)}, ${Math.round(y)}) ${Math.round(width)}x${Math.round(height)}`,
      );
    }

    // Optional: CSS selector
    if (includeCssSelector && element.selector) {
      details.push(`    - Selector: ${element.selector}`);
    }

    // Optional: XPath
    if (includeXPath && element.xpath) {
      details.push(`    - XPath: ${element.xpath}`);
    }

    // Actions (only if non-obvious)
    if (element.actions && element.actions.length > 0) {
      const nonObviousActions = element.actions.filter(
        (action) => !["click", "focus"].includes(action.toLowerCase()),
      );
      if (nonObviousActions.length > 0) {
        details.push(`    - Actions: ${nonObviousActions.join(", ")}`);
      }
    }

    // Add details
    lines.push(...details);

    // Blank line between elements
    lines.push("");
  }

  return lines.join("\n");
}

export default generateAIContext;
