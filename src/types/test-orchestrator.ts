/**
 * Test Orchestrator Types
 *
 * TypeScript types for the AI-driven multi-step API test orchestration system.
 */

import type { HttpMethod } from "./test-builder";

/**
 * User input for creating a test orchestration plan
 */
export interface TestOrchestrationRequest {
  /** IDs of saved API requests to include in the orchestration */
  request_ids: string[];
  /** User's description of what the test should verify */
  test_description: string;
  /** Optional additional context or constraints */
  context?: string;
}

/**
 * AI-generated execution plan for the test orchestration
 */
export interface TestOrchestrationPlan {
  /** Unique identifier for this plan */
  id: string;
  /** Original user input */
  request: TestOrchestrationRequest;
  /** Ordered steps to execute */
  steps: OrchestrationStep[];
  /** Variables that will be extracted and passed between steps */
  variable_mappings: VariableMappingInfo[];
  /** What the AI suggests to verify in the final response */
  verification_suggestions: VerificationSuggestion[];
  /** AI's explanation of the plan */
  explanation: string;
  /** Timestamp when the plan was created */
  created_at: string;
}

/**
 * A single step in the orchestration plan
 */
export interface OrchestrationStep {
  /** Step index (0-based) */
  step_index: number;
  /** Human-readable name for this step */
  name: string;
  /** ID of the saved API request to execute */
  request_id: string;
  /** Variables to extract from this step's response */
  extractions: VariableExtraction[];
  /** Indices of steps that must complete before this one */
  depends_on: number[];
  /** Description of what this step does in the context of the test */
  purpose: string;
  /** URL template with variables (for display) */
  url_template: string;
  /** Body template with variables (for display, if applicable) */
  body_template?: string;
}

/**
 * Variable extraction configuration
 */
export interface VariableExtraction {
  /** Variable name to store the extracted value */
  variable_name: string;
  /** JSON path to extract (e.g., "$.data.id") */
  json_path: string;
  /** Default value if extraction fails */
  default_value?: string;
}

/**
 * Information about how a variable is passed between steps
 */
export interface VariableMappingInfo {
  /** Name of the variable */
  variable_name: string;
  /** Step index that produces this variable */
  source_step: number;
  /** JSON path used to extract the variable */
  json_path: string;
  /** Step indices that use this variable */
  used_in_steps: number[];
  /** Description of what this variable represents */
  description: string;
}

/**
 * AI's suggestion for what to verify in the test
 */
export interface VerificationSuggestion {
  /** What to verify (e.g., "response.states.length > 3") */
  condition: string;
  /** Human-readable description */
  description: string;
  /** JSON path to the value to check (if applicable) */
  json_path?: string;
  /** The step index whose response to check */
  step_index: number;
}

/**
 * Result of executing a single step
 */
export interface StepExecutionResult {
  /** Step index */
  step_index: number;
  /** Step name */
  step_name: string;
  /** The full HTTP request that was made */
  request: ExecutedRequest;
  /** The HTTP response received */
  response: ExecutedResponse;
  /** Variables extracted from this step */
  extracted_variables: Record<string, unknown>;
  /** Whether the step completed successfully */
  success: boolean;
  /** Error message if step failed */
  error?: string;
  /** Duration in milliseconds */
  duration_ms: number;
}

/**
 * Details of the executed HTTP request
 */
export interface ExecutedRequest {
  /** HTTP method */
  method: HttpMethod;
  /** The actual URL (after variable substitution) */
  url: string;
  /** Headers sent */
  headers: Record<string, string>;
  /** Body sent (if any) */
  body?: string;
}

/**
 * Details of the HTTP response received
 */
export interface ExecutedResponse {
  /** HTTP status code */
  status_code: number;
  /** Response headers */
  headers: Record<string, string>;
  /** Response body (as JSON if possible, otherwise raw string) */
  body: unknown;
  /** Content type */
  content_type?: string;
  /** Response size in bytes */
  size_bytes: number;
}

/**
 * Full result of executing an orchestration plan
 */
export interface OrchestrationExecutionResult {
  /** ID of the plan that was executed */
  plan_id: string;
  /** Results of each step in order */
  step_results: StepExecutionResult[];
  /** All variables that were extracted during execution */
  all_variables: Record<string, unknown>;
  /** Whether all steps completed successfully */
  success: boolean;
  /** Index of the first step that failed (if any) */
  failed_at_step?: number;
  /** Total execution time in milliseconds */
  total_duration_ms: number;
  /** Timestamp when execution started */
  started_at: string;
  /** Timestamp when execution completed */
  completed_at: string;
}

/**
 * A single step in the generated test (for preview)
 */
export interface TestStep {
  /** Step number (1-based) */
  step_number: number;
  /** What this step does */
  action: string;
  /** Expected outcome */
  expected: string;
  /** Type of step: setup, request, assertion, cleanup */
  step_type: "setup" | "request" | "assertion" | "cleanup";
}

/**
 * Generated test code from the orchestration
 */
export interface GeneratedTest {
  /** Name for the test */
  name: string;
  /** Description of what the test verifies */
  description: string;
  /** The generated test code */
  code: string;
  /** Type of test (e.g., "python_script", "playwright_cdp") */
  test_type: string;
  /** AI's explanation of the generated test */
  explanation: string;
  /** Structured preview of test steps */
  steps: TestStep[];
}

/**
 * Status of an orchestration (for UI updates)
 */
export type OrchestrationStatus =
  | { status: "idle" }
  | { status: "planning"; message: string }
  | { status: "plan_ready"; plan: TestOrchestrationPlan }
  | {
      status: "executing";
      current_step: number;
      total_steps: number;
      step_name: string;
    }
  | { status: "step_completed"; step_index: number; step_result: StepExecutionResult }
  | { status: "execution_complete"; result: OrchestrationExecutionResult }
  | { status: "generating"; message: string }
  | { status: "complete"; test: GeneratedTest }
  | { status: "error"; message: string; phase: string };

/**
 * Saved API request (from the library)
 */
export interface SavedApiRequest {
  id: string;
  name: string;
  description: string;
  category: string;
  tags: string[];
  method: HttpMethod;
  url: string;
  headers: Record<string, string>;
  body?: string;
  body_content_type?: string;
  timeout_ms: number;
  follow_redirects: boolean;
  variable_extractions: VariableExtraction[];
  assertions: unknown[];
  credential_id?: string;
  created_at: string;
  updated_at: string;
}

/**
 * Phase of the orchestrator UI
 */
export type OrchestratorPhase =
  | "select"
  | "planning"
  | "review"
  | "executing"
  | "generating"
  | "complete";

/**
 * State for the orchestrator panel
 */
export interface OrchestratorState {
  phase: OrchestratorPhase;
  selectedRequests: SavedApiRequest[];
  testDescription: string;
  additionalContext: string;
  plan: TestOrchestrationPlan | null;
  executionResult: OrchestrationExecutionResult | null;
  generatedTest: GeneratedTest | null;
  error: string | null;
  currentStepIndex: number;
}
