import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CanvasPanel } from "@qontinui/shared-types";
import { type AnalysisType } from "./TerminalAnalysisPanel";
import type { CommandHistoryEntry } from "./useShellIntegration";
import type { CommandResponse } from "./types";

/**
 * What an analysis run actually produced.
 *
 * `handleAnalyze` used to be `Promise<void>` and routed BOTH of its failure
 * arms — a backend `success: false`, and a thrown IPC — into `analysisError`
 * state, which the `/analyze` command handler has no way to read back. So a
 * metered Claude call that came back empty or errored still rendered as a
 * success in the command bar. The panel keeps its state (that is how the
 * right-hand panel renders); this envelope is for the callers that need a
 * verdict rather than a render.
 */
export type AnalysisOutcome =
  | { ok: true; panels: number }
  | { ok: false; message: string };

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
  handleAnalyze: (type: AnalysisType) => Promise<AnalysisOutcome>;
} {
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [analysisType, setAnalysisType] = useState<AnalysisType>("session-summary");
  const [analysisPanels, setAnalysisPanels] = useState<CanvasPanel[] | null>(null);
  const [analysisError, setAnalysisError] = useState<string | undefined>();

  const handleAnalyze = useCallback(
    async (type: AnalysisType): Promise<AnalysisOutcome> => {
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
          const panels = data.panels ?? [];
          setAnalysisPanels(panels);
          return { ok: true, panels: panels.length };
        }
        const message = result.message || "Analysis failed";
        setAnalysisError(message);
        return { ok: false, message };
      } catch (err) {
        const message = err instanceof Error ? err.message : "Analysis failed";
        setAnalysisError(message);
        return { ok: false, message };
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
