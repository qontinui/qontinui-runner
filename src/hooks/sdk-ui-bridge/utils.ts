/**
 * Shared utilities for the SDK UI Bridge sub-hooks.
 */

import { INTERACTIVE_ELEMENT_TYPES } from "@qontinui/ui-bridge";

import type { ExternalElement } from "./types";

/** The fields of a raw element row the interactivity rule reads. */
export interface ElementRowFields {
  readonly category?: unknown;
  readonly kind?: unknown;
  readonly type?: unknown;
  readonly actions?: unknown;
}

/**
 * Whether a raw SDK element row is a content or media row — `@qontinui/ui-bridge`
 * `core/find-filter.ts` `isContentRow || isMediaRow`: a `category` of `content`
 * or `media`, or a `kind` of `content`.
 */
export function isContentOrMediaRow(raw: ElementRowFields): boolean {
  return raw.category === "content" || raw.category === "media" || raw.kind === "content";
}

/**
 * Whether a raw SDK element row is an interactive control — the SDK's own
 * `interactive_only` predicate (`core/find-filter.ts`): not a content or media
 * row, and of an interactive type or advertising at least one action.
 *
 * "Has actions" alone stopped meaning "interactive" with `@qontinui/ui-bridge`
 * 0.46: every kind advertises the verbs every element takes (`click`,
 * `sendKeys`, `scroll`, …), a media row included because a canvas takes
 * keyboard input, so an `<img>` read as a control by its list. The Rust
 * readers (the exploration engine, the journey affordance index) exclude
 * content and media rows by the same classification
 * (`src-tauri/src/mcp/ui_bridge/element_kind.rs`) but have no
 * `INTERACTIVE_ELEMENT_TYPES` arm: they read what a row advertises.
 *
 * This is the DISPLAY classification (the inspector, the AI context). A
 * reader that is about to SEND a verb asks {@link advertisesExplorationClick}
 * instead: a registration that declares `actions: []` is a control of its
 * kind here, but the runner's element-action door refuses every built-in verb
 * on it.
 */
export function isInteractiveElementRow(raw: ElementRowFields): boolean {
  if (isContentOrMediaRow(raw)) return false;
  const type = typeof raw.type === "string" ? raw.type : undefined;
  return (
    (type !== undefined && INTERACTIVE_ELEMENT_TYPES.has(type)) ||
    (Array.isArray(raw.actions) && raw.actions.length > 0)
  );
}

/** The verb the state explorer dispatches on every element it explores. */
export const EXPLORATION_VERB = "click";

/**
 * Whether the state explorer may queue this row: a control (not a content or
 * media row) that ADVERTISES the verb the explorer sends. The runner's
 * element-action door refuses a verb an element does not advertise, so a row
 * that advertises no `click` — a registration declaring `actions: []`, such as
 * a terminal proxy pane — would only be refused. The Rust exploration engine
 * applies the same rule (`exploration/engine.rs` `is_explorable_control`).
 */
export function advertisesExplorationClick(raw: ElementRowFields): boolean {
  return (
    !isContentOrMediaRow(raw) &&
    Array.isArray(raw.actions) &&
    raw.actions.includes(EXPLORATION_VERB)
  );
}

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
    is_interactive: isInteractiveElementRow(raw),
    selector: raw.selector as string | undefined,
    xpath: raw.xpath as string | undefined,
    classes: raw.classes as string[] | undefined,
    href: raw.href as string | undefined,
    src: raw.src as string | undefined,
    category: readCategory(raw.category),
  };
}

function readCategory(value: unknown): ExternalElement["category"] {
  return value === "interactive" || value === "content" || value === "media" ? value : undefined;
}
