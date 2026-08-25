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
 * The SDK's §4.6 content/value-axis string brand, derived STRUCTURALLY from a
 * field the SDK already declares rather than imported by name. ui-bridge does
 * not re-export `Scrubbed` (nor any of its minters) from a public entry point,
 * and the brand key is a module-private `unique symbol`, so a consumer can
 * neither name nor mint it. Deriving the alias from the wire type also keeps
 * this file compiling against BOTH the published `@qontinui/ui-bridge@0.22.0`
 * (where the field is a plain `string`, so this collapses to `string`) and
 * ui-bridge `main` (where it is `Scrubbed<string>`).
 */
type ScrubbedString = NonNullable<DiscoveredElement["label"]>;

/**
 * Brand a string that is ALREADY scrubbed — the consumer-side counterpart of
 * ui-bridge's internal `trustDeveloperContent`. The only values that may pass
 * through here are (a) developer-authored registration labels, which §4.6
 * exempts by design, and (b) fields that crossed the wire from an external UI
 * Bridge app which applied §4.6 at its own boundary. It must NEVER wrap a raw
 * DOM content read that has not been through a redaction-aware source.
 */
function asScrubbed(raw: string | undefined): ScrubbedString | undefined {
  return raw as ScrubbedString | undefined;
}

/**
 * Convert a RegisteredElement from the runner's UI Bridge registry
 * to a DiscoveredElement that SpecExecutor can evaluate assertions against.
 */
export function registeredToDiscovered(el: RegisteredElement): DiscoveredElement {
  const state = el.getState();
  return {
    id: el.id,
    type: el.type,
    // Developer-authored on the registration, not scraped from the DOM — the
    // documented §4.6 boundary exemption.
    label: asScrubbed(el.label),
    tagName: el.element?.tagName?.toLowerCase() ?? el.type,
    role: el.element?.getAttribute?.("role") ?? undefined,
    // Prefer the state's accessible name: the SDK computes it with the W3C
    // accessible-name algorithm AND scrubs it against the §4.6 redaction
    // boundary. The raw `aria-label` read is only a fallback for elements
    // whose custom `getState` does not populate it.
    accessibleName:
      state.accessibleName ?? asScrubbed(el.element?.getAttribute?.("aria-label") ?? undefined),
    actions: [...el.actions],
    state,
    registered: true,
  };
}

/**
 * Convert an ExternalElement (from SDK app or legacy extension via UI Bridge HTTP API)
 * to a DiscoveredElement that SpecExecutor can evaluate assertions against.
 *
 * Every content/value-bearing field here crossed the wire from an external UI
 * Bridge app that already applied §4.6 at its own boundary, so the runner
 * re-brands rather than re-scrubs (it holds no live element to derive a
 * redaction verdict from).
 */
export function externalToDiscovered(ext: ExternalElement): DiscoveredElement {
  return {
    id: ext.id,
    type: ext.type || ext.tagName,
    label: asScrubbed(ext.label || ext.text || ext.accessibleName),
    tagName: ext.tagName,
    role: ext.role || ext.accessibility?.role,
    accessibleName: asScrubbed(ext.accessibleName || ext.accessibility?.accessibleName),
    actions: ext.actions || [],
    state: {
      visible: ext.visible,
      enabled: ext.enabled,
      // `enabled` is a FOLD of three independent signals (native `disabled`,
      // `aria-disabled`, effective `pointer-events: none`) and cannot be
      // unfolded, so neither of these is inferred from it. An external app
      // that publishes only the folded boolean leaves both `false`, which
      // reads as NOT ASSERTED — inventing a native-disabled claim from a
      // folded signal is exactly the conflation the SDK split them to end.
      // (`ExternalElement.disabled`/`ariaDisabled` are the top-level fields
      // this same change adds below; `accessibility.ariaDisabled` remains a
      // fallback for a producer that only publishes the nested spelling.)
      disabled: ext.disabled ?? false,
      ariaDisabled: ext.ariaDisabled ?? ext.accessibility?.ariaDisabled ?? false,
      focused: ext.focused,
      textContent: asScrubbed(ext.text || ext.accessibleName || ext.accessibility?.accessibleName),
      value: asScrubbed(ext.value),
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
