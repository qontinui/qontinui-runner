/**
 * Playwright Widget
 *
 * Dashboard widget for displaying Playwright test execution status.
 * Exports both full widget and compact summary components.
 */

export { PlaywrightWidget } from "./PlaywrightWidget";
export { PlaywrightSummary } from "./PlaywrightSummary";
export { usePlaywrightData } from "./usePlaywrightData";
export type { PlaywrightData, TestSpec, TestStatus } from "./types";
export { DEFAULT_PLAYWRIGHT_DATA } from "./types";
