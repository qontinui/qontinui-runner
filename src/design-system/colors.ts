/**
 * Centralized color system for qontinui-runner
 *
 * Type definitions are re-exported from @qontinui/shared-types/library.
 * Color mapping constants and lookup functions are re-exported from @qontinui/workflow-utils.
 */

// ============================================
// Types from shared-types package
// ============================================

export type {
  SeverityLevel,
  SeverityColorClasses,
  StatusColorClasses,
  AccentColor,
  AccentColorClasses,
} from "@qontinui/shared-types/library";

// Re-export with runner-local names for backward compatibility
export type { StatusColorType as StatusType } from "@qontinui/shared-types/library";
export type { ActionColorType as ActionType } from "@qontinui/shared-types/library";
export type { ActionColorClasses } from "@qontinui/shared-types/library";

// ============================================
// Color functions from workflow-utils package
// ============================================

export {
  getSeverityColors,
  getStatusColors,
  getActionColors,
  getAccentColors,
} from "@qontinui/workflow-utils";
