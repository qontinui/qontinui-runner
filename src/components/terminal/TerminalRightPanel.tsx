import { TranscriptContentPanel } from "./TranscriptContentPanel";
import { TerminalAnalysisPanel } from "./TerminalAnalysisPanel";
import type { AnalysisType } from "./TerminalAnalysisPanel";
import { TerminalFindingsPanel } from "./TerminalFindingsPanel";
import { WorkflowPreviewPanel } from "@qontinui/workflow-ui";
import type { TranscriptSession, TranscriptMessage } from "./useTranscriptSessions";
import type { UnifiedWorkflow } from "@qontinui/shared-types";
import type { CanvasPanel } from "@qontinui/shared-types";
import type { Finding } from "@/types/findings";

interface TerminalRightPanelProps {
  rightPanelMode: "transcript" | "workflow" | "analysis" | "findings" | null;
  selectedTranscriptSessionId: string | null;
  transcriptSessions: TranscriptSession[];
  transcriptMessages: TranscriptMessage[];
  loadingMessages: boolean;
  onGenerateFromTranscript: (desc: string, ctx: string) => void;
  onGenerateAndRunFromTranscript: (desc: string, ctx: string) => void;
  onBuildPlanWorkflow: (planContent: string) => void;
  onResumeSession: (session: TranscriptSession) => void;
  onClosePanel: () => void;
  generatedWorkflow: UnifiedWorkflow | null;
  isGenerating: boolean;
  workflowError: string | undefined;
  onExecute: () => Promise<void>;
  onEditInBuilder: () => void;
  onRegenerate: () => Promise<void>;
  onSaveWorkflow: () => Promise<void>;
  onCloseWorkflow: () => void;
  analysisType: AnalysisType;
  analysisPanels: CanvasPanel[] | null;
  isAnalyzing: boolean;
  analysisError: string | undefined;
  onCloseAnalysis: () => void;
  activeFindings: Finding[];
  allFindings: Finding[];
  onFindingRespond: (findingId: string, text: string) => void;
  onFixFinding: (finding: Finding) => Promise<void>;
  onGenerateFromFindings: (findings: Finding[]) => Promise<void>;
  onCloseFindings: () => void;
}

export function TerminalRightPanel({
  rightPanelMode,
  selectedTranscriptSessionId,
  transcriptSessions,
  transcriptMessages,
  loadingMessages,
  onGenerateFromTranscript,
  onGenerateAndRunFromTranscript,
  onBuildPlanWorkflow,
  onResumeSession,
  onClosePanel,
  generatedWorkflow,
  isGenerating,
  workflowError,
  onExecute,
  onEditInBuilder,
  onRegenerate,
  onSaveWorkflow,
  onCloseWorkflow,
  analysisType,
  analysisPanels,
  isAnalyzing,
  analysisError,
  onCloseAnalysis,
  activeFindings,
  allFindings,
  onFindingRespond,
  onFixFinding,
  onGenerateFromFindings,
  onCloseFindings,
}: TerminalRightPanelProps) {
  if (rightPanelMode === "transcript" && selectedTranscriptSessionId) {
    return (
      <TranscriptContentPanel
        sessionId={selectedTranscriptSessionId}
        session={
          transcriptSessions.find((s) => s.session_id === selectedTranscriptSessionId) ?? null
        }
        messages={transcriptMessages}
        loading={loadingMessages}
        onGenerate={onGenerateFromTranscript}
        onGenerateAndRun={onGenerateAndRunFromTranscript}
        onBuildPlanWorkflow={onBuildPlanWorkflow}
        onResume={onResumeSession}
        onClose={onClosePanel}
      />
    );
  }

  if (rightPanelMode === "workflow") {
    return (
      <div className="w-[420px] h-full shrink-0">
        <WorkflowPreviewPanel
          workflow={generatedWorkflow}
          isLoading={isGenerating}
          error={workflowError}
          onExecute={onExecute}
          onEditInBuilder={onEditInBuilder}
          onRegenerate={onRegenerate}
          onSave={onSaveWorkflow}
          onClose={onCloseWorkflow}
        />
      </div>
    );
  }

  if (rightPanelMode === "analysis") {
    return (
      <TerminalAnalysisPanel
        analysisType={analysisType}
        panels={analysisPanels}
        isAnalyzing={isAnalyzing}
        error={analysisError}
        onClose={onCloseAnalysis}
      />
    );
  }

  if (rightPanelMode === "findings") {
    return (
      <TerminalFindingsPanel
        findings={activeFindings}
        allFindings={allFindings}
        onClose={onCloseFindings}
        onRespond={onFindingRespond}
        onFix={onFixFinding}
        onGenerateWorkflow={onGenerateFromFindings}
      />
    );
  }

  return null;
}
