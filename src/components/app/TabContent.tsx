import { invoke } from "@tauri-apps/api/core";
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
import { AutoresearchPage } from "@/pages/AutoresearchPage";
import { MetaOptimizerPage } from "@/pages/MetaOptimizerPage";
import { SpecsPage } from "@/pages/specs/SpecsPage";
import { UIBridgeIntegrationPage } from "@/pages/ui-bridge-integration/UIBridgeIntegrationPage";
import { UIBridgeStateMachinePage } from "@/pages/state-machine";
import { ImageQualityTestsPage } from "@/pages/ImageQualityTestsPage";
import { lazy, Suspense } from "react";
import { Loader2 } from "lucide-react";

const LlmObservabilityDashboard = lazy(() => import("../llm-observability/LlmObservabilityDashboard"));
const EvaluationDashboard = lazy(() => import("../evaluation/EvaluationDashboard"));
const ElementReliabilityDashboard = lazy(() => import("../element-reliability/ElementReliabilityDashboard"));
const PipelineEventsTimeline = lazy(() => import("../pipeline-events/PipelineEventsTimeline").then(m => ({ default: m.PipelineEventsTimeline })));
const RuleInfluencePanel = lazy(() => import("../rule-influence/RuleInfluencePanel").then(m => ({ default: m.RuleInfluencePanel })));
const UnifiedSearchPanel = lazy(() => import("../unified-search/UnifiedSearchPanel"));
const WorkflowVersionLineage = lazy(() => import("../workflow-versions/WorkflowVersionLineage").then(m => ({ default: m.WorkflowVersionLineage })));
const SkillApprovalPanel = lazy(() => import("../skills/SkillApprovalPanel").then(m => ({ default: m.SkillApprovalPanel })));
const AutomationHealthDashboard = lazy(() => import("../ui-bridge/AutomationHealthDashboard").then(m => ({ default: m.AutomationHealthDashboard })));
const ObservationBrowser = lazy(() => import("../observations/ObservationBrowser").then(m => ({ default: m.ObservationBrowser })));
const ActivityTimelinePanel = lazy(() => import("../activity-timeline/ActivityTimelinePanel").then(m => ({ default: m.ActivityTimelinePanel })));
const WatcherManagementPanel = lazy(() => import("../activity-timeline/WatcherManagementPanel").then(m => ({ default: m.WatcherManagementPanel })));

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
}: TabContentProps) {
  const execution = useExecution();

  switch (activeTab) {
    case "gui-automation":
      return <ExecuteTab onLog={addLog} onNavigateToActive={() => setActiveTab("active")} />;

    case "workflow-queue":
      return <WorkflowQueueTab onNavigateToActive={() => setActiveTab("active")} onLog={addLog} />;

    case "active":
      return (
        <ActiveDashboardPage
          onGoToExecute={() => setActiveTab("gui-automation")}
          onGoToRecap={lastRun ? handleGoToRecap : undefined}
          onRunLastWorkflow={lastRunWorkflowId ? handleRunLastWorkflow : undefined}
          isRunningLastWorkflow={isRunningLastWorkflow}
          lastRunWorkflowName={lastRunWorkflowName}
          lastRunWorkflowId={lastRunWorkflowId}
        />
      );

    case "runs":
    case "history":
      return (
        <HistoryTab
          onNavigateToRun={() => setActiveTab("gui-automation")}
          onNavigateToAi={(runId) => {
            instanceStorage.setJSON("qontinui-selected-task-run-id", runId);
            setActiveTab("run-recap");
          }}
        />
      );

    case "error-monitor":
      return (
        <div className="h-full overflow-hidden">
          <ErrorMonitorTab />
        </div>
      );

    case "processes":
      return (
        <div className="h-full overflow-hidden">
          <ProcessManagerTab />
        </div>
      );

    case "reflection":
      return (
        <div className="h-full overflow-hidden">
          <ReflectionDashboard />
        </div>
      );

    case "observations":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <ObservationBrowser projectId={projectSelection.selectedProjectId} />
          </Suspense>
        </div>
      );

    case "activity-timeline":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <ActivityTimelinePanel />
          </Suspense>
        </div>
      );

    case "watchers":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <WatcherManagementPanel />
          </Suspense>
        </div>
      );

    case "architecture":
      return (
        <div className="h-full overflow-hidden">
          <ArchitectureView />
        </div>
      );

    case "generator-eval":
      return (
        <div className="h-full overflow-hidden">
          <GeneratorEvalPage />
        </div>
      );

    case "autoresearch":
      return (
        <div className="h-full overflow-hidden">
          <AutoresearchPage />
        </div>
      );

    case "meta-optimizer":
      return (
        <div className="h-full overflow-hidden">
          <MetaOptimizerPage />
        </div>
      );

    case "skills":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <SkillApprovalPanel />
          </Suspense>
        </div>
      );

    case "orchestration-loop":
      return (
        <div className="h-full overflow-hidden">
          <OrchestrationLoopPanel />
        </div>
      );

    case "image-quality-tests":
      return (
        <div className="h-full overflow-hidden">
          <ImageQualityTestsPage />
        </div>
      );

    case "run-recap":
      return (
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
      );

    case "run-actions":
      return (
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
      );

    case "run-image":
      return (
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
      );

    case "run-findings":
      return (
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
      );

    case "run-state-explorer":
      return (
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
      );

    case "run-tests":
      return (
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
      );

    case "run-ai-output":
      return (
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
      );

    case "run-statistics":
      return (
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
      );

    case "run-ai-data":
      return (
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
      );

    case "run-traces":
      return (
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
      );

    case "ai":
      return (
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
      );

    case "logs":
      return (
        <div className="flex-1 flex flex-col min-h-0 p-4 h-full overflow-hidden">
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
      return <LibraryDashboard onLog={addLog} />;

    case "specs":
      return (
        <div className="h-full overflow-hidden">
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
        <div className="h-full overflow-hidden">
          <UIBridgeStateMachinePage />
        </div>
      );

    case "step-builders":
      return <StepBuildersPage onNavigate={(id) => setActiveTab(id as MainTabId)} />;

    case "check-builder":
      return <ChecksPage />;

    case "check-group-builder":
      return <CheckGroupsPage />;

    case "shell-command-builder":
      return <ShellCommandsPage />;

    case "task-builder":
      return <TasksPage />;

    case "context-builder":
      return <ContextsPage />;

    case "playwright-test-builder":
      return <PlaywrightTestsPage />;

    case "unified-workflow-builder":
      return (
        <div className="h-full overflow-hidden">
          <WorkflowBuilderTab
            editWorkflowId={editWorkflowId}
            onNavigateToActive={() => setActiveTab("active")}
          />
        </div>
      );

    case "monitor-findings":
      return (
        <div className="h-full overflow-hidden">
          <ExecutionReport />
        </div>
      );

    case "monitor-state-explorer":
      return (
        <div className="h-full overflow-hidden">
          <StateExplorerTab />
        </div>
      );

    case "monitor-statistics":
      return (
        <div className="h-full overflow-hidden">
          <StatisticsTab
            configId={execution.config?.path ?? null}
            configName={execution.config?.name}
          />
        </div>
      );

    case "config-log-sources": {
      const sources = globalLogSourceSettings?.sources || [];
      return <LogSourcesConfigTab sources={sources} onNavigate={setActiveTab} />;
    }

    case "config-findings":
      return (
        <div className="h-full overflow-y-auto">
          <CategoryManager onLog={addLog} />
        </div>
      );

    case "config-hooks":
      return (
        <div className="h-full overflow-hidden">
          <HooksManagerPanel />
        </div>
      );

    case "config-ui-bridge":
      return (
        <div className="h-full overflow-hidden">
          <UIBridgeIntegrationPage />
        </div>
      );

    case "capture":
      return (
        <div className="h-full overflow-y-auto">
          <CaptureTab onLog={addLog} />
        </div>
      );

    case "triggers":
      return <TriggersTab />;

    case "tasks":
      return <SchedulerTab />;

    case "settings":
    case "settings-account":
    case "settings-ai":
    case "settings-agentic":
    case "settings-self-healing":
    case "settings-playwright":
    case "settings-mobile":
    case "settings-cloud-relay":
    case "settings-mcp":
    case "settings-log-sources":
    case "settings-execution-variables":
    case "settings-general":
    case "settings-storage":
    case "settings-backup":
    case "settings-instances":
    case "settings-debug":
    case "settings-updates": {
      const settingsTabMap: Record<string, string> = {
        settings: "account",
        "settings-account": "account",
        "settings-ai": "ai",
        "settings-agentic": "agentic",
        "settings-self-healing": "self-healing",
        "settings-playwright": "playwright",
        "settings-mobile": "mobile",
        "settings-cloud-relay": "cloud-relay",
        "settings-mcp": "mcp",
        "settings-log-sources": "log-sources",
        "settings-execution-variables": "execution-variables",
        "settings-general": "general",
        "settings-storage": "storage",
        "settings-backup": "backup",
        "settings-instances": "instances",
        "settings-debug": "advanced",
        "settings-updates": "updates",
      };
      const defaultSettingsTab = settingsTabMap[activeTab] || "account";

      return (
        <div className="h-full overflow-hidden">
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
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <LlmObservabilityDashboard />
          </Suspense>
        </div>
      );

    case "evaluation":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <EvaluationDashboard />
          </Suspense>
        </div>
      );

    case "terminal":
      return null;

    case "element-reliability":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <ElementReliabilityDashboard />
          </Suspense>
        </div>
      );

    case "automation-health":
      return (
        <div className="h-full overflow-auto p-4">
          <Suspense fallback={<LazyFallback />}>
            <AutomationHealthDashboard />
          </Suspense>
        </div>
      );

    case "pipeline-events":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <PipelineEventsTimeline />
          </Suspense>
        </div>
      );

    case "rule-influence":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <RuleInfluencePanel />
          </Suspense>
        </div>
      );

    case "unified-search":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <UnifiedSearchPanel onSearch={() => {}} />
          </Suspense>
        </div>
      );

    case "workflow-versions":
      return (
        <div className="h-full overflow-hidden">
          <Suspense fallback={<LazyFallback />}>
            <WorkflowVersionLineage />
          </Suspense>
        </div>
      );

    case "knowledge-explorer":
      return (
        <div className="h-full overflow-hidden">
          <KnowledgeExplorerPage />
        </div>
      );

    case "help":
      return <HelpTab />;

    default:
      return null;
  }
}
