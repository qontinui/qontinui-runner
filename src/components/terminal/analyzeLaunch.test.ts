/**
 * "Analyze launch" (plan 2026-11-20-terminal-launch-failure-analyze-button,
 * Phase 3): the account pick (D4), the exact first message, the
 * promptCarried branch (D1 vet correction), every failure step's terminal
 * outcome, and the in-flight guard behind the disabled button.
 */

import { describe, it, expect, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@qontinui/ui-bridge", () => ({ useUIComponent: () => {} }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  PROMPT_SUBMIT_DELAY_MS,
  analyzeLaunchPlacement,
  analyzeOutcomeToast,
  analyzerFirstMessage,
  createAnalyzeLaunchRunner,
  hoverClusterIsWide,
  pickAnalyzerAccount,
  runAnalyzeLaunch,
  type AnalyzeDeps,
  type AnalyzeOutcome,
} from "./analyzeLaunch";
import { AnalyzeLaunchButton, analyzeButtonView } from "./AnalyzeLaunchButton";
import { AnalyzeLaunchContext, type AnalyzeLaunchApi } from "./useAnalyzeLaunch";

const acct = (config_dir: string) => ({ config_dir });

describe("pickAnalyzerAccount (D4)", () => {
  it("excludes the failed account even when it ranks best", () => {
    expect(
      pickAnalyzerAccount([acct("/h/.claude-a"), acct("/h/.claude-b")], "/h/.claude-a", {}),
    ).toBe("/h/.claude-b");
  });

  it("matches the failed account across separator and case differences", () => {
    expect(
      pickAnalyzerAccount(
        [acct("C:\\claude\\.claude-a"), acct("C:/claude/.claude-b")],
        "c:/claude/.claude-a/",
        {},
      ),
    ).toBe("C:/claude/.claude-b");
  });

  it("prefers a non-alias account over a better-ranked alias account", () => {
    expect(
      pickAnalyzerAccount([acct("/f"), acct("/alias"), acct("/plain")], "/f", { "/alias": "clh" }),
    ).toBe("/plain");
  });

  it("takes the best alias account when every other account is an alias", () => {
    expect(
      pickAnalyzerAccount([acct("/f"), acct("/a1"), acct("/a2")], "/f", { "/a1": "x", "/a2": "y" }),
    ).toBe("/a1");
  });

  it("ignores a blank alias entry", () => {
    expect(pickAnalyzerAccount([acct("/f"), acct("/a"), acct("/b")], "/f", { "/a": "  " })).toBe(
      "/a",
    );
  });

  it("falls back to the failed account when it is the only one", () => {
    expect(pickAnalyzerAccount([acct("/f")], "/f", {})).toBe("/f");
  });

  it("answers null with no account at all", () => {
    expect(pickAnalyzerAccount([], "/f", {})).toBeNull();
  });

  it("with no known failed account (a hand-typed launch) takes the best non-alias", () => {
    expect(pickAnalyzerAccount([acct("/a"), acct("/b")], undefined, { "/a": "x" })).toBe("/b");
  });
});

describe("analyzerFirstMessage", () => {
  it("is exactly the slash command and the evidence path", () => {
    expect(analyzerFirstMessage("/home/u/.qontinui/launch-evidence/t1-20261120T101500Z.md")).toBe(
      "/analyze-launch-failure /home/u/.qontinui/launch-evidence/t1-20261120T101500Z.md",
    );
  });
});

describe("analyzeLaunchPlacement", () => {
  it("puts it beside Restart for a recorded launch or an error pane", () => {
    expect(analyzeLaunchPlacement({ id: "t", launchCommand: "claude" }, "idle")).toBe("cluster");
    expect(analyzeLaunchPlacement({ id: "t" }, "error")).toBe("cluster");
  });
  it("puts it in the overflow menu for any other local pane", () => {
    expect(analyzeLaunchPlacement({ id: "t" }, "working")).toBe("overflow");
  });
  it("offers nothing for a remote, worker or plan tab, or no tab", () => {
    expect(analyzeLaunchPlacement({ id: "t", remote: {}, launchCommand: "c" }, "error")).toBe(
      "none",
    );
    expect(analyzeLaunchPlacement({ id: "t", sessionBacked: true }, "error")).toBe("none");
    expect(analyzeLaunchPlacement({ id: "t", type: "plan" }, "error")).toBe("none");
    expect(analyzeLaunchPlacement(undefined, "error")).toBe("none");
  });
  it("widens the hover cluster only when the button sits in it", () => {
    expect(hoverClusterIsWide({ id: "t", launchCommand: "c" }, "idle")).toBe(true);
    expect(hoverClusterIsWide({ id: "t" }, "idle")).toBe(false);
    expect(hoverClusterIsWide({ id: "t", remote: {} }, "idle")).toBe(true);
  });
});

// ---------------------------------------------------------------------------

const EVIDENCE = { path: "/ev/t1.md", cwd: "/work/repo", commandsProvisioned: true };

function makeDeps(over: Partial<AnalyzeDeps> = {}) {
  const writes: Array<[string, string]> = [];
  const order: string[] = [];
  const deps: AnalyzeDeps = {
    captureEvidence: vi.fn(async () => {
      order.push("capture");
      return EVIDENCE;
    }),
    rankedAccounts: () => [acct("/failed"), acct("/other")],
    launchAliases: () => ({}),
    buildCommand: vi.fn(async (p) => {
      order.push("build");
      return {
        command: `claude --session-id s -- '${p.initialPrompt}'`,
        pinnedSessionId: "s",
        promptCarried: true,
      };
    }),
    createPane: vi.fn(async () => {
      order.push("create");
      return "analyzer-1";
    }),
    write: vi.fn(async (tabId: string, text: string) => {
      order.push(`write:${text}`);
      writes.push([tabId, text]);
      return { success: true as const, bytes: text.length };
    }),
    waitHandshake: vi.fn(async () => {
      order.push("handshake");
      return "verified" as const;
    }),
    onLaunched: vi.fn(),
    newSessionId: () => "s",
    isWindows: false,
    sleep: vi.fn(async (ms: number) => {
      order.push(`sleep:${ms}`);
    }),
    ...over,
  };
  return { deps, writes, order };
}

const PANE = {
  id: "failed-1",
  launchCommand: "claude --session-id old",
  launchConfigDir: "/failed",
  claudeSessionId: "old",
  workingDir: "/fallback",
  tenantId: "ten",
};

describe("runAnalyzeLaunch", () => {
  it("captures evidence from the failed pane and builds with the exact prompt on another account", async () => {
    const { deps } = makeDeps();
    const out = await runAnalyzeLaunch(PANE, deps);
    expect(deps.captureEvidence).toHaveBeenCalledWith({
      terminalId: "failed-1",
      launchCommand: "claude --session-id old",
      configDir: "/failed",
      pinnedSessionId: "old",
    });
    expect(deps.buildCommand).toHaveBeenCalledWith({
      configDir: "/other",
      isWindows: false,
      sessionId: "s",
      initialPrompt: "/analyze-launch-failure /ev/t1.md",
    });
    expect(deps.createPane).toHaveBeenCalledWith("analyze launch", "/work/repo", "ten");
    expect(out).toMatchObject({ ok: true, analyzerTabId: "analyzer-1", account: "/other" });
  });

  it("promptCarried true: types the command once and nothing more, with no handshake wait", async () => {
    const { deps, writes } = makeDeps();
    await runAnalyzeLaunch(PANE, deps);
    expect(writes).toEqual([
      ["analyzer-1", "claude --session-id s -- '/analyze-launch-failure /ev/t1.md'\r"],
    ]);
    expect(deps.waitHandshake).not.toHaveBeenCalled();
  });

  it("promptCarried false: types the invocation after the handshake, then submits it with a separate CR after the paste delay", async () => {
    const { deps, writes, order } = makeDeps({
      buildCommand: vi.fn(async () => ({
        command: "clh",
        pinnedSessionId: null,
        promptCarried: false,
      })),
    });
    const out = await runAnalyzeLaunch(PANE, deps);
    // Text and CR are two writes, never one: one PTY read cycle holding both
    // is taken as a paste and the CR is eaten (session.rs POST_PASTE_DELAY).
    expect(writes).toEqual([
      ["analyzer-1", "clh\r"],
      ["analyzer-1", "/analyze-launch-failure /ev/t1.md"],
      ["analyzer-1", "\r"],
    ]);
    expect(order.slice(-4)).toEqual([
      "handshake",
      "write:/analyze-launch-failure /ev/t1.md",
      `sleep:${PROMPT_SUBMIT_DELAY_MS}`,
      "write:\r",
    ]);
    expect(PROMPT_SUBMIT_DELAY_MS).toBe(150);
    expect(out.ok).toBe(true);
  });

  it("opens in the provisioned cwd the backend reported, not the pane's workingDir", async () => {
    const { deps } = makeDeps();
    const out = await runAnalyzeLaunch(PANE, deps);
    expect(deps.createPane).toHaveBeenCalledWith("analyze launch", "/work/repo", "ten");
    expect(out.ok).toBe(true);
  });

  const notProvisionedCases: Array<[string, typeof EVIDENCE | (typeof EVIDENCE & { cwd: null })]> =
    [
      ["an unprovisioned cwd", { ...EVIDENCE, commandsProvisioned: false }],
      ["no resolvable cwd", { ...EVIDENCE, cwd: null, commandsProvisioned: false }],
      // A null cwd provisioned nothing, whatever the flag says.
      ["no resolvable cwd (flag set)", { ...EVIDENCE, cwd: null, commandsProvisioned: true }],
    ];
  for (const [name, evidence] of notProvisionedCases) {
    it(`${name}: no analyzer is created or launched, and the toast carries the evidence path`, async () => {
      const { deps, writes } = makeDeps({ captureEvidence: vi.fn(async () => evidence) });
      const out = await runAnalyzeLaunch(PANE, deps);
      expect(out).toMatchObject({ ok: false, step: "not-provisioned" });
      expect((out as { analyzerTabId?: string }).analyzerTabId).toBeUndefined();
      expect(deps.createPane).not.toHaveBeenCalled();
      expect(deps.buildCommand).not.toHaveBeenCalled();
      expect(deps.onLaunched).not.toHaveBeenCalled();
      expect(writes).toEqual([]);
      const toast = analyzeOutcomeToast(out);
      expect(toast?.type).toBe("error");
      expect(toast?.message).toContain(
        "the analyzer was not opened: the /analyze-launch-failure command could not be provisioned",
      );
      expect(toast?.message).toContain("run `/analyze-launch-failure /ev/t1.md` by hand");
      if (evidence.cwd === null) {
        expect(toast?.message).toContain("spawn working directory could not be resolved");
      } else {
        expect(toast?.message).toContain("provisioning into /work/repo");
      }
    });
  }

  it("a clean open has no toast", async () => {
    const { deps } = makeDeps();
    expect(analyzeOutcomeToast(await runAnalyzeLaunch(PANE, deps))).toBeNull();
  });

  const failures: Array<[string, Partial<AnalyzeDeps>, string, boolean]> = [
    [
      "evidence capture",
      { captureEvidence: vi.fn(async () => Promise.reject(new Error("disk full"))) },
      "evidence-capture",
      false,
    ],
    ["no account", { rankedAccounts: () => [] }, "no-account", false],
    [
      "command build refusal",
      { buildCommand: vi.fn(async () => Promise.reject(new Error("no account configured"))) },
      "command-build",
      false,
    ],
    ["pane create", { createPane: vi.fn(async () => null) }, "pane-create", false],
    [
      "write refusal",
      {
        write: vi.fn(async () => ({
          success: false as const,
          code: "TERMINAL_EXITED" as const,
          error: "TERMINAL_EXITED: gone",
          hint: "",
          terminalId: "analyzer-1",
        })),
      },
      "write",
      true,
    ],
    [
      "write that throws",
      { write: vi.fn(async () => Promise.reject(new Error("ipc down"))) },
      "write",
      true,
    ],
    [
      "launch record that throws",
      {
        onLaunched: vi.fn(() => {
          throw new Error("record failed");
        }),
      },
      "launch-record",
      true,
    ],
    [
      "handshake wait that throws",
      {
        buildCommand: vi.fn(async () => ({
          command: "clh",
          pinnedSessionId: null,
          promptCarried: false,
        })),
        waitHandshake: vi.fn(async () => Promise.reject(new Error("scrollback gone"))),
      },
      "handshake",
      true,
    ],
    [
      "prompt write refusal",
      {
        buildCommand: vi.fn(async () => ({
          command: "clh",
          pinnedSessionId: null,
          promptCarried: false,
        })),
        write: vi.fn(async (_id: string, text: string) =>
          text === "clh\r"
            ? { success: true as const, bytes: 4 }
            : {
                success: false as const,
                code: "TERMINAL_EXITED" as const,
                error: "TERMINAL_EXITED: gone",
                hint: "",
                terminalId: "analyzer-1",
              },
        ),
      },
      "prompt-write",
      true,
    ],
    [
      "prompt submit (CR) that throws",
      {
        buildCommand: vi.fn(async () => ({
          command: "clh",
          pinnedSessionId: null,
          promptCarried: false,
        })),
        write: vi.fn(async (_id: string, text: string) => {
          if (text === "\r") throw new Error("ipc down");
          return { success: true as const, bytes: text.length };
        }),
      },
      "prompt-submit",
      true,
    ],
    [
      "handshake timeout",
      {
        buildCommand: vi.fn(async () => ({
          command: "clh",
          pinnedSessionId: null,
          promptCarried: false,
        })),
        waitHandshake: vi.fn(async () => "timeout" as const),
      },
      "handshake",
      true,
    ],
    [
      "handshake failed",
      {
        buildCommand: vi.fn(async () => ({
          command: "clh",
          pinnedSessionId: null,
          promptCarried: false,
        })),
        waitHandshake: vi.fn(async () => "failed" as const),
      },
      "handshake",
      true,
    ],
  ];

  for (const [name, over, step, paneExists] of failures) {
    it(`a ${name} is a terminal error naming step "${step}"`, async () => {
      const { deps, writes } = makeDeps(over);
      const out = await runAnalyzeLaunch(PANE, deps);
      expect(out.ok).toBe(false);
      expect(out).toMatchObject({ step });
      expect(typeof (out as { message: string }).message).toBe("string");
      if (paneExists) expect((out as { analyzerTabId?: string }).analyzerTabId).toBe("analyzer-1");
      // A step before the pane strands no shell.
      else if (step !== "pane-create") expect(deps.createPane).not.toHaveBeenCalled();
      if (step === "handshake") {
        // The prompt line is never typed into a pane whose Claude was not seen.
        expect(writes).toEqual([["analyzer-1", "clh\r"]]);
      }
    });
  }
});

describe("createAnalyzeLaunchRunner (in-flight guard)", () => {
  it("reports running synchronously, refuses a second start, then settles", async () => {
    let resolveRun!: (o: AnalyzeOutcome) => void;
    const run = vi.fn(
      () =>
        new Promise<AnalyzeOutcome>((r) => {
          resolveRun = r;
        }),
    );
    const statuses: string[] = [];
    const runner = createAnalyzeLaunchRunner(run, (_id, s) => statuses.push(s));

    const first = runner.start({ id: "p" });
    expect(statuses).toEqual(["running"]);
    expect(runner.isRunning("p")).toBe(true);
    expect(runner.start({ id: "p" })).toBeNull();
    expect(run).toHaveBeenCalledTimes(1);

    resolveRun({ ok: false, step: "no-account", message: "x" });
    await first;
    expect(statuses).toEqual(["running", "error"]);
    expect(runner.isRunning("p")).toBe(false);
  });

  it("a step that throws inside a real run keeps its own step name and the analyzer pane id", async () => {
    const { deps } = makeDeps({ write: vi.fn(async () => Promise.reject(new Error("ipc down"))) });
    const statuses: string[] = [];
    const runner = createAnalyzeLaunchRunner(
      (pane) => runAnalyzeLaunch(pane, deps),
      (_id, s) => statuses.push(s),
    );
    const out = await runner.start(PANE);
    expect(out).toEqual({
      ok: false,
      step: "write",
      message: "ipc down",
      analyzerTabId: "analyzer-1",
    });
    expect(statuses).toEqual(["running", "error"]);
  });

  it("an unprovisioned analyzer settles to error, never opened", async () => {
    const { deps } = makeDeps({
      captureEvidence: vi.fn(async () => ({ ...EVIDENCE, commandsProvisioned: false })),
    });
    const statuses: string[] = [];
    const runner = createAnalyzeLaunchRunner(
      (pane) => runAnalyzeLaunch(pane, deps),
      (_id, s) => statuses.push(s),
    );
    await runner.start(PANE);
    expect(statuses).toEqual(["running", "error"]);
  });

  it("settles to opened on success", async () => {
    const statuses: string[] = [];
    const runner = createAnalyzeLaunchRunner(
      async () => ({
        ok: true,
        analyzerTabId: "a",
        account: "/o",
        evidence: EVIDENCE,
        promptCarried: true,
      }),
      (_id, s) => statuses.push(s),
    );
    await runner.start({ id: "p" });
    expect(statuses).toEqual(["running", "opened"]);
  });
});

describe("AnalyzeLaunchButton", () => {
  const render = (
    status: AnalyzeLaunchApi["statusOf"] extends (id: string) => infer S ? S : never,
  ) => {
    const api: AnalyzeLaunchApi = {
      analyze: () => {},
      statusOf: () => status,
      dismissLaunchFailure: () => {},
    };
    return renderToStaticMarkup(
      createElement(
        AnalyzeLaunchContext.Provider,
        { value: api },
        createElement(AnalyzeLaunchButton, { tabId: "t1", variant: "banner" }),
      ),
    );
  };

  it("is disabled with a spinner while in flight", () => {
    const html = render("running");
    expect(html).toContain('disabled=""');
    expect(html).toContain("animate-spin");
    expect(html).toContain('aria-busy="true"');
    expect(analyzeButtonView("running").disabled).toBe(true);
  });

  it("reads Analyzer opened on success and is enabled when idle", () => {
    expect(render("opened")).toContain("Analyzer opened");
    const idle = render(undefined);
    expect(idle).not.toContain('disabled=""');
    expect(idle).toContain('data-ui-bridge-id="terminal.analyze-launch"');
    expect(idle).toContain('data-terminal-id="t1"');
  });

  it("renders nothing outside a provider", () => {
    expect(
      renderToStaticMarkup(createElement(AnalyzeLaunchButton, { tabId: "t1", variant: "icon" })),
    ).toBe("");
  });
});
