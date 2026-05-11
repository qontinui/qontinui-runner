import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CanvasPanel } from "@qontinui/shared-types";
import { type AnalysisType } from "./TerminalAnalysisPanel";
import type { CommandHistoryEntry } from "./useShellIntegration";
import type { CommandResponse } from "./types";

interface UseAnalysisParams {
  activeId: string | null;
  tabs: Array<{ id: string; title: string }>;
  commandHistories: Record<string, CommandHistoryEntry[]>;
  getScrollback: (tabId: string, maxLines?: number) => string;
  getActiveSelection: () => string;
  latestPlanContent: string;
  setRightPanelMode: React.Dispatch<
    React.SetStateAction<"transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null>
  >;
}

export function useAnalysis({
  activeId,
  tabs,
  commandHistories,
  getScrollback,
  getActiveSelection,
  latestPlanContent,
  setRightPanelMode,
}: UseAnalysisParams): {
  isAnalyzing: boolean;
  analysisType: AnalysisType;
  analysisPanels: CanvasPanel[] | null;
  analysisError: string | undefined;
  handleAnalyze: (type: AnalysisType) => Promise<void>;
} {
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [analysisType, setAnalysisType] = useState<AnalysisType>("session-summary");
  const [analysisPanels, setAnalysisPanels] = useState<CanvasPanel[] | null>(null);
  const [analysisError, setAnalysisError] = useState<string | undefined>();

  const handleAnalyze = useCallback(
    async (type: AnalysisType) => {
      setAnalysisType(type);
      setIsAnalyzing(true);
      setAnalysisPanels(null);
      setAnalysisError(undefined);
      setRightPanelMode("analysis");

      // Collect the right input per analysis type
      let input = "";
      if (type === "session-summary") {
        // Prefer structured command history over raw ANSI-polluted scrollback
        const history = commandHistories[activeId ?? ""] ?? [];
        input =
          history.length > 0
            ? history.map((e) => `$ ${e.command}  [exit ${e.exitCode}]`).join("\n")
            : activeId
              ? getScrollback(activeId, 500)
              : "";
      } else if (type === "architecture") {
        // Prefer: plan content → terminal selection → scrollback
        if (latestPlanContent.trim().length > 0) {
          const sel = getActiveSelection();
          input =
            sel.trim().length > 20
              ? `${latestPlanContent}\n\n---\nSelected terminal context:\n${sel}`
              : latestPlanContent;
        } else {
          const sel = getActiveSelection();
          input = sel.trim().length > 20 ? sel : activeId ? getScrollback(activeId, 300) : "";
        }
      } else if (type === "change-impact") {
        const sel = getActiveSelection();
        input = sel.trim().length > 0 ? sel : activeId ? getScrollback(activeId, 200) : "";
      } else if (type === "progress") {
        // Prefer plan content as the plan; always append scrollback for evidence
        const scrollback = activeId ? getScrollback(activeId, 300) : "";
        if (latestPlanContent.trim().length > 0) {
          input = `${latestPlanContent}\n\n---\nTerminal activity (for progress evidence):\n${scrollback}`;
        } else {
          const sel = getActiveSelection();
          input = sel.trim().length > 20 ? `${sel}\n\n---\n${scrollback}` : scrollback;
        }
      } else if (type === "cross-tab") {
        const parts: string[] = [];
        for (const tab of tabs) {
          const history = commandHistories[tab.id] ?? [];
          const content =
            history.length > 0
              ? history.map((e) => `$ ${e.command}  [exit ${e.exitCode}]`).join("\n")
              : getScrollback(tab.id, 200);
          if (content.trim().length > 0) {
            parts.push(`--- Tab: ${tab.title} ---\n${content}`);
          }
        }
        input = parts.join("\n\n");
      } else if (type === "page-architecture") {
        input = "";
      }

      const commandMap: Record<AnalysisType, string> = {
        "session-summary": "analyze_session_summary",
        architecture: "analyze_architecture",
        "change-impact": "analyze_change_impact",
        progress: "analyze_plan_progress",
        "cross-tab": "analyze_cross_tab",
        "page-architecture": "analyze_page_architecture",
      };

      try {
        const args = type === "page-architecture" ? {} : { input };
        const result = await invoke<CommandResponse>(commandMap[type], args);

        if (result.success && result.data) {
          const data = result.data as { panels?: CanvasPanel[] };
          setAnalysisPanels(data.panels ?? []);
        } else {
          setAnalysisError(result.message || "Analysis failed");
        }
      } catch (err) {
        setAnalysisError(err instanceof Error ? err.message : "Analysis failed");
      } finally {
        setIsAnalyzing(false);
      }
    },
    [
      activeId,
      tabs,
      getScrollback,
      getActiveSelection,
      latestPlanContent,
      commandHistories,
      setRightPanelMode,
    ],
  );

  return {
    isAnalyzing,
    analysisType,
    analysisPanels,
    analysisError,
    handleAnalyze,
  };
}
