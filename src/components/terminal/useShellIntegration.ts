import { useCallback, useRef, useState } from "react";
import type { ShellIntegrationEvent } from "./TerminalInstance";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { SessionState } from "./useZoneLayout";
import { rememberSessionId } from "./lastKnownSessionIds";

export interface CommandHistoryEntry {
  command: string;
  exitCode: number;
  timestamp: number;
}

interface UseShellIntegrationParams {
  tabs: Array<{
    id: string;
    title: string;
    workingDir?: string;
    claudeSessionId?: string;
    claudeConfigDir?: string;
  }>;
  updateTab: (
    id: string,
    updates: Partial<{
      title: string;
      workingDir: string;
      claudeSessionId: string;
      claudeConfigDir: string;
    }>,
  ) => void;
  renameTab: (id: string, title: string) => void;
  createTerminal: (title?: string, workingDir?: string) => Promise<string | null>;
  setSessionStates: React.Dispatch<React.SetStateAction<Record<string, SessionState>>>;
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  setRightPanelMode: React.Dispatch<
    React.SetStateAction<
      "transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null
    >
  >;
  setSelectedTranscriptSessionId: React.Dispatch<React.SetStateAction<string | null>>;
}

interface UseShellIntegrationResult {
  commandHistories: Record<string, CommandHistoryEntry[]>;
  handleShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  handleResumeSession: (session: TranscriptSession) => void;
  handleFirstInput: (tabId: string, input: string) => void;
  pendingResumeRef: React.MutableRefObject<{ tabId: string; resumeCmd: string } | null>;
}

export function useShellIntegration({
  tabs,
  updateTab,
  renameTab,
  createTerminal,
  setSessionStates,
  terminalRefs,
  setRightPanelMode,
  setSelectedTranscriptSessionId,
}: UseShellIntegrationParams): UseShellIntegrationResult {
  // Shell integration: structured command history per tab
  const [commandHistories, setCommandHistories] = useState<Record<string, CommandHistoryEntry[]>>(
    {},
  );
  const pendingCommandRef = useRef<Record<string, string>>({});

  // Tracks the tab ID and session ID awaiting the first shell prompt to send the command.
  const pendingResumeRef = useRef<{ tabId: string; resumeCmd: string } | null>(null);

  const handleShellIntegration = useCallback(
    (tabId: string, event: ShellIntegrationEvent) => {
      // If this tab has a pending resume command, fire it on the first prompt
      if (event.type === "prompt_start") {
        const pending = pendingResumeRef.current;
        if (pending && pending.tabId === tabId) {
          pendingResumeRef.current = null;
          // Small defer so the prompt finishes rendering before we write
          setTimeout(() => {
            const ref = terminalRefs.current.get(tabId);
            ref?.current?.writeToTerminal(`${pending.resumeCmd}\r`);
          }, 50);
        }
        // Shell prompt appeared. A `prompt_start` only means "a shell prompt
        // is being drawn" — NOT that a Claude session is awaiting the user.
        // Claude Code redraws its prompt frequently while idle, so latching
        // every Claude-backed prompt_start to `needs-input` produced the
        // "N need input" phantom (3 sessions, 0 actually waiting). Treat a
        // bare prompt as `idle`; genuine "awaiting input" is detected from
        // the prompt *text* by `sessionStateDetector` (tool-approval / y-n
        // prompts), not from the prompt-start marker. Don't clobber a real
        // `needs-input`/`error` that the detector already set.
        setSessionStates((prev) => {
          const current = prev[tabId];
          if (current === "needs-input" || current === "error") return prev;
          return { ...prev, [tabId]: "idle" };
        });
      }
      if (event.type === "command_execute") {
        setSessionStates((prev) => ({ ...prev, [tabId]: "working" }));
      }
      if (event.type === "cwd") {
        updateTab(tabId, { workingDir: event.path });
        // Auto-name tab from project directory if still using default name
        const tab = tabs.find((t) => t.id === tabId);
        if (tab && /^Terminal \d+$/.test(tab.title)) {
          const dirName = event.path.split(/[/\\]/).pop();
          if (dirName) {
            renameTab(tabId, dirName);
          }
        }
      } else if (event.type === "command_line") {
        pendingCommandRef.current[tabId] = event.command;
      } else if (event.type === "command_done") {
        const cmd = pendingCommandRef.current[tabId];
        if (cmd) {
          delete pendingCommandRef.current[tabId];
          setCommandHistories((prev) => ({
            ...prev,
            [tabId]: [
              ...(prev[tabId] ?? []).slice(-99),
              { command: cmd, exitCode: event.exitCode, timestamp: Date.now() },
            ],
          }));
        }
      }
    },
    [updateTab, renameTab, tabs, terminalRefs, setSessionStates],
  );

  // ── Resume Claude Code session in terminal ─────────────────────────────────

  const handleResumeSession = useCallback(
    async (session: TranscriptSession) => {
      // Derive a short label from the session ID for the tab title
      const tabTitle = `claude ${session.session_id.slice(0, 8)}`;
      const tabId = await createTerminal(tabTitle, session.project_path);
      if (!tabId) return;

      // Track which Claude session is running in this tab so "Generate Workflow"
      // can find the correct transcript instead of picking a random recent session.
      updateTab(tabId, {
        claudeSessionId: session.session_id,
        claudeConfigDir: session.config_dir,
      });
      // Persist durably so the tab stays resumable across a close→reopen even
      // if the live tab object later loses the id.
      rememberSessionId(tabId, session.session_id, session.config_dir);

      // Close the transcript panel so the terminal is visible
      setRightPanelMode(null);
      setSelectedTranscriptSessionId(null);

      // Queue the resume command — it will be sent once the shell emits its first prompt.
      // Include the config_dir so Claude CLI searches the right directory.
      // Windows terminals use PowerShell ($env:VAR), others use bash (VAR=val cmd).
      const configDir = session.config_dir;
      const isWindows = navigator.platform.startsWith("Win");
      // Resume autonomously (`--permission-mode bypassPermissions`) to match
      // the operator's clg/clh/clp wrappers — a resumed session shouldn't
      // stall on a permission prompt either.
      let resumeCmd: string;
      if (configDir) {
        resumeCmd = isWindows
          ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; claude --permission-mode bypassPermissions --resume ${session.session_id}`
          : `CLAUDE_CONFIG_DIR="${configDir}" claude --permission-mode bypassPermissions --resume ${session.session_id}`;
      } else {
        resumeCmd = `claude --permission-mode bypassPermissions --resume ${session.session_id}`;
      }
      pendingResumeRef.current = { tabId, resumeCmd };

      // Fallback: send after 1.5 s regardless (in case shell integration isn't active)
      setTimeout(() => {
        const pending = pendingResumeRef.current;
        if (!pending || pending.tabId !== tabId) return;
        pendingResumeRef.current = null;
        const ref = terminalRefs.current.get(tabId);
        ref?.current?.writeToTerminal(`${pending.resumeCmd}\r`);
      }, 1500);
    },
    [createTerminal, updateTab, terminalRefs, setRightPanelMode, setSelectedTranscriptSessionId],
  );

  // ── Auto-naming from first input ──────────────────────────────────────────

  const handleFirstInput = useCallback(
    (terminalId: string, input: string) => {
      const tab = tabs.find((t) => t.id === terminalId);
      if (!tab) return;
      if (/^Terminal \d+$/.test(tab.title)) {
        renameTab(terminalId, input.slice(0, 30).trim());
      }
    },
    [tabs, renameTab],
  );

  return {
    commandHistories,
    handleShellIntegration,
    handleResumeSession,
    handleFirstInput,
    pendingResumeRef,
  };
}
