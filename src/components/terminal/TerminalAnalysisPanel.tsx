/**
 * TerminalAnalysisPanel
 *
 * Right pane shown when the user triggers one of the 5 terminal analysis actions.
 * Renders analysis output as canvas panels using the existing CanvasWidget infrastructure.
 */

import { X } from "lucide-react";
import { CanvasWidget } from "@/components/widgets/canvas/CanvasWidget";
import type { CanvasPanel } from "@qontinui/shared-types";

// ============================================================================
// Types
// ============================================================================

export type AnalysisType =
  | "session-summary"
  | "architecture"
  | "change-impact"
  | "progress"
  | "cross-tab"
  | "page-architecture";

const ANALYSIS_LABELS: Record<AnalysisType, string> = {
  "session-summary": "Session Summary",
  architecture: "Architecture",
  "change-impact": "Change Impact",
  progress: "Plan Progress",
  "cross-tab": "All Sessions",
  "page-architecture": "Page Map",
};

const ANALYSIS_ICONS: Record<AnalysisType, string> = {
  "session-summary": "📋",
  architecture: "🏗",
  "change-impact": "🔍",
  progress: "📊",
  "cross-tab": "🗂",
  "page-architecture": "🗺",
};

interface TerminalAnalysisPanelProps {
  analysisType: AnalysisType;
  panels: CanvasPanel[] | null;
  isAnalyzing: boolean;
  error: string | undefined;
  onClose: () => void;
}

// ============================================================================
// Component
// ============================================================================

export function TerminalAnalysisPanel({
  analysisType,
  panels,
  isAnalyzing,
  error,
  onClose,
}: TerminalAnalysisPanelProps) {
  const label = ANALYSIS_LABELS[analysisType];
  const icon = ANALYSIS_ICONS[analysisType];

  return (
    <div className="w-[460px] h-full shrink-0 flex flex-col bg-[#13141f] border-l border-[#2a2d3d]">
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2.5 border-b border-[#2a2d3d] bg-[#0d0e1a] shrink-0">
        <span className="text-base leading-none">{icon}</span>
        <span className="flex-1 text-sm font-semibold text-[#c0caf5]">{label}</span>
        {isAnalyzing && (
          <div className="flex items-center gap-1.5 text-xs text-[#e0af68]">
            <div className="w-3 h-3 border-2 border-[#e0af68] border-t-transparent rounded-full animate-spin" />
            Analyzing…
          </div>
        )}
        <button
          onClick={onClose}
          className="p-1 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title="Close"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Body */}
      <div className="flex-1 overflow-hidden">
        {/* Loading state */}
        {isAnalyzing && !panels && (
          <div className="h-full flex flex-col items-center justify-center gap-3 text-[#565f89]">
            <div className="w-8 h-8 border-2 border-[#7aa2f7] border-t-transparent rounded-full animate-spin" />
            <span className="text-sm">Generating {label.toLowerCase()}…</span>
          </div>
        )}

        {/* Error state */}
        {!isAnalyzing && error && (
          <div className="m-4 p-3 rounded-lg bg-[#f7768e]/10 border border-[#f7768e]/30">
            <div className="text-sm font-semibold text-[#f7768e] mb-1">Analysis failed</div>
            <div className="text-xs text-[#f7768e]/80">{error}</div>
          </div>
        )}

        {/* Canvas panels */}
        {panels && panels.length > 0 && (
          <CanvasWidget
            hideHeader
            isActive
            isSummary={false}
            status="completed"
            data={{
              panels,
              panelCount: panels.length,
              isLoading: false,
              error: null,
            }}
            className="h-full"
          />
        )}

        {/* Empty result (no panels returned) */}
        {!isAnalyzing && !error && panels && panels.length === 0 && (
          <div className="h-full flex items-center justify-center text-sm text-[#565f89]">
            No analysis panels returned
          </div>
        )}
      </div>
    </div>
  );
}
