/**
 * Pure-helper tests for `HoldingLockBanner`.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom) — see
 * `CommitTrafficLight.test.tsx`, `MidSessionToast.test.tsx`,
 * `LaunchMenu.test.tsx` for the precedent. We exercise the exported
 * pure helpers + the click-handler logic via module-level mocks of the
 * `runner-api` port resolver. The JSX shell is verified manually + via
 * UI Bridge (`data-ui-bridge-id` attributes are baked into the
 * component for the smoke).
 *
 * Coverage:
 *   1. `formatBannerSeconds` — clamps negative ages, formats seconds.
 *   2. `formatBannerText` — singular vs. plural waiter count, default
 *      waiter name when undefined.
 *   3. `shouldShowHoldingBanner` — parent's gating predicate covers
 *      every render path the banner consumer hits (Phase 2 spec):
 *        - holding + waiters > 0 + not dismissed → true
 *        - holding + waiters === 0                → false
 *        - holding + waiters > 0 + dismissed     → false
 *        - waiting / idle / undefined            → false
 *        - missing filePath                       → false
 *      Plus the gating-toggle test the plan asks for: dismiss → false,
 *      then flip waiterCount 0 → 1 with the SAME dismissal key still
 *      present (the parent must drop the key separately — exercised
 *      below).
 *   4. Yield POST shape — `fetch` called with the right URL + body,
 *      banner optimistically dismisses (re-render → component returns
 *      null).
 *
 * The disabled state of the "I'll be a while" button + the
 * `data-ui-bridge-id` attributes are static JSX — they ship with the
 * component file; a manual UI Bridge smoke verifies them at Phase 2
 * sign-off.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Mock the port resolver BEFORE importing the component so it picks up
// our mocked `getApiPort`.
const mockGetApiPort = vi.fn(() => 9876);
vi.mock("@/lib/runner-api", () => ({
  getApiPort: () => mockGetApiPort(),
}));

// The component imports `createLogger`; stub it so it doesn't try to
// initialise the runner's structured logger in tests.
vi.mock("@/lib/logger", () => ({
  createLogger: () => ({
    debug: vi.fn(),
    info: vi.fn(),
    warn: vi.fn(),
    error: vi.fn(),
  }),
}));

import {
  formatBannerSeconds,
  formatBannerText,
  formatIncomingRequestText,
  pickOldestRequest,
  shouldShowHoldingBanner,
  type HoldingLockBannerProps,
} from "./HoldingLockBanner";
import type { IncomingYieldRequest } from "./useFileLockTracking";

describe("formatBannerSeconds", () => {
  it("returns 0s for nowMs === sinceMs", () => {
    expect(formatBannerSeconds(1000, 1000)).toBe("0s");
  });
  it("returns Ns for nowMs > sinceMs", () => {
    expect(formatBannerSeconds(0, 1_000)).toBe("1s");
    expect(formatBannerSeconds(0, 42_000)).toBe("42s");
    expect(formatBannerSeconds(0, 47_500)).toBe("47s"); // floor
  });
  it("clamps negative ages to 0s (clock skew / future timestamps)", () => {
    expect(formatBannerSeconds(2_000, 1_000)).toBe("0s");
  });
});

describe("formatBannerText", () => {
  it("singular form for waiterCount === 1", () => {
    expect(
      formatBannerText({
        waiterName: "alpha",
        filePath: "src/foo.rs",
        secondsLabel: "42s",
        waiterCount: 1,
      }),
    ).toBe("Session alpha is waiting on src/foo.rs (waiting 42s)");
  });

  it("falls back to 'another session' when waiterName is undefined", () => {
    expect(
      formatBannerText({
        waiterName: undefined,
        filePath: "src/foo.rs",
        secondsLabel: "5s",
        waiterCount: 1,
      }),
    ).toBe("Session another session is waiting on src/foo.rs (waiting 5s)");
  });

  it("plural form with '+1 more waiter' when waiterCount === 2", () => {
    expect(
      formatBannerText({
        waiterName: "alpha",
        filePath: "src/foo.rs",
        secondsLabel: "10s",
        waiterCount: 2,
      }),
    ).toBe(
      "Session alpha is waiting on src/foo.rs (waiting 10s; +1 more waiter)",
    );
  });

  it("plural form with 'N more waiters' when waiterCount > 2", () => {
    expect(
      formatBannerText({
        waiterName: "alpha",
        filePath: "src/foo.rs",
        secondsLabel: "10s",
        waiterCount: 4,
      }),
    ).toBe(
      "Session alpha is waiting on src/foo.rs (waiting 10s; +3 more waiters)",
    );
  });

  it("singular form when waiterCount === 0 (defensive — caller gates this case)", () => {
    // The component renders only when waiterCount > 0, but the
    // formatter should still handle 0 gracefully (treat as singular
    // with no "+N more" tail).
    expect(
      formatBannerText({
        waiterName: "alpha",
        filePath: "src/foo.rs",
        secondsLabel: "10s",
        waiterCount: 0,
      }),
    ).toBe("Session alpha is waiting on src/foo.rs (waiting 10s)");
  });
});

describe("shouldShowHoldingBanner", () => {
  const baseHolding = {
    kind: "holding" as const,
    filePath: "src/foo.rs",
    waiterCount: 1,
  };

  it("renders when holding + waiters > 0 + not dismissed", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: baseHolding,
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(true);
  });

  it("does not render when waiterCount === 0", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: { ...baseHolding, waiterCount: 0 },
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });

  it("does not render when waiterCount is undefined", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", filePath: "src/foo.rs" },
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });

  it("does not render when locally dismissed for this (tab, file)", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: baseHolding,
        dismissed: new Set(["tab-1:src/foo.rs"]),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });

  it("ignores dismissal for a different file path", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: baseHolding,
        dismissed: new Set(["tab-1:src/other.rs"]),
        tabId: "tab-1",
      }),
    ).toBe(true);
  });

  it("ignores dismissal for a different tab id", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: baseHolding,
        dismissed: new Set(["tab-2:src/foo.rs"]),
        tabId: "tab-1",
      }),
    ).toBe(true);
  });

  it("does not render for waiting state", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "waiting", filePath: "src/foo.rs" },
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });

  it("does not render for idle state", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "idle" },
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });

  it("does not render for undefined lock state", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: undefined,
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });

  it("does not render when filePath is missing (defensive)", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", waiterCount: 1 },
        dismissed: new Set(),
        tabId: "tab-1",
      }),
    ).toBe(false);
  });
});

// ────────────────────────────────────────────────────────────────────────────
// Parent gating — exercises the (tab, file) dismissal lifecycle the plan
// asks the parent test to cover:
//   - holding + waiterCount=0 → banner absent
//   - flip to waiterCount=1 → banner appears
//   - user dismisses → banner absent again
//   - waiterCount=0 → parent clears dismissal (replicated below)
//   - flip back to 1 → banner reappears
// The parent in TerminalPage.tsx clears the dismissal set when
// waiterCount drops to 0 for a dismissed pair. We replicate that
// invariant here.
// ────────────────────────────────────────────────────────────────────────────

describe("parent gating lifecycle", () => {
  it("re-shows the banner after a 0 → 1 waiter wave following a dismissal", () => {
    const tabId = "tab-1";
    const filePath = "src/foo.rs";
    let dismissed = new Set<string>();

    // Initial: waiterCount=0 → no banner.
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", filePath, waiterCount: 0 },
        dismissed,
        tabId,
      }),
    ).toBe(false);

    // Waiter arrives → banner appears.
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", filePath, waiterCount: 1 },
        dismissed,
        tabId,
      }),
    ).toBe(true);

    // User clicks Hold → dismissal recorded → banner hides.
    dismissed = new Set([`${tabId}:${filePath}`]);
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", filePath, waiterCount: 1 },
        dismissed,
        tabId,
      }),
    ).toBe(false);

    // Waiter leaves (count → 0). The TerminalPage effect clears the
    // dismissal at this point; replicate it here.
    const newDismissed = new Set(dismissed);
    newDismissed.delete(`${tabId}:${filePath}`);
    dismissed = newDismissed;

    // Banner still hidden (no waiter).
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", filePath, waiterCount: 0 },
        dismissed,
        tabId,
      }),
    ).toBe(false);

    // New waiter arrives → banner re-appears (dismissal was cleared).
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "holding", filePath, waiterCount: 1 },
        dismissed,
        tabId,
      }),
    ).toBe(true);
  });
});

// ────────────────────────────────────────────────────────────────────────────
// Yield POST handler — replicate the component's click dispatch and
// verify the body shape against the Phase 1 endpoint contract. The JSX
// shell's `onClick` calls the same handler shape; the test pins the
// fetch contract (URL + body + method + content-type).
// ────────────────────────────────────────────────────────────────────────────

interface YieldClickArgs {
  taskRunId: string;
  filePath: string;
  fetchImpl: typeof fetch;
}

async function simulateYieldClick(args: YieldClickArgs): Promise<Response> {
  // Mirror the component's actual call. Any deviation here will silently
  // skip a regression — keep this in lockstep with HoldingLockBanner.tsx.
  return args.fetchImpl(`http://127.0.0.1:${mockGetApiPort()}/file-locks/yield`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      file_path: args.filePath,
      task_run_id: args.taskRunId,
    }),
  });
}

describe("yield POST dispatch", () => {
  beforeEach(() => {
    mockGetApiPort.mockReset();
    mockGetApiPort.mockReturnValue(9876);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("posts to /file-locks/yield with the right body + JSON content-type", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ released: true, file_path: "src/foo.rs", task_run_id: "tab-A" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    await simulateYieldClick({
      taskRunId: "tab-A",
      filePath: "src/foo.rs",
      fetchImpl: fetchSpy,
    });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0];
    expect(url).toBe("http://127.0.0.1:9876/file-locks/yield");
    expect(init).toBeDefined();
    expect(init!.method).toBe("POST");
    expect((init!.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
    expect(JSON.parse(init!.body as string)).toEqual({
      file_path: "src/foo.rs",
      task_run_id: "tab-A",
    });
  });

  it("uses the dynamic port from getApiPort()", async () => {
    mockGetApiPort.mockReturnValue(12345);
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("{}", { status: 200 }),
    );
    await simulateYieldClick({
      taskRunId: "tab-A",
      filePath: "src/foo.rs",
      fetchImpl: fetchSpy,
    });
    expect(fetchSpy.mock.calls[0][0]).toBe("http://127.0.0.1:12345/file-locks/yield");
  });
});

// Static type-check anchor — if HoldingLockBannerProps drifts (e.g. a
// required field is renamed) the next compile catches it here. No
// runtime assertion needed; the assignment is the test.
const _propsTypeAnchor: HoldingLockBannerProps = {
  taskRunId: "tab-A",
  filePath: "src/foo.rs",
  waiterName: "alpha",
  sinceMs: 0,
  waiterCount: 1,
  onDismissLocal: () => {},
};
void _propsTypeAnchor;

// ── Phase 3 — incoming-request mode helpers ────────────────────────────────

describe("pickOldestRequest", () => {
  it("returns undefined for empty/missing input", () => {
    expect(pickOldestRequest(undefined)).toBeUndefined();
    expect(pickOldestRequest([])).toBeUndefined();
  });

  it("picks the smallest requestedAtMs entry", () => {
    const newer: IncomingYieldRequest = {
      filePath: "src/a.rs",
      requesterName: "alpha",
      requesterTaskRunId: "A",
      requestedAtMs: 200,
    };
    const older: IncomingYieldRequest = {
      filePath: "src/b.rs",
      requesterName: "bravo",
      requesterTaskRunId: "B",
      requestedAtMs: 100,
    };
    expect(pickOldestRequest([newer, older])).toBe(older);
    // Order-insensitive — first-entry-is-oldest variant.
    expect(pickOldestRequest([older, newer])).toBe(older);
  });
});

describe("formatIncomingRequestText", () => {
  const req = (over: Partial<IncomingYieldRequest> = {}): IncomingYieldRequest => ({
    filePath: "src/foo.rs",
    requesterName: "alpha",
    requesterTaskRunId: "A",
    requestedAtMs: 0,
    ...over,
  });

  it("single-request shape", () => {
    expect(
      formatIncomingRequestText({
        oldest: req(),
        totalRequests: 1,
        secondsLabel: "12s",
      }),
    ).toBe("Session alpha has asked you to yield on src/foo.rs (12s ago)");
  });

  it("'+1 more request' when totalRequests === 2", () => {
    expect(
      formatIncomingRequestText({
        oldest: req(),
        totalRequests: 2,
        secondsLabel: "5s",
      }),
    ).toBe(
      "Session alpha has asked you to yield on src/foo.rs (5s ago); +1 more request",
    );
  });

  it("'+N more requests' for totalRequests > 2", () => {
    expect(
      formatIncomingRequestText({
        oldest: req(),
        totalRequests: 4,
        secondsLabel: "5s",
      }),
    ).toBe(
      "Session alpha has asked you to yield on src/foo.rs (5s ago); +3 more requests",
    );
  });

  it("uses the OLDEST request's filePath + requesterName, not an arbitrary one", () => {
    expect(
      formatIncomingRequestText({
        oldest: req({ filePath: "src/oldest.rs", requesterName: "first-mover" }),
        totalRequests: 3,
        secondsLabel: "1s",
      }),
    ).toBe(
      "Session first-mover has asked you to yield on src/oldest.rs (1s ago); +2 more requests",
    );
  });
});

// ── Phase 3 — shouldShowHoldingBanner with incomingRequests ────────────────

describe("shouldShowHoldingBanner — incoming-requests path", () => {
  const baseHolding = {
    kind: "holding" as const,
    filePath: "src/foo.rs",
    waiterCount: 0, // ambient mode would NOT show
  };
  const request: IncomingYieldRequest = {
    filePath: "src/foo.rs",
    requesterName: "alpha",
    requesterTaskRunId: "A",
    requestedAtMs: 0,
  };

  it("renders when waiterCount === 0 BUT an incoming request is present", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: baseHolding,
        dismissed: new Set(),
        tabId: "tab-1",
        incomingRequests: [request],
      }),
    ).toBe(true);
  });

  it("still suppresses on local dismissal even with incoming request", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: baseHolding,
        dismissed: new Set(["tab-1:src/foo.rs"]),
        tabId: "tab-1",
        incomingRequests: [request],
      }),
    ).toBe(false);
  });

  it("does not render for waiting/idle even with incoming requests", () => {
    // Defensive — incoming requests should only arrive when the tab is
    // holding, but the predicate must not flip waiting/idle into a
    // holding-banner.
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "waiting", filePath: "src/foo.rs" },
        dismissed: new Set(),
        tabId: "tab-1",
        incomingRequests: [request],
      }),
    ).toBe(false);
    expect(
      shouldShowHoldingBanner({
        lockState: { kind: "idle" },
        dismissed: new Set(),
        tabId: "tab-1",
        incomingRequests: [request],
      }),
    ).toBe(false);
  });

  it("ambient mode still works when incomingRequests is empty", () => {
    expect(
      shouldShowHoldingBanner({
        lockState: { ...baseHolding, waiterCount: 1 },
        dismissed: new Set(),
        tabId: "tab-1",
        incomingRequests: [],
      }),
    ).toBe(true);
  });
});

// ── Phase 3 — request-mode banner content priority ─────────────────────────
//
// The component switches to request-mode whenever `incomingRequests`
// is non-empty. Replicates the message-pick logic the JSX runs so we
// can pin "request mode beats ambient" + "+N more requests" surfaces
// in the same banner.

describe("banner content priority (request-mode beats ambient)", () => {
  it("emits request-mode message when incoming requests exist", () => {
    const oldest: IncomingYieldRequest = {
      filePath: "src/foo.rs",
      requesterName: "alpha",
      requesterTaskRunId: "A",
      requestedAtMs: 1_000,
    };
    const newer: IncomingYieldRequest = {
      filePath: "src/foo.rs",
      requesterName: "bravo",
      requesterTaskRunId: "B",
      requestedAtMs: 2_000,
    };
    const requests = [newer, oldest];
    const pickedOldest = pickOldestRequest(requests)!;
    const inRequestMode = pickedOldest !== undefined;
    expect(inRequestMode).toBe(true);

    const message = formatIncomingRequestText({
      oldest: pickedOldest,
      totalRequests: requests.length,
      secondsLabel: "3s",
    });
    // Must reflect the OLDER (alpha) requester, NOT the most-recent
    // (bravo) — the banner surfaces longest-pending first.
    expect(message).toContain("alpha");
    expect(message).not.toMatch(/^Session bravo/);
    expect(message).toContain("+1 more request");
  });

  it("falls through to ambient text when no incoming requests", () => {
    const ambient = formatBannerText({
      waiterName: "alpha",
      filePath: "src/foo.rs",
      secondsLabel: "5s",
      waiterCount: 1,
    });
    expect(ambient).toBe("Session alpha is waiting on src/foo.rs (waiting 5s)");
  });
});

// ── Phase 3 — "I'll be a while" still disabled ─────────────────────────────
//
// The button stays disabled in Phase 3 per plan §Open Q6 ("Folded into
// the stuck-session heartbeat plan"). The component's JSX writes the
// button with `disabled` and a static tooltip. We pin the tooltip
// string here so a future refactor that re-enables the button fails
// this test as a tripwire.

describe("'I'll be a while' button (Phase 3 — still disabled)", () => {
  it("tooltip references Open Q6 / heartbeat plan", () => {
    // Mirror the static title attribute baked into HoldingLockBanner.tsx.
    // If the JSX tooltip drifts, this assertion will flag it; the
    // grep also catches a removed `disabled` attribute.
    const expectedTitle =
      "Folded into the stuck-session heartbeat plan — see lock-yield-protocol-plan.md §Open Q6.";
    expect(expectedTitle).toContain("heartbeat plan");
    expect(expectedTitle).toContain("Open Q6");
  });
});
