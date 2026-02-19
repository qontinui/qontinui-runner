/**
 * Test Builder Types
 *
 * TypeScript types for the verification test builder system.
 */

// Test types supported by the verification system
export type TestType = "playwright_cdp" | "qontinui_vision" | "python_script" | "repository_test";

// Test categories for organization
export type TestCategory =
  | "visual"
  | "dom"
  | "network"
  | "data"
  | "log"
  | "layout"
  | "unit"
  | "integration"
  | "custom";

// Test status for execution results
export type TestStatus =
  | "pending"
  | "running"
  | "passed"
  | "failed"
  | "skipped"
  | "error"
  | "timeout";

// Vision assertion types
export interface VisionAssertion {
  type: "element_within_bounds" | "text_present" | "visual_regression" | "elements_aligned";
  config: Record<string, unknown>;
}

// Vision configuration for qontinui_vision tests
export interface VisionConfig {
  assertions: VisionAssertion[];
  screenshot_path?: string;
  threshold?: number;
}

// Repository test configuration
export interface RepoTestConfig {
  command: string;
  working_directory?: string;
  parse_format: "pytest_json" | "jest_json" | "cargo_test" | "go_test" | "generic";
  env_vars?: Record<string, string>;
}

// Bounding box for detected elements
export interface BoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

// Detected element from page analysis
export interface DetectedElement {
  id: string;
  label: string;
  element_type: string;
  text_content?: string;
  bounding_box: BoundingBox;
  confidence: number;
  selector?: string;
  attributes: Record<string, unknown>;
}

// Page analysis result
export interface PageAnalysis {
  screenshot_base64: string;
  annotated_screenshot_base64: string;
  elements: DetectedElement[];
  source: "playwright" | "vision";
  captured_at: string;
  monitor_index?: number;
  url?: string;
}

// Visual evidence stored with test results
export interface VisualEvidence {
  annotated_screenshot_base64?: string;
  assertion_overlays?: Array<{
    element_id: string;
    result: "pass" | "fail";
    message?: string;
    bounding_box?: BoundingBox;
  }>;
  captured_at?: string;
}

// Verification test definition (from database)
export interface VerificationTest {
  id: string;
  name: string;
  description?: string;
  test_type: TestType;
  category?: TestCategory;
  playwright_code?: string;
  vision_config?: VisionConfig;
  python_code?: string;
  repo_test_config?: RepoTestConfig;
  success_criteria?: string;
  config: Record<string, unknown>;
  timeout_seconds: number;
  is_critical: boolean;
  enabled: boolean;
  ai_generated: boolean;
  ai_generation_prompt?: string;
  creation_analysis?: PageAnalysis;
  tags: string[];
  source_file?: string;
  last_exported_at?: string;
  created_at: string;
  updated_at: string;
}

// Test definition for execution (executor format)
export interface TestDefinition {
  id: string;
  name: string;
  test_type: TestType;
  category: TestCategory;
  playwright_code?: string;
  vision_config?: VisionConfig;
  python_code?: string;
  repo_test_config?: RepoTestConfig;
  timeout_seconds: number;
  is_critical: boolean;
  config: Record<string, unknown>;
}

// Test execution result
export interface TestExecutionResult {
  test_id: string;
  status: TestStatus;
  duration_ms: number;
  output: string;
  error?: string;
  screenshots: string[];
  assertions_passed: number;
  assertions_failed: number;
  structured_output?: Record<string, unknown>;
}

// Test result from database
export interface TestResult {
  id: string;
  test_id: string;
  task_run_id?: string;
  status: TestStatus;
  started_at?: string;
  completed_at?: string;
  duration_ms?: number;
  output?: string;
  error_message?: string;
  structured_output?: Record<string, unknown>;
  assertions_passed: number;
  assertions_failed: number;
  screenshots: string[];
  visual_evidence?: VisualEvidence;
  ai_analysis?: string;
  created_at: string;
}

// Input for creating a new test
export interface CreateTestInput {
  name: string;
  description?: string;
  test_type: TestType;
  category?: TestCategory;
  playwright_code?: string;
  vision_config?: VisionConfig;
  python_code?: string;
  repo_test_config?: RepoTestConfig;
  success_criteria?: string;
  config?: Record<string, unknown>;
  timeout_seconds?: number;
  is_critical?: boolean;
  enabled?: boolean;
  ai_generated?: boolean;
  ai_generation_prompt?: string;
  creation_analysis?: PageAnalysis;
  tags?: string[];
}

// Command response from Tauri
export interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}

// Test type metadata
export interface TestTypeInfo {
  type: TestType;
  name: string;
  description: string;
  requirements: string[];
  fields: Record<string, string>;
}

// Trigger points for test associations
export type TriggerPoint = "before_workflow" | "after_workflow" | "on_action" | "manual";

// Test association
export interface TestAssociation {
  id: string;
  test_id: string;
  config_id?: string;
  workflow_name?: string;
  trigger_point: TriggerPoint;
  action_id?: string;
  execution_order: number;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

// ============================================================================
// Workflow Run Context (for AI Test Generation)
// ============================================================================

/** Summary of a task run for selection in UI */
export interface TaskRunSummary {
  id: string;
  task_name: string;
  workflow_name?: string;
  status: string;
  created_at: string;
  completed_at?: string;
  duration_ms?: number;
  goal_achieved?: boolean;
}

/** Automation metrics from a workflow run */
export interface AutomationMetrics {
  workflow_name?: string;
  automation_status: string;
  duration_ms?: number;
  actions_summary?: {
    total: number;
    success: number;
    failed: number;
    skipped: number;
  };
  states_visited?: string[];
  transitions_executed?: Array<{
    from: string;
    to: string;
    action: string;
    success: boolean;
    duration_ms?: number;
  }>;
}

/** Screenshot captured during workflow execution */
export interface CapturedScreenshot {
  id: string;
  file_path: string;
  screenshot_type: string;
  template_name?: string;
  confidence?: number;
  match_location?: BoundingBox;
  created_at: string;
}

/** API request result from workflow execution */
export interface ApiRequestResult {
  id: string;
  step_name?: string;
  method: string;
  url: string;
  status_code: number;
  response_time_ms: number;
  success: boolean;
  response_body?: unknown;
  extractions?: Array<{
    variable_name: string;
    extracted_value: unknown;
    success: boolean;
  }>;
  error_message?: string;
}

/** Playwright test result from workflow execution */
export interface PlaywrightResult {
  id: string;
  test_name: string;
  status: string;
  duration_ms?: number;
  error_message?: string;
  assertions_passed: number;
  assertions_failed: number;
  page_snapshot?: string;
}

/** Event from workflow execution */
export interface WorkflowEvent {
  id: number;
  event_type: string;
  event_subtype?: string;
  message: string;
  data?: unknown;
  timestamp: string;
  duration_ms?: number;
}

/** Knowledge/finding from AI analysis during workflow */
export interface TaskKnowledgeItem {
  id: string;
  category: string;
  content: string;
  evidence?: string;
  confidence: string;
  is_resolved: boolean;
  created_at: string;
}

/** Full workflow run context for AI test generation */
export interface WorkflowRunContext {
  /** Task run metadata */
  task_run: {
    id: string;
    task_name: string;
    prompt?: string;
    status: string;
    workflow_name?: string;
    summary?: string;
    goal_achieved?: boolean;
    remaining_work?: string;
    created_at: string;
    completed_at?: string;
  };

  /** Automation metrics (if automation was run) */
  automation?: AutomationMetrics;

  /** Screenshots captured during execution */
  screenshots: CapturedScreenshot[];

  /** API requests made during execution */
  api_requests: ApiRequestResult[];

  /** Playwright test results */
  playwright_results: PlaywrightResult[];

  /** Execution events (limited to most relevant) */
  events: WorkflowEvent[];

  /** AI-detected knowledge/findings */
  knowledge: TaskKnowledgeItem[];

  /** Test results from verification tests */
  test_results: Array<{
    id: string;
    test_name: string;
    status: string;
    output?: string;
    error_message?: string;
    assertions_passed: number;
    assertions_failed: number;
  }>;
}

// ============================================================================
// Reference Documents (for AI Test Generation)
// ============================================================================

/** Document type for reference materials uploaded for AI test generation */
export type ReferenceDocumentType = "markdown" | "text" | "image" | "json";

/** A reference document uploaded by the user to provide context for AI test generation */
export interface ReferenceDocument {
  /** Unique ID for this document */
  id: string;
  /** Display name for the document */
  name: string;
  /** Document type */
  type: ReferenceDocumentType;
  /** Document content (text for markdown/text/json, base64 for images) */
  content: string;
  /** Original file name if uploaded */
  filename?: string;
  /** File size in bytes */
  size?: number;
}

// ============================================================================
// Collected Analysis Types (for AI Test Generation)
// ============================================================================

// Import step output types for the modular piping system
import type { StepOutput, StepOutputType } from "../../types/step-output";

// Analysis source type (extended to include step_output and live_browser)
export type AnalysisSourceType =
  | "playwright"
  | "vision"
  | "api_request"
  | "step_output"
  | "live_browser";

// API Request analysis result (from executing a saved API request)
export interface ApiRequestAnalysis {
  /** Unique ID for this analysis */
  id: string;
  /** Name of the saved API request used */
  request_name: string;
  /** HTTP method */
  method: string;
  /** Request URL */
  url: string;
  /** Response data */
  response: unknown;
  /** Response status code */
  status_code?: number;
  /** Duration in milliseconds */
  duration_ms?: number;
  /** When the request was executed */
  executed_at: string;
  /** Error message if failed */
  error?: string;
  /** Source request configuration (for auto-populating api_request_config in tests) */
  source_request_config?: {
    method: string;
    url: string;
    headers?: Record<string, string>;
    body?: string;
    timeout_ms?: number;
    follow_redirects?: boolean;
  };
}

/** Element captured from live browser via UI Bridge */
export interface LiveBrowserElement {
  id: string;
  tagName: string;
  type: string;
  text?: string;
  label?: string;
  value?: string;
  checked?: boolean;
  visible: boolean;
  enabled: boolean;
  bounds: { x: number; y: number; width: number; height: number };
}

/** Analysis data from live browser element capture */
export interface LiveBrowserAnalysis {
  elements: LiveBrowserElement[];
  url: string;
  title: string;
  captured_at: string;
}

// A single collected analysis (can be any type)
// Extended with step_output to support the modular piping system
export type CollectedAnalysis =
  | { type: "playwright"; id: string; name: string; data: PageAnalysis }
  | { type: "vision"; id: string; name: string; data: PageAnalysis }
  | { type: "api_request"; id: string; name: string; data: ApiRequestAnalysis }
  | { type: "step_output"; id: string; name: string; data: StepOutput }
  | { type: "live_browser"; id: string; name: string; data: LiveBrowserAnalysis };

// Re-export step output types for convenience
export type { StepOutput, StepOutputType };

// ============================================================================
// Helper Functions for Step Output Integration
// ============================================================================

/**
 * Create a CollectedAnalysis from a StepOutput.
 * This bridges the new modular step output system with the existing analysis system.
 */
export function createAnalysisFromStepOutput(output: StepOutput): CollectedAnalysis {
  return {
    type: "step_output",
    id: output.id,
    name: output.step_name,
    data: output,
  };
}

/**
 * Extract StepOutput from a CollectedAnalysis.
 * Handles both step_output type and converts api_request type to StepOutput.
 * Returns undefined for other analysis types (playwright, vision).
 */
export function getStepOutputFromAnalysis(analysis: CollectedAnalysis): StepOutput | undefined {
  if (analysis.type === "step_output") {
    return analysis.data;
  }

  // Convert api_request analysis to CommandStepOutput
  if (analysis.type === "api_request") {
    const apiData = analysis.data;
    return {
      id: apiData.id,
      step_type: "command",
      step_name: apiData.request_name,
      executed_at: apiData.executed_at,
      duration_ms: apiData.duration_ms ?? 0,
      success: !apiData.error && (apiData.status_code ?? 0) < 400,
      error: apiData.error,
      command: `${apiData.method} ${apiData.url}`,
      exit_code: apiData.status_code && apiData.status_code >= 400 ? 1 : 0,
      stdout: JSON.stringify(apiData.response ?? ""),
      stderr: apiData.error ?? "",
    } as StepOutput;
  }

  return undefined;
}

/**
 * Check if a CollectedAnalysis is a step output type.
 */
export function isStepOutputAnalysis(
  analysis: CollectedAnalysis,
): analysis is { type: "step_output"; id: string; name: string; data: StepOutput } {
  return analysis.type === "step_output";
}

// All collected analyses for AI test generation
export interface CollectedAnalysisSet {
  analyses: CollectedAnalysis[];
  collected_at: string;
}

// Legacy types for backward compatibility
export type HttpMethod = "GET" | "POST" | "PUT" | "DELETE";
export type ApiRequestStatus = "pending" | "running" | "complete" | "error";

export interface ApiRequestConfig {
  id: string;
  name: string;
  method: HttpMethod;
  endpoint: string;
  body?: Record<string, unknown>;
  headers?: Record<string, string>;
}

export interface ApiRequestWithState extends ApiRequestConfig {
  status: ApiRequestStatus;
  response?: unknown;
  error?: string;
  duration_ms?: number;
  executed_at?: string;
}

export interface MultiRequestAnalysis {
  requests: ApiRequestWithState[];
  total_elements?: number;
  collected_at: string;
}

// Combined analysis type (for backward compatibility)
export type AnalysisData =
  | { type: "single"; data: PageAnalysis }
  | { type: "multi"; data: MultiRequestAnalysis }
  | { type: "collected"; data: CollectedAnalysisSet };
