/**
 * Shared utilities for the SDK UI Bridge sub-hooks.
 */

import type { ExternalElement } from "./types";

/** Map an SDK element to the ExternalElement interface used by the inspector UI */
export function mapSdkElement(raw: Record<string, unknown>): ExternalElement {
  // Bounds may be at raw.bounds (flat) or raw.state.rect (nested from SDK discover/snapshot)
  const rawState = raw.state as Record<string, unknown> | undefined;
  const rawRect = rawState?.rect as Record<string, number> | undefined;
  const bounds = (raw.bounds as Record<string, number>) || rawRect || {};
  const actions = (raw.actions as string[]) || [];
  const accessibility = raw.accessibility as ExternalElement["accessibility"] | undefined;

  return {
    id: (raw.id as string) || "",
    tagName: (raw.tagName as string) || (raw.type as string) || "div",
    type: (raw.type as string) || (raw.tagName as string) || "unknown",
    bounds: {
      x: bounds.x ?? 0,
      y: bounds.y ?? 0,
      width: bounds.width ?? 0,
      height: bounds.height ?? 0,
      top: bounds.top,
      right: bounds.right,
      bottom: bounds.bottom,
      left: bounds.left,
    },
    visible: (raw.visible as boolean) ?? (rawState?.visible as boolean) ?? true,
    enabled: (raw.enabled as boolean) ?? (rawState?.enabled as boolean) ?? true,
    focused: (raw.focused as boolean) ?? false,
    value: raw.value as string | undefined,
    checked: raw.checked as boolean | undefined,
    text: (raw.label as string) || (raw.text as string) || "",
    label: (raw.label as string) || "",
    parent: (raw.parent as string) || null,
    children: (raw.children as string[]) || [],
    actions,
    accessibility,
    role: accessibility?.role || (raw.role as string),
    accessibleName: accessibility?.accessibleName || (raw.accessibleName as string),
    is_interactive: actions.length > 0,
    selector: raw.selector as string | undefined,
    xpath: raw.xpath as string | undefined,
    classes: raw.classes as string[] | undefined,
    href: raw.href as string | undefined,
    src: raw.src as string | undefined,
  };
}
