/**
 * Fresh-launch verification and its banner (plan
 * 2026-11-20-terminal-launch-failure-analyze-button, Phase 4): a banner on a
 * handshake timeout, on a shim beacon, and on both; never on a seen handshake;
 * and an unread launch is UNKNOWN with no banner.
 */

import { describe, it, expect, vi } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: () => ({ ref: () => {} }),
  useUIComponent: () => {},
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./AdvisoryStack", () => ({
  AdvisorySlot: ({ children }: { children: unknown }) => children,
}));
vi.mock("./displayTitle", () => ({ TabTitle: ({ tab }: { tab: { id: string } }) => tab.id }));

import { detectClaudeHandshake, detectResumeFailure } from "./resumeVerification";
import {
  FRESH_LAUNCH_HANDSHAKE_PATTERNS,
  describeLaunchFailure,
  typeLaunchThenVerify,
  verifyFreshLaunch,
  type FreshLaunchDeps,
} from "./launchVerification";
import type { LaunchFailureRead } from "./launchFailureApi";
import { LaunchFailedBanner, failedLaunchTabs } from "./LaunchFailedBanner";
import type { TerminalTab } from "./useTerminalManager";

/** A clock the wait advances, so `waitedMs` is the wait actually spent. */
function deps(opts: {
  outcome: "verified" | "failed" | "timeout";
  tail: string | null;
  reads: LaunchFailureRead[];
  waitMs?: number;
}): FreshLaunchDeps {
  let t = 1_000;
  const reads = [...opts.reads];
  return {
    now: () => t,
    sleep: async () => {},
    readTail: async () => opts.tail,
    waitHandshake: async (tabId, readTail) => {
      await readTail(tabId);
      t += opts.waitMs ?? 15_000;
      return opts.outcome;
    },
    readFailure: async () => reads.shift() ?? reads.at(-1) ?? { state: "not-recorded" },
  };
}

describe("verifyFreshLaunch", () => {
  it("flags a handshake timeout over a readable scrollback, naming the wait used", async () => {
    const v = await verifyFreshLaunch(
      "t",
      deps({
        outcome: "timeout",
        tail: "$ claude\nbash: claude: not found",
        reads: [{ state: "not-recorded" }],
        waitMs: 15_200,
      }),
    );
    expect(v).toEqual({ kind: "failed", signals: { handshakeWaitedMs: 15_200 } });
    expect(describeLaunchFailure((v as { signals: object }).signals)).toBe(
      "handshake not seen in 15 s",
    );
  });

  it("treats a 'failed' wait the same as a timeout", async () => {
    const v = await verifyFreshLaunch("t", deps({ outcome: "failed", tail: "x", reads: [] }));
    expect(v.kind).toBe("failed");
  });

  it("flags a shim beacon even when the scrollback was never readable", async () => {
    const v = await verifyFreshLaunch(
      "t",
      deps({
        outcome: "timeout",
        tail: null,
        reads: [{ state: "recorded", kind: "resolve-failed" }],
      }),
    );
    expect(v).toEqual({ kind: "failed", signals: { beaconKind: "resolve-failed" } });
    expect(describeLaunchFailure((v as { signals: object }).signals)).toBe("shim: resolve-failed");
  });

  it("names both signals when both were observed", async () => {
    const v = await verifyFreshLaunch(
      "t",
      deps({
        outcome: "timeout",
        tail: "x",
        reads: [{ state: "recorded", kind: "exec-failed", detail: "ENOENT" }],
      }),
    );
    expect(v).toEqual({
      kind: "failed",
      signals: { handshakeWaitedMs: 15_000, beaconKind: "exec-failed", beaconDetail: "ENOENT" },
    });
    expect(describeLaunchFailure((v as { signals: object }).signals)).toBe(
      "handshake not seen in 15 s · shim: exec-failed (ENOENT)",
    );
  });

  it("never flags a seen handshake, even beside a beacon", async () => {
    const v = await verifyFreshLaunch(
      "t",
      deps({
        outcome: "verified",
        tail: "Claude Code v2",
        reads: [{ state: "recorded", kind: "exec-failed" }],
      }),
    );
    expect(v).toEqual({ kind: "started" });
  });

  it("is UNKNOWN when neither the scrollback nor the registry was read", async () => {
    const v = await verifyFreshLaunch(
      "t",
      deps({ outcome: "timeout", tail: null, reads: [{ state: "unread" }] }),
    );
    expect(v).toEqual({ kind: "unknown" });
  });

  it("is UNKNOWN when the registry answers not-recorded but the scrollback was unreadable", async () => {
    const v = await verifyFreshLaunch(
      "t",
      deps({ outcome: "timeout", tail: null, reads: [{ state: "not-recorded" }] }),
    );
    expect(v).toEqual({ kind: "unknown" });
  });

  it("a throwing registry read is unread, not a failure", async () => {
    const d = deps({ outcome: "timeout", tail: null, reads: [] });
    d.readFailure = async () => Promise.reject(new Error("ipc"));
    expect(await verifyFreshLaunch("t", d)).toEqual({ kind: "unknown" });
  });

  it("uses success markers only: a resume-failure phrase is not a fresh-launch verdict", () => {
    expect(detectResumeFailure("No conversation found", FRESH_LAUNCH_HANDSHAKE_PATTERNS)).toBe(
      false,
    );
    expect(detectClaudeHandshake("Claude Code v2.1.300", FRESH_LAUNCH_HANDSHAKE_PATTERNS)).toBe(
      true,
    );
    expect(detectClaudeHandshake("$ claude --session-id x", FRESH_LAUNCH_HANDSHAKE_PATTERNS)).toBe(
      false,
    );
  });
});

describe("typeLaunchThenVerify (launch menu)", () => {
  it("verifies after a successful write", async () => {
    const verify = vi.fn();
    const write = vi.fn(async () => ({ success: true as const, bytes: 7 }));
    await typeLaunchThenVerify("t1", "claude\r", write, verify);
    expect(write).toHaveBeenCalledWith("t1", "claude\r");
    expect(verify).toHaveBeenCalledWith("t1");
  });

  it("does not verify a refused write, so no false 'Claude did not start' banner", async () => {
    const verify = vi.fn();
    const result = await typeLaunchThenVerify(
      "t1",
      "claude\r",
      async () => ({
        success: false as const,
        code: "TERMINAL_EXITED" as const,
        error: "TERMINAL_EXITED: gone",
        hint: "",
        terminalId: "t1",
      }),
      verify,
    );
    expect(verify).not.toHaveBeenCalled();
    expect(result).toMatchObject({ success: false });
  });

  it("does not verify a write that throws", async () => {
    const verify = vi.fn();
    expect(
      await typeLaunchThenVerify(
        "t1",
        "claude\r",
        async () => Promise.reject(new Error("x")),
        verify,
      ),
    ).toBeNull();
    expect(verify).not.toHaveBeenCalled();
  });
});

describe("LaunchFailedBanner", () => {
  const tab = (over: Partial<TerminalTab>): TerminalTab =>
    ({ id: "t1", title: "t", pid: 1, isAlive: true, exitCode: null, ...over }) as TerminalTab;

  it("lists a timed-out, a beaconed and a both-signals launch", () => {
    const tabs = [
      tab({ id: "a", launchFailure: { handshakeWaitedMs: 15_000 } }),
      tab({ id: "b", launchFailure: { beaconKind: "resolve-failed" } }),
      tab({ id: "c", launchFailure: { handshakeWaitedMs: 15_000, beaconKind: "exec-failed" } }),
    ];
    expect(failedLaunchTabs(tabs).map((t) => t.id)).toEqual(["a", "b", "c"]);
    const html = renderToStaticMarkup(
      createElement(LaunchFailedBanner, { tabs, onDismiss: () => {} }),
    );
    expect(html).toContain('data-ui-bridge-id="terminal.launch-failed-banner"');
    expect(html).toContain("Claude did not start in 3 panes");
    expect(html).toContain("handshake not seen in 15 s");
    expect(html).toContain("shim: resolve-failed");
    expect(html).toContain('data-ui-bridge-id="terminal.launch-failed-dismiss"');
    expect(html).toContain('data-terminal-id="c"');
  });

  it("shows nothing for a started, an unread, or a dead pane", () => {
    const tabs = [
      tab({ id: "started" }),
      tab({ id: "empty", launchFailure: {} }),
      tab({ id: "dead", isAlive: false, launchFailure: { handshakeWaitedMs: 15_000 } }),
    ];
    expect(failedLaunchTabs(tabs)).toEqual([]);
    expect(
      renderToStaticMarkup(createElement(LaunchFailedBanner, { tabs, onDismiss: () => {} })),
    ).toBe("");
  });

  it("reads 'Claude did not start' for one pane", () => {
    const html = renderToStaticMarkup(
      createElement(LaunchFailedBanner, {
        tabs: [tab({ launchFailure: { beaconKind: "resolve-failed" } })],
        onDismiss: () => {},
      }),
    );
    expect(html).toContain("Claude did not start<");
  });
});
