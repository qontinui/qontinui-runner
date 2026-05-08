import type {
  RegisteredElement,
  RegisteredComponent,
  ElementIdentifier,
  ElementState,
  StyleGuideConfig,
} from "@qontinui/ui-bridge";

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
  | "execute_batch_with_diff"
  | "wait_for_change"
  | "categorize_last_diff"
  | "scoped_diff"
  | "summarize_diff"
  | "structured_changes"
  | "enable_change_buffer"
  | "disable_change_buffer"
  | "drain_change_buffer"
  | "get_change_buffer_size"
  | "wait_for_route_change"
  // Undo/Redo
  | "get_undo_state"
  | "undo"
  | "redo"
  // Forms
  | "get_forms"
  | "fill_form"
  | "snapshot_forms"
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
  | "wait_for_element_by_condition"
  | "wait_for_element_registered"
  | "wait_for_element_state_predicate"
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
  | "get_element_images"
  // Browser events & timeline
  | "get_browser_events"
  | "get_timeline"
  | "get_health_report"
  | "get_network_chains"
  | "get_error_snapshots"
  // Error sessions
  | "start_error_session"
  | "end_error_session"
  | "get_error_sessions"
  // Error baselines
  | "capture_error_baseline"
  | "compare_error_baseline"
  // Error report
  | "get_error_report"
  // AI endpoints (Phase 2)
  | "ai_execute"
  | "ai_assert"
  | "ai_assert_batch"
  | "ai_snapshot"
  | "ai_summary"
  // Idle sub-signals (Phase 4)
  | "get_idle_signal"
  | "wait_for_idle_signal"
  | "wait_for_targets"
  // Action history & metrics (Phase 4)
  | "get_action_history"
  | "get_interaction_metrics"
  // Annotations (Phase 5)
  | "annotations_list"
  | "annotations_create"
  | "annotations_get"
  | "annotations_update"
  | "annotations_delete"
  | "annotations_coverage"
  | "annotations_export"
  // State machine
  | "get_states"
  | "get_active_states"
  | "get_state_snapshot"
  | "get_state"
  | "activate_state"
  | "deactivate_state"
  | "get_state_groups"
  | "activate_state_group"
  | "deactivate_state_group"
  | "get_transitions"
  | "can_execute_transition"
  | "execute_transition"
  | "find_state_path"
  | "navigate_to_state"
  // AI semantic search & diff
  | "ai_semantic_search"
  | "ai_diff"
  // Intents
  | "get_intents"
  | "register_intent"
  | "find_intent"
  | "execute_intent"
  // Component state
  | "get_component_state"
  // Page scroll
  | "scroll_page"
  // Performance
  | "get_performance_entries"
  | "clear_performance_entries"
  // AI analysis
  | "ai_analyze_data"
  | "ai_analyze_regions"
  | "ai_analyze_structured_data"
  | "ai_analyze_cross_app"
  | "ai_recovery_attempt"
  // Design evaluation
  | "design_evaluate"
  | "design_evaluate_baseline"
  | "design_evaluate_contexts"
  | "design_evaluate_diff"
  // Media compare
  | "media_compare"
  // Pixel-accurate visual regression diff
  | "image_diff"
  // Single-element screenshot with DOM fallback strategies
  | "capture_single_element"
  // Annotations import
  | "annotations_import"
  // Intents NL
  | "execute_intent_from_query"
  // Debug
  | "get_element_tree"
  | "get_element_dom_tree"
  | "highlight_element"
  // App-agnostic convenience
  | "click_by_text"
  | "click_by_selector"
  | "type_into"
  | "read_value"
  | "find_by_text"
  // Diagnostics
  | "get_diagnostics"
  // Navigation adapter
  | "get_routes"
  | "navigate_by_adapter"
  // Wait for element stability
  | "wait_for_element_stable"
  // Wait for navigation complete
  | "wait_for_navigation_complete"
  // Stable ref resolution
  | "resolve_stable_ref"
  // Element assertion
  | "assert_element"
  // Runner-specific
  | "navigate_tab"
  | "clear_storage"
  // Tab control (F4 — first-class tab activation)
  | "tabs_list"
  | "tab_activate"
  // Page playbook (combined tab + component + intent + primary-action snapshot)
  | "get_playbook"
  // Network stubs (F2)
  | "register_network_stub"
  | "list_network_stubs"
  | "delete_network_stub"
  | "clear_network_stubs"
  // Non-consuming stub verification (N3)
  | "verify_network_stub"
  // Toast ring buffer (GET /control/toasts)
  | "get_toast_buffer"
  // Spec execution (POST /control/spec/{id}/run)
  | "run_spec"
  // Bucket C follow-up (plan 2026-05-07): 5 of 7 handlers wired here.
  | "receive_heartbeat"
  | "delete_intent"
  | "rank_elements"
  | "set_viewport_constraints"
  | "get_element_react_state"
  // Bucket C deferred (plan 2026-05-07-ui-bridge-change-buffer-peek):
  // both back the runner's view of ChangeTracker.changeBuffer via
  // peekBuffer() (SDK 0.3.5).
  | "get_changes_since"
  | "get_element_history";

// ============================================================
// N1 — Compile-time exhaustiveness registry for the chained
// sub-hook dispatcher in useUIBridgeEventHandler.
//
// Each sub-hook declares the UIBridgeRequestType variants it
// handles. The `AllHandledTypes` union is the compile-time sum
// of those declarations. The two `AssertEqual<>` asserts below
// fail to compile if:
//   (a) a sub-hook's declared variants drift from the variants it
//       actually `case`s in its switch, OR
//   (b) a new variant is added to UIBridgeRequestType without being
//       claimed by some sub-hook.
//
// This catches the F2-style ship where commands are added to the
// union but the runner dispatcher forgets its case (producing
// "Unknown request type: <name>" at runtime). The per-hook
// `never` checks in each switch default then guarantee the
// claimed variants are actually `case`d.
// ============================================================

export type ControlEventTypes =
  | "get_elements"
  | "get_element"
  | "execute_action"
  | "get_components"
  | "get_component"
  | "execute_component_action"
  | "navigate_tab"
  | "assert_element"
  | "resolve_stable_ref"
  | "clear_storage"
  | "receive_heartbeat";

export type DiscoveryEventTypes =
  | "discover"
  | "find"
  | "get_snapshot"
  | "get_component_state"
  | "get_states"
  | "get_active_states"
  | "get_state_snapshot"
  | "get_state"
  | "activate_state"
  | "deactivate_state"
  | "get_state_groups"
  | "activate_state_group"
  | "deactivate_state_group"
  | "get_transitions"
  | "can_execute_transition"
  | "execute_transition"
  | "find_state_path"
  | "navigate_to_state";

export type PageEventTypes =
  | "page_refresh"
  | "page_navigate"
  | "page_go_back"
  | "page_go_forward"
  | "scroll_page"
  | "query_selector"
  | "page_evaluate"
  | "click_by_text"
  | "click_by_selector"
  | "type_into"
  | "read_value"
  | "find_by_text"
  | "get_diagnostics"
  | "get_routes"
  | "navigate_by_adapter"
  | "tabs_list"
  | "tab_activate"
  | "get_playbook"
  | "register_network_stub"
  | "list_network_stubs"
  | "delete_network_stub"
  | "clear_network_stubs"
  | "verify_network_stub";

export type DesignEventTypes =
  | "design_get_snapshot"
  | "design_get_element_styles"
  | "design_get_state_styles"
  | "design_get_responsive"
  | "design_run_audit"
  | "design_load_style_guide"
  | "design_get_style_guide"
  | "design_clear_style_guide"
  | "design_evaluate"
  | "design_evaluate_baseline"
  | "design_evaluate_contexts"
  | "design_evaluate_diff"
  | "set_viewport_constraints";

export type ChangeTrackingEventTypes =
  | "wait_for_route_change"
  | "save_bookmark"
  | "get_bookmark"
  | "delete_bookmark"
  | "list_bookmarks"
  | "diff_from_bookmark"
  | "execute_with_diff"
  | "execute_batch_with_diff"
  | "wait_for_change"
  | "categorize_last_diff"
  | "scoped_diff"
  | "summarize_diff"
  | "structured_changes"
  | "enable_change_buffer"
  | "disable_change_buffer"
  | "drain_change_buffer"
  | "get_change_buffer_size"
  | "get_changes_since"
  | "get_element_history";

export type DebugInspectEventTypes =
  | "get_console_errors"
  | "clear_console_errors"
  | "get_specs"
  | "get_spec"
  | "get_undo_state"
  | "undo"
  | "redo"
  | "get_element_state"
  | "get_forms"
  | "fill_form"
  | "snapshot_forms"
  | "get_browser_events"
  | "get_timeline"
  | "get_health_report"
  | "get_network_chains"
  | "get_error_snapshots"
  | "start_error_session"
  | "end_error_session"
  | "get_error_sessions"
  | "capture_error_baseline"
  | "compare_error_baseline"
  | "get_error_report"
  | "get_action_history"
  | "get_interaction_metrics"
  | "get_performance_entries"
  | "clear_performance_entries"
  | "get_element_tree"
  | "get_element_dom_tree"
  | "highlight_element"
  | "get_toast_buffer"
  | "run_spec"
  | "get_element_react_state";

export type NetworkIdleEventTypes =
  | "get_network_requests"
  | "get_network_requests_in_flight"
  | "get_idle_status"
  | "wait_for_idle"
  | "get_idle_signal"
  | "wait_for_idle_signal"
  | "wait_for_targets"
  | "wait_for_navigation_complete"
  | "wait_for_element_stable"
  | "diagnose_stuck_screen"
  | "get_keyboard_shortcuts";

export type AISearchEventTypes =
  | "ai_search"
  | "ai_find"
  | "wait_for_element_registered"
  | "wait_for_element_state_predicate"
  | "wait_for_element_by_condition"
  | "ai_execute"
  | "ai_assert"
  | "ai_assert_batch"
  | "ai_snapshot"
  | "ai_summary"
  | "ai_semantic_search"
  | "ai_diff"
  | "ai_analyze_data"
  | "ai_analyze_regions"
  | "ai_analyze_structured_data"
  | "ai_analyze_cross_app"
  | "ai_recovery_attempt"
  | "get_intents"
  | "register_intent"
  | "find_intent"
  | "execute_intent"
  | "execute_intent_from_query"
  | "delete_intent"
  | "rank_elements";

export type WorkflowEventTypes = "get_workflows" | "run_workflow" | "get_workflow_status";

export type MediaEventTypes =
  | "find_media"
  | "media_audit"
  | "capture_media_snapshot"
  | "analyze_media"
  | "capture_element_images"
  | "get_element_images"
  | "capture_single_element"
  | "image_diff"
  | "media_compare";

export type AnnotationEventTypes =
  | "annotations_list"
  | "annotations_create"
  | "annotations_get"
  | "annotations_update"
  | "annotations_delete"
  | "annotations_coverage"
  | "annotations_export"
  | "annotations_import";

/** Union of every variant claimed by a sub-hook. */
export type AllHandledTypes =
  | ControlEventTypes
  | DiscoveryEventTypes
  | PageEventTypes
  | DesignEventTypes
  | ChangeTrackingEventTypes
  | DebugInspectEventTypes
  | NetworkIdleEventTypes
  | AISearchEventTypes
  | WorkflowEventTypes
  | MediaEventTypes
  | AnnotationEventTypes;

/**
 * Type-level assertion helper. `AssertEqual<X, Y>` is the literal type
 * `true` iff `X` and `Y` are mutually assignable, otherwise it's a
 * compile error where used.
 */
type AssertEqual<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : never;

// Forces a compile error if AllHandledTypes drifts from UIBridgeRequestType
// in either direction. Either:
//   - A new variant was added to UIBridgeRequestType but no sub-hook claimed
//     it (the fix: add it to the appropriate *EventTypes union above), OR
//   - A *EventTypes union claims a variant that isn't in UIBridgeRequestType
//     (the fix: remove the stale claim).
const _handledCoversUnion: AssertEqual<AllHandledTypes, UIBridgeRequestType> = true;
void _handledCoversUnion;

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
  /**
   * Optional navigation mode for `page_navigate`. `"hard"` (default, back-compat)
   * does a full reload; `"soft"` uses `history.pushState` + synthetic events
   * so SPA routers pick up the change without losing injected window state.
   */
  mode?: "hard" | "soft";
  params?: Record<string, unknown>;
  /** CSS selector for query_selector requests */
  selector?: string;
  /** JavaScript expression for page_evaluate requests */
  expression?: string;
  /**
   * When true, page_evaluate returns a consistent discriminated
   * `{ value, type }` shape regardless of result type. When false/omitted,
   * the legacy conditional-wrapping shape is preserved for backward-compat.
   */
  unwrap?: boolean;
  /**
   * Explicit opt-in for page_evaluate expressions that perform network I/O
   * (fetch / XMLHttpRequest / sendBeacon / WebSocket). Default (false)
   * blocks those as data-exfiltration risks. Setting true relaxes ONLY the
   * four network patterns — every structural code-injection block
   * (import, require, eval, Function, __proto__, location mutation, …)
   * remains in force.
   */
  allowNetworkRequests?: boolean;
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
  /** Maximum token budget for AI snapshot pruning (0 = unlimited) */
  maxTokens?: number;
  /** Target tab id for `tab_activate`. */
  tabId?: string;
  /** DOM-tree depth for `get_element_dom_tree` (clamped server-side to [1,6]). */
  depth?: number;
  /**
   * Discover-only: force a registry rebuild before scanning. When true, the
   * discovery handler dispatches `ui-bridge-route-change` (which clears the
   * registry + bbox trackers and re-runs the auto-register scan) and waits
   * for the scan-debounce window to settle so labels reflect live DOM.
   */
  force?: boolean;
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
  /**
   * Optional closest-match / recovery hint payload. Sibling field to
   * `error` (does NOT replace the success/error envelope shape). The
   * Rust side forwards this through `wrap_ipc_result` so the HTTP
   * response surfaces it as a top-level sibling of the error message.
   *
   * Currently used by three high-friction error paths:
   * - Element-not-found (useControlEvents.ts): `{ closestMatches: string[] }`
   * - Action-not-allowed (useControlEvents.ts): `{ allowedActions: string[] }`
   * - Eval-rejected (Rust page.rs): `string` workaround guidance.
   */
  hint?: unknown;
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
  /**
   * ID of a registered component that renders this element. Present when the
   * element was registered inside a `<UIBridgeComponentScope>`. Callers should
   * prefer higher-level component actions (see `componentActionBasePath`)
   * over driving the element directly.
   */
  ownedByComponent?: string;
  /**
   * Base path for invoking component-level actions that may supersede direct
   * element interaction. Call `GET <base>` for action details or
   * `POST <base>/action/<actionId>` with a matching params body.
   */
  componentActionBasePath?: string;
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
    paramSchema?: Record<string, unknown>;
    /** HTTP path to invoke this action (e.g. POST to this URL with `{params: ...}`). */
    path?: string;
  }>;
  /** Template showing how to invoke any action (`{actionId}` placeholder). */
  actionInvocationPath?: string;
  elementIds?: string[];
  registeredAt: number;
  mounted: boolean;
  /**
   * Phase 3.1 (plan 2026-05-03): discoverability scope echoed from the
   * underlying RegisteredComponent. `'global'` = available regardless of
   * route, `'route'` = only available while the mounting page is active.
   * Materialized from `component.scope ?? 'route'` so the field is always
   * present in the serialized payload (the SDK documents `undefined` as
   * "the default — i.e. route", and we make that default explicit).
   */
  scope: "global" | "route";
}

/**
 * The bridge context object passed to sub-hooks.
 * Each sub-hook destructures only the fields it needs.
 */
export interface UIBridgeEventContext {
  bridgeRef: React.MutableRefObject<ReturnType<typeof import("@qontinui/ui-bridge").useUIBridge>>;
  sendResponse: (response: UIBridgeResponsePayload) => Promise<void>;
  loadedStyleGuideRef: React.MutableRefObject<StyleGuideConfig | null>;
  changeTrackerRef: React.MutableRefObject<InstanceType<
    typeof import("@qontinui/ui-bridge/ai").ChangeTracker
  > | null>;
  networkTrackerRef: React.MutableRefObject<InstanceType<
    typeof import("@qontinui/ui-bridge").NetworkRequestTracker
  > | null>;
  idleDetectorRef: React.MutableRefObject<InstanceType<
    typeof import("@qontinui/ui-bridge").CompositeIdleDetector
  > | null>;
}

export type { RegisteredElement, RegisteredComponent, StyleGuideConfig };
