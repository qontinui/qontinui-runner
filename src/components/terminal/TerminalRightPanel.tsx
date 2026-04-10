import { TranscriptContentPanel } from "./TranscriptContentPanel";
import { TerminalAnalysisPanel } from "./TerminalAnalysisPanel";
import { TerminalFindingsPanel } from "./TerminalFindingsPanel";
import { WorkflowPreviewPanel } from "@qontinui/workflow-ui";
import { useAiFeatures, useSessionState } from "./contexts";

export function TerminalRightPanel() {
  const {
    workflowGen,
    analysis,
    findingsActions,
    shellIntegration,
    transcriptSessions,
    sessionSummary,
    sessionSummaryLoading,
    handleSummarizeSession,
  } = useAiFeatures();
  const { activeFindings, allFindings } = useSessionState();

  const rightPanelMode = workflowGen.rightPanelMode;

  if (rightPanelMode === "transcript" && workflowGen.selectedTranscriptSessionId) {
    return (
      <TranscriptContentPanel
        sessionId={workflowGen.selectedTranscriptSessionId}
        session={
          transcriptSessions.find(
            (s) => s.session_id === workflowGen.selectedTranscriptSessionId,
          ) ?? null
        }
        messages={workflowGen.transcriptMessages}
        loading={workflowGen.loadingMessages}
        onGenerate={workflowGen.handleGenerateFromTranscript}
        onGenerateAndRun={workflowGen.handleGenerateAndRunFromTranscript}
        onBuildPlanWorkflow={workflowGen.handleBuildPlanWorkflow}
        onBuildPlanImplementationWorkflow={workflowGen.handleBuildPlanImplementationWorkflow}
        onResume={shellIntegration.handleResumeSession}
        onSummarize={handleSummarizeSession}
        summaryText={sessionSummary}
        summaryLoading={sessionSummaryLoading}
        onClose={() => {
          workflowGen.setRightPanelMode(null);
          workflowGen.setSelectedTranscriptSessionId(null);
        }}
      />
    );
  }

  if (rightPanelMode === "workflow") {
    return (
      <div className="w-[420px] h-full shrink-0">
        <WorkflowPreviewPanel
          workflow={workflowGen.generatedWorkflow}
          isLoading={workflowGen.isGenerating}
          error={workflowGen.workflowError}
          onExecute={workflowGen.handleExecute}
          onEditInBuilder={workflowGen.handleEditInBuilder}
          onRegenerate={workflowGen.handleRegenerate}
          onSave={workflowGen.handleSaveWorkflow}
          onClose={() => workflowGen.setRightPanelMode(null)}
        />
      </div>
    );
  }

  if (rightPanelMode === "analysis") {
    return (
      <TerminalAnalysisPanel
        analysisType={analysis.analysisType}
        panels={analysis.analysisPanels}
        isAnalyzing={analysis.isAnalyzing}
        error={analysis.analysisError}
        onClose={() => workflowGen.setRightPanelMode(null)}
      />
    );
  }

  if (rightPanelMode === "findings") {
    return (
      <TerminalFindingsPanel
        findings={activeFindings}
        allFindings={allFindings}
        onClose={() => workflowGen.setRightPanelMode(null)}
        onRespond={findingsActions.handleFindingRespond}
        onFix={findingsActions.handleFixFinding}
        onGenerateWorkflow={findingsActions.handleGenerateFromFindings}
      />
    );
  }

  return null;
}
