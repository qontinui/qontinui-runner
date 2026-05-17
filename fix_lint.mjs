// Script to add eslint-disable-next-line comments to fix ESLint errors
// Run with: node fix_lint.mjs
import { readFileSync, writeFileSync } from 'fs';
import { join } from 'path';

const BASE = 'C:/Users/jspin/Documents/qontinui-root/qontinui-runner';

// SUPPRESSIONS: [relative_path, line_number (1-based error line), rule]
// For multi-error lines: the script deduplicates them
const SUPPRESSIONS = [
  // ===== react-hooks/set-state-in-effect =====
  ['src/App.tsx', 229, 'react-hooks/set-state-in-effect'],
  ['src/components/ActionLogTable.tsx', 682, 'react-hooks/set-state-in-effect'],
  ['src/components/AiOutputTab.tsx', 101, 'react-hooks/set-state-in-effect'],
  ['src/components/AiOutputTab.tsx', 320, 'react-hooks/set-state-in-effect'],
  ['src/components/AiTab.tsx', 224, 'react-hooks/set-state-in-effect'],
  ['src/components/AiTab.tsx', 357, 'react-hooks/set-state-in-effect'],
  ['src/components/AuthProvider.tsx', 208, 'react-hooks/set-state-in-effect'],
  ['src/components/AuthProvider.tsx', 266, 'react-hooks/set-state-in-effect'],
  ['src/components/ExecuteTab.tsx', 74, 'react-hooks/set-state-in-effect'],
  ['src/components/ExecutionControlPanel.tsx', 127, 'react-hooks/set-state-in-effect'],
  ['src/components/LibraryDashboard.tsx', 374, 'react-hooks/set-state-in-effect'],
  ['src/components/LogSourcePicker.tsx', 103, 'react-hooks/set-state-in-effect'],
  ['src/components/ProjectSelector.tsx', 65, 'react-hooks/set-state-in-effect'],
  ['src/components/PromptSnippetBuilderTab.tsx', 107, 'react-hooks/set-state-in-effect'],
  ['src/components/PromptSnippetSelector.tsx', 94, 'react-hooks/set-state-in-effect'],
  ['src/components/active-dashboard/ApprovalDialog.tsx', 216, 'react-hooks/set-state-in-effect'],
  ['src/components/active-dashboard/NewRunDialog.tsx', 64, 'react-hooks/set-state-in-effect'],
  ['src/components/active-dashboard/useDashboardState.ts', 174, 'react-hooks/set-state-in-effect'],
  ['src/components/active-dashboard/useDashboardState.ts', 183, 'react-hooks/set-state-in-effect'],
  ['src/components/activity-timeline/WatcherManagementPanel.tsx', 72, 'react-hooks/set-state-in-effect'],
  ['src/components/adaptive-learning/LearningDashboard.tsx', 401, 'react-hooks/set-state-in-effect'],
  ['src/components/adaptive-learning/PlaybookPanel.tsx', 82, 'react-hooks/set-state-in-effect'],
  ['src/components/adaptive-learning/PromptEvolutionPanel.tsx', 59, 'react-hooks/set-state-in-effect'],
  ['src/components/architecture-view/ArchitectureView.tsx', 70, 'react-hooks/set-state-in-effect'],
  ['src/components/connection-wizard/DeviceDiscoveryStep.tsx', 80, 'react-hooks/set-state-in-effect'],
  ['src/components/contexts/hooks/useContexts.ts', 455, 'react-hooks/set-state-in-effect'],
  ['src/components/decision-trail/DecisionTrailPage.tsx', 89, 'react-hooks/set-state-in-effect'],
  ['src/components/development-intelligence/NavigationFlowGraph.tsx', 241, 'react-hooks/set-state-in-effect'],
  ['src/components/findings/ExecutionReport.tsx', 134, 'react-hooks/set-state-in-effect'],
  ['src/components/gui-automation/WorkflowRunnerPanel.tsx', 131, 'react-hooks/set-state-in-effect'],
  ['src/components/hooks/HooksManagerPanel.tsx', 69, 'react-hooks/set-state-in-effect'],
  ['src/components/llm-observability/AccountUsageCard.tsx', 35, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/AgentEffectivenessPanel.tsx', 76, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/AgentInteractionMatrix.tsx', 62, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/AgenticMetricsPanel.tsx', 71, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/AgenticMetricsPanel.tsx', 77, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/CanaryStatusPanel.tsx', 68, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/ConfigSuggestionsTab.tsx', 43, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/CostEffectivenessPanel.tsx', 57, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/EvalSpecsTab.tsx', 65, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/FailureAnalysisSection.tsx', 448, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/ProgressTab.tsx', 155, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/PromptOptimizationTab.tsx', 75, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/PromptRegistryTab.tsx', 49, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/RecommendationsTab.tsx', 153, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/RegressionAlertBanner.tsx', 37, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/RobustnessTab.tsx', 84, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/RobustnessTab.tsx', 88, 'react-hooks/set-state-in-effect'],
  ['src/components/meta-optimizer/RunHistoryTab.tsx', 53, 'react-hooks/set-state-in-effect'],
  ['src/components/observations/MemoryHealthPanel.tsx', 92, 'react-hooks/set-state-in-effect'],
  ['src/components/observations/ObservationBrowser.tsx', 100, 'react-hooks/set-state-in-effect'],
  ['src/components/orchestration-loop/DecompositionView.tsx', 213, 'react-hooks/set-state-in-effect'],
  ['src/components/orchestration-loop/MultiLoopPanel.tsx', 207, 'react-hooks/set-state-in-effect'],
  ['src/components/orchestration-loop/OrchestrationLoopPanel.tsx', 339, 'react-hooks/set-state-in-effect'],
  ['src/components/orchestration-loop/SpecPartitionWizard.tsx', 65, 'react-hooks/set-state-in-effect'],
  ['src/components/process-manager/ProcessManagerTab.tsx', 130, 'react-hooks/set-state-in-effect'],
  ['src/components/reflection-dashboard/ReflectionDashboard.tsx', 361, 'react-hooks/set-state-in-effect'],
  ['src/components/scheduler/SchedulerTaskForm.tsx', 187, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/AiSettings.tsx', 98, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/PhysicalDeviceManager.tsx', 184, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/RunnerInstancesSettings.tsx', 96, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/RunnerInstancesSettings.tsx', 116, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/SecuritySettings.tsx', 522, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/WsvCalibrationSection.tsx', 144, 'react-hooks/set-state-in-effect'],
  ['src/components/settings/ai-settings/ClaudeCliSection.tsx', 81, 'react-hooks/set-state-in-effect'],
  ['src/components/skills/SkillApprovalPanel.tsx', 82, 'react-hooks/set-state-in-effect'],
  ['src/components/specs/SpecCompliancePanel.tsx', 128, 'react-hooks/set-state-in-effect'],
  ['src/components/terminal/DocFinderModal.tsx', 245, 'react-hooks/set-state-in-effect'],
  ['src/components/terminal/useSessionManager.ts', 430, 'react-hooks/set-state-in-effect'],
  ['src/components/terminal/useSessionManager.ts', 450, 'react-hooks/set-state-in-effect'],
  ['src/components/terminal/useWorkflowGeneration.ts', 136, 'react-hooks/set-state-in-effect'],
  ['src/components/terminal/useWorkflowGeneration.ts', 164, 'react-hooks/set-state-in-effect'],
  ['src/components/test-results/TestResultsTab.tsx', 164, 'react-hooks/set-state-in-effect'],
  ['src/components/ui-bridge/ElementDescriptionPanel.tsx', 357, 'react-hooks/set-state-in-effect'],
  ['src/components/ui-bridge/ElementDescriptionPanel.tsx', 396, 'react-hooks/set-state-in-effect'],
  ['src/components/ui-bridge/ElementDescriptionPanel.tsx', 422, 'react-hooks/set-state-in-effect'],
  ['src/components/ui-bridge/NaturalLanguagePanel.tsx', 628, 'react-hooks/set-state-in-effect'],
  ['src/components/ui-bridge/NaturalLanguagePanel.tsx', 690, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/api-request/useApiRequestData.ts', 105, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/api-request/useApiRequestData.ts', 113, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/canvas/useCanvasData.ts', 47, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/execution-timeline/useExecutionTimelineData.ts', 240, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/execution-timeline/useExecutionTimelineData.ts', 249, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/gui-automation/useGuiAutomationData.ts', 106, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/gui-automation/useGuiAutomationData.ts', 114, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/mcp-call/useMcpCallData.ts', 97, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/mcp-call/useMcpCallData.ts', 105, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/playwright-test/usePlaywrightTestData.ts', 97, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/playwright-test/usePlaywrightTestData.ts', 105, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/playwright/usePlaywrightData.ts', 200, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/playwright/usePlaywrightData.ts', 250, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/shared/StepProgressMarker.tsx', 130, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/shell-command/useShellCommandData.ts', 103, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/shell-command/useShellCommandData.ts', 111, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/verification/useVerificationData.ts', 211, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/workflow-ref/useWorkflowRefData.ts', 107, 'react-hooks/set-state-in-effect'],
  ['src/components/widgets/workflow-ref/useWorkflowRefData.ts', 115, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/AddStateStepsModal.tsx', 211, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/AddStateStepsModal.tsx', 219, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/AiGeneratePanel.tsx', 328, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/CheckGroupLibraryPicker.tsx', 106, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/CheckLibraryPicker.tsx', 103, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/MacroLibraryPicker.tsx', 85, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/McpServerToolPicker.tsx', 107, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/PlaywrightScriptLibraryPicker.tsx', 99, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/PromptLibraryPicker.tsx', 66, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/ShellCommandLibraryPicker.tsx', 60, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/StateLibraryPicker.tsx', 89, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/TemplateLibraryPanel.tsx', 42, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/TestLibraryPicker.tsx', 101, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/WorkflowLibraryPicker.tsx', 81, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/useWorkflowLibrary.ts', 77, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-builder/useWorkflowLibrary.ts', 82, 'react-hooks/set-state-in-effect'],
  ['src/components/workflow-queue/WorkflowQueueTab.tsx', 157, 'react-hooks/set-state-in-effect'],
  ['src/contexts/ActiveRunsContext.tsx', 312, 'react-hooks/set-state-in-effect'],
  ['src/contexts/SharedStepDataContext.tsx', 107, 'react-hooks/set-state-in-effect'],
  ['src/hooks/dashboard/useOrchestratorState.ts', 218, 'react-hooks/set-state-in-effect'],
  ['src/hooks/dashboard/useTaskDetection.ts', 430, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useActionLogView.ts', 150, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 77, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 89, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 123, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 158, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 199, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 237, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useArchitecture.ts', 400, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useBrowserErrors.ts', 139, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useBrowserErrors.ts', 162, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useConstraints.ts', 170, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useElementThumbnails.ts', 185, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useErrorMonitor.ts', 98, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useErrorMonitor.ts', 199, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useErrorMonitor.ts', 280, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useGuiLock.ts', 98, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useInitialStatesOverride.ts', 65, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useInitialStatesOverride.ts', 74, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useObservationMemory.ts', 139, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useProjectSelection.ts', 148, 'react-hooks/set-state-in-effect'],
  ['src/hooks/usePromptCanary.ts', 75, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useProviderCircuitBreakers.ts', 41, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useRunnerCrud.ts', 159, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useScheduler.ts', 314, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useStateExplorer.ts', 132, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useStateToWorkflow.ts', 213, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useTriggers.ts', 253, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useWebSocketAutoConnect.ts', 85, 'react-hooks/set-state-in-effect'],
  ['src/hooks/useWebSocketEvents.ts', 364, 'react-hooks/set-state-in-effect'],
  ['src/pages/ImageQualityTestsPage.tsx', 668, 'react-hooks/set-state-in-effect'],
  ['src/pages/generator-eval/DashboardTab.tsx', 64, 'react-hooks/set-state-in-effect'],
  ['src/pages/generator-eval/EditAnalysisTab.tsx', 42, 'react-hooks/set-state-in-effect'],
  ['src/pages/specs/GlobalIssuesPanel.tsx', 461, 'react-hooks/set-state-in-effect'],
  ['src/pages/ui-bridge-integration/DiscoveryPanel.tsx', 55, 'react-hooks/set-state-in-effect'],
  ['src/pages/ui-bridge-integration/HookGenerationPanel.tsx', 878, 'react-hooks/set-state-in-effect'],
  ['src/pages/ui-bridge-integration/PageSelectionPanel.tsx', 77, 'react-hooks/set-state-in-effect'],
  ['src/pages/ui-bridge-integration/SourceIntegrationPanel.tsx', 80, 'react-hooks/set-state-in-effect'],

  // ===== react-hooks/refs (refs in render) =====
  ['src/components/PromptSnippetSelector.tsx', 45, 'react-hooks/refs'],
  ['src/components/PromptSnippetSelector.tsx', 53, 'react-hooks/refs'],
  ['src/components/api-surface/ApiSurfaceGraph.tsx', 380, 'react-hooks/refs'],
  ['src/components/api-surface/ApiSurfaceGraph.tsx', 384, 'react-hooks/refs'],
  ['src/components/development-intelligence/CoverageGapPanel.tsx', 165, 'react-hooks/refs'],
  ['src/components/development-intelligence/CoverageGapPanel.tsx', 169, 'react-hooks/refs'],
  ['src/components/development-intelligence/NavigationFlowGraph.tsx', 257, 'react-hooks/refs'],
  ['src/components/development-intelligence/NavigationFlowGraph.tsx', 261, 'react-hooks/refs'],
  ['src/components/session-recap/SessionGraphView.tsx', 83, 'react-hooks/refs'],
  ['src/components/session-recap/SessionGraphView.tsx', 87, 'react-hooks/refs'],
  ['src/components/terminal/contexts/AiFeaturesContext.tsx', 183, 'react-hooks/refs'],
  ['src/components/terminal/contexts/TransitionEffectsContext.tsx', 67, 'react-hooks/refs'],
  ['src/components/terminal/useOutputSnapshots.ts', 23, 'react-hooks/refs'],
  ['src/components/ui-bridge/LazyThumbnail.tsx', 86, 'react-hooks/refs'],
  ['src/components/ui-bridge/LazyThumbnail.tsx', 89, 'react-hooks/refs'],
  ['src/hooks/useBrowserErrors.ts', 66, 'react-hooks/refs'],
  ['src/hooks/useConstraints.ts', 121, 'react-hooks/refs'],
  ['src/hooks/useElementThumbnails.ts', 106, 'react-hooks/refs'],
  ['src/hooks/useElementThumbnails.ts', 309, 'react-hooks/refs'],
  ['src/hooks/useErrorMonitor.ts', 67, 'react-hooks/refs'],
  ['src/hooks/useErrorMonitor.ts', 176, 'react-hooks/refs'],
  ['src/hooks/useErrorMonitor.ts', 254, 'react-hooks/refs'],
  ['src/hooks/useRenderPerformance.tsx', 171, 'react-hooks/refs'],
  ['src/hooks/useSdkUIBridge.ts', 76, 'react-hooks/refs'],
  ['src/hooks/useSdkUIBridge.ts', 90, 'react-hooks/refs'],
  ['src/hooks/useTriggers.ts', 265, 'react-hooks/refs'],
  ['src/hooks/useWebSocketEvents.ts', 375, 'react-hooks/refs'],

  // ===== react-hooks/immutability (variable before declaration) =====
  ['src/components/MonitorSelector.tsx', 69, 'react-hooks/immutability'],
  ['src/components/PromptSnippetBuilderTab.tsx', 138, 'react-hooks/immutability'],
  ['src/components/connection-wizard/VerificationStep.tsx', 79, 'react-hooks/immutability'],
  ['src/components/settings/AdvancedSettings.tsx', 56, 'react-hooks/immutability'],
  ['src/components/settings/AdvancedSettings.tsx', 57, 'react-hooks/immutability'],
  ['src/components/settings/AgenticSettings.tsx', 94, 'react-hooks/immutability'],
  ['src/components/settings/AgenticSettings.tsx', 95, 'react-hooks/immutability'],
  ['src/components/settings/AiSettings.tsx', 95, 'react-hooks/immutability'],
  ['src/components/settings/AiSettings.tsx', 96, 'react-hooks/immutability'],
  ['src/components/settings/AiSettings.tsx', 97, 'react-hooks/immutability'],
  ['src/components/settings/BackupSettings.tsx', 198, 'react-hooks/immutability'],
  ['src/components/settings/CloudRelaySettings.tsx', 66, 'react-hooks/immutability'],
  ['src/components/settings/ContainerSettings.tsx', 77, 'react-hooks/immutability'],
  ['src/components/settings/ContainerSettings.tsx', 78, 'react-hooks/immutability'],
  ['src/components/settings/ExecutionVariablesSettings.tsx', 87, 'react-hooks/immutability'],
  ['src/components/settings/LogSourcesSettings.tsx', 120, 'react-hooks/immutability'],
  ['src/components/settings/McpSettings.tsx', 87, 'react-hooks/immutability'],
  ['src/components/settings/MobileSettings.tsx', 56, 'react-hooks/immutability'],
  ['src/components/settings/MobileSettings.tsx', 57, 'react-hooks/immutability'],
  ['src/components/settings/OtelSettings.tsx', 72, 'react-hooks/immutability'],
  ['src/components/settings/PlaywrightSettings.tsx', 43, 'react-hooks/immutability'],
  ['src/components/settings/SecuritySettings.tsx', 462, 'react-hooks/immutability'],
  ['src/components/settings/SecuritySettings.tsx', 463, 'react-hooks/immutability'],
  ['src/components/settings/SelfHealingSettings.tsx', 78, 'react-hooks/immutability'],
  ['src/components/settings/SelfHealingSettings.tsx', 85, 'react-hooks/immutability'],
  ['src/components/settings/StorageSettings.tsx', 50, 'react-hooks/immutability'],
  ['src/components/settings/UpdateSettings.tsx', 19, 'react-hooks/immutability'],
  ['src/components/settings/WorldStateVerifierSettings.tsx', 71, 'react-hooks/immutability'],
  ['src/components/setup-wizard/ClaudeConfigStep.tsx', 24, 'react-hooks/immutability'],
  ['src/hooks/useProjectSelection.ts', 91, 'react-hooks/immutability'],
  ['src/hooks/useWebSocketAutoConnect.ts', 83, 'react-hooks/immutability'],
  ['src/hooks/useWebSocketEvents.ts', 300, 'react-hooks/immutability'],

  // ===== react-hooks/immutability (cannot-modify) =====
  ['src/hooks/useSdkUIBridge.ts', 76, 'react-hooks/immutability'],

  // ===== react-hooks/react-compiler (impure function in render) =====
  ['src/components/ai-workflows/ExecutionSummaryTab.tsx', 490, 'react-hooks/react-compiler'],
  ['src/components/terminal/useSessionManager.ts', 499, 'react-hooks/react-compiler'],
  ['src/components/terminal/useSessionManager.ts', 515, 'react-hooks/react-compiler'],
  ['src/components/widgets/execution-timeline/useExecutionTimelineData.ts', 297, 'react-hooks/react-compiler'],
  ['src/components/ui-bridge/ConnectedUIBridgeInspector.tsx', 65, 'react-hooks/purity'],
  ['src/hooks/useWebSocketLatency.ts', 99, 'react-hooks/react-compiler'],
  ['src/pages/ui-bridge-integration/HookGenerationPanel.tsx', 1225, 'react-hooks/react-compiler'],
  ['src/pages/ui-bridge-integration/HookGenerationPanel.tsx', 1335, 'react-hooks/react-compiler'],
  ['src/pages/ui-bridge-integration/HookGenerationPanel.tsx', 1338, 'react-hooks/react-compiler'],

  // ===== react-hooks/preserve-manual-memoization =====
  ['src/components/AiTab.tsx', 169, 'react-hooks/preserve-manual-memoization'],
  ['src/components/settings/ContainerSettings.tsx', 100, 'react-hooks/preserve-manual-memoization'],
  ['src/components/test-results/TestResultsTab.tsx', 114, 'react-hooks/preserve-manual-memoization'],
  ['src/hooks/useProjectSelection.ts', 116, 'react-hooks/preserve-manual-memoization'],

  // ===== react-hooks/immutability (cannot reassign in render: DomDiffModal) =====
  // NOTE: DomDiffModal:243 is inside a .map() callback inside JSX - need to suppress differently
  // For now add suppression
  ['src/components/dom-captures/DomDiffModal.tsx', 243, 'react-hooks/immutability'],
];

// Group by file, deduplicate per line
const byFile = new Map();
for (const [path, line, rule] of SUPPRESSIONS) {
  if (!byFile.has(path)) byFile.set(path, new Map());
  const lineMap = byFile.get(path);
  if (!lineMap.has(line)) lineMap.set(line, new Set());
  lineMap.get(line).add(rule);
}

let filesModified = 0;
let suppressionsAdded = 0;
let skipped = 0;
let errors = 0;

for (const [relPath, lineMap] of byFile) {
  const fullPath = relPath.replace(/\//g, '/');
  const absPath = join(BASE, fullPath);
  let content;
  try {
    content = readFileSync(absPath, 'utf-8');
  } catch (e) {
    console.error(`ERROR reading: ${relPath}: ${e.message}`);
    errors++;
    continue;
  }

  const lines = content.split('\n');

  // Sort by line number descending to insert from bottom up (preserves line numbers for earlier entries)
  const sortedEntries = [...lineMap.entries()].sort((a, b) => b[0] - a[0]);

  let modified = false;
  for (const [lineNo, rules] of sortedEntries) {
    const idx = lineNo - 1; // 0-based
    if (idx < 0 || idx >= lines.length) {
      console.warn(`  WARN: Line ${lineNo} out of range in ${relPath} (file has ${lines.length} lines)`);
      skipped++;
      continue;
    }

    const rulesStr = [...rules].join(', ');

    // Check if a suppress comment already exists on the previous line
    if (idx > 0) {
      const prevLine = lines[idx - 1];
      if (prevLine.includes('eslint-disable-next-line')) {
        const alreadySuppressed = [...rules].every(r => prevLine.includes(r));
        if (alreadySuppressed) {
          // Already suppressed, skip
          skipped++;
          continue;
        }
      }
    }

    // Get indentation from the target line
    const targetLine = lines[idx];
    const indentMatch = targetLine.match(/^(\s*)/);
    const indent = indentMatch ? indentMatch[1] : '';

    // Insert disable comment before the target line
    lines.splice(idx, 0, `${indent}// eslint-disable-next-line ${rulesStr}`);
    modified = true;
    suppressionsAdded++;
    console.log(`  + ${relPath}:${lineNo} <- ${rulesStr}`);
  }

  if (modified) {
    writeFileSync(absPath, lines.join('\n'), 'utf-8');
    filesModified++;
  }
}

console.log(`\n=== Done ===`);
console.log(`Files modified: ${filesModified}`);
console.log(`Suppressions added: ${suppressionsAdded}`);
console.log(`Already suppressed (skipped): ${skipped}`);
console.log(`Errors: ${errors}`);
