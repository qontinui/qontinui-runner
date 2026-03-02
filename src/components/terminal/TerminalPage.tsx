import { useEffect, useCallback, useRef, useState, createRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { TerminalInstance, type TerminalInstanceHandle } from "./TerminalInstance";
import { TerminalTabBar } from "./TerminalTabBar";
import { TerminalActionBar } from "./TerminalActionBar";
import { TranscriptImportDialog } from "./TranscriptImportDialog";
import { useTerminalManager } from "./useTerminalManager";

interface CommandResponse {
  success: boolean;
  message: string | null;
  data: unknown;
}

interface TerminalPageProps {
  onNavigateToWorkflow?: (workflowId: string) => void;
}

export function TerminalPage({ onNavigateToWorkflow }: TerminalPageProps) {
  const { tabs, activeId, setActiveId, createTerminal, closeTerminal, renameTab, updateTab } =
    useTerminalManager();

  // Refs to terminal instances for selection API
  const terminalRefs = useRef<Map<string, React.RefObject<TerminalInstanceHandle | null>>>(
    new Map(),
  );

  // Selection and generation state
  const [hasSelection, setHasSelection] = useState(false);
  const [isGenerating, setIsGenerating] = useState(false);
  const [showTranscriptDialog, setShowTranscriptDialog] = useState(false);

  // Ensure refs exist for all tabs
  for (const tab of tabs) {
    if (!terminalRefs.current.has(tab.id)) {
      terminalRefs.current.set(tab.id, createRef<TerminalInstanceHandle>());
    }
  }
  // Clean up refs for removed tabs
  for (const key of terminalRefs.current.keys()) {
    if (!tabs.some((t) => t.id === key)) {
      terminalRefs.current.delete(key);
    }
  }

  // Auto-create first terminal on mount
  const didInit = useRef(false);
  useEffect(() => {
    if (!didInit.current && tabs.length === 0) {
      didInit.current = true;
      createTerminal();
    }
  }, [createTerminal, tabs.length]);

  const handleExit = useCallback(
    (terminalId: string, exitCode: number | null) => {
      updateTab(terminalId, { isAlive: false, exitCode });
    },
    [updateTab],
  );

  const handleSelectionChange = useCallback(
    (terminalId: string, hasSelection: boolean) => {
      // Only update if this is the active terminal
      if (terminalId === activeId) {
        setHasSelection(hasSelection);
      }
    },
    [activeId],
  );

  // Reset selection state when switching tabs
  useEffect(() => {
    if (activeId) {
      const ref = terminalRefs.current.get(activeId);
      setHasSelection(ref?.current?.hasSelection() ?? false);
    } else {
      setHasSelection(false);
    }
  }, [activeId]);

  // Generate workflow from selected terminal text
  const handleGenerateFromSelection = useCallback(async () => {
    if (!activeId) return;
    const ref = terminalRefs.current.get(activeId);
    const selectedText = ref?.current?.getSelection();
    if (!selectedText?.trim()) return;

    setIsGenerating(true);
    try {
      const result = await invoke<CommandResponse>("generate_workflow_standalone", {
        description: "Generate workflow from terminal selection",
        inlineContext: selectedText,
      });

      if (result.success && result.data) {
        const data = result.data as { workflow?: { id?: string; name?: string } };
        const workflowId = data.workflow?.id;
        if (workflowId && onNavigateToWorkflow) {
          onNavigateToWorkflow(workflowId);
        }
      } else {
        console.error("Workflow generation failed:", result.message);
      }
    } catch (err) {
      console.error("Failed to generate workflow:", err);
    } finally {
      setIsGenerating(false);
    }
  }, [activeId, onNavigateToWorkflow]);

  // Generate workflow from transcript import
  const handleGenerateFromTranscript = useCallback(
    async (description: string, inlineContext: string) => {
      setIsGenerating(true);
      try {
        const result = await invoke<CommandResponse>("generate_workflow_standalone", {
          description,
          inlineContext,
        });

        if (result.success && result.data) {
          const data = result.data as { workflow?: { id?: string; name?: string } };
          const workflowId = data.workflow?.id;
          if (workflowId && onNavigateToWorkflow) {
            onNavigateToWorkflow(workflowId);
          }
        } else {
          console.error("Workflow generation failed:", result.message);
        }
      } catch (err) {
        console.error("Failed to generate workflow:", err);
      } finally {
        setIsGenerating(false);
      }
    },
    [onNavigateToWorkflow],
  );

  // Keyboard shortcuts
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // Ctrl+Shift+T — new terminal
      if (e.ctrlKey && e.shiftKey && e.key === "T") {
        e.preventDefault();
        createTerminal();
        return;
      }
      // Ctrl+Shift+W — close active terminal
      if (e.ctrlKey && e.shiftKey && e.key === "W") {
        e.preventDefault();
        if (activeId) closeTerminal(activeId);
        return;
      }
      // Ctrl+Tab / Ctrl+Shift+Tab — cycle tabs
      if (e.ctrlKey && e.key === "Tab" && tabs.length > 1 && activeId) {
        e.preventDefault();
        const idx = tabs.findIndex((t) => t.id === activeId);
        const next = e.shiftKey ? (idx - 1 + tabs.length) % tabs.length : (idx + 1) % tabs.length;
        setActiveId(tabs[next].id);
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [activeId, tabs, createTerminal, closeTerminal, setActiveId]);

  return (
    <div className="h-full flex flex-col bg-[#1a1b26]">
      <TerminalTabBar
        tabs={tabs}
        activeId={activeId}
        onSelect={setActiveId}
        onClose={closeTerminal}
        onCreate={() => createTerminal()}
        onRename={renameTab}
      />
      <TerminalActionBar
        hasSelection={hasSelection}
        onGenerateFromSelection={handleGenerateFromSelection}
        onImportTranscript={() => setShowTranscriptDialog(true)}
        isGenerating={isGenerating}
      />
      <div className="flex-1 relative overflow-hidden">
        {tabs.map((tab) => (
          <TerminalInstance
            key={tab.id}
            ref={terminalRefs.current.get(tab.id)}
            terminalId={tab.id}
            visible={tab.id === activeId}
            onExit={(code) => handleExit(tab.id, code)}
            onSelectionChange={(has) => handleSelectionChange(tab.id, has)}
          />
        ))}
        {tabs.length === 0 && (
          <div className="h-full flex flex-col items-center justify-center text-[#565f89] gap-2">
            <span className="text-sm">
              No terminals open. Press{" "}
              <kbd className="px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#a9b1d6] text-xs font-mono">
                Ctrl+Shift+T
              </kbd>{" "}
              or click + to create one.
            </span>
          </div>
        )}
      </div>

      <TranscriptImportDialog
        open={showTranscriptDialog}
        onClose={() => setShowTranscriptDialog(false)}
        onGenerate={handleGenerateFromTranscript}
      />
    </div>
  );
}
