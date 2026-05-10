import { useCallback } from "react";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { Finding } from "@/types/findings";
import { findingsTracker } from "@/services/FindingsTracker";

interface UseFindingsActionsParams {
  activeId: string | null;
  tabs: Array<{ id: string; workingDir?: string }>;
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  createTerminal: (title?: string, workingDir?: string) => Promise<string | null>;
  pendingResumeRef: React.MutableRefObject<{ tabId: string; resumeCmd: string } | null>;
  runGeneration: (description: string, inlineContext: string) => Promise<void>;
  setRightPanelMode: React.Dispatch<
    React.SetStateAction<"transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null>
  >;
}

export function useFindingsActions({
  activeId,
  tabs,
  terminalRefs,
  createTerminal,
  pendingResumeRef,
  runGeneration,
  setRightPanelMode,
}: UseFindingsActionsParams): {
  handleFindingRespond: (findingId: string, text: string) => void;
  handleFixFinding: (finding: Finding) => Promise<void>;
  handleGenerateFromFindings: (findings: Finding[]) => Promise<void>;
  handleToggleFindings: () => void;
} {
  const handleFindingRespond = useCallback(
    (findingId: string, text: string) => {
      findingsTracker.provideUserResponse(findingId, text);
      if (activeId) {
        terminalRefs.current.get(activeId)?.current?.writeToTerminal(text + "\r");
      }
    },
    [activeId, terminalRefs],
  );

  const handleFixFinding = useCallback(
    async (finding: Finding) => {
      const activeTab = tabs.find((t) => t.id === activeId);
      const workingDir = activeTab?.workingDir;
      const tabTitle = `fix: ${finding.title.slice(0, 20)}`;
      const tabId = await createTerminal(tabTitle, workingDir);
      if (!tabId) return;

      setRightPanelMode(null); // close findings panel to show terminal

      const title = finding.title.replace(/"/g, '\\"');
      const desc = finding.description.replace(/"/g, '\\"').slice(0, 500);
      const filePart = finding.codeContext?.file
        ? ` File: ${finding.codeContext.file}${finding.codeContext.line ? ":" + finding.codeContext.line : ""}.`
        : "";
      const resumeCmd = `claude "Fix this issue: ${title}.${filePart} Details: ${desc}"`;

      pendingResumeRef.current = { tabId, resumeCmd };
      setTimeout(() => {
        const pending = pendingResumeRef.current;
        if (!pending || pending.tabId !== tabId) return;
        pendingResumeRef.current = null;
        terminalRefs.current.get(tabId)?.current?.writeToTerminal(`${pending.resumeCmd}\r`);
      }, 1500);
    },
    [activeId, tabs, createTerminal, terminalRefs, pendingResumeRef, setRightPanelMode],
  );

  const handleGenerateFromFindings = useCallback(
    async (findings: Finding[]) => {
      const description =
        "Fix the following unresolved findings from the current development session";
      const inlineContext = findings
        .map((f) => {
          let entry = `- [${f.categoryId}:${f.severity}] ${f.title}`;
          if (f.description) entry += `\n  ${f.description}`;
          if (f.codeContext?.file) {
            entry += `\n  File: ${f.codeContext.file}`;
            if (f.codeContext.line) entry += `:${f.codeContext.line}`;
          }
          if (f.pendingQuestion) entry += `\n  Question: ${f.pendingQuestion.question}`;
          return entry;
        })
        .join("\n\n");
      await runGeneration(description, inlineContext);
    },
    [runGeneration],
  );

  const handleToggleFindings = useCallback(() => {
    setRightPanelMode((prev) => (prev === "findings" ? null : "findings"));
  }, [setRightPanelMode]);

  return {
    handleFindingRespond,
    handleFixFinding,
    handleGenerateFromFindings,
    handleToggleFindings,
  };
}
