import type {
  RegisteredElement,
  RegisteredComponent,
  ElementIdentifier,
  ElementState,
  StyleGuideConfig,
} from "ui-bridge";

/**
 * Request types that can come from the Rust backend
 */
export type UIBridgeRequestType =
  | "get_elements"
  | "get_element"
  | "execute_action"
  | "get_components"
  | "get_component"
  | "execute_component_action"
  | "discover"
  | "get_snapshot"
  | "get_console_errors"
  | "clear_console_errors"
  | "get_specs"
  | "get_spec"
  | "page_refresh"
  | "page_navigate"
  | "page_go_back"
  | "page_go_forward"
  | "design_get_snapshot"
  | "design_get_responsive"
  | "design_run_audit"
  | "design_load_style_guide"
  | "design_get_style_guide"
  | "design_clear_style_guide"
  | "design_get_element_styles"
  | "design_get_state_styles"
  | "query_selector"
  | "page_evaluate"
  // Change tracking
  | "save_bookmark"
  | "get_bookmark"
  | "delete_bookmark"
  | "list_bookmarks"
  | "diff_from_bookmark"
  | "execute_with_diff"
  | "wait_for_change"
  | "categorize_last_diff"
  | "scoped_diff"
  | "summarize_diff"
  | "structured_changes"
  | "enable_change_buffer"
  | "disable_change_buffer"
  | "drain_change_buffer"
  | "get_change_buffer_size"
  // Undo/Redo
  | "get_undo_state"
  // Forms
  | "get_forms"
  // Network requests
  | "get_network_requests"
  | "get_network_requests_in_flight"
  // Idle detection
  | "get_idle_status"
  | "wait_for_idle"
  | "diagnose_stuck_screen"
  // Keyboard shortcuts
  | "get_keyboard_shortcuts"
  // AI search & find
  | "ai_search"
  | "ai_find"
  | "find"
  | "get_workflows"
  | "get_element_state"
  // Workflow execution
  | "run_workflow"
  | "get_workflow_status"
  // Media discovery
  | "find_media"
  | "media_audit"
  | "capture_media_snapshot"
  | "analyze_media"
  // Element image capture (DOM-based, no screen capture)
  | "capture_element_images"
  // Element image metadata (reads <img> src attributes from DOM)
  | "get_element_images";

/**
 * Payload structure for UI Bridge requests from Rust
 */
export interface UIBridgeRequestPayload {
  requestId: string;
  type: UIBridgeRequestType;
  elementId?: string;
  componentId?: string;
  actionId?: string;
  action?: {
    action: string;
    params?: Record<string, unknown>;
    waitOptions?: {
      visible?: boolean;
      enabled?: boolean;
      focused?: boolean;
      timeout?: number;
      interval?: number;
    };
  };
  specId?: string;
  url?: string;
  params?: Record<string, unknown>;
  /** CSS selector for query_selector requests */
  selector?: string;
  /** JavaScript expression for page_evaluate requests */
  expression?: string;
  /** Element index for query_selector action targeting */
  index?: number;
  guide?: StyleGuideConfig;
  elementIds?: string[];
  includePseudoElements?: boolean;
  viewports?: Record<string, number>;
  /** Bookmark name for change tracking */
  name?: string;
  /** Full request body for change tracking commands */
  body?: Record<string, unknown>;
  options?: {
    root?: string;
    interactiveOnly?: boolean;
    includeHidden?: boolean;
    limit?: number;
    types?: string[];
    selector?: string;
  };
}

/**
 * Response structure sent back to Rust
 */
export interface UIBridgeResponsePayload {
  requestId: string;
  type: UIBridgeRequestType;
  success: boolean;
  data?: unknown;
  error?: string;
  timestamp: number;
}

/**
 * Element data formatted for the response (serializable version)
 */
export interface SerializedElement {
  id: string;
  type: string;
  label?: string;
  actions: string[];
  customActions?: string[];
  identifier: ElementIdentifier;
  state: ElementState;
  registeredAt: number;
  mounted: boolean;
}

/**
 * Component data formatted for the response (serializable version)
 */
export interface SerializedComponent {
  id: string;
  name: string;
  description?: string;
  actions: Array<{
    id: string;
    label?: string;
    description?: string;
  }>;
  elementIds?: string[];
  registeredAt: number;
  mounted: boolean;
}

/**
 * The bridge context object passed to sub-hooks.
 * Each sub-hook destructures only the fields it needs.
 */
export interface UIBridgeEventContext {
  bridgeRef: React.MutableRefObject<ReturnType<typeof import("ui-bridge").useUIBridge>>;
  sendResponse: (response: UIBridgeResponsePayload) => Promise<void>;
  loadedStyleGuideRef: React.MutableRefObject<StyleGuideConfig | null>;
  changeTrackerRef: React.MutableRefObject<InstanceType<
    typeof import("ui-bridge/ai").ChangeTracker
  > | null>;
  networkTrackerRef: React.MutableRefObject<InstanceType<
    typeof import("ui-bridge").NetworkRequestTracker
  > | null>;
  idleDetectorRef: React.MutableRefObject<InstanceType<
    typeof import("ui-bridge").CompositeIdleDetector
  > | null>;
}

export type { RegisteredElement, RegisteredComponent, StyleGuideConfig };
