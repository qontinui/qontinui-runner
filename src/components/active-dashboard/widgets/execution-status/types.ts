/**
 * Execution Status Widget Types
 *
 * Type definitions for the execution status dashboard widget.
 */

import type { ExecutionStatus } from "../../../../types/executionStatus";

/**
 * Data structure for the execution status widget.
 */
export interface ExecutionStatusWidgetData {
  /** Current execution status */
  status: ExecutionStatus;
  /** Whether connected to events */
  isConnected: boolean;
  /** Last event timestamp */
  lastEventTime: number | null;
  /** Clear status callback */
  clearStatus: () => void;
}
