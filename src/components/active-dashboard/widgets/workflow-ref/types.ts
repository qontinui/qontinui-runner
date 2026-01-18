/**
 * Workflow Ref Widget Types
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { WorkflowRefExecution, StepStats } from "../shared/types";

/**
 * Data provided by the useWorkflowRefData hook.
 */
export interface WorkflowRefData {
  /** List of workflow reference executions */
  workflows: WorkflowRefExecution[];
  /** Currently running workflow (if any) */
  currentWorkflow: WorkflowRefExecution | null;
  /** Execution statistics */
  stats: StepStats;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full WorkflowRefWidget component.
 */
export interface WorkflowRefWidgetProps extends BaseWidgetProps {
  data: WorkflowRefData;
}

/**
 * Props for the WorkflowRefSummary component.
 */
export interface WorkflowRefSummaryProps extends BaseWidgetProps {
  data: WorkflowRefData;
}
