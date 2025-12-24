/**
 * WorkflowBuilderSubTab.tsx
 *
 * Workflow Builder sub-tab - wraps AiBuilderTab functionality
 * for building and running AI automation workflows.
 */

import { AiBuilderTab } from "../AiBuilderTab";
import type { UseProjectLogsReturn } from "../../hooks/useProjectLogs";

interface WorkflowBuilderSubTabProps {
  projectLogs: UseProjectLogsReturn;
  onNavigateToLogLocations: () => void;
}

export function WorkflowBuilderSubTab({ projectLogs, onNavigateToLogLocations }: WorkflowBuilderSubTabProps) {
  return <AiBuilderTab projectLogs={projectLogs} onNavigateToLogLocations={onNavigateToLogLocations} />;
}
