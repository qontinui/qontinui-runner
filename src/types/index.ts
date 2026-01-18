/**
 * Type definitions index
 *
 * Central export point for all type definitions used in the application.
 */

// Tree event types for hierarchical action logging
// Re-exported from qontinui-schemas with runner-specific extensions
export type {
  TreeEventData,
  DisplayNode,
  TreeNode,
  NodeMetadata,
  PathElement,
  TreeEvent,
  TreeEventCreate,
  TreeEventResponse,
  TreeEventListResponse,
  ExecutionTreeResponse,
  RuntimeData,
  StateContext,
  TimingInfo,
  Outcome,
  MatchLocation,
  TopMatch,
} from "./treeEvents";

export {
  NodeType,
  NodeStatus,
  TreeEventType,
  ActionType as TreeActionType,
  createDisplayNode,
} from "./treeEvents";

// State machine types and functions
export type {
  TransitionExecutionResult,
  NavigationResult,
  ActiveStatesResult,
  TransitionInfo,
  AvailableTransitionsResult,
} from "./state-machine";

export {
  executeTransition,
  navigateToState,
  navigateToMultipleStates,
  getActiveStates,
  getAvailableTransitions,
  isStateActive,
  findTransitionById,
  getTransitionsToState,
  isAnyStateActive,
  areAllStatesActive,
} from "./state-machine";

// Display profile types for the new architecture
export type { ActionLogViewData, ActionLogEntry, CommandResponse } from "./displayProfile";

// Authentication types
export type { User, DeviceInfo, LoginResponse, AuthStatus, AuthContextValue } from "./auth";

// Web extraction types
export type {
  WebExtractionConfig,
  ExtractionStatus,
  ExtractionResult,
  ExtractedElement,
  ExtractedState,
  ExtractionSession,
  BoundingBox,
} from "./extraction";

// Playwright script types
export type {
  SyncStatus,
  DisplayMode,
  TestSpec,
  NetworkRequest,
  StructuredTestOutput,
  PlaywrightResult,
  PlaywrightScript,
  CreatePlaywrightScriptRequest,
  UpdatePlaywrightScriptRequest,
  RunPlaywrightScriptRequest,
  PlaywrightApiResponse,
  ScriptViewMode,
  ScriptExecutionState,
  ScriptBuilderFormState,
  ScriptFilterState,
} from "./playwright";

export { DEFAULT_SCRIPT_VALUES } from "./playwright";

// Scriptlet types
export type { Scriptlet, CreateScriptletRequest, UpdateScriptletRequest } from "./scriptlet";

// Context types (AI task guidance)
export type {
  Context,
  ContextAutoInclude,
  ContextScope,
  ContextMetadata,
  ContextWithMetadata,
  CreateContextRequest,
  UpdateContextRequest,
  ContextSelection,
  AutoDetectResult,
  ContextFilterOptions,
  ContextEditorState,
} from "./context";

// Findings types (categorized findings system)
export type {
  BuiltInCategoryId,
  ActionType,
  FindingStatus,
  FindingSeverity,
  UserInputType,
  FindingCategory,
  UserInputOption,
  UserInputRequest,
  CodeContext,
  Finding,
  CategorySummary,
  ReportSummary,
  PhaseInfo,
  ReportStatus,
  ExecutionReport,
  ParsedFinding,
  CategoryStore,
} from "./findings";

// Scheduler types (CI/CD scheduling system)
export type {
  ScheduleExpression,
  ScheduleOnce,
  ScheduleCron,
  ScheduleInterval,
  ScheduledTaskType,
  WorkflowTask,
  PromptTask,
  AutoFixTask,
  ScheduledTaskStatus,
  TaskExecutionRecord,
  ScheduledTask,
  SchedulerSettings,
  SchedulerStatus,
  NextTaskInfo,
  CreateScheduledTaskRequest,
  UpdateScheduledTaskRequest,
} from "./scheduler";

export {
  describeSchedule,
  describeTaskType,
  getStatusColor,
  isTaskRunning,
  hasCompletedSuccessfully,
  getTimeUntilNextRun,
} from "./scheduler";

// Geometry types (monitor, coordinates, regions)
// Re-exported from qontinui-schemas
export type { Coordinates, Region, Monitor, VirtualDesktop, MonitorInfo } from "./geometry";

export { CoordinateSystem } from "./geometry";

// Task run types (async AI task system)
export type { TaskRun, TaskRunStatus, RunPromptRequest, RunPromptResponse } from "./taskRun";

export {
  isTaskRunning as isAiTaskRunning,
  isTaskComplete,
  isTaskFailed,
  isTaskFinished,
} from "./taskRun";

// State explorer types (AI-driven state exploration)
export type {
  ExplorationStrategy,
  ExplorationConfig,
  ExplorationStatus,
  ElementCheck,
  StateExplorationResult,
  TransitionExplorationResult,
  ExplorationResult,
  ExplorationHistoryItem,
  ExplorationPlan,
  SavedExploration,
  CreateSavedExplorationRequest,
  UpdateSavedExplorationRequest,
} from "./state-explorer";

export { getDefaultExplorationConfig, getDefaultSavedExploration } from "./state-explorer";

// Statistics types (Tiered Information Model for dashboard)
export type {
  TieredInfoResponse,
  RunStatus,
  AnomalyType,
  AnomalySeverity,
  FlakyItemType,
  ActionsSummary,
  TransitionRecord,
  TemplateMatchRecord,
  Anomaly,
  RunDetails,
  TransitionStats,
  TemplateStats,
  StateStats,
  ErrorPattern,
  ConfigStatistics,
  FlakyItem,
  FlakinessSummary,
  RunFailureSummary,
  DebuggingContext,
  ExecutionOptions,
  RecordRunInput,
} from "./statistics";

export { DEFAULT_EXECUTION_OPTIONS } from "./statistics";

// Discovery types (Discovery Push mechanism)
export type {
  DiscoveryResponse,
  DiscoveryType,
  DiscoveryEvidence,
  DiscoveryPayload,
  PendingDiscovery,
  DiscoveryPreview,
  DiscoverySummary,
  SyncStatus as DiscoverySyncStatus,
  SyncResult,
} from "./discoveries";

export { getDiscoveryTypeLabel, getDiscoveryTypeColor, formatConfidence } from "./discoveries";

// GUI Workflow types (deterministic action sequences)
export type {
  GuiActionType,
  GuiWorkflowStep,
  SavedGuiWorkflow,
  CreateGuiWorkflowRequest,
  UpdateGuiWorkflowRequest,
  GuiWorkflowStepResult,
  GuiWorkflowRunResult,
} from "./gui-workflow";

export {
  getDefaultGuiWorkflowStep,
  getDefaultStepName,
  getDefaultGuiWorkflow,
  getActionTypeIcon,
  getActionTypeLabel,
  validateGuiWorkflowStep,
  validateGuiWorkflow,
} from "./gui-workflow";

// DOM Capture types (browser DOM snapshot capture)
export type {
  DomCaptureSource,
  DomCaptureTrigger,
  DomCaptureType,
  DomCapture,
  DomCaptureWithHtml,
  CaptureFromExtensionRequest,
  DomCaptureApiResponse,
} from "./domCapture";

// Unified Workflow types (phase-based workflow builder)
export type {
  WorkflowPhase,
  LogSourceSelection,
  ScriptStep,
  StateStep,
  WorkflowRefStep,
  GuiActionStep,
  TestStep,
  ScreenshotStep,
  PromptStep,
  UnifiedStep,
  SetupStep,
  VerificationStep,
  AgenticStep,
  CompletionStep,
  UnifiedWorkflow,
  WorkflowFeatures,
  StepTypeInfo,
  GuiActionType as UnifiedGuiActionType,
  TestType as UnifiedTestType,
  PlaywrightExecutionMode,
  ApiVariableExtraction,
  ApiAssertion,
  ApiRequestStep,
  HttpMethod,
  ApiContentType,
} from "./unified-workflow";

// Re-export unified workflow types with original names for internal use
export type {
  GuiActionType as WorkflowGuiActionType,
  TestType as WorkflowTestType,
} from "./unified-workflow";

export {
  detectWorkflowFeatures,
  STEP_TYPES,
  GUI_ACTION_TYPES,
  PHASE_INFO,
  generateStepId,
  createDefaultStep,
  createDefaultWorkflow,
  isWorkflowEmpty,
  getTotalStepCount,
  getStepPhase,
  canStepExistInPhase,
} from "./unified-workflow";

// Saved API Request types (Library templates)
export type {
  SavedApiRequest,
  CreateSavedApiRequestRequest,
  UpdateSavedApiRequestRequest,
} from "./saved-api-request";

// Saved Prompt types (AI prompt library)
export type {
  SavedPrompt,
  CreateSavedPromptRequest,
  UpdateSavedPromptRequest,
} from "./saved-prompt";

// Saved Shell Command types (shell command library)
export type {
  SavedShellCommand,
  CreateSavedShellCommandRequest,
  UpdateSavedShellCommandRequest,
} from "./saved-shell-command";

// Lifecycle Hooks types (execution event triggers)
export type {
  HookTrigger,
  HookActionType,
  CommandAction,
  WebhookAction,
  LogAction,
  NotificationAction,
  HookAction,
  ConditionOperator,
  HookCondition,
  Hook,
  CreateHookRequest,
  UpdateHookRequest,
  HookExecutionResult,
} from "./hooks";

export {
  HOOK_TRIGGER_DISPLAY_NAMES,
  HOOK_TRIGGER_DESCRIPTIONS,
  HOOK_ACTION_TYPE_DISPLAY_NAMES,
  HOOK_ACTION_TYPE_DESCRIPTIONS,
  CONDITION_OPERATOR_DISPLAY_NAMES,
  CONDITION_VARIABLES,
  createDefaultCommandAction,
  createDefaultWebhookAction,
  createDefaultLogAction,
  createDefaultNotificationAction,
  createDefaultAction,
  createDefaultHook,
} from "./hooks";

// Learning Insights Dashboard types
export type {
  OutcomeStatus,
  DecisionOutcome,
  PatternType,
  InsightCategory,
  Trend,
  FeedbackType,
  Decision,
  TaskOutcome,
  Pattern,
  Insight,
  AnalysisResult,
  Feedback,
  LearningSummary,
  LearningDashboardData,
  // Task run integration types
  TaskRunInfo,
  TaskLearningOutcome,
  TaskRunWithOutcome,
  CurrentRunningTask,
  LearningStatsSummary,
} from "./learning";

// Flow Designer types (deterministic workflows)
export type {
  ParallelMerge,
  FlowStatus,
  StepType,
  Condition,
  FlowInput,
  FlowOutput,
  FlowStep,
  Flow,
  StepExecution,
  FlowState,
  FlowSummary,
  FlowExecutionSummary,
} from "./flow";

export {
  STEP_TYPE_LABELS,
  STEP_TYPE_COLORS,
  getStepTypeName,
  createExpressionCondition,
} from "./flow";

// Checkpoint Browser types (time-travel debugging)
export type {
  ChannelSnapshot,
  KnowledgeEntry,
  CriterionResult,
  VerificationSnapshot,
  FindingSnapshot,
  StateSnapshot,
  CheckpointTrigger,
  Checkpoint,
  CheckpointSummary,
  AutoCheckpointSettings,
  ChannelChange,
  CheckpointDiff,
  ReplayStatus,
  ModificationType,
  ReplayModification,
  ReplaySession,
  CheckpointStats,
} from "./checkpoint";

export {
  TRIGGER_TYPE_LABELS,
  TRIGGER_TYPE_COLORS,
  formatTimestamp,
  formatDateTime,
} from "./checkpoint";

// Recap types (Session overview)
export type { RecapStep, FailureInfo, RecapStats, RecapData, RecapResponse } from "./recap";
