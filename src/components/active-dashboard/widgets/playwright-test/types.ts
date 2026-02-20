/**
 * Playwright Test Widget Types
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { ScriptExecution, StepStats } from "../shared/types";

/**
 * Data provided by the usePlaywrightTestData hook.
 */
export interface PlaywrightTestData {
  /** List of script executions */
  scripts: ScriptExecution[];
  /** Currently running script (if any) */
  currentScript: ScriptExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full PlaywrightTestWidget component.
 */
export interface PlaywrightTestWidgetProps extends BaseWidgetProps {
  data: PlaywrightTestData;
}

/**
 * Props for the PlaywrightTestSummary component.
 */
export interface PlaywrightTestSummaryProps extends BaseWidgetProps {
  data: PlaywrightTestData;
}
