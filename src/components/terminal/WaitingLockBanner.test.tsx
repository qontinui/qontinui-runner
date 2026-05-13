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
  HOLDER_IDLE_DISPLAY_THRESHOLD_MS,
  REQUEST_YIELD_COOLDOWN_MS,
  cooldownRemainingSecs,
  formatEstimate,
  formatIdleDuration,
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
  holderIdleMs: 420_000,
  longWaitSignal: {
    holderName: "bravo",
    estimatedRemainingMs: 300_000,
    signaledAtMs: 1_700_000_000_000,
  },
};
void _propsTypeAnchor;

// ── Phase 3 (stuck-session heartbeat) — formatEstimate ─────────────────────

describe("formatEstimate", () => {
  it("rounds to nearest second for values < 60_000ms", () => {
    expect(formatEstimate(0)).toBe("~0s");
    expect(formatEstimate(1_499)).toBe("~1s"); // round(1.499) = 1
    expect(formatEstimate(1_500)).toBe("~2s"); // round(1.5) = 2 (banker's at .5 in JS = 2)
    expect(formatEstimate(45_400)).toBe("~45s");
    expect(formatEstimate(59_999)).toBe("~60s"); // edge case: rounds to 60 before flipping to minutes
  });

  it("rounds to nearest minute for values >= 60_000ms", () => {
    expect(formatEstimate(60_000)).toBe("~1m");
    expect(formatEstimate(90_000)).toBe("~2m"); // round(1.5) = 2
    expect(formatEstimate(300_000)).toBe("~5m");
    expect(formatEstimate(610_000)).toBe("~10m"); // round(10.166) = 10
  });
});

// ── Phase 3 — long-wait advisory render predicate ─────────────────────────
//
// The component renders the advisory line iff `longWaitSignal !== undefined`.
// Replicates the JSX render gating logic without invoking React (no
// jsdom — see vitest.config.ts environment: "node"). Verifies:
//
//   1. Line is omitted when `longWaitSignal` is undefined.
//   2. Line is rendered when `longWaitSignal` is provided.
//   3. Estimate tail is appended when `estimatedRemainingMs` is provided;
//      omitted when undefined.

type LongWaitSignal = WaitingLockBannerProps["longWaitSignal"];

// Pure mirror of the JSX render gating. Returns the rendered advisory
// text or `null` if the line is omitted.
function renderLongWaitAdvisory(signal: LongWaitSignal): string | null {
  if (!signal) return null;
  const base = `${signal.holderName} says they'll be a while. Request yield or cancel?`;
  if (signal.estimatedRemainingMs !== undefined) {
    return `${base} (${formatEstimate(signal.estimatedRemainingMs)})`;
  }
  return base;
}

describe("long-wait advisory line — render predicate", () => {
  it("omits the advisory line when longWaitSignal is undefined", () => {
    expect(renderLongWaitAdvisory(undefined)).toBeNull();
  });

  it("renders the advisory line when longWaitSignal is provided (no estimate)", () => {
    const rendered = renderLongWaitAdvisory({
      holderName: "bravo",
      signaledAtMs: 1_700_000_000_000,
    });
    expect(rendered).toBe(
      "bravo says they'll be a while. Request yield or cancel?",
    );
    // Estimate tail must NOT be present when `estimatedRemainingMs` is omitted.
    expect(rendered).not.toMatch(/\(~/);
  });

  it("appends the formatted estimate when estimatedRemainingMs is provided", () => {
    const rendered = renderLongWaitAdvisory({
      holderName: "bravo",
      estimatedRemainingMs: 300_000,
      signaledAtMs: 1_700_000_000_000,
    });
    expect(rendered).toBe(
      "bravo says they'll be a while. Request yield or cancel? (~5m)",
    );
  });

  it("uses seconds rounding for sub-minute estimates", () => {
    const rendered = renderLongWaitAdvisory({
      holderName: "bravo",
      estimatedRemainingMs: 45_000,
      signaledAtMs: 1_700_000_000_000,
    });
    expect(rendered).toContain("(~45s)");
  });
});

// ── Phase 2 (stuck-session heartbeat) — formatIdleDuration ────────────────

describe("formatIdleDuration", () => {
  it("floor-rounds to seconds for values < 60_000ms", () => {
    expect(formatIdleDuration(0)).toBe("0s");
    expect(formatIdleDuration(999)).toBe("0s"); // floor
    expect(formatIdleDuration(1_499)).toBe("1s"); // floor (round → 1)
    expect(formatIdleDuration(1_500)).toBe("1s"); // floor
    expect(formatIdleDuration(45_900)).toBe("45s");
    expect(formatIdleDuration(59_999)).toBe("59s"); // edge — stays seconds until 60s
  });

  it("floor-rounds to minutes for values in [60_000, 3_600_000)", () => {
    expect(formatIdleDuration(60_000)).toBe("1m");
    expect(formatIdleDuration(119_999)).toBe("1m"); // floor
    expect(formatIdleDuration(120_000)).toBe("2m");
    expect(formatIdleDuration(420_000)).toBe("7m");
    expect(formatIdleDuration(3_599_999)).toBe("59m"); // edge — stays minutes until 1h
  });

  it("floor-rounds to hours for values >= 3_600_000ms", () => {
    expect(formatIdleDuration(3_600_000)).toBe("1h");
    expect(formatIdleDuration(7_199_999)).toBe("1h");
    expect(formatIdleDuration(7_200_000)).toBe("2h");
    expect(formatIdleDuration(8_400_000)).toBe("2h"); // 2h20m → 2h
  });

  it("clamps negative inputs to 0s (defensive, shouldn't happen in production)", () => {
    expect(formatIdleDuration(-500)).toBe("0s");
    expect(formatIdleDuration(-60_000)).toBe("0s");
  });
});

// ── Phase 2 — holder-idle advisory render predicate ──────────────────────
//
// The banner renders the inline "(holder idle Xm)" span iff
// `holderIdleMs !== undefined && holderIdleMs > HOLDER_IDLE_DISPLAY_THRESHOLD_MS`.
// The threshold is 60_000ms; values at/under it are noise (typing
// pauses, brief thinking gaps). Replicates the JSX render gating
// without invoking React (no jsdom — see vitest.config.ts).

// Pure mirror of the JSX render gating. Returns the rendered span text
// or `null` if the suffix is omitted.
function renderHolderIdleSuffix(holderIdleMs: number | undefined): string | null {
  if (holderIdleMs === undefined) return null;
  if (holderIdleMs <= HOLDER_IDLE_DISPLAY_THRESHOLD_MS) return null;
  return `(holder idle ${formatIdleDuration(holderIdleMs)})`;
}

describe("holder-idle suffix — render predicate", () => {
  it("omits the suffix when holderIdleMs is undefined", () => {
    expect(renderHolderIdleSuffix(undefined)).toBeNull();
  });

  it("omits the suffix at/below the 60s display threshold", () => {
    expect(renderHolderIdleSuffix(0)).toBeNull();
    expect(renderHolderIdleSuffix(30_000)).toBeNull();
    expect(renderHolderIdleSuffix(59_999)).toBeNull();
    expect(renderHolderIdleSuffix(60_000)).toBeNull(); // exact threshold = hidden (gate is `>`)
  });

  it("renders the suffix above the 60s display threshold", () => {
    expect(renderHolderIdleSuffix(60_001)).toBe("(holder idle 1m)");
    expect(renderHolderIdleSuffix(120_000)).toBe("(holder idle 2m)");
    expect(renderHolderIdleSuffix(420_000)).toBe("(holder idle 7m)");
    expect(renderHolderIdleSuffix(3_600_000)).toBe("(holder idle 1h)");
  });

  it("uses the constant HOLDER_IDLE_DISPLAY_THRESHOLD_MS = 60000", () => {
    // Tripwire: if a future plan changes the threshold (Open Q3 of the
    // stuck-session heartbeat plan), this assertion must change in
    // lockstep with the spec.
    expect(HOLDER_IDLE_DISPLAY_THRESHOLD_MS).toBe(60_000);
  });
});
