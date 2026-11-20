/**
 * "Analyze launch" — the pure half (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phase 3).
 *
 * A click on a pane whose `claude` launch failed:
 *   1. captures that pane's launch evidence (backend, `capture_launch_evidence`);
 *   2. picks the analyzer's account (D4): the headroom-best account that is NOT
 *      the one that failed, preferring accounts with no per-account launch
 *      alias (an alias drops the positional prompt, D1), and the failed
 *      account only when it is the only one;
 *   3. builds the launch command with `initialPrompt` =
 *      `/analyze-launch-failure <evidence path>`;
 *   4. opens a NEW pane (the failed one is the evidence, D3) in the failed
 *      pane's cwd and types the command once. When the backend reports the
 *      prompt was not carried in argv (`promptCarried: false`), the one-line
 *      invocation is typed once after the Claude handshake is seen — never
 *      after a timer — and a handshake that is not seen is a failure.
 *
 * Every step's failure is a terminal outcome naming that step, so the button
 * can never sit on "Analyzer opened" for a pane that got no first turn.
 *
 * No React and no Tauri here: the page binds the deps (`useAnalyzeLaunch`).
 */

import type { AiLaunchCommand } from "./aiLaunchCommand";
import type { LaunchEvidence } from "./launchFailureApi";
import type { TerminalWriteResult } from "./terminalWriteResult";
import { samePath } from "./pathCompare";

/** The slash command the analyzer pane runs as its first turn (Phase 5). */
export const ANALYZE_LAUNCH_SLASH_COMMAND = "/analyze-launch-failure";

/** The analyzer's first message for an evidence file. */
export function analyzerFirstMessage(evidencePath: string): string {
  return `${ANALYZE_LAUNCH_SLASH_COMMAND} ${evidencePath}`;
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/** The tab fields placement reads. */
export interface AnalyzableTab {
  id: string;
  launchCommand?: string;
  remote?: unknown;
  sessionBacked?: boolean;
  type?: "terminal" | "plan";
}

/**
 * Where a pane offers "Analyze launch":
 * - `cluster`: beside Restart, for a pane with a recorded launch or in `error`;
 * - `overflow`: the zone's overflow menu only, for every other local pane (a
 *   hand-typed launch can be analyzed too);
 * - `none`: no local PTY to analyze (a remote mirror, a Conductor worker, a
 *   plan viewer, an empty zone).
 */
export function analyzeLaunchPlacement(
  tab: AnalyzableTab | undefined,
  state: string | undefined,
): "cluster" | "overflow" | "none" {
  if (!tab || tab.remote || tab.sessionBacked || tab.type === "plan") return "none";
  if (tab.launchCommand || state === "error") return "cluster";
  return "overflow";
}

/**
 * Whether the zone hover cluster carries one more button than a plain local
 * pane's: a remote tab's End button, or Analyze beside Restart. Such a pane
 * uses the remote-width reserve and collapse classes (`headerReserve.ts`),
 * which budget exactly one extra button.
 */
export function hoverClusterIsWide(tab: AnalyzableTab, state: string | undefined): boolean {
  return !!tab.remote || analyzeLaunchPlacement(tab, state) === "cluster";
}

// ---------------------------------------------------------------------------
// Account pick (D4)
// ---------------------------------------------------------------------------

/**
 * Pick the analyzer's account from `ranked` (already sorted best-first by
 * `compareByUsageHeadroom`). Returns null when there is no account at all.
 */
export function pickAnalyzerAccount(
  ranked: readonly { config_dir: string }[],
  failedConfigDir: string | undefined,
  launchAliases: Readonly<Record<string, string>> | undefined,
): string | null {
  const isFailed = (dir: string) => failedConfigDir !== undefined && samePath(dir, failedConfigDir);
  const hasAlias = (dir: string) => {
    const alias = launchAliases?.[dir];
    return typeof alias === "string" && alias.trim().length > 0;
  };
  const others = ranked.filter((a) => !isFailed(a.config_dir));
  const composed = others.find((a) => !hasAlias(a.config_dir));
  if (composed) return composed.config_dir;
  if (others.length > 0) return others[0].config_dir;
  return ranked[0]?.config_dir ?? null;
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/** The step a failed run stopped at — named in the operator's toast. */
export type AnalyzeStep =
  | "evidence-capture"
  | "no-account"
  | "command-build"
  | "pane-create"
  | "write"
  | "handshake";

export const ANALYZE_STEP_LABEL: Record<AnalyzeStep, string> = {
  "evidence-capture": "evidence capture failed",
  "no-account": "no logged-in account to run the analyzer",
  "command-build": "the launch command build was refused",
  "pane-create": "the analyzer pane could not be created",
  write: "the launch command could not be typed",
  handshake: "the analyzer's Claude did not start (handshake not seen)",
};

export type AnalyzeOutcome =
  | {
      ok: true;
      analyzerTabId: string;
      account: string;
      evidence: LaunchEvidence;
      promptCarried: boolean;
    }
  | { ok: false; step: AnalyzeStep; message: string; analyzerTabId?: string };

/** What the run needs from the failed pane. */
export interface FailedPane {
  id: string;
  launchCommand?: string;
  launchConfigDir?: string;
  claudeConfigDir?: string;
  claudeSessionId?: string;
  workingDir?: string;
  tenantId?: string;
}

export interface AnalyzeDeps {
  captureEvidence: (params: {
    terminalId: string;
    launchCommand: string | null;
    configDir: string | null;
    pinnedSessionId: string | null;
  }) => Promise<LaunchEvidence>;
  /** Spawn candidates, sorted best-first by usage headroom. */
  rankedAccounts: () => readonly { config_dir: string }[];
  launchAliases: () => Readonly<Record<string, string>> | undefined;
  buildCommand: (params: {
    configDir: string;
    isWindows: boolean;
    sessionId: string;
    initialPrompt: string;
  }) => Promise<AiLaunchCommand>;
  /** The page's create path; resolves the new tab id or null. */
  createPane: (title: string, cwd: string | undefined, tenantId?: string) => Promise<string | null>;
  write: (tabId: string, text: string) => Promise<TerminalWriteResult | void>;
  waitHandshake: (tabId: string) => Promise<"verified" | "failed" | "timeout">;
  /** Record the launched pane (session id, launch fields). */
  onLaunched?: (tabId: string, launch: AiLaunchCommand, configDir: string) => void;
  newSessionId: () => string;
  isWindows: boolean;
}

function describe(e: unknown): string {
  if (e instanceof Error) return e.message;
  return typeof e === "string" ? e : JSON.stringify(e);
}

function refused(result: TerminalWriteResult | void): string | null {
  if (result && result.success === false) return `${result.error}`;
  return null;
}

/** Run the analyzer open for `pane`. Never throws. */
export async function runAnalyzeLaunch(
  pane: FailedPane,
  deps: AnalyzeDeps,
): Promise<AnalyzeOutcome> {
  const failedConfigDir = pane.launchConfigDir ?? pane.claudeConfigDir;

  // 1. Evidence.
  let evidence: LaunchEvidence;
  try {
    evidence = await deps.captureEvidence({
      terminalId: pane.id,
      launchCommand: pane.launchCommand ?? null,
      configDir: failedConfigDir ?? null,
      pinnedSessionId: pane.claudeSessionId ?? null,
    });
  } catch (e) {
    return { ok: false, step: "evidence-capture", message: describe(e) };
  }

  // 2. Account.
  const account = pickAnalyzerAccount(deps.rankedAccounts(), failedConfigDir, deps.launchAliases());
  if (!account) {
    return { ok: false, step: "no-account", message: "no Claude account is configured" };
  }

  // 3. Command. Built BEFORE the pane exists, so a refusal strands no shell.
  const prompt = analyzerFirstMessage(evidence.path);
  let launch: AiLaunchCommand;
  try {
    launch = await deps.buildCommand({
      configDir: account,
      isWindows: deps.isWindows,
      sessionId: deps.newSessionId(),
      initialPrompt: prompt,
    });
  } catch (e) {
    return { ok: false, step: "command-build", message: describe(e) };
  }

  // 4. Pane + one write.
  const cwd = evidence.cwd ?? pane.workingDir;
  let tabId: string | null;
  try {
    tabId = await deps.createPane("analyze launch", cwd, pane.tenantId);
  } catch (e) {
    return { ok: false, step: "pane-create", message: describe(e) };
  }
  if (!tabId) {
    return { ok: false, step: "pane-create", message: "the terminal was not created" };
  }
  const commandRefusal = refused(await deps.write(tabId, `${launch.command}\r`));
  if (commandRefusal) {
    return { ok: false, step: "write", message: commandRefusal, analyzerTabId: tabId };
  }
  deps.onLaunched?.(tabId, launch, account);

  if (!launch.promptCarried) {
    const seen = await deps.waitHandshake(tabId);
    if (seen !== "verified") {
      return {
        ok: false,
        step: "handshake",
        message: `handshake ${seen === "timeout" ? "not seen before the timeout" : "failed"}; the analyzer prompt was not typed`,
        analyzerTabId: tabId,
      };
    }
    const promptRefusal = refused(await deps.write(tabId, `${prompt}\r`));
    if (promptRefusal) {
      return { ok: false, step: "write", message: promptRefusal, analyzerTabId: tabId };
    }
  }

  return {
    ok: true,
    analyzerTabId: tabId,
    account,
    evidence,
    promptCarried: launch.promptCarried,
  };
}

// ---------------------------------------------------------------------------
// In-flight tracking
// ---------------------------------------------------------------------------

/** Per-pane button status: absent = idle. */
export type AnalyzeStatus = "running" | "opened" | "error";

/**
 * Serialise runs per failed pane. `start` reports `running` SYNCHRONOUSLY
 * (before any await), so the button disables on the click itself, and refuses
 * a second start while one is in flight.
 */
export function createAnalyzeLaunchRunner(
  run: (pane: FailedPane) => Promise<AnalyzeOutcome>,
  onStatus: (paneId: string, status: AnalyzeStatus) => void,
) {
  const inFlight = new Set<string>();
  return {
    isRunning: (paneId: string) => inFlight.has(paneId),
    start(pane: FailedPane): Promise<AnalyzeOutcome> | null {
      if (inFlight.has(pane.id)) return null;
      inFlight.add(pane.id);
      onStatus(pane.id, "running");
      return run(pane)
        .catch(
          (e): AnalyzeOutcome => ({ ok: false, step: "evidence-capture", message: describe(e) }),
        )
        .then((outcome) => {
          inFlight.delete(pane.id);
          onStatus(pane.id, outcome.ok ? "opened" : "error");
          return outcome;
        });
    },
  };
}
