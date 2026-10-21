import { useCallback, useRef, useState } from "react";
import type { ShellIntegrationEvent } from "./TerminalInstance";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { SessionState } from "./useZoneLayout";
import type { ResourceGuardSource } from "@/lib/resourceGuard";
import { resumeInNewTab, type ResumeAttempt, type ResumeTarget } from "./resumeInNewTab";

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
      isReconnecting: boolean;
      resumeFailed: boolean;
    }>,
  ) => void;
  renameTab: (id: string, title: string) => void;
  createTerminal: (
    title?: string,
    workingDir?: string,
    tenantId?: string,
    spawnSource?: ResourceGuardSource,
  ) => Promise<string | null>;
  setSessionStates: React.Dispatch<React.SetStateAction<Record<string, SessionState>>>;
  terminalRefs: React.MutableRefObject<Map<string, React.RefObject<TerminalInstanceHandle | null>>>;
  setRightPanelMode: React.Dispatch<
    React.SetStateAction<
      "transcript" | "workflow" | "analysis" | "findings" | "file-ownership" | null
    >
  >;
  setSelectedTranscriptSessionId: React.Dispatch<React.SetStateAction<string | null>>;
  /** Page the resumed tab lands on — recorded in the durable registry. */
  pageId: string;
}

interface UseShellIntegrationResult {
  commandHistories: Record<string, CommandHistoryEntry[]>;
  handleShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  handleResumeSession: (session: TranscriptSession) => void;
  /** THE resume path — see `resumeInNewTab`. */
  resumeTarget: (target: ResumeTarget, spawnSource?: ResourceGuardSource) => Promise<ResumeAttempt>;
  handleFirstInput: (tabId: string, input: string) => void;
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
  pageId,
}: UseShellIntegrationParams): UseShellIntegrationResult {
  // Shell integration: structured command history per tab
  const [commandHistories, setCommandHistories] = useState<Record<string, CommandHistoryEntry[]>>(
    {},
  );
  const pendingCommandRef = useRef<Record<string, string>>({});

  const handleShellIntegration = useCallback(
    (tabId: string, event: ShellIntegrationEvent) => {
      if (event.type === "prompt_start") {
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
    [updateTab, renameTab, tabs, setSessionStates],
  );

  // ── Resume a session in a new tab ────────────────────────────────────────
  //
  // THE one resume path (`resumeInNewTab`): `createTerminal` + the verified
  // `runVerifiedResume`, the boot restore's own pair. Every one-click Resume
  // (transcript panel, session manager, Previous Sessions) and the "Since
  // restart" bulk resume go through `resumeTarget`, so single and bulk resume
  // type the same shared command and verify it the same way.
  const resumeTarget = useCallback(
    (target: ResumeTarget, spawnSource?: ResourceGuardSource): Promise<ResumeAttempt> =>
      resumeInNewTab(
        { createTerminal, updateTab, terminalRefs: terminalRefs.current, pageId },
        target,
        spawnSource,
      ),
    [createTerminal, updateTab, terminalRefs, pageId],
  );

  const handleResumeSession = useCallback(
    async (session: TranscriptSession) => {
      // Close the transcript panel so the new terminal is visible.
      setRightPanelMode(null);
      setSelectedTranscriptSessionId(null);
      // The transcript was found under `config_dir`, so that IS the account.
      await resumeTarget({
        claudeSessionId: session.session_id,
        // The real `--resume` name (`/rename` / ai-title), not the first-message
        // preview in `display_name`.
        displayName: session.resume_name?.trim() || `claude ${session.session_id.slice(0, 8)}`,
        workingDir: session.project_path,
        configDir: session.config_dir || undefined,
      });
    },
    [resumeTarget, setRightPanelMode, setSelectedTranscriptSessionId],
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
    resumeTarget,
    handleFirstInput,
  };
}
