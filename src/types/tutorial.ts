/**
 * Tutorial Framework Type Definitions for qontinui-runner
 *
 * Event-driven interactive tutorial system inspired by Node-RED and Appsmith.
 * All tutorials are contextual (spotlight + tooltip) - no overlay mode.
 */

/**
 * Tutorial display mode
 * All tutorials are now contextual (in-page with spotlights and tooltips)
 */
export type TutorialMode = "contextual";

/**
 * Highlight types for contextual tutorials
 */
export type HighlightType = "spotlight" | "border" | "pulse" | "arrow";

/**
 * Tooltip position relative to target element
 */
export type TooltipPosition = "top" | "bottom" | "left" | "right" | "center";

/**
 * Difficulty levels for tutorials and steps
 */
export type DifficultyLevel = "beginner" | "intermediate" | "advanced";

/**
 * Target element configuration for contextual tutorials
 * Defines which UI element to highlight and how to interact with it
 */
export interface TargetElement {
  /** CSS selector or data-tutorial-id attribute to find the element */
  selector: string;

  /** Type of visual highlighting to apply */
  highlightType: HighlightType;

  /** Position of the tooltip relative to the target */
  position: TooltipPosition;

  /** Whether user can interact with the highlighted element */
  allowInteraction?: boolean;

  /** Optional scroll behavior when focusing element */
  scrollIntoView?: boolean;

  /** Optional delay before highlighting (milliseconds) */
  delay?: number;

  /** Optional custom offset from element (pixels) */
  offset?: { x: number; y: number };
}

/**
 * Validation feedback messages
 */
export interface ValidationFeedback {
  /** Message shown when validation passes */
  success: string;

  /** Message shown when validation fails */
  failure: string;

  /** Optional hint to help user succeed */
  hint?: string;
}

/**
 * Validation configuration for interactive tutorial steps
 * Uses callback functions instead of serialized strings for type safety
 */
export interface StepValidation {
  /** Validation function that returns true if step is valid */
  validate: () => boolean | Promise<boolean>;

  /** Feedback messages for different outcomes */
  feedback: ValidationFeedback;

  /** Optional flag to allow skipping validation */
  optional?: boolean;
}

/**
 * Event types that can trigger step advancement
 * Inspired by Node-RED's tour system
 */
export type WaitEventType = "dom-event" | "tauri-event" | "dom-appear" | "app-action";

/**
 * Tour state passed to filter functions
 * Allows steps to store data for later steps (Node-RED pattern)
 */
export interface TourState {
  /** Current step index (0-based) */
  stepIndex: number;

  /** ID of the current tutorial */
  tutorialId: string;

  /** Shared data object - steps can store values here for later steps */
  data: Record<string, unknown>;
}

/**
 * Timeout behavior when user doesn't complete expected action
 */
export type TimeoutBehavior = "show-hint" | "allow-skip";

/**
 * Event-driven step wait condition (Node-RED inspired)
 * Defines what the tutorial waits for before advancing to the next step
 */
export interface StepWaitCondition {
  /**
   * Type of event to wait for:
   * - dom-event: Standard DOM events (click, input, change, etc.)
   * - tauri-event: Tauri backend events (workflow-saved, execution-started, etc.)
   * - dom-appear: Wait for a DOM element to appear (MutationObserver)
   * - app-action: Custom application action (via useTutorialAware hook)
   */
  type: WaitEventType;

  /**
   * DOM event name (for type: 'dom-event')
   * Examples: 'click', 'input', 'change', 'focus', 'blur'
   */
  event?: string;

  /**
   * CSS selector for DOM events or dom-appear
   * Supports both CSS selectors and data-tutorial-id attributes
   */
  selector?: string;

  /**
   * Tauri event name (for type: 'tauri-event')
   * Examples: 'workflow-saved', 'execution-started', 'config-loaded'
   */
  tauriEvent?: string;

  /**
   * App action name (for type: 'app-action')
   * Components call notifyAction(actionName) to trigger this
   */
  actionName?: string;

  /**
   * Filter function to validate if event should trigger advancement
   * Receives event data and tour state, returns true to advance
   * Can also store data in tourState.data for later steps
   */
  filter?: (eventData: unknown, tourState: TourState) => boolean;

  /**
   * Timeout in milliseconds before showing hint (default: 30000)
   * Set to 0 or undefined to disable timeout
   */
  timeout?: number;

  /**
   * What happens when timeout is reached
   * - show-hint: Display the hint message
   * - allow-skip: Show skip button in addition to hint
   */
  onTimeout?: TimeoutBehavior;

  /**
   * Hint message to display after timeout
   * Should help the user understand what action to take
   */
  hint?: string;

  /**
   * Delay in milliseconds after the condition is met before advancing
   * Useful for giving users time to see what happened before moving on
   * Default: 0 (advance immediately)
   */
  advanceDelay?: number;
}

/**
 * External resource link
 */
export interface TutorialResource {
  title: string;
  url: string;
  type?: "documentation" | "video" | "article" | "api-reference";
}

/**
 * Individual step in a tutorial sequence
 */
export interface TutorialStep {
  /** Unique identifier for this step */
  id: string;

  /** Title of this step */
  title: string;

  /** Main content (supports basic markdown formatting) */
  content: string;

  /** Optional additional details text */
  details?: string;

  /** Optional keyboard shortcuts relevant to this step */
  shortcuts?: string[];

  /** Optional path to a screenshot image for this step */
  screenshot?: string;

  /** Optional difficulty level specific to this step */
  difficulty?: DifficultyLevel;

  /** Optional estimated duration for this step in minutes */
  estimatedDuration?: number;

  /** Optional tips or best practices relevant to this step */
  tips?: string[];

  /** Optional links to external resources or documentation */
  resources?: TutorialResource[];

  /** Optional target element for contextual tutorials */
  targetElement?: TargetElement;

  /** Optional validation for interactive steps (legacy - prefer wait) */
  validation?: StepValidation;

  /** Optional action instruction string to display to user */
  action?: string;

  /**
   * Event-driven wait condition (Node-RED inspired)
   * When specified, tutorial waits for this condition before advancing
   * If not specified, user must click Next manually
   */
  wait?: StepWaitCondition;

  /**
   * Whether user can interact with the target element during this step
   * When true, spotlight overlay allows clicks to pass through
   */
  interactive?: boolean;

  /**
   * Optional callback run before step is displayed
   * Use for setup like scrolling to element, opening panels, etc.
   */
  prepare?: () => void | Promise<void>;

  /**
   * Optional callback run after step is completed
   * Use for cleanup or state changes before next step
   */
  complete?: () => void | Promise<void>;
}

/**
 * Tutorial trigger configuration
 */
export interface TutorialTriggers {
  /** Auto-start on first app load */
  automatic?: boolean;

  /** Can be manually started from help menu */
  manual?: boolean;

  /** Tauri event name that triggers this tutorial */
  tauriEvent?: string;
}

/**
 * Page/tab identifiers that tutorials can navigate to
 */
export type TutorialFocusPage =
  | "gui-automation"
  | "run" // Legacy alias for gui-automation
  | "active"
  | "unified-workflow-builder"
  | "workflow-builder"
  | "macro-builder"
  | "playwright-test-builder"
  | "check-builder"
  | "library"
  | "ai"
  | "settings"
  | "help"
  | "evaluation"
  | "llm-analytics"
  | "meta-optimizer"
  | "knowledge-explorer"
  | "activity-timeline"
  | "watchers";

/**
 * Complete tutorial definition
 */
export interface Tutorial {
  /** Unique identifier for the tutorial */
  id: string;

  /** Display title of the tutorial */
  title: string;

  /** Detailed description of what the tutorial teaches */
  description: string;

  /** Estimated duration (e.g., "5 minutes", "15 minutes") */
  duration: string;

  /** Overall difficulty level of the tutorial */
  difficulty: DifficultyLevel;

  /** Ordered array of tutorial steps */
  steps: TutorialStep[];

  /** Tutorial display mode */
  mode: TutorialMode;

  /** Optional page/tab to navigate to when starting the tutorial */
  focusPage?: TutorialFocusPage;

  /** Optional trigger configuration */
  triggers?: TutorialTriggers;

  /** Optional prerequisites (tutorial IDs that should be completed first) */
  prerequisites?: string[];

  /** Optional learning objectives for the entire tutorial */
  learningObjectives?: string[];

  /** Optional category for organizing tutorials */
  category?: string;

  /** Optional tags for search and filtering */
  tags?: string[];
}

/**
 * User progress for a single tutorial step
 */
export interface StepProgress {
  /** ID of the tutorial step */
  stepId: string;

  /** Whether the step has been completed */
  completed: boolean;

  /** Timestamp when step was marked as completed (milliseconds since epoch) */
  timestamp: number;

  /** Optional time spent on this step in milliseconds */
  timeSpent?: number;
}

/**
 * Overall user progress tracking for a complete tutorial
 */
export interface TutorialProgress {
  /** ID of the tutorial being tracked */
  tutorialId: string;

  /** Current step index (0-based) */
  currentStepIndex: number;

  /** Array of progress records for each step */
  stepProgress: StepProgress[];

  /** Timestamp when user started the tutorial (milliseconds since epoch) */
  startedAt: number;

  /** Optional timestamp when user completed the tutorial (milliseconds since epoch) */
  completedAt?: number;
}

/**
 * Validation state for current step
 */
export interface ValidationState {
  /** Whether validation is currently running */
  isValidating: boolean;

  /** Whether the last validation passed */
  isValid: boolean;

  /** Feedback message to display */
  feedback: string;

  /** Type of feedback (success or failure) */
  feedbackType: "success" | "failure";
}

/**
 * Tutorial state persisted to localStorage
 */
export interface PersistedTutorialState {
  /** IDs of completed tutorials */
  completedTutorials: string[];

  /** IDs of tutorials that were started but not finished */
  inProgressTutorials: string[];

  /** Whether user has opted out of auto-show tutorials */
  dontShowAgain: boolean;

  /** Tutorial progress records for in-progress tutorials */
  progressRecords: Record<string, TutorialProgress>;
}

/**
 * Type guard to check if a value is a valid DifficultyLevel
 */
export function isDifficultyLevel(value: unknown): value is DifficultyLevel {
  return ["beginner", "intermediate", "advanced"].includes(value as string);
}

/**
 * Type guard to check if a value is a valid TutorialMode
 */
export function isTutorialMode(value: unknown): value is TutorialMode {
  return value === "contextual";
}

/**
 * Type guard to check if a value is a valid HighlightType
 */
export function isHighlightType(value: unknown): value is HighlightType {
  return ["spotlight", "border", "pulse", "arrow"].includes(value as string);
}
