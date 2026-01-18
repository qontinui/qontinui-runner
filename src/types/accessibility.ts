/**
 * Accessibility Types
 *
 * TypeScript type definitions for accessibility tree capture and interaction.
 * Mirrors the Python accessibility schemas from qontinui_schemas.accessibility.
 */

/**
 * Accessibility role enumeration.
 * These represent ARIA roles and native accessibility roles.
 */
export type AccessibilityRole =
  | "button"
  | "checkbox"
  | "combobox"
  | "dialog"
  | "document"
  | "form"
  | "generic"
  | "grid"
  | "gridcell"
  | "group"
  | "heading"
  | "img"
  | "link"
  | "list"
  | "listbox"
  | "listitem"
  | "main"
  | "menu"
  | "menubar"
  | "menuitem"
  | "menuitemcheckbox"
  | "menuitemradio"
  | "navigation"
  | "option"
  | "progressbar"
  | "radio"
  | "region"
  | "row"
  | "rowgroup"
  | "rowheader"
  | "search"
  | "searchbox"
  | "separator"
  | "slider"
  | "spinbutton"
  | "switch"
  | "tab"
  | "table"
  | "tablist"
  | "tabpanel"
  | "textbox"
  | "timer"
  | "toolbar"
  | "tooltip"
  | "tree"
  | "treeitem"
  | "unknown";

/**
 * State information for an accessibility node.
 */
export interface AccessibilityState {
  /** Whether the element can receive focus */
  is_focusable?: boolean;
  /** Whether the element is currently focused */
  is_focused?: boolean;
  /** Whether the element is disabled */
  is_disabled?: boolean;
  /** Whether the element is checked (for checkboxes, switches) */
  is_checked?: boolean;
  /** Whether the element is expanded (for collapsible items) */
  is_expanded?: boolean;
  /** Whether the element is selected */
  is_selected?: boolean;
  /** Whether the element is required */
  is_required?: boolean;
  /** Whether the element is read-only */
  is_readonly?: boolean;
  /** Whether the element is pressed (for buttons) */
  is_pressed?: boolean;
}

/**
 * Bounding box for an accessibility node.
 */
export interface AccessibilityBounds {
  /** X coordinate of top-left corner */
  x: number;
  /** Y coordinate of top-left corner */
  y: number;
  /** Width of the bounding box */
  width: number;
  /** Height of the bounding box */
  height: number;
}

/**
 * A single node in the accessibility tree.
 */
export interface AccessibilityNode {
  /** Reference ID for this node (e.g., "@e1", "@e2") */
  ref: string;
  /** The role of this element */
  role: AccessibilityRole;
  /** The accessible name of this element */
  name?: string;
  /** The value of this element (for inputs, sliders, etc.) */
  value?: string;
  /** Description for this element */
  description?: string;
  /** Whether this element is interactive (clickable, focusable, etc.) */
  is_interactive: boolean;
  /** State information */
  state?: AccessibilityState;
  /** Bounding box */
  bounds?: AccessibilityBounds;
  /** Child nodes */
  children: AccessibilityNode[];
  /** Parent ref (for navigation) */
  parent_ref?: string;
  /** Additional properties */
  properties?: Record<string, unknown>;
}

/**
 * Capture statistics for an accessibility snapshot.
 */
export interface AccessibilityStats {
  /** Total number of nodes in the tree */
  total_nodes: number;
  /** Number of interactive nodes */
  interactive_nodes: number;
  /** The capture backend used */
  backend: string;
  /** Page URL (if from a web page) */
  url?: string;
  /** Page title (if from a web page) */
  title?: string;
}

/**
 * A complete accessibility tree snapshot.
 */
export interface AccessibilitySnapshot {
  /** Root node of the tree */
  root: AccessibilityNode;
  /** Total number of nodes */
  total_nodes: number;
  /** Number of interactive nodes */
  interactive_nodes: number;
  /** The capture backend used */
  backend: string;
  /** Page URL (if from a web page) */
  url?: string;
  /** Page title (if from a web page) */
  title?: string;
  /** Timestamp of capture */
  captured_at: string;
}

/**
 * Result of an accessibility tree capture.
 */
export interface AccessibilityCaptureResult {
  success: boolean;
  /** Captured snapshot (if successful) */
  snapshot?: AccessibilitySnapshot;
  /** AI-friendly text representation */
  ai_context?: string;
  /** Capture statistics */
  stats?: AccessibilityStats;
  /** Error message (if failed) */
  error?: string;
}

/**
 * Result of a ref-based action (click, fill, focus).
 */
export interface AccessibilityActionResult {
  success: boolean;
  /** The ref that was acted upon */
  ref?: string;
  /** Element name */
  element_name?: string;
  /** Element role */
  element_role?: string;
  /** The value that was typed (for fill actions) */
  value?: string;
  /** Error message (if failed) */
  error?: string;
}

/**
 * CDP port scan result.
 */
export interface CDPTarget {
  /** Page title */
  title: string;
  /** Page URL */
  url: string;
  /** Target type (usually "page") */
  type?: string;
  /** Target ID */
  id?: string;
  /** Description */
  description?: string;
}

/**
 * Result of scanning CDP ports.
 */
export interface CDPScanResult {
  success: boolean;
  /** List of ports with available CDP servers */
  available_ports?: number[];
  /** Mapping of port to page targets */
  targets?: Record<number, CDPTarget[]>;
  /** Error message (if failed) */
  error?: string;
}

/**
 * Result of listing browser targets on a port.
 */
export interface BrowserTargetsResult {
  success: boolean;
  /** Number of page targets */
  count?: number;
  /** List of page targets */
  targets?: CDPTarget[];
  /** Error message (if failed) */
  error?: string;
}

/**
 * Result of auto-connecting to a browser.
 */
export interface AutoConnectResult {
  success: boolean;
  /** Port that was connected to */
  port?: number;
  /** Target info (title, URL) */
  target?: CDPTarget;
  /** Captured snapshot */
  snapshot?: AccessibilitySnapshot;
  /** AI-friendly context */
  ai_context?: string;
  /** Error message (if failed) */
  error?: string;
}

/**
 * Options for capturing the accessibility tree.
 */
export interface AccessibilityCaptureOptions {
  /** Target to capture: "auto", URL pattern, or page index */
  target?: string;
  /** CDP host */
  cdp_host?: string;
  /** CDP port */
  cdp_port?: number;
  /** Only include interactive elements in refs */
  interactive_only?: boolean;
  /** Include hidden elements */
  include_hidden?: boolean;
  /** Maximum depth to capture */
  max_depth?: number;
}

/**
 * Options for finding elements.
 */
export interface AccessibilityFindOptions {
  /** Role(s) to match */
  role?: AccessibilityRole | AccessibilityRole[];
  /** Exact name to match */
  name?: string;
  /** Partial name match */
  name_contains?: string;
  /** Filter by interactivity */
  is_interactive?: boolean;
}

/**
 * Result of finding elements.
 */
export interface AccessibilityFindResult {
  success: boolean;
  /** Number of matching elements */
  count?: number;
  /** Matching elements */
  elements?: AccessibilityNode[];
  /** Error message (if failed) */
  error?: string;
}

/**
 * API response wrapper for accessibility endpoints.
 */
export interface AccessibilityApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}
