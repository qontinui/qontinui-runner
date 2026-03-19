/**
 * Shared types for UI Bridge Inspector components.
 *
 * Extracted from UIBridgeInspectorPanel to break circular dependencies
 * between the inspector panel and its child views.
 */

export interface UIBridgeElement {
  id: string;
  tagName: string;
  type: string;
  bounds: {
    x: number;
    y: number;
    width: number;
    height: number;
  };
  visible: boolean;
  enabled: boolean;
  focused: boolean;
  value?: string;
  text?: string;
  label?: string;
  parent?: string | null;
  children?: string[];
  actions?: string[];

  // Accessibility object (matches what extension already collects)
  accessibility?: {
    role: string; // ARIA role (explicit or implicit)
    accessibleName?: string; // Computed accessible name
    accessibleDescription?: string; // aria-describedby content
    ariaLabel?: string;
    ariaLabelledBy?: string;
    ariaDescribedBy?: string;
    ariaExpanded?: boolean;
    ariaSelected?: boolean;
    ariaChecked?: boolean | "mixed";
    ariaHidden?: boolean;
    ariaDisabled?: boolean;
    ariaRequired?: boolean;
    ariaLive?: "off" | "polite" | "assertive";
    tabIndex?: number;
    isInTabOrder?: boolean;
    isKeyboardAccessible?: boolean;
    implicitRole?: string;
    hasExplicitRole?: boolean;
  };

  // Top-level convenience properties (flattened from accessibility)
  role?: string; // ARIA role
  accessibleName?: string; // Computed accessible name
  is_expanded?: boolean; // aria-expanded
  is_pressed?: boolean; // aria-pressed
  is_selected?: boolean; // aria-selected
  is_required?: boolean; // required or aria-required
  is_readonly?: boolean; // readonly or aria-readonly
  is_interactive?: boolean; // Can be interacted with
  ref?: string; // Auto-generated reference like @e1, @e2

  // Cross-origin iframe detection
  isCrossOrigin?: boolean; // True if element is inside a cross-origin iframe (thumbnail unavailable)
}

export interface UIBridgeState {
  id: string;
  name: string;
  elements: string[];
  isActive: boolean;
  blocking?: boolean;
  blocks?: string[];
  group?: string;
}

export interface UIBridgeTransition {
  id: string;
  name: string;
  fromStates: string[];
  activateStates: string[];
  exitStates: string[];
  pathCost: number;
}

export interface UIBridgeEvent {
  id: number;
  timestamp: number;
  eventType: string;
  elementId?: string;
  stateId?: string;
  transitionId?: string;
  action?: string;
  params?: Record<string, unknown>;
  result?: Record<string, unknown>;
  durationMs?: number;
  success: boolean;
  errorMessage?: string;
}

export interface UIBridgeSnapshot {
  elements: UIBridgeElement[];
  states: UIBridgeState[];
  transitions: UIBridgeTransition[];
  activeStates: string[];
  timestamp: number;
}
