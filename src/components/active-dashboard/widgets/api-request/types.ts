/**
 * API Request Widget Types
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { ApiRequestExecution, StepStats } from "../shared/types";

/**
 * Data provided by the useApiRequestData hook.
 */
export interface ApiRequestData {
  /** List of API request executions */
  requests: ApiRequestExecution[];
  /** Currently running request (if any) */
  currentRequest: ApiRequestExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full ApiRequestWidget component.
 */
export interface ApiRequestWidgetProps extends BaseWidgetProps {
  data: ApiRequestData;
}

/**
 * Props for the ApiRequestSummary component.
 */
export interface ApiRequestSummaryProps extends BaseWidgetProps {
  data: ApiRequestData;
}
