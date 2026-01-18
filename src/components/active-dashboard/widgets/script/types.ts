/**
 * Script Widget Types
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { ScriptExecution, StepStats } from "../shared/types";

/**
 * Data provided by the useScriptData hook.
 */
export interface ScriptData {
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
 * Props for the full ScriptWidget component.
 */
export interface ScriptWidgetProps extends BaseWidgetProps {
  data: ScriptData;
}

/**
 * Props for the ScriptSummary component.
 */
export interface ScriptSummaryProps extends BaseWidgetProps {
  data: ScriptData;
}
