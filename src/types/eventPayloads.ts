/**
 * Event Payload Types
 *
 * Type definitions for event payloads used throughout the event system.
 * Provides type safety for EventRouter and EventHandlers.
 */

/**
 * Base event payload structure
 */
export interface BaseEventPayload {
  event?: string;
  type?: string;
  data?: Record<string, unknown>;
}

/**
 * Error event payload
 */
export interface ErrorEventPayload extends BaseEventPayload {
  data?: {
    message?: string;
    [key: string]: unknown;
  };
}

/**
 * Log event payload
 */
export interface LogEventPayload extends BaseEventPayload {
  data?: {
    level?: string;
    message?: string;
    [key: string]: unknown;
  };
}

/**
 * Image recognition debug data structure
 */
export interface ImageRecognitionDebugData {
  top_matches?: Array<{
    confidence: number;
    location: { x: number; y: number };
    dimensions?: { w: number; h: number };
  }>;
  [key: string]: unknown;
}

/**
 * Image recognition location structure
 */
export interface ImageRecognitionLocation {
  x: number;
  y: number;
  width?: number;
  height?: number;
}

/**
 * Hierarchy data structure
 */
export interface HierarchyData {
  nesting_level?: number;
  parent_id?: string;
  workflow_name?: string;
}

/**
 * Image recognition event data
 */
export interface ImageRecognitionEventData {
  node_id?: string;
  hierarchy?: HierarchyData | string;
  template_name?: string;
  image_path?: string;
  confidence?: number;
  found?: boolean;
  threshold?: number;
  location?: ImageRecognitionLocation | string;
  gap?: number;
  percent_off?: number;
  best_match_location?: string;
  screenshot_path?: string;
  screenshot_base64?: string;
  screenshot_timestamp?: number;
  visual_debug_image?: string;
  template_path?: string;
  image_data?: string;
  matched_region_image?: string;
  debug?: ImageRecognitionDebugData;
  monitor_index?: number | null;
  [key: string]: unknown;
}

/**
 * Image recognition event payload
 */
export interface ImageRecognitionEventPayload extends BaseEventPayload {
  data?: ImageRecognitionEventData;
}

/**
 * Action event data
 */
export interface ActionEventData {
  action_type?: string;
  type?: string;
  [key: string]: unknown;
}

/**
 * Action event payload
 */
export interface ActionEventPayload extends BaseEventPayload {
  data?: ActionEventData;
}

/**
 * AI output stream event data
 */
export interface AiOutputStreamEventData {
  line?: string;
  source?: string;
  action_id?: string;
  [key: string]: unknown;
}

/**
 * AI output stream event payload
 */
export interface AiOutputStreamEventPayload extends BaseEventPayload {
  data?: AiOutputStreamEventData;
}

/**
 * Union type of all possible event payloads
 */
export type EventPayload =
  | BaseEventPayload
  | ErrorEventPayload
  | LogEventPayload
  | ImageRecognitionEventPayload
  | ActionEventPayload
  | AiOutputStreamEventPayload;
