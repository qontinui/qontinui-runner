export type Maybe<T> = T | null;
export type InputMaybe<T> = Maybe<T>;
export type Exact<T extends { [key: string]: unknown }> = { [K in keyof T]: T[K] };
export type MakeOptional<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]?: Maybe<T[SubKey]> };
export type MakeMaybe<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]: Maybe<T[SubKey]> };
export type MakeEmpty<T extends { [key: string]: unknown }, K extends keyof T> = {
  [_ in K]?: never;
};
export type Incremental<T> =
  | T
  | { [P in keyof T]?: P extends " $fragmentName" | "__typename" ? T[P] : never };
/** All built-in and custom scalars, mapped to their actual values */
export type Scalars = {
  ID: { input: string; output: string };
  String: { input: string; output: string };
  Boolean: { input: boolean; output: boolean };
  Int: { input: number; output: number };
  Float: { input: number; output: number };
  /** A scalar that can represent any JSON value. */
  JSON: { input: unknown; output: unknown };
};

/** Result of a UI Bridge action (click, navigate, etc.). */
export type ActionResult = {
  /** Response data from the browser (if successful). */
  data?: Maybe<Scalars["JSON"]["output"]>;
  /** Action execution time in milliseconds. */
  durationMs: Scalars["String"]["output"];
  /** Structured error detail (if failed). */
  error?: Maybe<UiBridgeErrorDetail>;
  /** Whether the action succeeded. */
  success: Scalars["Boolean"]["output"];
};

export type AiOutputChunkEvent = {
  accumulatedLength: Scalars["Int"]["output"];
  chunk: Scalars["String"]["output"];
  taskRunId: Scalars["String"]["output"];
};

/** Circuit breaker state for the UI Bridge relay. */
export const CircuitBreakerState = {
  /** Circuit is closed — requests flow normally. */
  Closed: "CLOSED",
  /** Circuit is half-open — a single probe request is allowed through. */
  HalfOpen: "HALF_OPEN",
  /** Circuit is open — requests are rejected to prevent cascading failure. */
  Open: "OPEN",
} as const;

export type CircuitBreakerState = (typeof CircuitBreakerState)[keyof typeof CircuitBreakerState];
/** Input for creating a new task run. */
export type CreateTaskRunInput = {
  autoContinue?: Scalars["Boolean"]["input"];
  configId?: InputMaybe<Scalars["String"]["input"]>;
  maxSessions?: InputMaybe<Scalars["Int"]["input"]>;
  prompt?: InputMaybe<Scalars["String"]["input"]>;
  taskName: Scalars["String"]["input"];
  taskType?: Scalars["String"]["input"];
  workflowId?: InputMaybe<Scalars["String"]["input"]>;
  workflowName?: InputMaybe<Scalars["String"]["input"]>;
};

export type FindingDetectedEvent = {
  finding: Scalars["JSON"]["output"];
};

export type FindingResolvedEvent = {
  finding: Scalars["JSON"]["output"];
};

export type GenericRunnerEvent = {
  data: Scalars["JSON"]["output"];
  eventType: Scalars["String"]["output"];
};

/** A finding detected by AI analysis. */
export type GqlFinding = {
  actionType: GqlFindingActionType;
  category: GqlFindingCategory;
  codeContext?: Maybe<GqlFindingCodeContext>;
  description: Scalars["String"]["output"];
  detectedAt: Scalars["String"]["output"];
  id: Scalars["String"]["output"];
  resolution?: Maybe<Scalars["String"]["output"]>;
  resolvedAt?: Maybe<Scalars["String"]["output"]>;
  resolvedInSession?: Maybe<Scalars["Int"]["output"]>;
  sessionNum: Scalars["Int"]["output"];
  severity: GqlFindingSeverity;
  signatureHash: Scalars["String"]["output"];
  status: GqlFindingStatus;
  taskRunId: Scalars["String"]["output"];
  title: Scalars["String"]["output"];
  updatedAt: Scalars["String"]["output"];
  userResponse?: Maybe<Scalars["String"]["output"]>;
};

/** Finding action type. */
export const GqlFindingActionType = {
  AutoFix: "AUTO_FIX",
  Informational: "INFORMATIONAL",
  NeedsUserInput: "NEEDS_USER_INPUT",
} as const;

export type GqlFindingActionType = (typeof GqlFindingActionType)[keyof typeof GqlFindingActionType];
/** Finding category enum for GraphQL. */
export const GqlFindingCategory = {
  AlreadyFixed: "ALREADY_FIXED",
  CodeBug: "CODE_BUG",
  ConfigIssue: "CONFIG_ISSUE",
  DataMigration: "DATA_MIGRATION",
  Documentation: "DOCUMENTATION",
  Enhancement: "ENHANCEMENT",
  ExpectedBehavior: "EXPECTED_BEHAVIOR",
  Performance: "PERFORMANCE",
  RuntimeIssue: "RUNTIME_ISSUE",
  Security: "SECURITY",
  TestIssue: "TEST_ISSUE",
  Todo: "TODO",
  Warning: "WARNING",
} as const;

export type GqlFindingCategory = (typeof GqlFindingCategory)[keyof typeof GqlFindingCategory];
/** Code context for a finding. */
export type GqlFindingCodeContext = {
  column?: Maybe<Scalars["Int"]["output"]>;
  file?: Maybe<Scalars["String"]["output"]>;
  line?: Maybe<Scalars["Int"]["output"]>;
  snippet?: Maybe<Scalars["String"]["output"]>;
};

/** Finding severity enum for GraphQL. */
export const GqlFindingSeverity = {
  Critical: "CRITICAL",
  High: "HIGH",
  Info: "INFO",
  Low: "LOW",
  Medium: "MEDIUM",
} as const;

export type GqlFindingSeverity = (typeof GqlFindingSeverity)[keyof typeof GqlFindingSeverity];
/** Finding lifecycle status. */
export const GqlFindingStatus = {
  Deferred: "DEFERRED",
  Detected: "DETECTED",
  InProgress: "IN_PROGRESS",
  NeedsInput: "NEEDS_INPUT",
  Resolved: "RESOLVED",
  WontFix: "WONT_FIX",
} as const;

export type GqlFindingStatus = (typeof GqlFindingStatus)[keyof typeof GqlFindingStatus];
/** Summary statistics for findings in a task run. */
export type GqlFindingSummary = {
  byCategory: Scalars["JSON"]["output"];
  bySeverity: Scalars["JSON"]["output"];
  byStatus: Scalars["JSON"]["output"];
  needsInputCount: Scalars["Int"]["output"];
  outstandingCount: Scalars["Int"]["output"];
  resolvedCount: Scalars["Int"]["output"];
  taskRunId: Scalars["String"]["output"];
  total: Scalars["Int"]["output"];
};

/** Orchestration loop status. */
export type GqlOrchestrationLoopStatus = {
  currentIteration: Scalars["Int"]["output"];
  error?: Maybe<Scalars["String"]["output"]>;
  phase: Scalars["String"]["output"];
  running: Scalars["Boolean"]["output"];
  startedAt?: Maybe<Scalars["String"]["output"]>;
};

/** Overall runner status. */
export type GqlRunnerStatus = {
  aiAnalysisRunning: Scalars["Boolean"]["output"];
  apiPort: Scalars["Int"]["output"];
  configLoaded: Scalars["Boolean"]["output"];
  configPath?: Maybe<Scalars["String"]["output"]>;
  executorRunning: Scalars["Boolean"]["output"];
  executorState: Scalars["String"]["output"];
  instanceName?: Maybe<Scalars["String"]["output"]>;
};

/** A task run — the single concept for all runs (AI, automation, or mixed). */
export type GqlTaskRun = {
  autoContinue: Scalars["Boolean"]["output"];
  bridgeId?: Maybe<Scalars["String"]["output"]>;
  completedAt?: Maybe<Scalars["String"]["output"]>;
  configId?: Maybe<Scalars["String"]["output"]>;
  createdAt: Scalars["String"]["output"];
  depth: Scalars["Int"]["output"];
  errorMessage?: Maybe<Scalars["String"]["output"]>;
  fixerSourceTaskRunId?: Maybe<Scalars["String"]["output"]>;
  followUpSourceTaskRunId?: Maybe<Scalars["String"]["output"]>;
  goalAchieved?: Maybe<Scalars["Boolean"]["output"]>;
  id: Scalars["String"]["output"];
  isFixer: Scalars["Boolean"]["output"];
  isFollowUp: Scalars["Boolean"]["output"];
  isMetaOptimizer: Scalars["Boolean"]["output"];
  isReflection: Scalars["Boolean"]["output"];
  maxSessions?: Maybe<Scalars["Int"]["output"]>;
  parentTaskRunId?: Maybe<Scalars["String"]["output"]>;
  prompt?: Maybe<Scalars["String"]["output"]>;
  reflectionSourceTaskRunId?: Maybe<Scalars["String"]["output"]>;
  remainingWork?: Maybe<Scalars["String"]["output"]>;
  resultData?: Maybe<Scalars["JSON"]["output"]>;
  rootTaskRunId?: Maybe<Scalars["String"]["output"]>;
  sessionsCount: Scalars["Int"]["output"];
  status: Scalars["String"]["output"];
  summary?: Maybe<Scalars["String"]["output"]>;
  taskName: Scalars["String"]["output"];
  taskType: Scalars["String"]["output"];
  triggeredBy?: Maybe<Scalars["String"]["output"]>;
  updatedAt: Scalars["String"]["output"];
  workflowId?: Maybe<Scalars["String"]["output"]>;
  workflowName?: Maybe<Scalars["String"]["output"]>;
  workflowType?: Maybe<Scalars["String"]["output"]>;
  workspaceId?: Maybe<Scalars["String"]["output"]>;
};

/** Paginated task run output. */
export type GqlTaskRunOutput = {
  /** Output text (may be truncated by offset/limit). */
  content: Scalars["String"]["output"];
  /** Whether there is more content after this chunk. */
  hasMore: Scalars["Boolean"]["output"];
  /** Character offset of this content within the full output. */
  offset: Scalars["Int"]["output"];
  /** The task run ID. */
  taskRunId: Scalars["String"]["output"];
  /** Total length of the full output in characters. */
  totalLength: Scalars["Int"]["output"];
};

/** Full unified workflow definition including step details as JSON. */
export type GqlWorkflow = {
  agenticSteps: Scalars["JSON"]["output"];
  approvalGate: Scalars["Boolean"]["output"];
  category: Scalars["String"]["output"];
  completionSteps: Scalars["JSON"]["output"];
  createdAt: Scalars["String"]["output"];
  description: Scalars["String"]["output"];
  enforceTokenBudget: Scalars["Boolean"]["output"];
  id: Scalars["String"]["output"];
  isFavorite: Scalars["Boolean"]["output"];
  maxIterations: Scalars["Int"]["output"];
  model?: Maybe<Scalars["String"]["output"]>;
  multiAgentMode: Scalars["Boolean"]["output"];
  name: Scalars["String"]["output"];
  provider?: Maybe<Scalars["String"]["output"]>;
  reflectionMode: Scalars["Boolean"]["output"];
  setupSteps: Scalars["JSON"]["output"];
  stages: Scalars["JSON"]["output"];
  stopOnFailure: Scalars["Boolean"]["output"];
  strictCwd: Scalars["Boolean"]["output"];
  tags: Array<Scalars["String"]["output"]>;
  timeoutSeconds?: Maybe<Scalars["Int"]["output"]>;
  updatedAt: Scalars["String"]["output"];
  useWorktree: Scalars["Boolean"]["output"];
  verificationSteps: Scalars["JSON"]["output"];
  workflowArchitecture?: Maybe<Scalars["String"]["output"]>;
};

/** Summary of a unified workflow (lightweight, no step details). */
export type GqlWorkflowSummary = {
  category: Scalars["String"]["output"];
  createdAt: Scalars["String"]["output"];
  description: Scalars["String"]["output"];
  id: Scalars["String"]["output"];
  isFavorite: Scalars["Boolean"]["output"];
  maxIterations: Scalars["Int"]["output"];
  model?: Maybe<Scalars["String"]["output"]>;
  multiAgentMode: Scalars["Boolean"]["output"];
  name: Scalars["String"]["output"];
  provider?: Maybe<Scalars["String"]["output"]>;
  stageCount: Scalars["Int"]["output"];
  tags: Array<Scalars["String"]["output"]>;
  timeoutSeconds?: Maybe<Scalars["Int"]["output"]>;
  updatedAt: Scalars["String"]["output"];
  workflowArchitecture?: Maybe<Scalars["String"]["output"]>;
};

export type MutationRoot = {
  /** Create a new task run. */
  createTaskRun: GqlTaskRun;
  /** Delete a task run by ID. */
  deleteTaskRun: Scalars["Boolean"]["output"];
  /** Delete a unified workflow by ID. */
  deleteWorkflow: Scalars["Boolean"]["output"];
  /** Pause a running task run. */
  pauseTaskRun: Scalars["Boolean"]["output"];
  /** Simple ping mutation for connectivity testing. */
  ping: Scalars["String"]["output"];
  /** Set a user response on a finding that needs input. */
  respondToFinding: Scalars["Boolean"]["output"];
  /**
   * Run a unified workflow by ID. Creates a task run record and returns it.
   * Note: For full workflow execution (verification-agentic loop), use
   * POST /unified-workflows/{id}/run which handles session management,
   * command sanitization, and the full executor pipeline.
   */
  runWorkflow: GqlTaskRun;
  /** Stop a running task run (kills AI processes and marks as stopped). */
  stopTaskRun: Scalars["Boolean"]["output"];
  /** Clear captured console errors. */
  uiBridgeClearConsoleErrors: ActionResult;
  /** Evaluate a JavaScript expression in the browser. */
  uiBridgeEvaluate: Scalars["JSON"]["output"];
  /** Execute an action on a UI element (click, type, scroll, etc.). */
  uiBridgeExecuteAction: ActionResult;
  /** Fill a form with the provided values. */
  uiBridgeFillForm: ActionResult;
  /** Navigate back in browser history. */
  uiBridgePageBack: ActionResult;
  /** Navigate forward in browser history. */
  uiBridgePageForward: ActionResult;
  /** Navigate to a URL. */
  uiBridgePageNavigate: ActionResult;
  /** Refresh the current page. */
  uiBridgePageRefresh: ActionResult;
  /** Execute a CSS selector query in the browser. */
  uiBridgeQuerySelector: Scalars["JSON"]["output"];
  /** Execute any UI Bridge command with raw JSON params. */
  uiBridgeRawCommand: ActionResult;
  /** Trigger redo in the connected browser. */
  uiBridgeRedo: ActionResult;
  /** Reset the UI Bridge circuit breaker to Closed state. */
  uiBridgeResetCircuitBreaker: Scalars["Boolean"]["output"];
  /** Trigger undo in the connected browser. */
  uiBridgeUndo: ActionResult;
  /** Unpause a paused task run. */
  unpauseTaskRun: Scalars["Boolean"]["output"];
  /** Update a finding's status (e.g., resolve, defer, mark won't-fix). */
  updateFindingStatus: Scalars["Boolean"]["output"];
};

export type MutationRootCreateTaskRunArgs = {
  input: CreateTaskRunInput;
};

export type MutationRootDeleteTaskRunArgs = {
  id: Scalars["String"]["input"];
};

export type MutationRootDeleteWorkflowArgs = {
  id: Scalars["String"]["input"];
};

export type MutationRootPauseTaskRunArgs = {
  id: Scalars["String"]["input"];
};

export type MutationRootRespondToFindingArgs = {
  findingId: Scalars["String"]["input"];
  response: Scalars["String"]["input"];
};

export type MutationRootRunWorkflowArgs = {
  prompt?: InputMaybe<Scalars["String"]["input"]>;
  workflowId: Scalars["String"]["input"];
};

export type MutationRootStopTaskRunArgs = {
  id: Scalars["String"]["input"];
};

export type MutationRootUiBridgeEvaluateArgs = {
  expression: Scalars["String"]["input"];
};

export type MutationRootUiBridgeExecuteActionArgs = {
  action: Scalars["String"]["input"];
  elementId: Scalars["String"]["input"];
  params?: InputMaybe<Scalars["JSON"]["input"]>;
};

export type MutationRootUiBridgeFillFormArgs = {
  formId?: InputMaybe<Scalars["String"]["input"]>;
  values: Scalars["JSON"]["input"];
};

export type MutationRootUiBridgePageNavigateArgs = {
  url: Scalars["String"]["input"];
};

export type MutationRootUiBridgePageRefreshArgs = {
  hard?: Scalars["Boolean"]["input"];
};

export type MutationRootUiBridgeQuerySelectorArgs = {
  selector: Scalars["String"]["input"];
};

export type MutationRootUiBridgeRawCommandArgs = {
  command: Scalars["String"]["input"];
  params?: InputMaybe<Scalars["JSON"]["input"]>;
};

export type MutationRootUnpauseTaskRunArgs = {
  id: Scalars["String"]["input"];
};

export type MutationRootUpdateFindingStatusArgs = {
  input: UpdateFindingStatusInput;
};

export type OrchestratorStateChangeEvent = {
  iteration?: Maybe<Scalars["Int"]["output"]>;
  phase: Scalars["String"]["output"];
  stateData?: Maybe<Scalars["JSON"]["output"]>;
  taskRunId: Scalars["String"]["output"];
  workflowStage: Scalars["String"]["output"];
};

export type QueryRoot = {
  /** Get child task runs of a parent task. */
  childTaskRuns: Array<GqlTaskRun>;
  /** Get finding summary statistics for a task run. */
  findingSummary: GqlFindingSummary;
  /** Get findings for a specific task run. */
  findings: Array<GqlFinding>;
  /** Get orchestration loop status. */
  orchestrationLoopStatus: GqlOrchestrationLoopStatus;
  /** Get the current runner status. */
  runnerStatus: GqlRunnerStatus;
  /** List only currently running task runs. */
  runningTaskRuns: Array<GqlTaskRun>;
  /** List all connected SDK app connections. */
  sdkConnections: Array<SdkConnectionInfo>;
  /** Get a single task run by ID. */
  taskRun?: Maybe<GqlTaskRun>;
  /**
   * Get task run output with optional offset/limit for pagination.
   * Defaults to last 10000 characters (tail) if no offset specified.
   */
  taskRunOutput: GqlTaskRunOutput;
  /** List recent task runs with optional filters. */
  taskRuns: Array<GqlTaskRun>;
  /** Get a specific component by ID. */
  uiBridgeComponent: Scalars["JSON"]["output"];
  /** Get all registered React/Vue/etc. components. */
  uiBridgeComponents: Scalars["JSON"]["output"];
  /** Get console errors from the connected browser. */
  uiBridgeConsoleErrors: Scalars["JSON"]["output"];
  /** Get full UI Bridge diagnostics as raw JSON. */
  uiBridgeDiagnostics: Scalars["JSON"]["output"];
  /** Get a specific element by ID. */
  uiBridgeElement: Scalars["JSON"]["output"];
  /** Get all registered UI elements from the connected browser. */
  uiBridgeElements: Scalars["JSON"]["output"];
  /** Get all forms on the current page. */
  uiBridgeForms: Scalars["JSON"]["output"];
  /** Get the current health status of the UI Bridge relay. */
  uiBridgeHealth: UiBridgeHealth;
  /** Get the idle/busy status of the browser. */
  uiBridgeIdleStatus: Scalars["JSON"]["output"];
  /** Get keyboard shortcuts registered in the connected app. */
  uiBridgeKeyboardShortcuts: Scalars["JSON"]["output"];
  /** Get network requests captured by the browser. */
  uiBridgeNetworkRequests: Scalars["JSON"]["output"];
  /**
   * Generic UI Bridge query — send any command and get raw JSON back.
   * Use this for commands not yet covered by typed resolvers.
   */
  uiBridgeRawQuery: Scalars["JSON"]["output"];
  /** Get the render log (in-memory circular buffer of render operations). */
  uiBridgeRenderLog: Scalars["JSON"]["output"];
  /** Get a full DOM snapshot from the connected browser. */
  uiBridgeSnapshot: Scalars["JSON"]["output"];
  /** Get a specific page spec by ID. */
  uiBridgeSpec: Scalars["JSON"]["output"];
  /** Get page specs registered via the UI Bridge SDK. */
  uiBridgeSpecs: Scalars["JSON"]["output"];
  /** Get undo/redo state from the connected browser. */
  uiBridgeUndoState: Scalars["JSON"]["output"];
  /** List all open windows/tabs. */
  uiBridgeWindows: Scalars["JSON"]["output"];
  /** Get a full workflow definition by ID. */
  workflow?: Maybe<GqlWorkflow>;
  /** List all unified workflows (summary view). */
  workflows: Array<GqlWorkflowSummary>;
};

export type QueryRootChildTaskRunsArgs = {
  parentTaskRunId: Scalars["String"]["input"];
};

export type QueryRootFindingSummaryArgs = {
  taskRunId: Scalars["String"]["input"];
};

export type QueryRootFindingsArgs = {
  taskRunId: Scalars["String"]["input"];
};

export type QueryRootTaskRunArgs = {
  id: Scalars["String"]["input"];
};

export type QueryRootTaskRunOutputArgs = {
  id: Scalars["String"]["input"];
  limit?: Scalars["Int"]["input"];
  offset?: Scalars["Int"]["input"];
};

export type QueryRootTaskRunsArgs = {
  limit?: Scalars["Int"]["input"];
  workflowType?: InputMaybe<Scalars["String"]["input"]>;
};

export type QueryRootUiBridgeComponentArgs = {
  id: Scalars["String"]["input"];
};

export type QueryRootUiBridgeElementArgs = {
  id: Scalars["String"]["input"];
};

export type QueryRootUiBridgeRawQueryArgs = {
  command: Scalars["String"]["input"];
  params?: InputMaybe<Scalars["JSON"]["input"]>;
};

export type QueryRootUiBridgeRenderLogArgs = {
  limit?: InputMaybe<Scalars["Int"]["input"]>;
};

export type QueryRootUiBridgeSpecArgs = {
  id: Scalars["String"]["input"];
};

export type QueryRootWorkflowArgs = {
  id: Scalars["String"]["input"];
};

/**
 * Typed runner event for the `runnerEvents` subscription.
 * Replaces untyped serde_json::Value events from /ws/events.
 */
export type RunnerEvent =
  | AiOutputChunkEvent
  | FindingDetectedEvent
  | FindingResolvedEvent
  | GenericRunnerEvent
  | OrchestratorStateChangeEvent
  | StepProgressEvent
  | TaskRunUpdateEvent;

/** Information about an external SDK app connection. */
export type SdkConnectionInfo = {
  /** App identifier. */
  appId: Scalars["String"]["output"];
  /** Display name. */
  appName: Scalars["String"]["output"];
  /** Whether this is the active (selected) connection. */
  isActive: Scalars["Boolean"]["output"];
  /** Whether the app is currently responsive. */
  responsive: Scalars["Boolean"]["output"];
  /** Base URL of the connected app. */
  url: Scalars["String"]["output"];
};

export type StepProgressEvent = {
  details?: Maybe<Scalars["JSON"]["output"]>;
  status: Scalars["String"]["output"];
  stepIndex: Scalars["Int"]["output"];
  stepName: Scalars["String"]["output"];
  taskRunId: Scalars["String"]["output"];
  timestamp: Scalars["String"]["output"];
};

export type SubscriptionRoot = {
  /**
   * Finding updates stream — polls DB for finding changes on a task run.
   * Emits the full findings list whenever the count or statuses change.
   */
  findingUpdates: Array<GqlFinding>;
  /** Orchestration loop status stream — pushes loop state at a configurable interval. */
  orchestrationLoopStatusStream: GqlOrchestrationLoopStatus;
  /**
   * Typed runner event stream — replaces untyped /ws/events endpoint.
   * Optional filter to receive only specific event types.
   */
  runnerEvents: RunnerEvent;
  /**
   * Task run progress stream — replaces 3s polling for task run status.
   * Filters events to only those matching the specified task run ID.
   */
  taskRunProgress: RunnerEvent;
  /**
   * Health status stream — pushes UI Bridge health at a configurable interval.
   * Replaces polling GET /health with server push.
   */
  uiBridgeHealthStream: UiBridgeHealth;
};

export type SubscriptionRootFindingUpdatesArgs = {
  intervalMs?: Scalars["Int"]["input"];
  taskRunId: Scalars["String"]["input"];
};

export type SubscriptionRootOrchestrationLoopStatusStreamArgs = {
  intervalMs?: Scalars["Int"]["input"];
};

export type SubscriptionRootRunnerEventsArgs = {
  filter?: InputMaybe<Array<Scalars["String"]["input"]>>;
};

export type SubscriptionRootTaskRunProgressArgs = {
  taskRunId: Scalars["String"]["input"];
};

export type SubscriptionRootUiBridgeHealthStreamArgs = {
  intervalMs?: Scalars["Int"]["input"];
};

export type TaskRunUpdateEvent = {
  details?: Maybe<Scalars["JSON"]["output"]>;
  iteration?: Maybe<Scalars["Int"]["output"]>;
  status: Scalars["String"]["output"];
  taskRunId: Scalars["String"]["output"];
  timestamp: Scalars["String"]["output"];
};

/**
 * Machine-readable error codes for UI Bridge operations.
 * Maps 1:1 to the existing UiBridgeErrorCode enum.
 */
export const UiBridgeErrorCode = {
  ActionFailed: "ACTION_FAILED",
  AssertionFailed: "ASSERTION_FAILED",
  CircuitBreakerOpen: "CIRCUIT_BREAKER_OPEN",
  ConcurrencyLimitReached: "CONCURRENCY_LIMIT_REACHED",
  ElementNotEnabled: "ELEMENT_NOT_ENABLED",
  ElementNotFound: "ELEMENT_NOT_FOUND",
  ElementNotVisible: "ELEMENT_NOT_VISIBLE",
  ElementStale: "ELEMENT_STALE",
  FrontendUnresponsive: "FRONTEND_UNRESPONSIVE",
  InternalError: "INTERNAL_ERROR",
  Timeout: "TIMEOUT",
  UnknownAssertionType: "UNKNOWN_ASSERTION_TYPE",
  WindowNotFound: "WINDOW_NOT_FOUND",
} as const;

export type UiBridgeErrorCode = (typeof UiBridgeErrorCode)[keyof typeof UiBridgeErrorCode];
/** Structured error detail with recovery hint. */
export type UiBridgeErrorDetail = {
  /** Machine-readable error code. */
  code: UiBridgeErrorCode;
  /** Additional context as JSON. */
  context?: Maybe<Scalars["JSON"]["output"]>;
  /** Human-readable error message. */
  message: Scalars["String"]["output"];
  /** Suggested recovery action (e.g., "Resnapshot", "RetryAfterDelay"). */
  recovery?: Maybe<Scalars["String"]["output"]>;
};

/** Real-time health status of the UI Bridge relay. */
export type UiBridgeHealth = {
  /** Current circuit breaker state. */
  circuitBreaker: CircuitBreakerState;
  /** Milliseconds since the last heartbeat. */
  heartbeatAgeMs: Scalars["String"]["output"];
  /** Timestamp of the last frontend heartbeat (epoch ms). */
  lastHeartbeat: Scalars["String"]["output"];
  /** Number of pending UI Bridge requests awaiting browser response. */
  pendingRequests: Scalars["Int"]["output"];
  /** Whether the browser frontend is responsive (last pong < 15s). */
  responsive: Scalars["Boolean"]["output"];
  /** Number of available semaphore permits (out of 6 max). */
  semaphoreAvailable: Scalars["Int"]["output"];
  /** Server uptime in seconds. */
  uptimeSeconds: Scalars["String"]["output"];
};

/** Input for updating a finding's status. */
export type UpdateFindingStatusInput = {
  findingId: Scalars["String"]["input"];
  resolution?: InputMaybe<Scalars["String"]["input"]>;
  status: GqlFindingStatus;
};

export type TaskRunFieldsFragment = {
  id: string;
  taskName: string;
  prompt?: string | null;
  taskType: string;
  status: string;
  sessionsCount: number;
  maxSessions?: number | null;
  autoContinue: boolean;
  errorMessage?: string | null;
  configId?: string | null;
  workflowName?: string | null;
  workflowId?: string | null;
  summary?: string | null;
  goalAchieved?: boolean | null;
  remainingWork?: string | null;
  workflowType?: string | null;
  parentTaskRunId?: string | null;
  rootTaskRunId?: string | null;
  depth: number;
  isReflection: boolean;
  isFollowUp: boolean;
  isFixer: boolean;
  isMetaOptimizer: boolean;
  createdAt: string;
  updatedAt: string;
  completedAt?: string | null;
};

export type FindingFieldsFragment = {
  id: string;
  taskRunId: string;
  sessionNum: number;
  category: GqlFindingCategory;
  severity: GqlFindingSeverity;
  status: GqlFindingStatus;
  actionType: GqlFindingActionType;
  title: string;
  description: string;
  resolution?: string | null;
  signatureHash: string;
  userResponse?: string | null;
  detectedAt: string;
  resolvedAt?: string | null;
  resolvedInSession?: number | null;
  updatedAt: string;
  codeContext?: {
    file?: string | null;
    line?: number | null;
    column?: number | null;
    snippet?: string | null;
  } | null;
};

export type TaskRunsQueryVariables = Exact<{
  limit?: InputMaybe<Scalars["Int"]["input"]>;
  workflowType?: InputMaybe<Scalars["String"]["input"]>;
}>;

export type TaskRunsQuery = {
  taskRuns: Array<{
    id: string;
    taskName: string;
    prompt?: string | null;
    taskType: string;
    status: string;
    sessionsCount: number;
    maxSessions?: number | null;
    autoContinue: boolean;
    errorMessage?: string | null;
    configId?: string | null;
    workflowName?: string | null;
    workflowId?: string | null;
    summary?: string | null;
    goalAchieved?: boolean | null;
    remainingWork?: string | null;
    workflowType?: string | null;
    parentTaskRunId?: string | null;
    rootTaskRunId?: string | null;
    depth: number;
    isReflection: boolean;
    isFollowUp: boolean;
    isFixer: boolean;
    isMetaOptimizer: boolean;
    createdAt: string;
    updatedAt: string;
    completedAt?: string | null;
  }>;
};

export type TaskRunQueryVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type TaskRunQuery = {
  taskRun?: {
    id: string;
    taskName: string;
    prompt?: string | null;
    taskType: string;
    status: string;
    sessionsCount: number;
    maxSessions?: number | null;
    autoContinue: boolean;
    errorMessage?: string | null;
    configId?: string | null;
    workflowName?: string | null;
    workflowId?: string | null;
    summary?: string | null;
    goalAchieved?: boolean | null;
    remainingWork?: string | null;
    workflowType?: string | null;
    parentTaskRunId?: string | null;
    rootTaskRunId?: string | null;
    depth: number;
    isReflection: boolean;
    isFollowUp: boolean;
    isFixer: boolean;
    isMetaOptimizer: boolean;
    createdAt: string;
    updatedAt: string;
    completedAt?: string | null;
  } | null;
};

export type RunningTaskRunsQueryVariables = Exact<{ [key: string]: never }>;

export type RunningTaskRunsQuery = {
  runningTaskRuns: Array<{
    id: string;
    taskName: string;
    prompt?: string | null;
    taskType: string;
    status: string;
    sessionsCount: number;
    maxSessions?: number | null;
    autoContinue: boolean;
    errorMessage?: string | null;
    configId?: string | null;
    workflowName?: string | null;
    workflowId?: string | null;
    summary?: string | null;
    goalAchieved?: boolean | null;
    remainingWork?: string | null;
    workflowType?: string | null;
    parentTaskRunId?: string | null;
    rootTaskRunId?: string | null;
    depth: number;
    isReflection: boolean;
    isFollowUp: boolean;
    isFixer: boolean;
    isMetaOptimizer: boolean;
    createdAt: string;
    updatedAt: string;
    completedAt?: string | null;
  }>;
};

export type WorkflowsQueryVariables = Exact<{ [key: string]: never }>;

export type WorkflowsQuery = {
  workflows: Array<{
    id: string;
    name: string;
    description: string;
    category: string;
    tags: Array<string>;
    maxIterations: number;
    timeoutSeconds?: number | null;
    provider?: string | null;
    model?: string | null;
    isFavorite: boolean;
    multiAgentMode: boolean;
    workflowArchitecture?: string | null;
    stageCount: number;
    createdAt: string;
    updatedAt: string;
  }>;
};

export type WorkflowQueryVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type WorkflowQuery = {
  workflow?: {
    id: string;
    name: string;
    description: string;
    category: string;
    tags: Array<string>;
    setupSteps: unknown;
    verificationSteps: unknown;
    agenticSteps: unknown;
    completionSteps: unknown;
    maxIterations: number;
    timeoutSeconds?: number | null;
    provider?: string | null;
    model?: string | null;
    isFavorite: boolean;
    multiAgentMode: boolean;
    reflectionMode: boolean;
    approvalGate: boolean;
    stopOnFailure: boolean;
    useWorktree: boolean;
    strictCwd: boolean;
    enforceTokenBudget: boolean;
    workflowArchitecture?: string | null;
    stages: unknown;
    createdAt: string;
    updatedAt: string;
  } | null;
};

export type FindingsQueryVariables = Exact<{
  taskRunId: Scalars["String"]["input"];
}>;

export type FindingsQuery = {
  findings: Array<{
    id: string;
    taskRunId: string;
    sessionNum: number;
    category: GqlFindingCategory;
    severity: GqlFindingSeverity;
    status: GqlFindingStatus;
    actionType: GqlFindingActionType;
    title: string;
    description: string;
    resolution?: string | null;
    signatureHash: string;
    userResponse?: string | null;
    detectedAt: string;
    resolvedAt?: string | null;
    resolvedInSession?: number | null;
    updatedAt: string;
    codeContext?: {
      file?: string | null;
      line?: number | null;
      column?: number | null;
      snippet?: string | null;
    } | null;
  }>;
};

export type FindingSummaryQueryVariables = Exact<{
  taskRunId: Scalars["String"]["input"];
}>;

export type FindingSummaryQuery = {
  findingSummary: {
    taskRunId: string;
    total: number;
    byCategory: unknown;
    bySeverity: unknown;
    byStatus: unknown;
    needsInputCount: number;
    resolvedCount: number;
    outstandingCount: number;
  };
};

export type TaskRunOutputQueryVariables = Exact<{
  id: Scalars["String"]["input"];
  offset?: InputMaybe<Scalars["Int"]["input"]>;
  limit?: InputMaybe<Scalars["Int"]["input"]>;
}>;

export type TaskRunOutputQuery = {
  taskRunOutput: {
    taskRunId: string;
    content: string;
    totalLength: number;
    offset: number;
    hasMore: boolean;
  };
};

export type RunnerStatusQueryVariables = Exact<{ [key: string]: never }>;

export type RunnerStatusQuery = {
  runnerStatus: {
    instanceName?: string | null;
    apiPort: number;
    executorRunning: boolean;
    executorState: string;
    configLoaded: boolean;
    configPath?: string | null;
    aiAnalysisRunning: boolean;
  };
};

export type OrchestrationLoopStatusQueryVariables = Exact<{ [key: string]: never }>;

export type OrchestrationLoopStatusQuery = {
  orchestrationLoopStatus: {
    running: boolean;
    phase: string;
    currentIteration: number;
    startedAt?: string | null;
    error?: string | null;
  };
};

export type UiBridgeHealthQueryVariables = Exact<{ [key: string]: never }>;

export type UiBridgeHealthQuery = {
  uiBridgeHealth: {
    responsive: boolean;
    lastHeartbeat: string;
    heartbeatAgeMs: string;
    uptimeSeconds: string;
    pendingRequests: number;
    circuitBreaker: CircuitBreakerState;
    semaphoreAvailable: number;
  };
};

export type StopTaskRunMutationVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type StopTaskRunMutation = { stopTaskRun: boolean };

export type PauseTaskRunMutationVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type PauseTaskRunMutation = { pauseTaskRun: boolean };

export type UnpauseTaskRunMutationVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type UnpauseTaskRunMutation = { unpauseTaskRun: boolean };

export type DeleteTaskRunMutationVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type DeleteTaskRunMutation = { deleteTaskRun: boolean };

export type CreateTaskRunMutationVariables = Exact<{
  input: CreateTaskRunInput;
}>;

export type CreateTaskRunMutation = {
  createTaskRun: {
    id: string;
    taskName: string;
    prompt?: string | null;
    taskType: string;
    status: string;
    sessionsCount: number;
    maxSessions?: number | null;
    autoContinue: boolean;
    errorMessage?: string | null;
    configId?: string | null;
    workflowName?: string | null;
    workflowId?: string | null;
    summary?: string | null;
    goalAchieved?: boolean | null;
    remainingWork?: string | null;
    workflowType?: string | null;
    parentTaskRunId?: string | null;
    rootTaskRunId?: string | null;
    depth: number;
    isReflection: boolean;
    isFollowUp: boolean;
    isFixer: boolean;
    isMetaOptimizer: boolean;
    createdAt: string;
    updatedAt: string;
    completedAt?: string | null;
  };
};

export type RunWorkflowMutationVariables = Exact<{
  workflowId: Scalars["String"]["input"];
  prompt?: InputMaybe<Scalars["String"]["input"]>;
}>;

export type RunWorkflowMutation = {
  runWorkflow: {
    id: string;
    taskName: string;
    prompt?: string | null;
    taskType: string;
    status: string;
    sessionsCount: number;
    maxSessions?: number | null;
    autoContinue: boolean;
    errorMessage?: string | null;
    configId?: string | null;
    workflowName?: string | null;
    workflowId?: string | null;
    summary?: string | null;
    goalAchieved?: boolean | null;
    remainingWork?: string | null;
    workflowType?: string | null;
    parentTaskRunId?: string | null;
    rootTaskRunId?: string | null;
    depth: number;
    isReflection: boolean;
    isFollowUp: boolean;
    isFixer: boolean;
    isMetaOptimizer: boolean;
    createdAt: string;
    updatedAt: string;
    completedAt?: string | null;
  };
};

export type UpdateFindingStatusMutationVariables = Exact<{
  input: UpdateFindingStatusInput;
}>;

export type UpdateFindingStatusMutation = { updateFindingStatus: boolean };

export type RespondToFindingMutationVariables = Exact<{
  findingId: Scalars["String"]["input"];
  response: Scalars["String"]["input"];
}>;

export type RespondToFindingMutation = { respondToFinding: boolean };

export type DeleteWorkflowMutationVariables = Exact<{
  id: Scalars["String"]["input"];
}>;

export type DeleteWorkflowMutation = { deleteWorkflow: boolean };

export type UiBridgeHealthStreamSubscriptionVariables = Exact<{
  intervalMs?: InputMaybe<Scalars["Int"]["input"]>;
}>;

export type UiBridgeHealthStreamSubscription = {
  uiBridgeHealthStream: {
    responsive: boolean;
    lastHeartbeat: string;
    heartbeatAgeMs: string;
    uptimeSeconds: string;
    pendingRequests: number;
    circuitBreaker: CircuitBreakerState;
    semaphoreAvailable: number;
  };
};

export type RunnerEventsSubscriptionVariables = Exact<{
  filter?: InputMaybe<Array<Scalars["String"]["input"]> | Scalars["String"]["input"]>;
}>;

export type RunnerEventsSubscription = {
  runnerEvents:
    | { taskRunId: string; chunk: string; accumulatedLength: number }
    | { finding: unknown }
    | { finding: unknown }
    | { eventType: string; data: unknown }
    | {
        taskRunId: string;
        workflowStage: string;
        iteration?: number | null;
        phase: string;
        stateData?: unknown | null;
      }
    | {
        taskRunId: string;
        stepIndex: number;
        stepName: string;
        status: string;
        details?: unknown | null;
        timestamp: string;
      }
    | {
        taskRunId: string;
        status: string;
        iteration?: number | null;
        details?: unknown | null;
        timestamp: string;
      };
};

export type TaskRunProgressSubscriptionVariables = Exact<{
  taskRunId: Scalars["String"]["input"];
}>;

export type TaskRunProgressSubscription = {
  taskRunProgress:
    | { taskRunId: string; chunk: string; accumulatedLength: number }
    | { eventType: string; data: unknown }
    | {
        taskRunId: string;
        workflowStage: string;
        iteration?: number | null;
        phase: string;
        stateData?: unknown | null;
      }
    | {
        taskRunId: string;
        stepIndex: number;
        stepName: string;
        status: string;
        details?: unknown | null;
        timestamp: string;
      }
    | {
        taskRunId: string;
        status: string;
        iteration?: number | null;
        details?: unknown | null;
        timestamp: string;
      }
    | Record<PropertyKey, never>;
};

export type FindingUpdatesSubscriptionVariables = Exact<{
  taskRunId: Scalars["String"]["input"];
  intervalMs?: InputMaybe<Scalars["Int"]["input"]>;
}>;

export type FindingUpdatesSubscription = {
  findingUpdates: Array<{
    id: string;
    taskRunId: string;
    sessionNum: number;
    category: GqlFindingCategory;
    severity: GqlFindingSeverity;
    status: GqlFindingStatus;
    actionType: GqlFindingActionType;
    title: string;
    description: string;
    resolution?: string | null;
    signatureHash: string;
    userResponse?: string | null;
    detectedAt: string;
    resolvedAt?: string | null;
    resolvedInSession?: number | null;
    updatedAt: string;
    codeContext?: {
      file?: string | null;
      line?: number | null;
      column?: number | null;
      snippet?: string | null;
    } | null;
  }>;
};

export type OrchestrationLoopStatusStreamSubscriptionVariables = Exact<{
  intervalMs?: InputMaybe<Scalars["Int"]["input"]>;
}>;

export type OrchestrationLoopStatusStreamSubscription = {
  orchestrationLoopStatusStream: {
    running: boolean;
    phase: string;
    currentIteration: number;
    startedAt?: string | null;
    error?: string | null;
  };
};
