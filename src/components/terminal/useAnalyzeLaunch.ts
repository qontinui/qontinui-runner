/**
 * "Analyze launch" + failed-launch detection, bound to the Terminal page (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phases 3 and 4).
 *
 * The logic lives in `analyzeLaunch.ts` (the analyzer open) and
 * `launchVerification.ts` (did a fresh launch start?); this hook binds them to
 * the page's create path, writer, account roster and tab state, and shares one
 * per-pane status between every Analyze button (zone cluster, overflow menu,
 * `LaunchFailedBanner`) through {@link AnalyzeLaunchContext}.
 *
 * It also registers the UI Bridge component `terminal-analyze-launch` with the
 * `analyze-launch` action (`{ terminalId }`), so automation drives the same
 * path a click does.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useUIComponent } from "@qontinui/ui-bridge";
import { compareByUsageHeadroom } from "../settings/types";
import { buildAiLaunchCommand, type AiLaunchCommand } from "./aiLaunchCommand";
import {
  analyzeOutcomeToast,
  createAnalyzeLaunchRunner,
  runAnalyzeLaunch,
  type AnalyzeOutcome,
  type AnalyzeStatus,
} from "./analyzeLaunch";
import { captureLaunchEvidence, readTerminalLaunchFailure } from "./launchFailureApi";
import { FRESH_LAUNCH_HANDSHAKE_PATTERNS, watchFreshLaunch } from "./launchVerification";
import { waitForClaudeHandshake } from "./resumeVerification";
import type { TerminalTab } from "./useTerminalManager";
import type { TerminalWriteResult } from "./terminalWriteResult";

type RankableAccount = Parameters<typeof compareByUsageHeadroom>[0] & { config_dir: string };

/** What the page's Analyze buttons read. */
export interface AnalyzeLaunchApi {
  /** Start the analyzer for a pane (no-op while one is in flight for it). */
  analyze: (tabId: string) => void;
  /** The pane's button status; `undefined` = idle. */
  statusOf: (tabId: string) => AnalyzeStatus | undefined;
  /** Clear a pane's failed-launch banner. */
  dismissLaunchFailure: (tabId: string) => void;
}

export const AnalyzeLaunchContext = createContext<AnalyzeLaunchApi | null>(null);

/** The page's Analyze API, or null outside a provider (test mounts). */
export function useAnalyzeLaunchApi(): AnalyzeLaunchApi | null {
  return useContext(AnalyzeLaunchContext);
}

export interface UseAnalyzeLaunchArgs {
  tabs: readonly TerminalTab[];
  updateTab: (id: string, updates: Partial<TerminalTab>) => void;
  /** `createAndAssignTerminal(title, workingDir, tenantId)`. */
  createPane: (title: string, cwd: string | undefined, tenantId?: string) => Promise<string | null>;
  /** The page's `writeWhenReady`. */
  write: (tabId: string, text: string) => Promise<TerminalWriteResult>;
  accounts: readonly RankableAccount[];
  launchAliases: Readonly<Record<string, string>> | undefined;
  notify: (n: { message: string; type: "success" | "error" }) => void;
  /** Record a launched pane exactly as the launch menu does. */
  onLaunched: (tabId: string, launch: AiLaunchCommand, configDir: string) => void;
  setActiveId: (tabId: string) => void;
  zoneAssignments: Readonly<Record<number, string>>;
  setFocusedZone: (zone: number) => void;
}

export function useAnalyzeLaunch(args: UseAnalyzeLaunchArgs) {
  const argsRef = useRef(args);
  useEffect(() => {
    argsRef.current = args;
  });

  const [statuses, setStatuses] = useState<Record<string, AnalyzeStatus>>({});

  // Focus the analyzer pane once the grid has given it a zone.
  const pendingFocusRef = useRef<string | null>(null);
  const { zoneAssignments, setFocusedZone } = args;
  const focusWhenPlaced = useCallback(() => {
    const pending = pendingFocusRef.current;
    if (!pending) return;
    const zone = Object.entries(argsRef.current.zoneAssignments).find(
      ([, id]) => id === pending,
    )?.[0];
    if (zone === undefined) return;
    pendingFocusRef.current = null;
    argsRef.current.setFocusedZone(Number(zone));
  }, []);
  useEffect(focusWhenPlaced, [zoneAssignments, setFocusedZone, focusWhenPlaced]);
  const focusAnalyzer = useCallback(
    (tabId: string) => {
      argsRef.current.setActiveId(tabId);
      pendingFocusRef.current = tabId;
      focusWhenPlaced();
    },
    [focusWhenPlaced],
  );

  // Built on first use (an event), never during render: it closes over the
  // latest page bindings through `argsRef`.
  const runnerRef = useRef<ReturnType<typeof createAnalyzeLaunchRunner> | null>(null);
  const getRunner = useCallback(() => {
    runnerRef.current ??= createAnalyzeLaunchRunner(
      (pane) =>
        runAnalyzeLaunch(pane, {
          captureEvidence: captureLaunchEvidence,
          rankedAccounts: () => [...argsRef.current.accounts].sort(compareByUsageHeadroom),
          launchAliases: () => argsRef.current.launchAliases,
          buildCommand: buildAiLaunchCommand,
          createPane: (title, cwd, tenantId) => argsRef.current.createPane(title, cwd, tenantId),
          write: (tabId, text) => argsRef.current.write(tabId, text),
          waitHandshake: (tabId) =>
            waitForClaudeHandshake(tabId, { handshakePatterns: FRESH_LAUNCH_HANDSHAKE_PATTERNS }),
          onLaunched: (tabId, launch, configDir) =>
            argsRef.current.onLaunched(tabId, launch, configDir),
          newSessionId: () => crypto.randomUUID(),
          isWindows: navigator.platform.startsWith("Win"),
        }),
      (paneId, status) => setStatuses((prev) => ({ ...prev, [paneId]: status })),
    );
    return runnerRef.current;
  }, []);

  const report = useCallback(
    (outcome: AnalyzeOutcome) => {
      const a = argsRef.current;
      if (outcome.ok) {
        focusAnalyzer(outcome.analyzerTabId);
        return;
      }
      // Every failure is toasted, `not-provisioned` included: the pane is open
      // and focused, but its first turn may not resolve, so the button leaves
      // "Analyzer opened" (D1) and the operator is told why.
      if (outcome.analyzerTabId) focusAnalyzer(outcome.analyzerTabId);
      console.error(`[AnalyzeLaunch] ${outcome.step}: ${outcome.message}`);
      const toast = analyzeOutcomeToast(outcome);
      if (toast) a.notify(toast);
    },
    [focusAnalyzer],
  );

  const start = useCallback(
    (tabId: string): Promise<AnalyzeOutcome> | null => {
      const tab = argsRef.current.tabs.find((t) => t.id === tabId);
      if (!tab) return null;
      const run = getRunner().start(tab);
      if (!run) return null;
      return run.then((outcome) => {
        report(outcome);
        return outcome;
      });
    },
    [getRunner, report],
  );

  const analyze = useCallback((tabId: string) => void start(tabId), [start]);
  const statusOf = useCallback((tabId: string) => statuses[tabId], [statuses]);
  const dismissLaunchFailure = useCallback(
    (tabId: string) => argsRef.current.updateTab(tabId, { launchFailure: undefined }),
    [],
  );

  /**
   * Phase 4: observe a fresh launch typed into `tabId`. A launch SEEN not to
   * start raises the banner; a handshake seen at any point — in the first wait
   * or by the bounded late re-check — clears it. A newer launch in the same
   * tab, or the tab closing, ends this watch. An unread launch flags nothing.
   */
  const watchGenRef = useRef<Record<string, number>>({});
  const verifyLaunch = useCallback((tabId: string) => {
    const gen = (watchGenRef.current[tabId] ?? 0) + 1;
    watchGenRef.current[tabId] = gen;
    const isActive = () =>
      watchGenRef.current[tabId] === gen && argsRef.current.tabs.some((t) => t.id === tabId);
    void watchFreshLaunch(tabId, {
      readFailure: (id) => readTerminalLaunchFailure(id),
      isActive,
      setFailure: (signals) => argsRef.current.updateTab(tabId, { launchFailure: signals }),
    }).then((result) => {
      if (result === "unknown") {
        console.warn(`[LaunchAI] launch status of ${tabId} is UNKNOWN (nothing could be read)`);
      } else if (result === "retracted") {
        console.warn(`[LaunchAI] ${tabId}: Claude started late; launch-failure banner retracted`);
      }
    });
  }, []);

  useUIComponent({
    id: "terminal-analyze-launch",
    name: "Analyze launch",
    description:
      "Open an analyzer session for a terminal pane whose Claude launch failed: captures the pane's launch evidence and launches `/analyze-launch-failure <evidence>` in a new pane on another account.",
    actions: [
      {
        id: "analyze-launch",
        label: "Analyze launch",
        description:
          "Analyze the launch in `{ terminalId }`: capture evidence, then open an analyzer Claude session in a new pane.",
        // `destructive` — launches an autonomous Claude agent that may retry the
        // launch, post a finding or run /pvi against the operator's repositories
        // and spends account budget; closing the pane undoes none of it.
        effect: "destructive",
        handler: async (params?: unknown) => {
          const terminalId = (params as { terminalId?: unknown } | undefined)?.terminalId;
          if (typeof terminalId !== "string" || terminalId.length === 0) {
            throw new Error("analyze-launch requires { terminalId: string }");
          }
          const run = start(terminalId);
          if (!run) {
            throw new Error(
              `analyze-launch: no terminal ${terminalId}, or an analysis is already running for it`,
            );
          }
          return run;
        },
      },
    ],
  });

  const api: AnalyzeLaunchApi = useMemo(
    () => ({ analyze, statusOf, dismissLaunchFailure }),
    [analyze, statusOf, dismissLaunchFailure],
  );

  return { api, verifyLaunch };
}
