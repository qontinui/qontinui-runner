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

// Verification agent types (AI-driven state verification)
export type {
  VerificationStrategy,
  VerificationTaskConfig,
  VerificationStatus,
  ElementCheck,
  StateVerification,
  TransitionVerification,
  VerificationResult,
  VerificationHistoryItem,
  VerificationPlan,
  SavedVerification,
  CreateSavedVerificationRequest,
  UpdateSavedVerificationRequest,
} from "./verification-agent";

export { getDefaultVerificationConfig, getDefaultSavedVerification } from "./verification-agent";

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
