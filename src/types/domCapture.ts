/**
 * DOM Capture Types
 *
 * Types for the DOM capture feature which allows capturing and storing
 * browser DOM snapshots for AI analysis.
 */

/** Source of the DOM capture */
export type DomCaptureSource = "playwright" | "extension";

/** What triggered the DOM capture */
export type DomCaptureTrigger = "on_demand" | "auto" | "error";

/** Type of DOM capture */
export type DomCaptureType = "full" | "selector";

/** A DOM capture record */
export interface DomCapture {
  /** Unique identifier (dom-{uuid}) */
  id: string;
  /** ISO 8601 timestamp */
  timestamp: string;
  /** Page URL */
  url: string;
  /** Document title */
  pageTitle: string;
  /** Type of capture (full page or selector-based) */
  captureType: DomCaptureType;
  /** CSS selector if partial capture */
  selector?: string;
  /** Size of HTML content in bytes */
  htmlSizeBytes: number;
  /** Path to the HTML file */
  htmlFilePath: string;
  /** Path to related screenshot (if any) */
  linkedScreenshot?: string;
  /** Source of the capture */
  source: DomCaptureSource;
  /** Associated task run ID */
  taskRunId?: string;
  /** What triggered the capture */
  trigger: DomCaptureTrigger;
}

/** DOM capture with full HTML content */
export interface DomCaptureWithHtml extends DomCapture {
  /** Full HTML content */
  htmlContent: string;
}

/** Request to capture DOM from extension */
export interface CaptureFromExtensionRequest {
  /** Page URL */
  url: string;
  /** Document title */
  pageTitle: string;
  /** Full HTML content */
  html: string;
  /** CSS selector if partial capture */
  selector?: string;
  /** Task run ID to associate with */
  taskRunId?: string;
}

/** API response wrapper */
export interface DomCaptureApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}
