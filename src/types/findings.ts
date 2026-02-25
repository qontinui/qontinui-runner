/**
 * findings.ts
 *
 * Re-exported from @qontinui/shared-types/library.
 * The categorized findings types live in the library subpath of shared-types.
 */

// =============================================================================
// Types from shared-types package
// =============================================================================

export type {
  BuiltInCategoryId,
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
  ReportStatus,
  ExecutionReport,
  ParsedFinding,
  CategoryStore,
  PhaseInfo,
} from "@qontinui/shared-types/library";

// Re-export FindingActionType as ActionType for backward compatibility
// (runner code uses "ActionType" from findings.ts)
export type { FindingActionType as ActionType } from "@qontinui/shared-types/library";
