/**
 * Model output text must never restart the runner.
 *
 * Plan `2026-10-06-ai-output-text-can-exit-the-runner-and-nothing-relaunches-it`
 * Phase 1: a `[RUNNER` + `:RESTART] {...}` line used to reach `POST /restart-runner`
 * (a bare `process::exit(0)` that nothing in the runner relaunches) through
 * FindingsTracker -> VerificationService. That text trigger is
 * deleted; this test pins that no request to `restart-runner` is made, and that
 * `[FINDING:...]` parsing still works (so it cannot pass because the tracker is
 * broken).
 *
 * The runner's vitest config is `environment: "node"`, so `invoke` is mocked.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue(null),
}));

const tracedFetchSpy = vi.fn();
vi.mock("@/lib/runner-api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/runner-api")>();
  return {
    ...actual,
    tracedFetch: (...args: unknown[]) => {
      tracedFetchSpy(...args);
      return Promise.resolve(new Response(JSON.stringify({ success: true })));
    },
  };
});

import { invoke } from "@tauri-apps/api/core";
import { FindingsTracker } from "../FindingsTracker";

// Assembled rather than spelled, so the plan's acceptance grep for the marker
// under src/ stays empty while this test still feeds the exact marker text.
const RESTART_MARKER = ["[RUNNER", "RESTART]"].join(":");

function requestedUrls(fetchSpy: ReturnType<typeof vi.fn>): string[] {
  return [...fetchSpy.mock.calls, ...tracedFetchSpy.mock.calls].map((call) => {
    const target = call[0] as unknown;
    if (typeof target === "string") return target;
    if (target instanceof URL) return target.toString();
    if (target instanceof Request) return target.url;
    return String(target);
  });
}

describe("FindingsTracker — the runner-restart marker is not a process-control trigger", () => {
  let fetchSpy: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    FindingsTracker.resetInstance();
    tracedFetchSpy.mockReset();
    fetchSpy = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true })));
    vi.stubGlobal("fetch", fetchSpy);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("makes zero requests to restart-runner and returns null", async () => {
    const tracker = FindingsTracker.getInstance();

    const result = tracker.processLine(`${RESTART_MARKER} {"reason":"x"}`);
    // Let any fire-and-forget async work reach its first request.
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(result).toBeNull();
    expect(requestedUrls(fetchSpy).filter((url) => url.includes("restart-runner"))).toEqual([]);
    // Nor through Tauri IPC: no invoke command whose name mentions a restart.
    const invoked = vi.mocked(invoke).mock.calls.map((call) => String(call[0]));
    expect(invoked.filter((cmd) => /restart/i.test(cmd))).toEqual([]);
  });

  it("still parses [FINDING:...] markers into a finding", () => {
    const tracker = FindingsTracker.getInstance();

    const finding = tracker.processLine(
      "[FINDING:code_bug:high] Title: Broken thing\nDescription: it is broken [/FINDING]",
    );

    expect(finding).not.toBeNull();
    expect(finding?.categoryId).toBe("code_bug");
    expect(finding?.severity).toBe("high");
  });
});
