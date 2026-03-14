/**
 * Playwright Widget Types
 *
 * Type definitions for the Playwright test execution widget.
 */

/**
 * Status of an individual test spec.
 */
export type TestStatus = "passed" | "failed" | "skipped" | "running" | "pending";

/**
 * Individual test specification result.
 */
export interface TestSpec {
  /** Test title/name */
  title: string;
  /** Current status of the test */
  status: TestStatus;
  /** Duration in milliseconds */
  durationMs: number;
  /** Error message if test failed */
  error?: string;
}

/**
 * Complete Playwright execution data for the widget.
 */
export interface PlaywrightData {
  /** List of test specifications */
  tests: TestSpec[];
  /** Currently executing test name (null if not running) */
  currentTest: string | null;
  /** Total number of tests */
  totalTests: number;
  /** Number of passed tests */
  passedTests: number;
  /** Number of failed tests */
  failedTests: number;
  /** Number of skipped tests */
  skippedTests: number;
  /** Console output lines */
  consoleOutput: string[];
  /** Screenshot paths from test execution */
  screenshots: string[];
  /** Whether tests are currently running */
  isRunning: boolean;
  /** Total duration in milliseconds */
  durationMs: number;
}

/**
 * Default empty state for Playwright data.
 */
export const DEFAULT_PLAYWRIGHT_DATA: PlaywrightData = {
  tests: [],
  currentTest: null,
  totalTests: 0,
  passedTests: 0,
  failedTests: 0,
  skippedTests: 0,
  consoleOutput: [],
  screenshots: [],
  isRunning: false,
  durationMs: 0,
};
