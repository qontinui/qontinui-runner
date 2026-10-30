import { useCallback, useEffect, useRef, useState } from "react";
import type { ShellIntegrationEvent } from "./TerminalInstance";
import type { TerminalInstanceHandle } from "./TerminalInstance";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { SessionState } from "./useZoneLayout";
import type { ResourceGuardSource } from "@/lib/resourceGuard";
import {
  fetchLiveClaudeHolders,
  liveSessionIdSet,
  type LiveClaudeSession,
} from "./liveClaudeSessions";
import {
  forkInNewTab,
  resumeFailureNotice,
  resumeInNewTab,
  resumeThrownNotice,
  type ResumeAttempt,
  type ResumeInNewTabDeps,
  type ResumeTarget,
} from "./resumeInNewTab";
import {
  forkTabTitle,
  runClaimedResume,
  runClickResume,
  type ClickResumeOptions,
  type ResumeGateChoice,
  type ResumeGatePrompt,
  type ResumeOutcome,
  type StartResumeMode,
} from "./resumeGate";

export interface CommandHistoryEntry {
  command: string;
  exitCode: number;
  timestamp: number;
}

/** The notice an interactive click posts while its liveness read runs. */
export function resumeCheckingNotice(displayName: string): string {
  return `Resuming "${displayName}" — checking whether it is already open…`;
}

/** A page notification, as `useWorkflowGeneration().notification` holds it. */
export type PageNotice = { message: string; type: "success" | "error" } | null;

/**
 * The state update that withdraws `message` if it is still the page's notice,
 * and leaves any newer notice (another click's, a failure's) in place.
 */
export function withdrawNoticeUpdate(message: string): (prev: PageNotice) => PageNotice {
  return (prev) => (prev !== null && prev.message === message ? null : prev);
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
  /**
   * The operator just resumed a session into this new tab
   * (`useZoneLayout().showSpawnedTab`): in tmux mode it is shown, like any
   * terminal the operator creates; in grid mode its zone comes from the
   * reconcile and this is a no-op.
   */
  onResumedTab?: (tabId: string) => void;
  /**
   * The page's notification: says why a click-resume did not come back
   * (refused, no tab, unverified, or threw), so a click is never silent.
   */
  notify?: (message: string, type: "success" | "error") => void;
  /**
   * Withdraw a notice `notify` posted, if it is still the one showing (see
   * {@link withdrawNoticeUpdate}). The click's "checking…" acknowledgement is
   * withdrawn once its liveness read returns, so it never outlives the check.
   */
  withdrawNotice?: (message: string) => void;
  /**
   * Injectable for tests: the liveness oracle — the gate's read
   * (`fetchLiveClaudeHolders`, rows with their holding panes).
   */
  readLiveSessions?: () => Promise<LiveClaudeSession[] | null>;
}

/** Options of an operator resume through the click gate. */
export interface ResumeSessionOptions {
  /**
   * `false` (bulk "resume selected"): never open the resume-live dialog — a
   * live id returns `skipped-live` and an unreadable registry
   * `skipped-unknown`, and failures are left to the caller's summary.
   */
  interactive?: boolean;
}

export interface UseShellIntegrationResult {
  commandHistories: Record<string, CommandHistoryEntry[]>;
  handleShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  /**
   * Resume a transcript session (Session Manager, transcript panel, keyboard
   * shortcut) through the click gate — see `resumeGate.runClickResume`.
   */
  handleResumeSession: (
    session: TranscriptSession,
    opts?: ResumeSessionOptions,
  ) => Promise<ResumeOutcome>;
  /** Resume any target through the click gate (Previous Sessions). */
  resumeClick: (target: ResumeTarget, opts?: ClickResumeOptions) => Promise<ResumeOutcome>;
  /**
   * The "Since restart" queue's resume — `resumeInNewTab` with its own pre-type
   * liveness refusal (live → `already-running`, unreadable →
   * `liveness-unknown`), and no dialog: the queue reports each item. Held under
   * the same in-flight claim as a click (`runClaimedResume`): an id a click or
   * an earlier item is still resuming is refused as `in-flight`.
   */
  resumeTarget: (target: ResumeTarget, spawnSource?: ResourceGuardSource) => Promise<ResumeAttempt>;
  /** The open resume-live dialog, or `null`. Rendered by `ResumeGateHost`. */
  resumeGatePrompt: ResumeGatePrompt | null;
  /** Open the resume-live dialog and resolve with the operator's choice. */
  askResumeGate: (prompt: ResumeGatePrompt) => Promise<ResumeGateChoice>;
  /** Answer the open dialog (resolves its {@link askResumeGate} promise). */
  answerResumeGate: (choice: ResumeGateChoice) => void;
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
  onResumedTab,
  notify,
  withdrawNotice,
  readLiveSessions,
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
  // (transcript panel, session manager, Previous Sessions) enters through the
  // click gate below (`gatedResume`), which reads liveness first; the "Since
  // restart" bulk resume calls `resumeTarget`, whose pre-type liveness read
  // refuses a live or unreadable id. Single and bulk resume type the same
  // shared command and verify it the same way.
  const newTabDeps = useCallback(
    (): ResumeInNewTabDeps => ({
      createTerminal,
      updateTab,
      terminalRefs: terminalRefs.current,
      pageId,
      onTabCreated: onResumedTab,
      // The pre-type re-read uses the SAME oracle as the gate.
      liveSessionIds: readLiveSessions
        ? async () => {
            const rows = await readLiveSessions();
            return rows === null ? null : liveSessionIdSet(rows);
          }
        : undefined,
    }),
    [createTerminal, updateTab, terminalRefs, pageId, onResumedTab, readLiveSessions],
  );

  const resumeTarget = useCallback(
    (target: ResumeTarget, spawnSource?: ResourceGuardSource): Promise<ResumeAttempt> =>
      runClaimedResume(target, () => resumeInNewTab(newTabDeps(), target, spawnSource)),
    [newTabDeps],
  );

  /**
   * Start a resume the gate cleared or the operator chose: a plain `--resume`
   * (re-checked for liveness right before typing unless the operator chose
   * "Resume anyway"), or a fork into a new conversation.
   */
  const startResume = useCallback(
    (target: ResumeTarget, mode: StartResumeMode): Promise<ResumeAttempt> =>
      mode.fork
        ? forkInNewTab(
            newTabDeps(),
            target,
            mode.forkTitle ?? forkTabTitle(null, target.claudeSessionId),
          )
        : resumeInNewTab(newTabDeps(), target, undefined, { allowLive: mode.allowLive }),
    [newTabDeps],
  );

  // ── The resume-live dialog ───────────────────────────────────────────────
  //
  // One open prompt at a time. A newer prompt cancels the older one rather
  // than leave its caller awaiting forever.
  const [resumeGatePrompt, setResumeGatePrompt] = useState<ResumeGatePrompt | null>(null);
  const gateResolverRef = useRef<((choice: ResumeGateChoice) => void) | null>(null);
  const askResumeGate = useCallback(
    (prompt: ResumeGatePrompt): Promise<ResumeGateChoice> =>
      new Promise<ResumeGateChoice>((resolve) => {
        gateResolverRef.current?.("cancel");
        gateResolverRef.current = resolve;
        setResumeGatePrompt(prompt);
      }),
    [],
  );
  const answerResumeGate = useCallback((choice: ResumeGateChoice) => {
    const resolve = gateResolverRef.current;
    gateResolverRef.current = null;
    setResumeGatePrompt(null);
    resolve?.(choice);
  }, []);
  // A dialog left open when this page's scope unmounts (the page was closed)
  // can never be answered, and its click holds the id's in-flight claim while
  // it waits. Answer it "cancel". `ResumeGateHost` does the same for a dialog
  // it stops showing on a page switch.
  useEffect(() => () => answerResumeGate("cancel"), [answerResumeGate]);

  const gatedResume = useCallback(
    (
      target: ResumeTarget,
      opts: ClickResumeOptions = {},
      beforeStart?: () => void,
    ): Promise<ResumeOutcome> => {
      // A non-interactive (bulk) caller reports failures in its own summary.
      const say = opts.interactive === false ? undefined : notify;
      return runClickResume(
        {
          readLive: readLiveSessions ?? fetchLiveClaudeHolders,
          start: (t, mode) => {
            beforeStart?.();
            return startResume(t, mode);
          },
          ask: askResumeGate,
          onFailed: (attempt, t) => {
            const notice = resumeFailureNotice(attempt, t.displayName);
            if (notice) say?.(notice, "error");
          },
          onError: (err, t) => say?.(resumeThrownNotice(err, t.displayName), "error"),
          // The click acknowledges itself at once: the liveness read can take
          // a couple of seconds (a holder census), and a click that shows
          // nothing invites the second click the in-flight claim refuses.
          onChecking: (t) => say?.(resumeCheckingNotice(t.displayName), "success"),
          // ...and withdraws that acknowledgement once the read returns: the
          // dialog, the new tab or a failure notice is the answer from then on.
          onChecked: (t) => withdrawNotice?.(resumeCheckingNotice(t.displayName)),
          onInFlight: (t) =>
            say?.(
              `"${t.displayName}" is already being resumed in this window — wait for it, or finish its dialog`,
              "error",
            ),
        },
        target,
        opts,
      );
    },
    [notify, withdrawNotice, readLiveSessions, startResume, askResumeGate],
  );

  const resumeClick = useCallback(
    (target: ResumeTarget, opts?: ClickResumeOptions): Promise<ResumeOutcome> =>
      gatedResume(target, opts),
    [gatedResume],
  );

  const handleResumeSession = useCallback(
    (session: TranscriptSession, opts: ResumeSessionOptions = {}): Promise<ResumeOutcome> =>
      gatedResume(
        {
          claudeSessionId: session.session_id,
          // The real `--resume` name (`/rename` / ai-title), not the
          // first-message preview in `display_name`.
          displayName: session.resume_name?.trim() || `claude ${session.session_id.slice(0, 8)}`,
          workingDir: session.project_path,
          // The transcript was found under `config_dir`, so that IS the account.
          configDir: session.config_dir || undefined,
        },
        { interactive: opts.interactive, conversationName: session.resume_name },
        () => {
          // Close the transcript panel so the new terminal is visible — only
          // once a tab is actually being opened, so a cancelled dialog leaves
          // the panel the operator was reading.
          setRightPanelMode(null);
          setSelectedTranscriptSessionId(null);
        },
      ),
    [gatedResume, setRightPanelMode, setSelectedTranscriptSessionId],
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
    resumeClick,
    resumeTarget,
    resumeGatePrompt,
    askResumeGate,
    answerResumeGate,
    handleFirstInput,
  };
}
