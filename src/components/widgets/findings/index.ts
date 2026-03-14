/**
 * Findings Widget
 *
 * Dashboard widget for displaying AI-detected issues.
 * Exports both full widget and summary components.
 */

export { FindingsWidget } from "./FindingsWidget";
export { FindingsSummary } from "./FindingsSummary";
export {
  useFindingsData,
  getSeverityConfig,
  getCategoryConfig,
  getStatusConfig,
  getActionableFindingsCount,
} from "./useFindingsData";
export type {
  Finding,
  FindingsData,
  FindingSeverity,
  FindingCategory,
  FindingStatus,
} from "./types";
