/**
 * Accessibility Explorer Types
 *
 * TypeScript interfaces for the native accessibility tree data
 * returned by Tauri a11y commands.
 */

export interface UnifiedNode {
  ref: string;
  role: string;
  name?: string;
  value?: string;
  description?: string;
  bounds?: { x: number; y: number; width: number; height: number };
  state: {
    is_focused: boolean;
    is_disabled: boolean;
    is_hidden: boolean;
    is_expanded: string;
    is_selected: string;
    is_checked: string;
    is_pressed: string;
    is_readonly: boolean;
    is_required: boolean;
    is_multiselectable: boolean;
    is_editable: boolean;
    is_focusable: boolean;
    is_modal: boolean;
  };
  is_interactive: boolean;
  level?: number;
  automation_id?: string;
  class_name?: string;
  html_tag?: string;
  url?: string;
  children: UnifiedNode[];
  source: string;
  platform_handle?: number;
  supported_patterns: string[];
  generation: number;
}

export interface UnifiedSnapshot {
  root: UnifiedNode;
  timestamp_ms: number;
  source: string;
  url?: string;
  title?: string;
  total_nodes: number;
  interactive_nodes: number;
  generation: number;
}

export interface ConnectionResult {
  connected: boolean;
  backend: string;
}

export interface ActionResult {
  success: boolean;
  message?: string;
}

export interface QueryResult {
  elements: UnifiedNode[];
}
