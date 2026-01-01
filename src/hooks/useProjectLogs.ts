/**
 * useProjectLogs - Re-export for backward compatibility
 *
 * This file re-exports from the new modular structure in project-logs/.
 * New code should import directly from './project-logs' or '../hooks'.
 *
 * @deprecated Import from './project-logs' or '../hooks' instead
 */

export { useProjectLogs } from "./project-logs";
export type { UseProjectLogsReturn } from "./project-logs";
