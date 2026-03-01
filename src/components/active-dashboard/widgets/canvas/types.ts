/**
 * Canvas Widget Types
 */

import type { BaseWidgetProps } from "../../../../types/dashboard/widget-props";
import type { CanvasPanel, CanvasUpdateEvent } from "@qontinui/shared-types";

/**
 * Data provided by the useCanvasData hook.
 */
export interface CanvasData {
  /** Active canvas panels sorted by priority */
  panels: CanvasPanel[];
  /** Number of active panels */
  panelCount: number;
  /** Whether data is loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
}

/**
 * Props for the full CanvasWidget component.
 */
export interface CanvasWidgetProps extends BaseWidgetProps {
  data: CanvasData;
}

/**
 * Props for the CanvasSummary component.
 */
export interface CanvasSummaryProps extends BaseWidgetProps {
  data: CanvasData;
}

export type { CanvasPanel, CanvasUpdateEvent };
