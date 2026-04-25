import { invoke } from "@tauri-apps/api/core";
import { useUIComponent } from "@qontinui/ui-bridge";
import {
  Zap,
  Image,
  ClipboardCheck,
  FileText,
  FileSearch,
  Bot,
  BarChart3,
  Database,
  TestTube,
  Activity,
} from "lucide-react";
import { useExecution } from "@/contexts";
import { instanceStorage } from "@/lib/instance-storage";
import type { ActionLogEntry } from "@/types/displayProfile";
import type { TaskRun } from "@/types/aiData";
import type { UseLogManagerResult } from "@/hooks/useLogManager";
import type { UseUIStateResult } from "@/hooks/useUIState";
import type { UseModalStateResult } from "@/hooks/useModalState";
import type { UseLogFilterResult } from "@/hooks/useLogFilter";
import type { UseProjectLogsReturn } from "@/hooks/project-logs";
import type { useProjectSelection } from "@/hooks/useProjectSelection";
import type { UseWebSocketAutoConnectReturn } from "@/hooks/useWebSocketAutoConnect";
import type { MainTabId, LogSubTab } from "./tab-types";
import type { ErrorMonitorScope } from "./useAppNavigation";
import { LogSourcesConfigTab } from "./LogSourcesConfigTab";

import { LogsTab } from "@/components/LogsTab";
import { CaptureTab } from "@/components/CaptureTab";
import { Settings } from "@/components/Settings";
import { AiTab } from "@/components/AiTab";
import { LibraryDashboard } from "@/components/LibraryDashboard";
import {
  ChecksPage,
  CheckGroupsPage,
  ShellCommandsPage,
  TasksPage,
  ContextsPage,
  PlaywrightTestsPage,
} from "@/components/library";
import { StepBuildersPage } from "@/components/StepBuildersPage";
import { HelpTab } from "@/components/HelpTab";
import { KnowledgeExplorerPage } from "@/components/knowledge-acquisition";
import { SchedulerTab } from "@/components/scheduler";
import { TriggersTab } from "@/components/triggers";
import { WorkflowBuilderTab } from "@/components/workflow-builder";
import { ActiveDashboardPage } from "@/components/active-dashboard";
import { HistoryTab } from "@/components/HistoryTab";
import { ExecuteTab } from "@/components/ExecuteTab";
import { WorkflowQueueTab } from "@/components/workflow-queue";
import { ExecutionReport } from "@/components/findings";
import { StateExplorerTab } from "@/components/state-explorer";
import { TestResultsTab } from "@/components/test-results";
import { StatisticsTab } from "@/components/statistics";
import { RunSelectionProvider } from "@/contexts/RunSelectionContext";
import { RunPageLayout } from "@/components/run-dashboard/RunPageLayout";
import { TraceViewerPage } from "@/components/run-dashboard/TraceViewerPage";
import { RunActionsTab } from "@/components/run-logs/RunActionsTab";
import { RunImageRecognitionTab } from "@/components/run-logs/RunImageRecognitionTab";
import { AiDataViewerTab } from "@/components/run-logs/AiDataViewerTab";
import { RunRecapTab } from "@/components/run-recap";
import { CategoryManager } from "@/components/findings/CategoryManager";
import { HooksManagerPanel } from "@/components/hooks";
import { ErrorMonitorTab } from "@/components/error-monitor";
import { ProcessManagerTab } from "@/components/process-manager";
import { ReflectionDashboard } from "@/components/reflection-dashboard/ReflectionDashboard";
import { ArchitectureView } from "@/components/architecture-view/ArchitectureView";
import { GeneratorEvalPage } from "@/pages/GeneratorEvalPage";
import { OrchestrationLoopPanel } from "@/components/orchestration-loop/OrchestrationLoopPanel";
import { MetaOptimizerPage } from "@/pages/MetaOptimizerPage";
import { OnlineLearningDashboard } from "@/components/online-learning/OnlineLearningDashboard";
import { SpecsPage } from "@/pages/specs/SpecsPage";
import { UIBridgeIntegrationPage } from "@/pages/ui-bridge-integration/UIBridgeIntegrationPage";
import { WrappersLibraryPage } from "@/pages/wrappers/WrappersLibraryPage";
import { UIBridgeStateMachinePage } from "@/pages/state-machine";
import { ImageQualityTestsPage } from "@/pages/ImageQualityTestsPage";
import { EventHistoryPage } from "@/pages/EventHistoryPage";
import { lazy, Suspense } from "react";
import { Loader2 } from "lucide-react";

const LlmObservabilityDashboard = lazy(
  () => import("../llm-observability/LlmObservabilityDashboard"),
);
const CostControlPanel = lazy(() => import("../cost-control/CostControlPanel"));
const EvaluationDashboard = lazy(() => import("../evaluation/EvaluationDashboard"));
const SkillApprovalPanel = lazy(() =>
  import("../skills/SkillApprovalPanel").then((m) => ({ default: m.SkillApprovalPanel })),
);
const AutomationHealthDashboard = lazy(() =>
  import("../ui-bridge/AutomationHealthDashboard").then((m) => ({
    default: m.AutomationHealthDashboard,
  })),
);
const ObservationBrowser = lazy(() =>
  import("../observations/ObservationBrowser").then((m) => ({ default: m.ObservationBrowser })),
);
const MemoryHealthPanel = lazy(() =>
  import("../observations/MemoryHealthPanel").then((m) => ({ default: m.MemoryHealthPanel })),
);
const ActivityTimelinePanel = lazy(() =>
  import("../activity-timeline/ActivityTimelinePanel").then((m) => ({
    default: m.ActivityTimelinePanel,
  })),
);
const WatcherManagementPanel = lazy(() =>
  import("../activity-timeline/WatcherManagementPanel").then((m) => ({
    default: m.WatcherManagementPanel,
  })),
);
const DemoVideoPanel = lazy(() =>
  import("../demo-video/DemoVideoPanel").then((m) => ({ default: m.DemoVideoPanel })),
);
const DevelopmentIntelligencePage = lazy(() =>
  import("../development-intelligence/DevelopmentIntelligencePage").then((m) => ({
    default: m.DevelopmentIntelligencePage,
  })),
);
const TourCatalog = lazy(() =>
  import("../product-tour/TourCatalog").then((m) => ({ default: m.TourCatalog })),
);
const SessionRecapPage = lazy(() =>
  import("../session-recap/SessionRecapPage").then((m) => ({ default: m.SessionRecapPage })),
);
const ApiSurfacePage = lazy(() =>
  import("../api-surface/ApiSurfacePage").then((m) => ({ default: m.ApiSurfacePage })),
);
const DecisionTrailPage = lazy(() =>
  import("../decision-trail/DecisionTrailPage").then((m) => ({ default: m.DecisionTrailPage })),
);
const MemorySearchPanel = lazy(() =>
  import("../memory-search").then((m) => ({ default: m.MemorySearchPanel })),
);
const AccessibilityExplorer = lazy(
  () => import("@/components/accessibility-explorer/AccessibilityExplorer"),
);
const PromptHomePage = lazy(() =>
  import("../prompt-home/PromptHomePage").then((m) => ({ default: m.PromptHomePage })),
);
const DagWorkflowEditor = lazy(() =>
  import("../dag-workflow-editor").then((m) => ({ default: m.DagWorkflowEditor })),
);
const ProjectExplainerPage = lazy(() =>
  import("../../pages/project-explainer/ProjectExplainerPage").then((m) => ({
    default: m.ProjectExplainerPage,
  })),
);

/** Register the active page with UI Bridge for AI discoverability */
function PageRegistration({
  id,
  name,
  description,
}: {
  id: string;
  name: string;
  description: string;
}) {
  useUIComponent({ id: `page-${id}`, name, description, actions: [] });
  return null;
}

/** Suspense fallback for lazy-loaded tab panels */
function LazyFallback() {
  return (
    <div className="flex items-center justify-center h-full gap-2 text-muted-foreground">
      <Loader2 className="w-5 h-5 animate-spin" />
      <span className="text-sm">Loading...</span>
    </div>
  );
}

interface ActionLogViewData {
  actions: ActionLogEntry[];
  visible_count: number;
}

interface GlobalLogSourceSettings {
  sources: Array<{
    id: string;
    name: string;
    path: string;
    enabled: boolean;
    color?: string;
    category: string;
    description?: string;
    tail_lines: number;
  }>;
}

export interface TabContentProps {
  activeTab: MainTabId;
  setActiveTab: (tab: MainTabId) => void;
  addLog: UseLogManagerResult["addLog"];
  addAiOutputLog: UseLogManagerResult["addAiOutputLog"];
  logs: UseLogManagerResult["logs"];
  imageLogs: UseLogManagerResult["imageLogs"];
  aiOutputLogs: UseLogManagerResult["aiOutputLogs"];
  clearGeneralLogs: UseLogManagerResult["clearGeneralLogs"];
  clearImageLogs: UseLogManagerResult["clearImageLogs"];
  clearAiOutputLogs: UseLogManagerResult["clearAiOutputLogs"];
  logCount: number;
  imageLogCount: number;
  filteredLogs: UseLogFilterResult["filteredLogs"];
  logLevel: UseLogFilterResult["logLevel"];
  setLogLevel: UseLogFilterResult["setLogLevel"];
  uiState: UseUIStateResult;
  modalState: UseModalStateResult;
  actionLogViewData: ActionLogViewData | null;
  actionLogLoading: boolean;
  actionLogError: string | null;
  refreshActionLog: () => void;
  activeLogSubTab: LogSubTab;
  setActiveLogSubTab: (tab: LogSubTab) => void;
  editWorkflowId: string | null;
  setEditWorkflowId: (id: string | null) => void;
  globalLogSourceSettings: GlobalLogSourceSettings | null;
  projectSelection: ReturnType<typeof useProjectSelection>;
  projectLogs: UseProjectLogsReturn;
  webSocket: UseWebSocketAutoConnectReturn;
  lastRun: TaskRun | null;
  lastRunWorkflowId: string | null;
  lastRunWorkflowName: string | null;
  isRunningLastWorkflow: boolean;
  handleRunLastWorkflow: () => Promise<void>;
  handleGoToRecap: () => void;
  handleCopyLogs: () => Promise<void>;
  clearActionLogs: () => Promise<void>;
  clearAllLogs: () => Promise<void>;
  errorMonitorScope?: ErrorMonitorScope;
  clearErrorMonitorScope?: () => void;
}

export function TabContent({
  activeTab,
  setActiveTab,
  addLog,
  addAiOutputLog,
  logs,
  imageLogs,
  aiOutputLogs,
  clearGeneralLogs,
  clearImageLogs,
  clearAiOutputLogs,
  logCount,
  imageLogCount,
  filteredLogs,
  logLevel,
  setLogLevel,
  uiState,
  modalState,
  actionLogViewData,
  actionLogLoading,
  actionLogError,
  activeLogSubTab,
  setActiveLogSubTab,
  editWorkflowId,
  setEditWorkflowId,
  globalLogSourceSettings,
  projectSelection,
  projectLogs: _projectLogs,
  webSocket,
  lastRun,
  lastRunWorkflowId,
  lastRunWorkflowName,
  isRunningLastWorkflow,
  handleRunLastWorkflow,
  handleGoToRecap,
  handleCopyLogs,
  clearActionLogs,
  clearAllLogs,
  errorMonitorScope,
  clearErrorMonitorScope,
}: TabContentProps) {
  const execution = useExecution();

  switch (activeTab) {
    case "prompt-home":
      return (
        <div data-page-id="prompt-home" className="h-full overflow-hidden">
          <PageRegistration
            id="prompt-home"
            name="Home"
            description="Tell the runner what to do in plain English"
          />
          <Suspense fallback={<LazyFallback />}>
            <PromptHomePage />
          </Suspense>
        </div>
      );

    case "gui-automation":
      return (
        <div data-page-id="gui-automation" className="h-full flex flex-col">
          <PageRegistration
            id="gui-automation"
            name="Workflows"
            description="Configure and launch GUI automation workflows"
          />
          <ExecuteTab onLog={addLog} onNavigateToActive={() => setActiveTab("active")} />
        </div>
      );

    case "workflow-queue":
      return (
        <div data-page-id="workflow-queue" className="h-full flex flex-col">
          <PageRegistration
            id="workflow-queue"
            name="Workflow Queue"
            description="Queue and manage multiple workflow executions"
          />
          <WorkflowQueueTab onNavigateToActive={() => setActiveTab("active")} onLog={addLog} />
        </div>
      );

    case "active":
      return (
        <div data-page-id="active" className="h-full overflow-hidden">
          <ActiveDashboardPage
            onGoToExecute={() => setActiveTab("gui-automation")}
            onGoToRecap={lastRun ? handleGoToRecap : undefined}
            onRunLastWorkflow={lastRunWorkflowId ? handleRunLastWorkflow : undefined}
            isRunningLastWorkflow={isRunningLastWorkflow}
            lastRunWorkflowName={lastRunWorkflowName}
            lastRunWorkflowId={lastRunWorkflowId}
          />
        </div>
      );

    case "runs":
    case "history":
      return (
        <div data-page-id="runs" className="h-full overflow-hidden">
          <HistoryTab
            onNavigateToRun={() => setActiveTab("gui-automation")}
            onNavigateToAi={(runId) => {
              instanceStorage.setJSON("qontinui-selected-task-run-id", runId);
              setActiveTab("run-recap");
            }}
          />
        </div>
      );

    case "error-monitor":
      return (
        <div data-page-id="error-monitor" className="h-full overflow-hidden">
          <PageRegistration
            id="error-monitor"
            name="Error Monitor"
            description="Real-time application error monitoring and log analysis"
          />
          <ErrorMonitorTab
            taskRunId={errorMonitorScope?.taskRunId}
            taskRunName={errorMonitorScope?.taskRunName}
            onClearScope={clearErrorMonitorScope}
          />
        </div>
      );

    case "processes":
      return (
        <div data-page-id="processes" className="h-full overflow-hidden">
          <PageRegistration
            id="processes"
            name="Process Manager"
            description="Manage and monitor development processes (web backend, frontend, mobile)"
          />
          <ProcessManagerTab />
        </div>
      );

    case "reflection":
      return (
        <div data-page-id="reflection" className="h-full overflow-hidden">
          <PageRegistration
            id="reflection"
            name="Reflection"
            description="Review reflection analysis from automation runs"
          />
          <ReflectionDashboard />
        </div>
      );

    case "observations":
      return (
        <div data-page-id="observations" className="h-full overflow-auto">
          <PageRegistration
            id="observations"
            name="Memory"
            description="Cross-session observation memory from past automation runs"
          />
          <div className="border-b p-4">
            <Suspense fallback={<LazyFallback />}>
              <MemoryHealthPanel />
            </Suspense>
          </div>
          <div className="h-full overflow-hidden">
            <Suspense fallback={<LazyFallback />}>
              <ObservationBrowser projectId={projectSelection.selectedProjectId} />
            </Suspense>
          </div>
        </div>
      );

    case "activity-timeline":
      return (
        <div data-page-id="activity-timeline" className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <ActivityTimelinePanel />
          </Suspense>
        </div>
      );

    case "watchers":
      return (
        <div data-page-id="watchers" className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <WatcherManagementPanel />
          </Suspense>
        </div>
      );

    case "architecture":
      return (
        <div data-page-id="architecture" className="h-full overflow-hidden">
          <ArchitectureView />
        </div>
      );

    case "generator-eval":
      return (
        <div data-page-id="generator-eval" className="h-full overflow-hidden">
          <GeneratorEvalPage />
        </div>
      );

    case "meta-optimizer":
      return (
        <div data-page-id="meta-optimizer" className="h-full overflow-hidden">
          <MetaOptimizerPage />
        </div>
      );

    case "online-learning":
      return (
        <div data-page-id="online-learning" className="h-full overflow-hidden">
          <OnlineLearningDashboard />
        </div>
      );

    case "skills":
      return (
        <div data-page-id="skills" className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <SkillApprovalPanel />
          </Suspense>
        </div>
      );

    case "orchestration-loop":
      return (
        <div data-page-id="orchestration-loop" className="h-full overflow-hidden">
          <OrchestrationLoopPanel />
        </div>
      );

    case "image-quality-tests":
      return (
        <div data-page-id="image-quality-tests" className="h-full overflow-hidden">
          <ImageQualityTestsPage />
        </div>
      );

    case "run-recap":
      return (
        <div data-page-id="run-recap" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Recap"
              icon={ClipboardCheck}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <RunRecapTab
                  onNavigateToAiOutput={(phase, iteration) => {
                    instanceStorage.setJSON("qontinui-ai-output-navigate", {
                      phase,
                      phaseIteration: iteration,
                    });
                    setActiveTab("run-ai-output");
                  }}
                />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-actions":
      return (
        <div data-page-id="run-actions" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Actions"
              icon={Zap}
              badgeCount={actionLogViewData?.visible_count || 0}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full p-4 overflow-hidden">
                <div className="h-full card overflow-hidden">
                  <RunActionsTab
                    actionLogData={actionLogViewData}
                    actionLogLoading={actionLogLoading}
                    actionLogError={actionLogError}
                    onActionRowClick={modalState.openActionModal}
                    actionCount={actionLogViewData?.visible_count || 0}
                  />
                </div>
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-image":
      return (
        <div data-page-id="run-image" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Image Recognition"
              icon={Image}
              badgeCount={imageLogCount}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full p-4 overflow-hidden">
                <div className="h-full card overflow-hidden">
                  <RunImageRecognitionTab
                    imageLogs={imageLogs}
                    onImageRowClick={modalState.openImageModal}
                    imageLogCount={imageLogCount}
                  />
                </div>
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-findings":
      return (
        <div data-page-id="run-findings" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Findings"
              icon={FileText}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <ExecutionReport />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-state-explorer":
      return (
        <div data-page-id="run-state-explorer" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="State Explorer"
              icon={FileSearch}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <StateExplorerTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-tests":
      return (
        <div data-page-id="run-tests" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Test Results"
              icon={TestTube}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <TestResultsTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-ai-output":
      return (
        <div data-page-id="run-ai-output" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="AI Output"
              icon={Bot}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <AiTab
                aiOutputLines={aiOutputLogs}
                onClearAiOutput={clearAiOutputLogs}
                onAddAiOutputLine={(line) =>
                  addAiOutputLog(
                    line.line,
                    line.source,
                    line.actionId,
                    line.taskRunId,
                    line.sessionId,
                    line.sessionName,
                    line.phase,
                    line.phaseIteration,
                  )
                }
                onNavigateToLibrary={() => setActiveTab("library")}
              />
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-statistics":
      return (
        <div data-page-id="run-statistics" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Statistics"
              icon={BarChart3}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <StatisticsTab
                  configId={execution.config?.path ?? null}
                  configName={execution.config?.name}
                />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-ai-data":
      return (
        <div data-page-id="run-ai-data" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="AI Data Viewer"
              icon={Database}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <AiDataViewerTab />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "run-traces":
      return (
        <div data-page-id="run-traces" className="h-full overflow-hidden">
          <RunSelectionProvider>
            <RunPageLayout
              title="Traces"
              icon={Activity}
              onNavigateToActive={() => setActiveTab("active")}
            >
              <div className="h-full overflow-hidden">
                <TraceViewerPage />
              </div>
            </RunPageLayout>
          </RunSelectionProvider>
        </div>
      );

    case "ai":
      return (
        <div data-page-id="ai" className="h-full overflow-hidden">
          <AiTab
            aiOutputLines={aiOutputLogs}
            onClearAiOutput={clearAiOutputLogs}
            onAddAiOutputLine={(line) =>
              addAiOutputLog(
                line.line,
                line.source,
                line.actionId,
                line.taskRunId,
                line.sessionId,
                line.sessionName,
                line.phase,
                line.phaseIteration,
              )
            }
            onNavigateToLibrary={() => setActiveTab("library")}
          />
        </div>
      );

    case "logs":
      return (
        <div
          data-page-id="logs"
          className="flex-1 flex flex-col min-h-0 p-4 h-full overflow-hidden"
        >
          <div className="flex-1 flex flex-col min-h-0 card overflow-hidden">
            <LogsTab
              logs={logs}
              filteredLogs={filteredLogs}
              logLevel={logLevel}
              onLogLevelChange={setLogLevel}
              showLogFilter={uiState.showLogFilter}
              onToggleLogFilter={uiState.setShowLogFilter}
              imageLogs={imageLogs}
              onImageRowClick={modalState.openImageModal}
              actionLogData={actionLogViewData}
              actionLogLoading={actionLogLoading}
              actionLogError={actionLogError}
              onActionRowClick={modalState.openActionModal}
              logCount={logCount}
              imageLogCount={imageLogCount}
              actionCount={actionLogViewData?.visible_count || 0}
              onClearGeneralLogs={clearGeneralLogs}
              onClearImageLogs={clearImageLogs}
              onClearActionLogs={clearActionLogs}
              onClearAllLogs={clearAllLogs}
              onCopyLogs={handleCopyLogs}
              copySuccess={uiState.copySuccess}
              activeSubTab={activeLogSubTab}
              onSubTabChange={setActiveLogSubTab}
            />
          </div>
        </div>
      );

    case "library":
      return (
        <div data-page-id="library" className="h-full flex flex-col">
          <PageRegistration
            id="library"
            name="Library"
            description="Prompt library, macros, checks, shell commands, and reusable components"
          />
          <LibraryDashboard onLog={addLog} />
        </div>
      );

    case "specs":
      return (
        <div data-page-id="specs" className="h-full overflow-hidden">
          <SpecsPage
            onNavigateToWorkflowBuilder={(id) => {
              setEditWorkflowId(id);
              setActiveTab("unified-workflow-builder");
            }}
          />
        </div>
      );

    case "state-machine":
      return (
        <div data-page-id="state-machine" className="h-full overflow-hidden">
          <UIBridgeStateMachinePage />
        </div>
      );

    case "step-builders":
      return (
        <div data-page-id="step-builders" className="h-full overflow-hidden">
          <StepBuildersPage onNavigate={(id) => setActiveTab(id as MainTabId)} />
        </div>
      );

    case "check-builder":
      return (
        <div data-page-id="check-builder" className="h-full overflow-hidden">
          <ChecksPage />
        </div>
      );

    case "check-group-builder":
      return (
        <div data-page-id="check-group-builder" className="h-full overflow-hidden">
          <CheckGroupsPage />
        </div>
      );

    case "shell-command-builder":
      return (
        <div data-page-id="shell-command-builder" className="h-full overflow-hidden">
          <ShellCommandsPage />
        </div>
      );

    case "task-builder":
      return (
        <div data-page-id="task-builder" className="h-full overflow-hidden">
          <TasksPage />
        </div>
      );

    case "context-builder":
      return (
        <div data-page-id="context-builder" className="h-full overflow-hidden">
          <ContextsPage />
        </div>
      );

    case "playwright-test-builder":
      return (
        <div data-page-id="playwright-test-builder" className="h-full overflow-hidden">
          <PlaywrightTestsPage />
        </div>
      );

    case "unified-workflow-builder":
      return (
        <div data-page-id="unified-workflow-builder" className="h-full overflow-hidden">
          <PageRegistration
            id="workflow-builder"
            name="Workflow Builder"
            description="Build and edit multi-step automation workflows with AI assistance"
          />
          <WorkflowBuilderTab
            editWorkflowId={editWorkflowId}
            onNavigateToActive={() => setActiveTab("active")}
          />
        </div>
      );

    case "dag-workflow-editor":
      return (
        <div data-page-id="dag-workflow-editor" className="h-full overflow-hidden">
          <PageRegistration
            id="dag-workflow-editor"
            name="DAG Workflow Editor"
            description="Visual DAG workflow editor with YAML syntax and graph visualization"
          />
          <Suspense fallback={<LazyFallback />}>
            <DagWorkflowEditor />
          </Suspense>
        </div>
      );

    case "monitor-findings":
      return (
        <div data-page-id="monitor-findings" className="h-full overflow-hidden">
          <ExecutionReport />
        </div>
      );

    case "monitor-state-explorer":
      return (
        <div data-page-id="monitor-state-explorer" className="h-full overflow-hidden">
          <StateExplorerTab />
        </div>
      );

    case "monitor-statistics":
      return (
        <div data-page-id="monitor-statistics" className="h-full overflow-hidden">
          <StatisticsTab
            configId={execution.config?.path ?? null}
            configName={execution.config?.name}
          />
        </div>
      );

    case "config-log-sources": {
      const sources = globalLogSourceSettings?.sources || [];
      return (
        <div data-page-id="config-log-sources" className="h-full overflow-hidden">
          <LogSourcesConfigTab sources={sources} onNavigate={setActiveTab} />
        </div>
      );
    }

    case "config-findings":
      return (
        <div data-page-id="config-findings" className="h-full overflow-y-auto">
          <CategoryManager onLog={addLog} />
        </div>
      );

    case "config-hooks":
      return (
        <div data-page-id="config-hooks" className="h-full overflow-hidden">
          <HooksManagerPanel />
        </div>
      );

    case "config-ui-bridge":
      return (
        <div data-page-id="config-ui-bridge" className="h-full overflow-hidden">
          <UIBridgeIntegrationPage />
        </div>
      );

    case "capture":
      return (
        <div data-page-id="capture" className="h-full overflow-y-auto">
          <CaptureTab onLog={addLog} />
        </div>
      );

    case "triggers":
      return (
        <div data-page-id="triggers" className="h-full overflow-hidden">
          <TriggersTab />
        </div>
      );

    case "tasks":
      return (
        <div data-page-id="tasks" className="h-full overflow-hidden">
          <SchedulerTab />
        </div>
      );

    case "settings":
    case "settings-account":
    case "settings-ai":
    case "settings-agentic":
    case "settings-self-healing":
    case "settings-world-state-verifier":
    case "settings-playwright":
    case "settings-mobile":
    case "settings-cloud-relay":
    case "settings-discovery":
    case "settings-web-integration":
    case "settings-mcp":
    case "settings-log-sources":
    case "settings-execution-variables":
    case "settings-general":
    case "settings-storage":
    case "settings-backup":
    case "settings-instances":
    case "settings-debug":
    case "settings-security":
    case "settings-updates": {
      const settingsTabMap: Record<string, string> = {
        settings: "account",
        "settings-account": "account",
        "settings-ai": "ai",
        "settings-agentic": "agentic",
        "settings-self-healing": "self-healing",
        "settings-world-state-verifier": "world-state-verifier",
        "settings-playwright": "playwright",
        "settings-mobile": "mobile",
        "settings-cloud-relay": "cloud-relay",
        "settings-discovery": "discovery",
        "settings-web-integration": "web-integration",
        "settings-mcp": "mcp",
        "settings-log-sources": "log-sources",
        "settings-execution-variables": "execution-variables",
        "settings-general": "general",
        "settings-storage": "storage",
        "settings-backup": "backup",
        "settings-instances": "instances",
        "settings-debug": "advanced",
        "settings-security": "security",
        "settings-updates": "updates",
      };
      const defaultSettingsTab = settingsTabMap[activeTab] || "account";

      return (
        <div data-page-id="settings" className="h-full overflow-hidden">
          <Settings
            defaultTab={defaultSettingsTab}
            onLog={addLog}
            onDebugModeChange={async (enabled) => {
              try {
                await invoke("set_debug_settings", {
                  settings: {
                    enable_image_debug: enabled,
                    top_matches_count: 5,
                  },
                });
                addLog("info", `Image debug mode ${enabled ? "enabled" : "disabled"}`);
              } catch (error) {
                addLog("error", `Failed to set debug mode: ${error}`);
              }
            }}
            projects={projectSelection.projects}
            selectedProjectId={projectSelection.selectedProjectId}
            onProjectSelect={projectSelection.setSelectedProject}
            onLoadProjects={projectSelection.loadProjects}
            webSocketState={webSocket}
          />
        </div>
      );
    }

    case "llm-analytics":
      return (
        <div data-page-id="llm-analytics" className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <LlmObservabilityDashboard />
          </Suspense>
        </div>
      );

    case "cost-control":
      return (
        <div data-page-id="cost-control" className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <CostControlPanel />
          </Suspense>
        </div>
      );

    case "accessibility-explorer":
      return (
        <div data-page-id="accessibility-explorer" className="h-full overflow-hidden">
          <PageRegistration
            id="accessibility-explorer"
            name="Accessibility Explorer"
            description="Inspect and interact with native desktop accessibility trees via UIA, AT-SPI, or AX APIs"
          />
          <Suspense fallback={<LazyFallback />}>
            <AccessibilityExplorer />
          </Suspense>
        </div>
      );

    case "evaluation":
      return (
        <div data-page-id="evaluation" className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <EvaluationDashboard />
          </Suspense>
        </div>
      );

    case "terminal":
      return null;

    case "automation-health":
      return (
        <div data-page-id="automation-health" className="h-full overflow-auto p-4">
          <Suspense fallback={<LazyFallback />}>
            <AutomationHealthDashboard />
          </Suspense>
        </div>
      );

    case "knowledge-explorer":
      return (
        <div data-page-id="knowledge-explorer" className="h-full overflow-hidden">
          <KnowledgeExplorerPage />
        </div>
      );

    case "decision-trail":
      return (
        <div data-page-id="decision-trail" className="h-full overflow-hidden">
          <PageRegistration
            id="decision-trail"
            name="Decision Trail"
            description="Architectural decision history and concept summaries"
          />
          <Suspense fallback={<LazyFallback />}>
            <DecisionTrailPage />
          </Suspense>
        </div>
      );

    case "project-explainer":
      return (
        <div data-page-id="project-explainer" className="h-full overflow-hidden">
          <PageRegistration
            id="project-explainer"
            name="Project Explainer"
            description="Navigable project documentation generated from specs + architecture diagrams, with an AI chat side panel"
          />
          <Suspense fallback={<LazyFallback />}>
            <ProjectExplainerPage />
          </Suspense>
        </div>
      );

    case "event-history":
      return (
        <div data-page-id="event-history" className="h-full overflow-hidden">
          <EventHistoryPage />
        </div>
      );

    case "development-intelligence":
      return (
        <div data-page-id="development-intelligence" className="h-full overflow-hidden">
          <PageRegistration
            id="development-intelligence"
            name="Dev Intelligence"
            description="Coverage gap analysis, complexity scoring, and dead feature detection"
          />
          <Suspense fallback={<LazyFallback />}>
            <DevelopmentIntelligencePage />
          </Suspense>
        </div>
      );

    case "demo-video":
      return (
        <div data-page-id="demo-video" className="h-full overflow-hidden">
          <PageRegistration
            id="demo-video"
            name="Demo Videos"
            description="Generate demo videos from UI Bridge page specs"
          />
          <Suspense fallback={<LazyFallback />}>
            <DemoVideoPanel />
          </Suspense>
        </div>
      );

    case "product-tours":
      return (
        <div data-page-id="product-tours" className="h-full overflow-hidden">
          <PageRegistration
            id="product-tours"
            name="Product Tours"
            description="Generate and manage interactive product tours"
          />
          <Suspense fallback={<LazyFallback />}>
            <TourCatalog />
          </Suspense>
        </div>
      );

    case "session-recap":
      return (
        <div data-page-id="session-recap" className="h-full overflow-hidden">
          <PageRegistration
            id="session-recap"
            name="Session Recap"
            description="Semantic timeline of what was built during a development session — files, types, endpoints, dependencies"
          />
          <Suspense fallback={<LazyFallback />}>
            <SessionRecapPage />
          </Suspense>
        </div>
      );

    case "api-surface":
      return (
        <div data-page-id="api-surface" className="h-full overflow-hidden">
          <PageRegistration
            id="api-surface"
            name="API Surface Map"
            description="Interactive map of every endpoint, command, query, and their connections — shows orphaned endpoints"
          />
          <Suspense fallback={<LazyFallback />}>
            <ApiSurfacePage />
          </Suspense>
        </div>
      );

    case "memory-search":
      return (
        <div data-page-id="memory-search" className="h-full overflow-hidden">
          <PageRegistration
            id="memory-search"
            name="Memory Search"
            description="Unified memory retrieval with RRF fusion"
          />
          <Suspense fallback={<LazyFallback />}>
            <MemorySearchPanel />
          </Suspense>
        </div>
      );

    case "help":
      return (
        <div data-page-id="help" className="h-full flex flex-col">
          <PageRegistration
            id="help"
            name="Help"
            description="Tutorials, documentation, and getting started guides"
          />
          <HelpTab />
        </div>
      );

    case "wrappers":
      return (
        <div data-page-id="wrappers" className="h-full overflow-hidden">
          <PageRegistration
            id="wrappers"
            name="Wrappers"
            description="Install and manage wrapper extensions — typed actions for the runner"
          />
          <WrappersLibraryPage />
        </div>
      );

    default:
      return null;
  }
}
