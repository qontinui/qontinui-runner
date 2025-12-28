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
