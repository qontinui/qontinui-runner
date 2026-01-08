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
