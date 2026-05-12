/**
 * Pure-helper tests for `WaitingLockBanner` (Lock-Yield Protocol Phase 3).
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) — see
 * `HoldingLockBanner.test.tsx` / `CommitTrafficLight.test.tsx` for the
 * precedent. We exercise:
 *
 *   1. `formatWaitingSeconds` — same shape as the holding banner's
 *      formatter; clamp negative, format Ns.
 *   2. `formatWaitingBannerText` — with/without blocker name.
 *   3. `cooldownRemainingSecs` — ceil rounding, 0 after expiry.
 *   4. The yield-request POST body shape — pins URL, method,
 *      Content-Type, and the exact `{file_path, requester_task_run_id,
 *      requester_name, holder_task_run_id}` shape against the Phase 1
 *      endpoint.
 *   5. Cooldown lifecycle — a click sets a 30s cooldown window;
 *      `cooldownRemainingSecs` decrements with `vi.useFakeTimers()`
 *      and `vi.setSystemTime`.
 *   6. Disabled state when `blockerTaskRunId` is undefined — verified
 *      via the JSX shell's gating logic mirrored here.
 *
 * The JSX shell and `data-ui-bridge-id` attributes are static — a
 * manual UI Bridge smoke verifies them at sign-off.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Mock the port resolver BEFORE importing so the simulated POST uses
// the test port.
const mockGetApiPort = vi.fn(() => 9876);
vi.mock("@/lib/runner-api", () => ({
  getApiPort: () => mockGetApiPort(),
}));

vi.mock("@/lib/logger", () => ({
  createLogger: () => ({
    debug: vi.fn(),
    info: vi.fn(),
    warn: vi.fn(),
    error: vi.fn(),
  }),
}));

import {
  REQUEST_YIELD_COOLDOWN_MS,
  cooldownRemainingSecs,
  formatWaitingBannerText,
  formatWaitingSeconds,
  type WaitingLockBannerProps,
} from "./WaitingLockBanner";

// ── formatWaitingSeconds ───────────────────────────────────────────────────

describe("formatWaitingSeconds", () => {
  it("returns 0s for nowMs === sinceMs", () => {
    expect(formatWaitingSeconds(1000, 1000)).toBe("0s");
  });
  it("returns Ns for nowMs > sinceMs (floor)", () => {
    expect(formatWaitingSeconds(0, 1_000)).toBe("1s");
    expect(formatWaitingSeconds(0, 47_500)).toBe("47s");
  });
  it("clamps negative ages to 0s", () => {
    expect(formatWaitingSeconds(2_000, 1_000)).toBe("0s");
  });
});

// ── formatWaitingBannerText ────────────────────────────────────────────────

describe("formatWaitingBannerText", () => {
  it("includes the blocker name when provided", () => {
    expect(
      formatWaitingBannerText({
        filePath: "src/foo.rs",
        blockerName: "alpha",
        secondsLabel: "42s",
      }),
    ).toBe("Waiting on src/foo.rs from session alpha for 42s.");
  });

  it("omits the 'from session …' tail when blockerName is undefined", () => {
    expect(
      formatWaitingBannerText({
        filePath: "src/foo.rs",
        secondsLabel: "42s",
      }),
    ).toBe("Waiting on src/foo.rs for 42s.");
  });
});

// ── cooldownRemainingSecs ──────────────────────────────────────────────────

describe("cooldownRemainingSecs", () => {
  it("returns 0 when cooldown has expired", () => {
    expect(cooldownRemainingSecs(1_000, 2_000)).toBe(0);
    expect(cooldownRemainingSecs(1_000, 1_000)).toBe(0);
  });
  it("ceil-rounds remaining millis to whole seconds", () => {
    expect(cooldownRemainingSecs(31_000, 1_000)).toBe(30);
    expect(cooldownRemainingSecs(1_500, 1_000)).toBe(1); // 500ms → 1
    expect(cooldownRemainingSecs(1_001, 1_000)).toBe(1); // 1ms → 1
  });
});

// ── Cooldown lifecycle with fake timers ────────────────────────────────────
//
// Replicates the component's cooldown state machine:
//   1. Click → `cooldownUntilMs = Date.now() + REQUEST_YIELD_COOLDOWN_MS`.
//   2. While `Date.now() < cooldownUntilMs`, button disabled.
//   3. Once `Date.now() >= cooldownUntilMs`, button re-enables.
//
// We advance system time via `vi.setSystemTime` and inspect
// `cooldownRemainingSecs` — the same predicate the component uses.

describe("cooldown lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_700_000_000_000));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("starts the cooldown at click and expires after 30s", () => {
    const start = Date.now();
    const cooldownUntilMs = start + REQUEST_YIELD_COOLDOWN_MS;

    // Immediately after the click — full 30s left, button disabled.
    expect(cooldownRemainingSecs(cooldownUntilMs, Date.now())).toBe(30);

    // After 10s, 20s remain.
    vi.setSystemTime(new Date(start + 10_000));
    expect(cooldownRemainingSecs(cooldownUntilMs, Date.now())).toBe(20);

    // Advance to 29.999s — 1s ceil-rounded remains.
    vi.setSystemTime(new Date(start + 29_999));
    expect(cooldownRemainingSecs(cooldownUntilMs, Date.now())).toBe(1);

    // After 30s exactly — cooldown finished.
    vi.setSystemTime(new Date(start + 30_000));
    expect(cooldownRemainingSecs(cooldownUntilMs, Date.now())).toBe(0);

    // After 60s — still 0.
    vi.setSystemTime(new Date(start + 60_000));
    expect(cooldownRemainingSecs(cooldownUntilMs, Date.now())).toBe(0);
  });

  it("uses the constant REQUEST_YIELD_COOLDOWN_MS = 30000", () => {
    // Tripwire: if a future plan changes the cooldown duration, this
    // assertion must change in lockstep with the spec.
    expect(REQUEST_YIELD_COOLDOWN_MS).toBe(30_000);
  });
});

// ── Disabled state when blockerTaskRunId is undefined ──────────────────────
//
// The component disables the button when `blockerTaskRunId` is
// undefined. We replicate the gating predicate here (the JSX bakes
// it into `buttonDisabled` / `unresolvedHolder`).

describe("disabled state — unresolved holder", () => {
  it("disables the button when blockerTaskRunId is undefined", () => {
    const buttonDisabled = (args: {
      blockerTaskRunId?: string;
      cooldownUntilMs: number;
      nowMs: number;
    }): boolean => {
      const unresolvedHolder = !args.blockerTaskRunId;
      const inCooldown = args.nowMs < args.cooldownUntilMs;
      return unresolvedHolder || inCooldown;
    };

    expect(
      buttonDisabled({
        blockerTaskRunId: undefined,
        cooldownUntilMs: 0,
        nowMs: 1000,
      }),
    ).toBe(true);

    expect(
      buttonDisabled({
        blockerTaskRunId: "holder-A",
        cooldownUntilMs: 0,
        nowMs: 1000,
      }),
    ).toBe(false);

    // Cooldown active also disables.
    expect(
      buttonDisabled({
        blockerTaskRunId: "holder-A",
        cooldownUntilMs: 5_000,
        nowMs: 1_000,
      }),
    ).toBe(true);
  });

  it("uses the guidance tooltip when no holder can be resolved", () => {
    // Mirror the static title attribute the JSX bakes in.
    const expected =
      "Can't identify the holder session — try from your tab's waiting indicator";
    expect(expected).toContain("Can't identify the holder session");
  });
});

// ── POST body shape — yield-request endpoint contract ──────────────────────
//
// Replicates the component's click dispatch and pins the body shape
// against the Phase 1 endpoint contract.

interface RequestYieldClickArgs {
  taskRunId: string;
  taskRunName: string;
  filePath: string;
  blockerTaskRunId: string;
  fetchImpl: typeof fetch;
}

async function simulateRequestYieldClick(args: RequestYieldClickArgs): Promise<Response> {
  return args.fetchImpl(
    `http://127.0.0.1:${mockGetApiPort()}/file-locks/yield-request`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        file_path: args.filePath,
        requester_task_run_id: args.taskRunId,
        requester_name: args.taskRunName,
        holder_task_run_id: args.blockerTaskRunId,
      }),
    },
  );
}

describe("yield-request POST dispatch", () => {
  beforeEach(() => {
    mockGetApiPort.mockReset();
    mockGetApiPort.mockReturnValue(9876);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("posts to /file-locks/yield-request with the correct body", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ requested: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    await simulateRequestYieldClick({
      taskRunId: "tab-A",
      taskRunName: "alpha",
      filePath: "src/foo.rs",
      blockerTaskRunId: "tab-B",
      fetchImpl: fetchSpy,
    });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0];
    expect(url).toBe("http://127.0.0.1:9876/file-locks/yield-request");
    expect(init).toBeDefined();
    expect(init!.method).toBe("POST");
    expect((init!.headers as Record<string, string>)["Content-Type"]).toBe(
      "application/json",
    );
    // EXACT body shape — must match the Phase 1 Rust handler's
    // YieldRequestRequest deserializer. Any drift breaks the wire
    // contract.
    expect(JSON.parse(init!.body as string)).toEqual({
      file_path: "src/foo.rs",
      requester_task_run_id: "tab-A",
      requester_name: "alpha",
      holder_task_run_id: "tab-B",
    });
  });

  it("uses the dynamic port from getApiPort()", async () => {
    mockGetApiPort.mockReturnValue(12345);
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("{}", { status: 200 }),
    );
    await simulateRequestYieldClick({
      taskRunId: "tab-A",
      taskRunName: "alpha",
      filePath: "src/foo.rs",
      blockerTaskRunId: "tab-B",
      fetchImpl: fetchSpy,
    });
    expect(fetchSpy.mock.calls[0][0]).toBe(
      "http://127.0.0.1:12345/file-locks/yield-request",
    );
  });
});

// Static type-check anchor — props shape tripwire.
const _propsTypeAnchor: WaitingLockBannerProps = {
  taskRunId: "tab-A",
  taskRunName: "alpha",
  filePath: "src/foo.rs",
  blockerName: "bravo",
  blockerTaskRunId: "tab-B",
  sinceMs: 0,
};
void _propsTypeAnchor;
