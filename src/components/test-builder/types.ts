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
// Collected Analysis Types (for AI Test Generation)
// ============================================================================

// Analysis source type
export type AnalysisSourceType = "playwright" | "vision" | "api_request";

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

// A single collected analysis (can be any type)
export type CollectedAnalysis =
  | { type: "playwright"; id: string; name: string; data: PageAnalysis }
  | { type: "vision"; id: string; name: string; data: PageAnalysis }
  | { type: "api_request"; id: string; name: string; data: ApiRequestAnalysis };

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
