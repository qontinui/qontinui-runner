/**
 * Virtual DOM renderer for spec assertion testing.
 *
 * Renders React components in jsdom (via vitest) and extracts elements
 * in the DiscoveredElement format used by the SpecExecutor. This allows
 * spec assertions to be verified without a real browser.
 *
 * Usage (in a vitest test):
 *
 *   import { renderAndDiscover } from "@/lib/spec-execution/vdom-renderer";
 *   import { EventHistoryPage } from "@/pages/EventHistoryPage";
 *
 *   const elements = await renderAndDiscover(<EventHistoryPage />);
 *   // Run spec assertions against elements...
 */

import { type ReactElement } from "react";

/**
 * Minimal element shape matching the UI Bridge DiscoveredElement interface.
 * Only includes fields needed for spec assertion evaluation.
 */
export interface VdomElement {
  id: string;
  type: string;
  textContent: string;
  role?: string;
  tagName: string;
  visible: boolean;
  interactive: boolean;
  attributes: Record<string, string>;
}

/**
 * Render a React element and extract all elements from the resulting DOM.
 *
 * Requires @testing-library/react and a jsdom environment (vitest with
 * environment: "jsdom").
 */
export async function renderAndDiscover(
  element: ReactElement,
  options?: { waitMs?: number },
): Promise<VdomElement[]> {
  // Dynamic import to avoid pulling in test deps at runtime
  const { render } = await import("@testing-library/react");

  const { container, unmount } = render(element);

  // Wait for async effects (useEffect, data loading)
  if (options?.waitMs) {
    await new Promise((r) => setTimeout(r, options.waitMs));
  }

  // Extract all elements
  const elements: VdomElement[] = [];
  let idCounter = 0;

  function traverse(node: Element, depth: number = 0) {
    if (depth > 20) return; // Prevent infinite recursion

    const tagName = node.tagName?.toLowerCase() ?? "";
    const role = node.getAttribute("role") ?? inferRole(tagName, node);
    const textContent = getDirectText(node);
    const isInteractive = isInteractiveElement(tagName, node);

    // Only include elements with meaningful content or interactivity
    if (textContent || isInteractive || role) {
      const attrs: Record<string, string> = {};
      for (const attr of Array.from(node.attributes ?? [])) {
        if (attr.name.startsWith("data-") || attr.name === "aria-label" || attr.name === "placeholder") {
          attrs[attr.name] = attr.value;
        }
      }

      elements.push({
        id: node.id || `vdom-${idCounter++}`,
        type: mapType(tagName, role, node),
        textContent: textContent.trim().slice(0, 200),
        role: role || undefined,
        tagName,
        visible: true, // jsdom has no layout, assume visible
        interactive: isInteractive,
        attributes: attrs,
      });
    }

    // Recurse into children
    for (const child of Array.from(node.children ?? [])) {
      traverse(child, depth + 1);
    }
  }

  traverse(container);

  unmount();

  return elements;
}

// =============================================================================
// Helpers
// =============================================================================

function getDirectText(el: Element): string {
  let text = "";
  for (const node of Array.from(el.childNodes)) {
    if (node.nodeType === 3) { // TEXT_NODE
      text += node.textContent ?? "";
    }
  }
  // Fall back to full textContent for leaf nodes
  if (!text && el.children.length === 0) {
    text = el.textContent ?? "";
  }
  return text;
}

function inferRole(tagName: string, el: Element): string {
  switch (tagName) {
    case "button": return "button";
    case "a": return "link";
    case "input": {
      const type = el.getAttribute("type") ?? "text";
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (type === "number") return "spinbutton";
      if (type === "search") return "searchbox";
      return "textbox";
    }
    case "select": return "combobox";
    case "textarea": return "textbox";
    case "table": return "table";
    case "tr": return "row";
    case "th": return "columnheader";
    case "h1": case "h2": case "h3": case "h4": case "h5": case "h6":
      return "heading";
    case "nav": return "navigation";
    case "main": return "main";
    case "img": return "img";
    default: return "";
  }
}

function isInteractiveElement(tagName: string, el: Element): boolean {
  if (["button", "a", "input", "select", "textarea"].includes(tagName)) return true;
  if (el.getAttribute("onclick") || el.getAttribute("tabindex")) return true;
  if (el.getAttribute("role") === "button") return true;
  return false;
}

function mapType(tagName: string, role: string, _el: Element): string {
  if (role === "button" || tagName === "button") return "button";
  if (role === "link" || tagName === "a") return "link";
  if (role === "textbox" || role === "searchbox" || tagName === "input" || tagName === "textarea") return "input";
  if (role === "spinbutton") return "input";
  if (role === "combobox" || tagName === "select") return "select";
  if (role === "checkbox") return "checkbox";
  if (role === "heading") return "heading";
  if (tagName === "table") return "table";
  if (tagName === "tr") return "row";
  if (tagName === "th") return "columnheader";
  if (tagName === "img") return "image";
  return "text";
}
