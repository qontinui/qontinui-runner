/**
 * Widget Props
 *
 * Common prop interfaces for dashboard widgets.
 * All widgets implement these interfaces for consistent behavior.
 */

import type { ActivityStatus, ActivityType, TaskPhase } from "./activity-types";

/**
 * Base props that all widgets receive.
 */
export interface BaseWidgetProps {
  /** Whether this is the active (primary) widget taking 65% of space */
  isActive: boolean;
  /** Whether to render in compact summary mode */
  isSummary: boolean;
  /** Current activity status */
  status: ActivityStatus;
  /** Callback to navigate to the detail page */
  onNavigateToDetail?: () => void;
  /** Callback when widget wants to become the active widget */
  onRequestFocus?: () => void;
  /** Additional CSS class for styling */
  className?: string;
}

/**
 * Props for the shared widget header component.
 */
export interface WidgetHeaderProps {
  /** Widget display name */
  title: string;
  /** Icon name from lucide-react */
  icon: string;
  /** Accent color (Tailwind color name) */
  accentColor: string;
  /** Current status */
  status: ActivityStatus;
  /** Whether this is the active widget */
  isActive: boolean;
  /** Compact mode for summary headers */
  compact?: boolean;
  /** Progress percentage (0-100) if applicable */
  progress?: number;
  /** Item count to display (e.g., "5 actions") */
  itemCount?: number;
  /** Label for item count (e.g., "actions", "tests") */
  itemLabel?: string;
  /** Route for "View All" link */
  detailRoute?: string;
  /** Callback for "View All" click */
  onViewAll?: () => void;
}

/**
 * Standard result pattern for widget data hooks.
 *
 * @template TData The type of data returned by the hook
 */
export interface WidgetDataResult<TData> {
  /** The widget data */
  data: TData;
  /** Whether data is currently loading */
  isLoading: boolean;
  /** Error message if fetch failed */
  error: string | null;
  /** Function to manually refresh data */
  refresh: () => Promise<void>;
}

/**
 * Props for the dashboard control bar.
 */
export interface ControlBarProps {
  /** Task name to display */
  taskName: string | null;
  /** Current task phase */
  phase: TaskPhase;
  /** Overall dashboard status */
  status: "idle" | "running" | "completed" | "failed" | "stopped";
  /** Whether to show phase badge */
  showPhaseBadge: boolean;
  /** Callback for play/pause button */
  onPlayPause?: () => void;
  /** Callback for stop button */
  onStop?: () => void;
  /** Callback to navigate to execute page */
  onGoToExecute?: () => void;
  /** Whether play/pause is enabled */
  canPlayPause?: boolean;
  /** Whether stop is enabled */
  canStop?: boolean;
}

/**
 * Props for the dashboard bottom bar.
 */
export interface BottomBarProps {
  /** Current iteration (1-based) */
  iteration: number;
  /** Maximum iterations */
  maxIterations: number;
  /** Currently running activity type */
  activeActivity: ActivityType | null;
  /** Description of current action */
  currentAction: string | null;
  /** Whether any activity is running */
  isRunning: boolean;
}

/**
 * Props for the idle state component.
 */
export interface IdleStateProps {
  /** Callback to navigate to execute page */
  onGoToExecute: () => void;
}

/**
 * Summary widget click handler type.
 */
export type SummaryWidgetClickHandler = (activityType: ActivityType) => void;

/**
 * Navigation handler for detail pages.
 */
export type DetailNavigationHandler = (activityType: ActivityType) => void;
