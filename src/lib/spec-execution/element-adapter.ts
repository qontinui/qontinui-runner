/**
 * Element Adapter
 *
 * Converts various element formats to DiscoveredElement
 * (what SpecExecutor needs for assertion evaluation).
 */

import type { DiscoveredElement } from "@qontinui/ui-bridge/control";
import type { RegisteredElement } from "@qontinui/ui-bridge";
import type { ExternalElement } from "../../types/ui-bridge-types";

/**
 * Convert a RegisteredElement from the runner's UI Bridge registry
 * to a DiscoveredElement that SpecExecutor can evaluate assertions against.
 */
export function registeredToDiscovered(el: RegisteredElement): DiscoveredElement {
  const state = el.getState();
  return {
    id: el.id,
    type: el.type,
    label: el.label,
    tagName: el.element?.tagName?.toLowerCase() ?? el.type,
    role: el.element?.getAttribute?.("role") ?? undefined,
    accessibleName: el.element?.getAttribute?.("aria-label") ?? undefined,
    actions: [...el.actions],
    state,
    registered: true,
  };
}

/**
 * Convert an ExternalElement (from SDK app or legacy extension via UI Bridge HTTP API)
 * to a DiscoveredElement that SpecExecutor can evaluate assertions against.
 */
export function externalToDiscovered(ext: ExternalElement): DiscoveredElement {
  return {
    id: ext.id,
    type: ext.type || ext.tagName,
    label: ext.label || ext.text || ext.accessibleName,
    tagName: ext.tagName,
    role: ext.role || ext.accessibility?.role,
    accessibleName: ext.accessibleName || ext.accessibility?.accessibleName,
    actions: ext.actions || [],
    state: {
      visible: ext.visible,
      enabled: ext.enabled,
      focused: ext.focused,
      textContent: ext.text || ext.accessibleName || ext.accessibility?.accessibleName,
      value: ext.value,
      checked: ext.checked,
      rect: ext.bounds
        ? {
            x: ext.bounds.x,
            y: ext.bounds.y,
            width: ext.bounds.width,
            height: ext.bounds.height,
            top: ext.bounds.top ?? ext.bounds.y,
            right: ext.bounds.right ?? ext.bounds.x + ext.bounds.width,
            bottom: ext.bounds.bottom ?? ext.bounds.y + ext.bounds.height,
            left: ext.bounds.left ?? ext.bounds.x,
          }
        : { x: 0, y: 0, width: 0, height: 0, top: 0, right: 0, bottom: 0, left: 0 },
    },
    registered: false,
  };
}
