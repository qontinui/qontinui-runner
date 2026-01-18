/**
 * GUI Automation Widget Types
 *
 * Data types for the GUI automation dashboard widget.
 * These types define the shape of data displayed during GUI automation execution.
 */

import type { ActionItem, ImageRecognitionResult, ScreenshotInfo } from "../../types";

/**
 * Statistics for the GUI automation widget.
 */
export interface GuiAutomationStats {
  /** Number of actions that have completed (success, failed, or error) */
  actionsCompleted: number;
  /** Success rate as a percentage (0-100) */
  successRate: number;
  /** Average confidence from image recognition operations */
  averageConfidence: number;
  /** Elapsed time in seconds since workflow started */
  elapsedTime: number;
}

/**
 * Workflow metadata for the current automation run.
 */
export interface GuiAutomationWorkflow {
  /** Name of the currently running workflow */
  name: string | null;
  /** Current iteration (1-based) */
  iteration: number;
  /** Maximum number of iterations */
  maxIterations: number;
}

/**
 * Complete data structure for the GUI Automation widget.
 */
export interface GuiAutomationData {
  /** List of actions executed during the automation */
  actions: ActionItem[];
  /** Description of the currently running action */
  currentAction: string | null;
  /** Recent image recognition results */
  imageRecognitionResults: ImageRecognitionResult[];
  /** Captured screenshots (both annotated and playwright) */
  screenshots: ScreenshotInfo[];
  /** Execution statistics */
  stats: GuiAutomationStats;
  /** Workflow metadata */
  workflow: GuiAutomationWorkflow;
  /** Whether data is currently loading */
  isLoading: boolean;
  /** Error message if data fetch failed */
  error: string | null;
}

/**
 * Props for the full GUI Automation widget component.
 */
export interface GuiAutomationWidgetProps {
  /** Whether this is the active (primary) widget */
  isActive: boolean;
  /** Whether to render in compact summary mode */
  isSummary: boolean;
  /** Current activity status */
  status: import("../../../../types/dashboard/activity-types").ActivityStatus;
  /** Widget data */
  data: GuiAutomationData;
  /** Callback to navigate to detail page */
  onNavigateToDetail?: () => void;
  /** Callback when widget wants to become the active widget */
  onRequestFocus?: () => void;
  /** Additional CSS class for styling */
  className?: string;
}

/**
 * Props for the summary GUI Automation widget component.
 */
export interface GuiAutomationSummaryProps {
  /** Whether this is the active (primary) widget */
  isActive: boolean;
  /** Whether to render in compact summary mode */
  isSummary: boolean;
  /** Current activity status */
  status: import("../../../../types/dashboard/activity-types").ActivityStatus;
  /** Widget data */
  data: GuiAutomationData;
}
