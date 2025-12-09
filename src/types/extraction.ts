/**
 * Types for web extraction functionality
 */

export interface WebExtractionConfig {
  urls: string[];
  /** Physical resolution from target monitor for pattern matching compatibility */
  viewport: [number, number];
  capture_hover_states?: boolean;
  capture_focus_states?: boolean;
  capture_scroll_states?: boolean;
  max_depth?: number;
  max_pages?: number;
  auth_cookies?: Record<string, string>;
}

export interface ExtractionStatus {
  extraction_id: string;
  status: "pending" | "running" | "completed" | "failed";
  progress: {
    pages_extracted: number;
    total_pages: number;
    current_url: string;
  };
  error?: string;
}

export interface ExtractionResult {
  extraction_id: string;
  urls_processed: string[];
  total_screenshots: number;
  total_states: number;
  duration_seconds: number;
  timestamp: string;
  error?: string;
}

export interface Screenshot {
  id: string;
  url: string;
  viewport: [number, number];
  state?: string;
  timestamp: string;
  thumbnail_path?: string;
  full_path?: string;
}

export interface ExtractionSession {
  id: string;
  config: WebExtractionConfig;
  status: ExtractionStatus;
  result?: ExtractionResult;
  created_at: string;
  updated_at: string;
}

export interface BoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ExtractedElement {
  id: string;
  bbox: BoundingBox;
  element_type: string;
  text_content?: string;
  selector: string;
  is_interactive: boolean;
  is_enabled: boolean;
  semantic_role?: string;
  aria_label?: string;
}

export interface ExtractedState {
  id: string;
  name: string;
  bbox: BoundingBox;
  state_type: string;
  element_ids: string[];
  screenshot_id: string;
}

/**
 * State structure discovered during extraction.
 * This represents the state machine model discovered from the web pages.
 */
export interface StateStructure {
  states: StateNode[];
  transitions: StateTransition[];
  initial_state?: string;
  metadata?: Record<string, unknown>;
}

export interface StateNode {
  id: string;
  name: string;
  url_pattern?: string;
  screenshot_ids: string[];
  elements: ExtractedElement[];
}

export interface StateTransition {
  id: string;
  from_state: string;
  to_state: string;
  action_type: string;
  element_id?: string;
  description?: string;
}

/**
 * Event payload when extraction completes with state structure
 */
export interface ExtractionCompletedPayload {
  extraction_id: string;
  session_id?: string;
  result?: ExtractionResult;
  state_structure?: StateStructure;
}

/**
 * Supported training data export formats
 */
export type TrainingDataFormat = "coco" | "yolo" | "jsonl";

/**
 * Configuration for exporting training data
 */
export interface TrainingDataExportConfig {
  extraction_id: string;
  format: TrainingDataFormat;
  output_path: string;
  annotations?: unknown;
  include_states?: boolean;
}

/**
 * Cloud extraction session from qontinui-web
 */
export interface CloudExtractionSession {
  id: string;
  project_id: string;
  source_urls: string[];
  config: WebExtractionConfig;
  status: "pending" | "running" | "completed" | "failed";
  stats: {
    pages_extracted?: number;
    screenshots_captured?: number;
    states_discovered?: number;
  };
  error_message?: string;
  created_at: string;
  started_at?: string;
  completed_at?: string;
  created_by?: string;
}
