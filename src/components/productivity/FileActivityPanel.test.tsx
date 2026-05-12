/**
 * Pure-helper tests for FileActivityPanel and fileActivityApi.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom — see
 * CompletionReportSections.test.tsx for the same pattern). That means we
 * can't render the panel and assert on its DOM. The plan's three Phase 2
 * test cases ("renders empty state", "click on hot session row calls
 * setActiveId", "window selector change triggers a re-fetch") map to:
 *
 *   - empty state            → covered by manual + UI Bridge spec
 *   - click → setActiveId    → covered by manual + UI Bridge spec
 *                              (v1 ships a page-nav stub; per-tab focus
 *                              is a documented follow-up — see panel
 *                              header comment)
 *   - window selector reset  → loadStoredWindowSecs / storeWindowSecs
 *                              round-trip covered here
 *
 * The pure helpers `ageLabel`, `barFor`, `loadStoredWindowSecs`, and
 * `storeWindowSecs` carry the load-bearing logic and are tested here.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Mock the port resolver + logger BEFORE importing the panel so the
// yield-request POST simulation picks up our mocked values. Mirrors the
// pattern in `terminal/WaitingLockBanner.test.tsx`.
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
  ageLabel,
  barFor,
  buildYieldRequestBody,
  hasExclusiveLock,
  lockYieldCooldownKey,
  lockYieldCooldownRemainingSecs,
} from "./FileActivityPanel";
import {
  DEFAULT_WINDOW_SECS,
  WINDOW_OPTIONS,
  WINDOW_STORAGE_KEY,
  loadStoredWindowSecs,
  storeWindowSecs,
  type FileLockInfoEntry,
  type FileRegistryInfoEntry,
} from "./fileActivityApi";

// ---------------------------------------------------------------------------
// ageLabel
// ---------------------------------------------------------------------------

describe("ageLabel", () => {
  // Fixed "now" so subsecond drift between Date.now() reads doesn't flake
  // the test. The function exposes nowMs precisely for this reason.
  const NOW = Date.parse("2026-05-10T20:00:00Z");

  it("returns 'now' for timestamps in the future", () => {
    expect(ageLabel("2026-05-10T20:01:00Z", NOW)).toBe("now");
  });

  it("formats seconds for ages under a minute", () => {
    expect(ageLabel("2026-05-10T19:59:30Z", NOW)).toBe("30s ago");
  });

  it("formats minutes for ages under an hour", () => {
    expect(ageLabel("2026-05-10T19:45:00Z", NOW)).toBe("15m ago");
  });

  it("formats hours for ages under a day", () => {
    expect(ageLabel("2026-05-10T17:00:00Z", NOW)).toBe("3h ago");
  });

  it("formats days otherwise", () => {
    expect(ageLabel("2026-05-08T20:00:00Z", NOW)).toBe("2d ago");
  });

  it("returns 'unknown' for unparseable input", () => {
    expect(ageLabel("not-a-timestamp", NOW)).toBe("unknown");
  });
});

// ---------------------------------------------------------------------------
// barFor
// ---------------------------------------------------------------------------

describe("barFor", () => {
  it("returns empty string when max is 0 (avoid /0 NaN)", () => {
    expect(barFor(0, 0)).toBe("");
    expect(barFor(5, 0)).toBe("");
  });

  it("renders a 10-segment bar with at least one filled cell for any nonzero count", () => {
    // 1 out of 100 rounds to 0.1 → would render 0 filled without the
    // `Math.max(1, ...)` floor. The floor keeps the bar legible.
    const bar = barFor(1, 100);
    expect(bar).toMatch(/^▮+▯+$/);
    expect(bar.length).toBe(10);
    expect((bar.match(/▮/g) ?? []).length).toBeGreaterThanOrEqual(1);
  });

  it("renders all-filled when count equals max", () => {
    expect(barFor(7, 7)).toBe("▮▮▮▮▮▮▮▮▮▮");
  });

  it("scales linearly between extremes", () => {
    const bar = barFor(5, 10);
    expect(bar.length).toBe(10);
    // 5/10 → 5 filled.
    expect((bar.match(/▮/g) ?? []).length).toBe(5);
    expect((bar.match(/▯/g) ?? []).length).toBe(5);
  });
});

// ---------------------------------------------------------------------------
// loadStoredWindowSecs / storeWindowSecs
// ---------------------------------------------------------------------------

describe("window secs persistence", () => {
  // Shim a minimal localStorage onto globalThis for the node-environment
  // test runner. Cleanup restores any prior shape (none, in practice).
  let store: Record<string, string>;
  const originalWindow = (globalThis as Record<string, unknown>).window;

  beforeEach(() => {
    store = {};
    (globalThis as Record<string, unknown>).window = {
      localStorage: {
        getItem: (k: string): string | null => (k in store ? store[k] : null),
        setItem: (k: string, v: string): void => {
          store[k] = v;
        },
        removeItem: (k: string): void => {
          delete store[k];
        },
        clear: (): void => {
          store = {};
        },
        get length(): number {
          return Object.keys(store).length;
        },
        key: (i: number): string | null => Object.keys(store)[i] ?? null,
      },
    };
  });

  afterEach(() => {
    if (originalWindow === undefined) {
      delete (globalThis as Record<string, unknown>).window;
    } else {
      (globalThis as Record<string, unknown>).window = originalWindow;
    }
    vi.restoreAllMocks();
  });

  it("returns the default when localStorage is empty", () => {
    expect(loadStoredWindowSecs()).toBe(DEFAULT_WINDOW_SECS);
  });

  it("round-trips a stored value when it matches a WINDOW_OPTIONS entry", () => {
    const sixHours = WINDOW_OPTIONS.find((o) => o.secs === 21_600)!;
    storeWindowSecs(sixHours.secs);
    expect(store[WINDOW_STORAGE_KEY]).toBe("21600");
    expect(loadStoredWindowSecs()).toBe(21_600);
  });

  it("ignores an unrecognized stored value and returns the default", () => {
    store[WINDOW_STORAGE_KEY] = "999999"; // not in WINDOW_OPTIONS
    expect(loadStoredWindowSecs()).toBe(DEFAULT_WINDOW_SECS);
  });

  it("ignores a non-numeric stored value and returns the default", () => {
    store[WINDOW_STORAGE_KEY] = "garbage";
    expect(loadStoredWindowSecs()).toBe(DEFAULT_WINDOW_SECS);
  });
});

// ---------------------------------------------------------------------------
// Lock-Yield Protocol Phase 4 — pure helpers + click-handler simulation
// ---------------------------------------------------------------------------
//
// The runner's vitest config is `environment: "node"` (no jsdom) — same
// constraint as the existing tests above. We exercise the exported pure
// helpers (`hasExclusiveLock`, `lockYieldCooldownKey`,
// `lockYieldCooldownRemainingSecs`, `buildYieldRequestBody`,
// `REQUEST_YIELD_COOLDOWN_MS`) plus a click-handler simulation that
// pins the POST shape against the Phase 1 endpoint contract.

// ── hasExclusiveLock — registry × lock-info join predicate ─────────────────

describe("hasExclusiveLock — Phase 4 row gating", () => {
  const registryEntry: FileRegistryInfoEntry = {
    file_path: "src/foo.rs",
    holder_task_run_id: "tab-B",
    holder_name: "bravo",
    registered_at: 1_700_000_000_000,
  };
  const matchingLock: FileLockInfoEntry = {
    file_path: "src/foo.rs",
    holder_task_run_id: "tab-B",
    holder_name: "bravo",
    acquired_at: 1_700_000_000,
  };

  it("returns false when lockInfo is null (first-poll not landed yet)", () => {
    expect(hasExclusiveLock(registryEntry, null)).toBe(false);
  });

  it("returns false when lockInfo is empty", () => {
    expect(hasExclusiveLock(registryEntry, [])).toBe(false);
  });

  it("returns true when (file_path, holder_task_run_id) matches a lock entry", () => {
    expect(hasExclusiveLock(registryEntry, [matchingLock])).toBe(true);
  });

  it("returns false when file_path matches but holder_task_run_id differs", () => {
    const lockWithDifferentHolder: FileLockInfoEntry = {
      ...matchingLock,
      holder_task_run_id: "tab-C",
    };
    expect(hasExclusiveLock(registryEntry, [lockWithDifferentHolder])).toBe(false);
  });

  it("returns false when holder_task_run_id matches but file_path differs", () => {
    const lockWithDifferentFile: FileLockInfoEntry = {
      ...matchingLock,
      file_path: "src/bar.rs",
    };
    expect(hasExclusiveLock(registryEntry, [lockWithDifferentFile])).toBe(false);
  });
});

// ── lockYieldCooldownKey — per-(file, holder) keying invariant ─────────────

describe("lockYieldCooldownKey — per-(file_path, holder_task_run_id) keying", () => {
  it("composes a stable key from the two identifiers", () => {
    expect(lockYieldCooldownKey("src/foo.rs", "tab-B")).toBe("src/foo.rs::tab-B");
  });

  it("yields distinct keys for different files with the same holder", () => {
    expect(lockYieldCooldownKey("src/foo.rs", "tab-B")).not.toBe(
      lockYieldCooldownKey("src/bar.rs", "tab-B"),
    );
  });

  it("yields distinct keys for the same file with different holders", () => {
    expect(lockYieldCooldownKey("src/foo.rs", "tab-B")).not.toBe(
      lockYieldCooldownKey("src/foo.rs", "tab-C"),
    );
  });
});

// ── lockYieldCooldownRemainingSecs — ceil rounding mirrors WaitingLockBanner ─

describe("lockYieldCooldownRemainingSecs", () => {
  it("returns 0 once the cooldown deadline is reached", () => {
    expect(lockYieldCooldownRemainingSecs(1_000, 1_000)).toBe(0);
    expect(lockYieldCooldownRemainingSecs(1_000, 2_000)).toBe(0);
  });

  it("ceil-rounds remaining millis to whole seconds", () => {
    expect(lockYieldCooldownRemainingSecs(31_000, 1_000)).toBe(30);
    expect(lockYieldCooldownRemainingSecs(1_500, 1_000)).toBe(1);
    expect(lockYieldCooldownRemainingSecs(1_001, 1_000)).toBe(1);
  });

  it("uses REQUEST_YIELD_COOLDOWN_MS = 30000 (tripwire vs. Phase 3 constant)", () => {
    // If a future plan changes the cooldown duration this assertion
    // must change in lockstep with both this file and
    // `terminal/WaitingLockBanner.tsx`'s constant. The TODO at
    // `FileActivityPanel.tsx` marks the dedupe candidate.
    expect(REQUEST_YIELD_COOLDOWN_MS).toBe(30_000);
  });
});

// ── buildYieldRequestBody — synthetic Coordinator Dashboard identity ───────

describe("buildYieldRequestBody — §Open Q5 synthetic requester identity", () => {
  it("uses 'coordinator-dashboard' / 'Coordinator Dashboard' as requester", () => {
    const body = buildYieldRequestBody("src/foo.rs", "tab-B");
    expect(body).toEqual({
      file_path: "src/foo.rs",
      requester_task_run_id: "coordinator-dashboard",
      requester_name: "Coordinator Dashboard",
      holder_task_run_id: "tab-B",
    });
  });

  it("threads file_path and holder_task_run_id through unchanged", () => {
    const body = buildYieldRequestBody("/abs/path/with spaces.rs", "named-12345-uuid");
    expect(body.file_path).toBe("/abs/path/with spaces.rs");
    expect(body.holder_task_run_id).toBe("named-12345-uuid");
  });
});

// ── Click-handler simulation — POST shape pin + cooldown lifecycle ─────────
//
// Mirrors the component's `handleRequestYield` callback. The map-based
// per-(file, holder) cooldown state machine is replicated here directly
// (the JSX merely renders the `disabled` flag derived from
// `lockYieldCooldownRemainingSecs > 0`). The simulation pins:
//
//   1. POST URL + body shape against the Phase 1 endpoint contract.
//   2. Cooldown disables only the clicked row's button (not other rows).
//   3. After REQUEST_YIELD_COOLDOWN_MS elapses the row re-enables.

interface YieldClickSimulator {
  cooldowns: Map<string, number>;
  click(args: { filePath: string; holderTaskRunId: string }): Promise<void>;
  isInCooldown(args: { filePath: string; holderTaskRunId: string; nowMs: number }): boolean;
}

function makeYieldClickSimulator(fetchImpl: typeof fetch): YieldClickSimulator {
  const cooldowns = new Map<string, number>();
  return {
    cooldowns,
    async click({ filePath, holderTaskRunId }) {
      const key = lockYieldCooldownKey(filePath, holderTaskRunId);
      cooldowns.set(key, Date.now() + REQUEST_YIELD_COOLDOWN_MS);
      await fetchImpl(
        `http://127.0.0.1:${mockGetApiPort()}/file-locks/yield-request`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(buildYieldRequestBody(filePath, holderTaskRunId)),
        },
      );
    },
    isInCooldown({ filePath, holderTaskRunId, nowMs }) {
      const key = lockYieldCooldownKey(filePath, holderTaskRunId);
      const until = cooldowns.get(key) ?? 0;
      return lockYieldCooldownRemainingSecs(until, nowMs) > 0;
    },
  };
}

describe("yield-request POST dispatch — Phase 4 wire contract", () => {
  beforeEach(() => {
    mockGetApiPort.mockReset();
    mockGetApiPort.mockReturnValue(9876);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("posts to /file-locks/yield-request with the Coordinator Dashboard body", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ requested: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    const sim = makeYieldClickSimulator(fetchSpy);
    await sim.click({ filePath: "src/foo.rs", holderTaskRunId: "tab-B" });

    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const [url, init] = fetchSpy.mock.calls[0];
    expect(url).toBe("http://127.0.0.1:9876/file-locks/yield-request");
    expect(init).toBeDefined();
    expect(init!.method).toBe("POST");
    expect((init!.headers as Record<string, string>)["Content-Type"]).toBe(
      "application/json",
    );
    // EXACT body shape — must match the Phase 1 Rust handler's
    // `YieldRequestRequest` deserializer. Any drift breaks the wire
    // contract. §Open Q5 (synthetic Coordinator Dashboard identity) is
    // pinned here too: a future refactor that swaps in a real
    // task_run_id must update both this assertion AND
    // `buildYieldRequestBody`.
    expect(JSON.parse(init!.body as string)).toEqual({
      file_path: "src/foo.rs",
      requester_task_run_id: "coordinator-dashboard",
      requester_name: "Coordinator Dashboard",
      holder_task_run_id: "tab-B",
    });
  });

  it("uses the dynamic port from getApiPort()", async () => {
    mockGetApiPort.mockReturnValue(54321);
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("{}", { status: 200 }),
    );
    const sim = makeYieldClickSimulator(fetchSpy);
    await sim.click({ filePath: "src/foo.rs", holderTaskRunId: "tab-B" });
    expect(fetchSpy.mock.calls[0][0]).toBe(
      "http://127.0.0.1:54321/file-locks/yield-request",
    );
  });
});

describe("yield cooldown lifecycle — per-(file, holder) keying", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(1_700_000_000_000));
    mockGetApiPort.mockReset();
    mockGetApiPort.mockReturnValue(9876);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("disables the clicked row for 30s and re-enables after the cooldown expires", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("{}", { status: 200 }),
    );
    const sim = makeYieldClickSimulator(fetchSpy);
    const target = { filePath: "src/foo.rs", holderTaskRunId: "tab-B" };

    // Before click — not in cooldown.
    expect(sim.isInCooldown({ ...target, nowMs: Date.now() })).toBe(false);

    await sim.click(target);
    const clickedAt = Date.now();

    // Immediately after click — full cooldown.
    expect(sim.isInCooldown({ ...target, nowMs: Date.now() })).toBe(true);

    // 15s in — still in cooldown.
    vi.setSystemTime(new Date(clickedAt + 15_000));
    expect(sim.isInCooldown({ ...target, nowMs: Date.now() })).toBe(true);

    // 29.999s in — still in cooldown (ceil rounds 1ms → 1s).
    vi.setSystemTime(new Date(clickedAt + 29_999));
    expect(sim.isInCooldown({ ...target, nowMs: Date.now() })).toBe(true);

    // 30s exactly — cooldown finished.
    vi.setSystemTime(new Date(clickedAt + REQUEST_YIELD_COOLDOWN_MS));
    expect(sim.isInCooldown({ ...target, nowMs: Date.now() })).toBe(false);
  });

  it("keeps other rows enabled when one row is in cooldown (per-pair keying)", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("{}", { status: 200 }),
    );
    const sim = makeYieldClickSimulator(fetchSpy);
    const row1 = { filePath: "src/foo.rs", holderTaskRunId: "tab-B" };
    const row2 = { filePath: "src/bar.rs", holderTaskRunId: "tab-B" };
    const row3 = { filePath: "src/foo.rs", holderTaskRunId: "tab-C" };

    await sim.click(row1);

    // row1 in cooldown.
    expect(sim.isInCooldown({ ...row1, nowMs: Date.now() })).toBe(true);
    // row2 (different file, same holder) — NOT in cooldown.
    expect(sim.isInCooldown({ ...row2, nowMs: Date.now() })).toBe(false);
    // row3 (same file, different holder) — NOT in cooldown.
    expect(sim.isInCooldown({ ...row3, nowMs: Date.now() })).toBe(false);

    // Click row2 — row2 enters cooldown; row1 is still in cooldown; row3
    // remains free.
    await sim.click(row2);
    expect(sim.isInCooldown({ ...row1, nowMs: Date.now() })).toBe(true);
    expect(sim.isInCooldown({ ...row2, nowMs: Date.now() })).toBe(true);
    expect(sim.isInCooldown({ ...row3, nowMs: Date.now() })).toBe(false);

    expect(fetchSpy).toHaveBeenCalledTimes(2);
  });
});

// ---------------------------------------------------------------------------
// fetchLockInfo — siblings-of-fetchHeatmap unit test
// ---------------------------------------------------------------------------

describe("fetchLockInfo — Phase 4 lock-info endpoint wrapper", () => {
  // We import the function lazily here so the module-level `vi.mock` calls
  // at the top of the file are already installed. The fetcher uses the
  // window-injected port (NOT getApiPort), mirroring `fetchHeatmap`.
  let fetchLockInfo: typeof import("./fileActivityApi").fetchLockInfo;

  beforeEach(async () => {
    // Inject a minimal window shape so apiBaseUrl() resolves consistently.
    (globalThis as Record<string, unknown>).window = { __QONTINUI_PORT__: 9876 };
    ({ fetchLockInfo } = await import("./fileActivityApi"));
  });

  afterEach(() => {
    delete (globalThis as Record<string, unknown>).window;
    vi.restoreAllMocks();
  });

  it("GETs /file-locks/info and returns the parsed array", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify([
          {
            file_path: "src/foo.rs",
            holder_task_run_id: "tab-B",
            holder_name: "bravo",
            acquired_at: 1_700_000_000,
          },
        ]),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchSpy);

    const result = await fetchLockInfo();
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(fetchSpy.mock.calls[0][0]).toBe("http://127.0.0.1:9876/file-locks/info");
    expect(result).toEqual([
      {
        file_path: "src/foo.rs",
        holder_task_run_id: "tab-B",
        holder_name: "bravo",
        acquired_at: 1_700_000_000,
      },
    ]);
  });

  it("throws when the response is not ok", async () => {
    const fetchSpy = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("oops", { status: 500 }),
    );
    vi.stubGlobal("fetch", fetchSpy);

    await expect(fetchLockInfo()).rejects.toThrow(/HTTP 500/);
  });
});
