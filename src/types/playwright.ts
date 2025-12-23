/**
 * Playwright Script Library Types
 *
 * TypeScript definitions for Playwright test script management.
 * Mirrors the Rust data structures in src-tauri/src/playwright.rs
 */

/**
 * Sync status for cloud backup
 */
export type SyncStatus = "local_only" | "synced" | "local_modified" | "cloud_modified" | "conflict";

/**
 * Display mode for Playwright test execution
 * - headless: Run without browser UI (fastest, for CI)
 * - headed: Launch a new browser window with UI visible
 * - connect_existing: Connect to an existing Chrome browser via CDP (Chrome DevTools Protocol)
 */
export type DisplayMode = "headless" | "headed" | "connect_existing";

/**
 * A single test spec result
 */
export interface TestSpec {
  /** Test title */
  title: string;
  /** Source file */
  file: string;
  /** Test status: passed, failed, skipped */
  status: "passed" | "failed" | "skipped" | string;
  /** Duration in milliseconds */
  duration_ms: number;
  /** Error message if failed */
  error?: string;
  /** Retry count */
  retry: number;
}

/**
 * Network request info (optional tracing)
 */
export interface NetworkRequest {
  url: string;
  method: string;
  status: number;
  duration_ms: number;
}

/**
 * Structured output for AI analysis
 */
export interface StructuredTestOutput {
  /** Individual test results */
  specs: TestSpec[];
  /** Console output lines */
  console_output: string[];
  /** Network requests (optional) */
  network_requests?: NetworkRequest[];
  /** Page snapshot from error-context.md (YAML showing all elements on page) */
  page_snapshot?: string;
}

/**
 * Result of a Playwright test execution
 */
export interface PlaywrightResult {
  /** Whether all tests passed */
  passed: boolean;
  /** Number of tests that passed */
  tests_passed: number;
  /** Number of tests that failed */
  tests_failed: number;
  /** Number of tests skipped */
  tests_skipped: number;
  /** Total duration in milliseconds */
  duration_ms: number;
  /** Error message if execution failed */
  error?: string;
  /** Path to HTML report */
  report_path?: string;
  /** Paths to screenshots taken */
  screenshots: string[];
  /** Path to trace file (if enabled) */
  trace_path?: string;
  /** ISO 8601 timestamp of execution */
  executed_at: string;
  /** Structured output for AI analysis */
  structured_output?: StructuredTestOutput;
}

/**
 * A saved Playwright script
 */
export interface PlaywrightScript {
  /** Local unique identifier (UUID v4) */
  id: string;
  /** Cloud ID (set after first sync) */
  cloud_id?: string;
  /** Display name for the script */
  name: string;
  /** Natural language description of what this test does */
  description: string;
  /** Additional instructions for AI code generation/refinement (not part of the test description) */
  ai_instructions?: string;
  /** Target web application URL (base URL for tests) */
  target_url: string;
  /** The complete .spec.ts file content */
  script_content: string;
  /** Category for organization (e.g., "E2E", "Smoke", "Regression") */
  category: string;
  /** Tags for filtering/searching */
  tags: string[];
  /** Timeout for test execution in seconds */
  timeout_seconds: number;
  /** Display mode: headless, headed, or connect_existing */
  display_mode: DisplayMode;
  /** Browser to use: chromium, firefox, webkit */
  browser: "chromium" | "firefox" | "webkit";
  /** Sync status for cloud backup */
  sync_status: SyncStatus;
  /** Cloud version (for conflict detection) */
  cloud_version?: number;
  /** Last sync timestamp (ISO 8601) */
  last_synced_at?: string;
  /** ISO 8601 timestamp of creation */
  created_at: string;
  /** ISO 8601 timestamp of last modification */
  modified_at: string;
  /** Last execution result (cached) */
  last_result?: PlaywrightResult;
}

/**
 * Request to create a new script
 */
export interface CreatePlaywrightScriptRequest {
  name: string;
  description?: string;
  ai_instructions?: string;
  target_url?: string;
  script_content: string;
  category?: string;
  tags?: string[];
  timeout_seconds?: number;
  display_mode?: DisplayMode;
  browser?: "chromium" | "firefox" | "webkit";
}

/**
 * Request to update an existing script
 */
export interface UpdatePlaywrightScriptRequest {
  name?: string;
  description?: string;
  ai_instructions?: string;
  target_url?: string;
  script_content?: string;
  category?: string;
  tags?: string[];
  timeout_seconds?: number;
  display_mode?: DisplayMode;
  browser?: "chromium" | "firefox" | "webkit";
}

/**
 * Request to run a script
 */
export interface RunPlaywrightScriptRequest {
  /** Optional URL override for this run */
  target_url_override?: string;
}

/**
 * API response wrapper
 */
export interface PlaywrightApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

/**
 * View mode for the Script Builder UI
 */
export type ScriptViewMode = "natural_language" | "code";

/**
 * Script execution state for UI
 */
export type ScriptExecutionState = "idle" | "running" | "completed" | "failed";

/**
 * Script builder form state
 */
export interface ScriptBuilderFormState {
  name: string;
  description: string;
  ai_instructions: string;
  target_url: string;
  script_content: string;
  category: string;
  tags: string[];
  timeout_seconds: number;
  display_mode: DisplayMode;
  browser: "chromium" | "firefox" | "webkit";
}

/**
 * Default values for new scripts
 */
export const DEFAULT_SCRIPT_VALUES: ScriptBuilderFormState = {
  name: "",
  description: "",
  ai_instructions: "",
  target_url: "",
  script_content: `import { test, expect } from '@playwright/test';

test('example test', async ({ page }) => {
  // Navigate to the target page
  await page.goto('/');

  // Add your test assertions here
  await expect(page).toHaveTitle(/./);
});
`,
  category: "",
  tags: [],
  timeout_seconds: 60,
  display_mode: "headless",
  browser: "chromium",
};

/**
 * Filter/search state for script list
 */
export interface ScriptFilterState {
  search: string;
  category: string;
  tags: string[];
  syncStatus?: SyncStatus;
}
