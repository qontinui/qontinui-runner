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
  LATE_HANDSHAKE_INTERVAL_MS,
  LATE_HANDSHAKE_MAX_MS,
  typeLaunchThenVerify,
  verifyFreshLaunch,
  watchFreshLaunch,
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

describe("watchFreshLaunch (the banner's lifecycle)", () => {
  /**
   * The first wait times out over a readable scrollback (banner raised); the
   * late probes then read `lateTails` in order, one per probe.
   */
  function watch(lateTails: Array<string | null>, isActive: () => boolean = () => true) {
    const banner: Array<object | undefined> = [];
    const sleeps: number[] = [];
    const tails = [...lateTails];
    let firstWaitDone = false;
    const base = deps({ outcome: "timeout", tail: "$ claude", reads: [] });
    const run = watchFreshLaunch("t", {
      ...base,
      waitHandshake: async (tabId, readTail) => {
        const r = await base.waitHandshake!(tabId, readTail);
        firstWaitDone = true;
        return r;
      },
      readTail: async () => (firstWaitDone ? (tails.shift() ?? null) : "$ claude"),
      sleep: async (ms) => {
        if (firstWaitDone) sleeps.push(ms);
      },
      setFailure: (s) => banner.push(s),
      isActive,
      detectHandshake: (tail) => tail.includes("HANDSHAKE"),
    });
    return { run, banner, sleeps };
  }

  it("a late handshake retracts the banner", async () => {
    const { run, banner, sleeps } = watch(["$ claude", null, "... HANDSHAKE ..."]);
    expect(await run).toBe("retracted");
    expect(banner).toEqual([{ handshakeWaitedMs: 15_000 }, undefined]);
    expect(sleeps).toEqual([10_000, 10_000, 10_000]);
  });

  it("re-checks at most every 10 s for 5 min, then leaves the banner up", async () => {
    const { run, banner, sleeps } = watch([]);
    expect(await run).toBe("still-failed");
    expect(banner).toEqual([{ handshakeWaitedMs: 15_000 }]);
    expect(LATE_HANDSHAKE_INTERVAL_MS).toBe(10_000);
    expect(LATE_HANDSHAKE_MAX_MS).toBe(300_000);
    expect(sleeps).toHaveLength(30);
    expect(sleeps.every((ms) => ms === LATE_HANDSHAKE_INTERVAL_MS)).toBe(true);
  });

  it("stops when the tab closes or a newer launch takes over, without touching the banner", async () => {
    let active = true;
    let lateProbes = 0;
    const banner: Array<object | undefined> = [];
    const base = deps({ outcome: "timeout", tail: "$ claude", reads: [] });
    let firstWaitDone = false;
    const result = await watchFreshLaunch("t", {
      ...base,
      waitHandshake: async (tabId, readTail) => {
        const r = await base.waitHandshake!(tabId, readTail);
        firstWaitDone = true;
        return r;
      },
      readTail: async () => {
        if (!firstWaitDone) return "$ claude";
        lateProbes += 1;
        if (lateProbes === 2) active = false; // the tab closed mid-probe
        return lateProbes === 2 ? "HANDSHAKE" : null;
      },
      setFailure: (s) => banner.push(s),
      isActive: () => active,
      detectHandshake: (tail) => tail.includes("HANDSHAKE"),
    });
    expect(result).toBe("abandoned");
    expect(lateProbes).toBe(2);
    expect(banner).toEqual([{ handshakeWaitedMs: 15_000 }]);
  });

  it("a handshake seen in the first wait clears any earlier banner", async () => {
    const banner: Array<object | undefined> = [];
    const result = await watchFreshLaunch("t", {
      ...deps({ outcome: "verified", tail: "x", reads: [] }),
      setFailure: (s) => banner.push(s),
      isActive: () => true,
    });
    expect(result).toBe("started");
    expect(banner).toEqual([undefined]);
  });

  it("an unread launch leaves the banner alone", async () => {
    const banner: Array<object | undefined> = [];
    const result = await watchFreshLaunch("t", {
      ...deps({ outcome: "timeout", tail: null, reads: [] }),
      setFailure: (s) => banner.push(s),
      isActive: () => true,
    });
    expect(result).toBe("unknown");
    expect(banner).toEqual([]);
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
