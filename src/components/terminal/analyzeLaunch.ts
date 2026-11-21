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
 *      after a timer — and submitted with a separate CR; a handshake that is
 *      not seen is a failure.
 *   Before any of 2–4: evidence whose cwd was not provisioned (or that has no
 *   cwd) opens NO analyzer — `not-provisioned`, naming the evidence path so the
 *   operator can run the command by hand somewhere it resolves (D1).
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
  | "launch-record"
  | "handshake"
  | "prompt-write"
  | "prompt-submit"
  | "not-provisioned"
  | "unexpected";

export const ANALYZE_STEP_LABEL: Record<AnalyzeStep, string> = {
  "evidence-capture": "evidence capture failed",
  "no-account": "no logged-in account to run the analyzer",
  "command-build": "the launch command build was refused",
  "pane-create": "the analyzer pane could not be created",
  write: "the launch command could not be typed",
  "launch-record": "the analyzer pane's launch could not be recorded",
  handshake: "the analyzer's Claude did not start (handshake not seen)",
  "prompt-write": "the analyzer prompt could not be typed",
  "prompt-submit": "the analyzer prompt was typed but could not be submitted",
  "not-provisioned": `the analyzer was not opened: the ${ANALYZE_LAUNCH_SLASH_COMMAND} command could not be provisioned into the failed pane's spawn working directory`,
  unexpected: "the analyzer run failed unexpectedly",
};

/**
 * The pause between typing the fallback prompt and sending its submitting CR,
 * in ms. The same 150 ms, and the same reason, as `POST_PASTE_DELAY` in
 * `src-tauri/src/terminal/session.rs`: text and `\r` that reach Claude's TUI in
 * one read cycle are taken as one paste and the CR is eaten as paste-tail, so
 * the prompt sits unsubmitted. `TerminalSession::submit_prompt` writes the
 * text, sleeps, then writes the CR separately; no Tauri command exposes that
 * path to the frontend, so this run does the same two writes itself.
 */
export const PROMPT_SUBMIT_DELAY_MS = 150;

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
  /** Waits {@link PROMPT_SUBMIT_DELAY_MS}; defaults to a timer. */
  sleep?: (ms: number) => Promise<void>;
}

function describe(e: unknown): string {
  if (e instanceof Error) return e.message;
  return typeof e === "string" ? e : JSON.stringify(e);
}

function refused(result: TerminalWriteResult | void): string | null {
  if (result && result.success === false) return `${result.error}`;
  return null;
}

/**
 * Why no analyzer was opened, and what the operator can run by hand. The
 * evidence file exists either way, so the message carries its path.
 */
function notProvisionedMessage(evidence: LaunchEvidence): string {
  const why =
    evidence.cwd === null
      ? "the failed pane's spawn working directory could not be resolved, so the command was provisioned nowhere"
      : `provisioning into ${evidence.cwd} did not leave .claude/commands/analyze-launch-failure.md there`;
  return `${why}. Evidence was captured: run \`${analyzerFirstMessage(evidence.path)}\` by hand in a session where the command resolves`;
}

/** Run the analyzer open for `pane`. Never throws: every step names itself. */
export async function runAnalyzeLaunch(
  pane: FailedPane,
  deps: AnalyzeDeps,
): Promise<AnalyzeOutcome> {
  const failedConfigDir = pane.launchConfigDir ?? pane.claudeConfigDir;
  const sleep = deps.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));

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

  // 2. D1, decided BEFORE any pane exists: an analyzer whose first turn may
  // not resolve is never opened. No cwd means nothing was provisioned.
  if (!evidence.commandsProvisioned || evidence.cwd === null) {
    return { ok: false, step: "not-provisioned", message: notProvisionedMessage(evidence) };
  }
  const cwd = evidence.cwd;

  // 3. Account.
  let account: string | null;
  try {
    account = pickAnalyzerAccount(deps.rankedAccounts(), failedConfigDir, deps.launchAliases());
  } catch (e) {
    return { ok: false, step: "no-account", message: describe(e) };
  }
  if (!account) {
    return { ok: false, step: "no-account", message: "no Claude account is configured" };
  }

  // 4. Command. Built BEFORE the pane exists, so a refusal strands no shell.
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

  // 5. Pane, in the provisioned cwd, where `/analyze-launch-failure` resolves (D2).
  let tabId: string | null;
  try {
    tabId = await deps.createPane("analyze launch", cwd, pane.tenantId);
  } catch (e) {
    return { ok: false, step: "pane-create", message: describe(e) };
  }
  if (!tabId) {
    return { ok: false, step: "pane-create", message: "the terminal was not created" };
  }
  const analyzerTabId = tabId;
  const fail = (step: AnalyzeStep, message: string): AnalyzeOutcome => ({
    ok: false,
    step,
    message,
    analyzerTabId,
  });

  // 6. One write of the launch command, then record it.
  try {
    const commandRefusal = refused(await deps.write(analyzerTabId, `${launch.command}\r`));
    if (commandRefusal) return fail("write", commandRefusal);
  } catch (e) {
    return fail("write", describe(e));
  }
  try {
    deps.onLaunched?.(analyzerTabId, launch, account);
  } catch (e) {
    return fail("launch-record", describe(e));
  }

  // 7. An alias dropped the positional prompt: type it once the handshake is
  // seen, then submit with a SEPARATE CR (see PROMPT_SUBMIT_DELAY_MS).
  if (!launch.promptCarried) {
    let seen: "verified" | "failed" | "timeout";
    try {
      seen = await deps.waitHandshake(analyzerTabId);
    } catch (e) {
      return fail("handshake", `${describe(e)}; the analyzer prompt was not typed`);
    }
    if (seen !== "verified") {
      return fail(
        "handshake",
        `handshake ${seen === "timeout" ? "not seen before the timeout" : "failed"}; the analyzer prompt was not typed`,
      );
    }
    try {
      const promptRefusal = refused(await deps.write(analyzerTabId, prompt));
      if (promptRefusal) return fail("prompt-write", promptRefusal);
    } catch (e) {
      return fail("prompt-write", describe(e));
    }
    try {
      await sleep(PROMPT_SUBMIT_DELAY_MS);
      const submitRefusal = refused(await deps.write(analyzerTabId, "\r"));
      if (submitRefusal) return fail("prompt-submit", submitRefusal);
    } catch (e) {
      return fail("prompt-submit", describe(e));
    }
  }

  return {
    ok: true,
    analyzerTabId,
    account,
    evidence,
    promptCarried: launch.promptCarried,
  };
}

/**
 * The operator toast for an outcome, or null for a clean open. Every failure
 * names its step. `not-provisioned` opened no pane, so its toast gives the
 * `/analyze-launch-failure <evidence path>` line to run by hand (D1).
 */
export function analyzeOutcomeToast(
  outcome: AnalyzeOutcome,
): { message: string; type: "error" } | null {
  if (outcome.ok) return null;
  return {
    message: `Analyze launch: ${ANALYZE_STEP_LABEL[outcome.step]} — ${outcome.message}`,
    type: "error",
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
          // `run` never throws; a throw here is a defect, not a step's failure.
          (e): AnalyzeOutcome => ({ ok: false, step: "unexpected", message: describe(e) }),
        )
        .then((outcome) => {
          inFlight.delete(pane.id);
          onStatus(pane.id, outcome.ok ? "opened" : "error");
          return outcome;
        });
    },
  };
}
