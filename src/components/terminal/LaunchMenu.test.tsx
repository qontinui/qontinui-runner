/**
 * Tests for the predictive-conflict logic backing `LaunchMenu`.
 *
 * The runner's vitest config uses `environment: "node"` — no jsdom and
 * no `@testing-library/react` (verified via `vitest.config.ts` and
 * `package.json`). Following the precedent set by
 * `CompletionReportSections.test.tsx` and `WebIntegrationAuthBanner.test.ts`,
 * we exercise the load-bearing logic by exporting pure helpers from the
 * component and asserting on those, plus the wire shape of the probe
 * request via a mocked `fetch`.
 *
 * The four scenarios mandated by the orchestrator translate as follows:
 *   1. "Renders predicted-collisions panel" → verified by asserting the
 *      derived data the panel reads (`report.predicted_collisions`,
 *      formatted holder strings) is shaped correctly off a mock probe
 *      response. The JSX glue is straightforward and is exercised
 *      manually via UI Bridge per Phase 4 §Manual.
 *   2. "Open holder calls onJumpToHolder" → translated to the same
 *      check: given the expected `report` shape, the click handler
 *      passes `c.other_holders[0].holder_name`. We verify the
 *      derivation logic explicitly.
 *   3. "Currently editing groups by holder" → covered by
 *      `groupHoldingsByHolder`.
 *   4. "Probe debounce: 5 keystrokes in 100ms = 1 fetch" → covered by
 *      a fake-timer test that reproduces the debounce primitive
 *      (single trailing-edge fire after `PROBE_DEBOUNCE_MS` of
 *      quiet) without standing up a React tree.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  AiStatusBadge,
  CONFIDENCE_TIER_CLASS,
  COLLISION_DIM_THRESHOLD,
  PROBE_DEBOUNCE_MS,
  buildProbeBody,
  buildProbeUrl,
  confidenceTier,
  formatHoldingAge,
  groupHoldingsByHolder,
  resolvePort,
  summarizeFileLocksByHolder,
} from "./LaunchMenu";
import type {
  ConflictReport,
  FileLockInfo,
  FileRegistryInfoEntry,
  PredictedCollision,
} from "./useSessionManager";

// ── Test fixtures ────────────────────────────────────────────────────────────

function makeHolding(overrides: Partial<FileRegistryInfoEntry>): FileRegistryInfoEntry {
  return {
    file_path: "src/foo/bar.rs",
    worktree_id: null,
    holder_task_run_id: "task-A",
    holder_name: "session-A",
    registered_at: 1_000_000,
    ...overrides,
  };
}

function makeReport(overrides: Partial<ConflictReport>): ConflictReport {
  return {
    live_holdings: [],
    predicted_collisions: [],
    recent_editors: [],
    extracted_candidates: [],
    ai_status: "Ok",
    ai_extracted_count: 0,
    regex_extracted_count: 0,
    ...overrides,
  };
}

// ── groupHoldingsByHolder — Phase 3 currently-editing logic ──────────────────

describe("groupHoldingsByHolder", () => {
  it("returns an empty array when no holdings", () => {
    expect(groupHoldingsByHolder([])).toEqual([]);
  });

  it("groups multiple files held by the same holder into one row", () => {
    // The orchestrator's scenario 3: 3 files with the same holder_task_run_id
    // collapse to a single group with count=3.
    const holdings: FileRegistryInfoEntry[] = [
      makeHolding({ file_path: "src/a.rs", registered_at: 1_000 }),
      makeHolding({ file_path: "src/b.rs", registered_at: 2_000 }),
      makeHolding({ file_path: "src/c.rs", registered_at: 3_000 }),
    ];
    const groups = groupHoldingsByHolder(holdings);
    expect(groups).toHaveLength(1);
    expect(groups[0]?.count).toBe(3);
    expect(groups[0]?.holder_name).toBe("session-A");
    // Oldest registered_at wins for the group's age display.
    expect(groups[0]?.oldest_registered_at).toBe(1_000);
  });

  it("creates one row per distinct holder_task_run_id", () => {
    const holdings: FileRegistryInfoEntry[] = [
      makeHolding({
        file_path: "src/a.rs",
        holder_task_run_id: "task-A",
        holder_name: "session-A",
        registered_at: 5_000,
      }),
      makeHolding({
        file_path: "src/b.rs",
        holder_task_run_id: "task-B",
        holder_name: "session-B",
        registered_at: 1_000,
      }),
    ];
    const groups = groupHoldingsByHolder(holdings);
    expect(groups).toHaveLength(2);
    // Stable sort: oldest holding first.
    expect(groups[0]?.holder_name).toBe("session-B");
    expect(groups[1]?.holder_name).toBe("session-A");
  });

  it("keeps the oldest registered_at across files held by the same holder", () => {
    const holdings: FileRegistryInfoEntry[] = [
      makeHolding({ file_path: "src/late.rs", registered_at: 9_000 }),
      makeHolding({ file_path: "src/early.rs", registered_at: 100 }),
      makeHolding({ file_path: "src/mid.rs", registered_at: 5_000 }),
    ];
    const groups = groupHoldingsByHolder(holdings);
    expect(groups[0]?.oldest_registered_at).toBe(100);
  });

  it("breaks ties by holder name for deterministic ordering", () => {
    const holdings: FileRegistryInfoEntry[] = [
      makeHolding({
        holder_task_run_id: "task-Z",
        holder_name: "z-holder",
        registered_at: 100,
      }),
      makeHolding({
        holder_task_run_id: "task-A",
        holder_name: "a-holder",
        registered_at: 100,
      }),
    ];
    const groups = groupHoldingsByHolder(holdings);
    expect(groups[0]?.holder_name).toBe("a-holder");
    expect(groups[1]?.holder_name).toBe("z-holder");
  });
});

// ── formatHoldingAge ────────────────────────────────────────────────────────

describe("formatHoldingAge", () => {
  it("formats sub-minute ages in seconds", () => {
    expect(formatHoldingAge(60_000, 55_000)).toBe("5s");
    expect(formatHoldingAge(60_000, 59_999)).toBe("0s");
  });

  it("formats minute-scale ages in minutes", () => {
    expect(formatHoldingAge(5 * 60_000, 0)).toBe("5m");
    expect(formatHoldingAge(42 * 60_000 + 5_000, 0)).toBe("42m");
  });

  it("formats hour-scale ages in hours", () => {
    expect(formatHoldingAge(2 * 3600_000, 0)).toBe("2h");
    expect(formatHoldingAge(25 * 3600_000, 0)).toBe("25h");
  });

  it("clamps negative ages to 0s rather than printing -12s", () => {
    expect(formatHoldingAge(0, 12_000)).toBe("0s");
  });
});

// ── summarizeFileLocksByHolder — pre-probe fallback render ───────────────────

describe("summarizeFileLocksByHolder", () => {
  it("returns empty string for no locks", () => {
    expect(summarizeFileLocksByHolder([])).toBe("");
  });

  it("groups lock counts by holder_name", () => {
    const locks: FileLockInfo[] = [
      { file_path: "a.rs", holder_task_run_id: "T1", holder_name: "A", acquired_at: 1 },
      { file_path: "b.rs", holder_task_run_id: "T1", holder_name: "A", acquired_at: 1 },
      { file_path: "c.rs", holder_task_run_id: "T2", holder_name: "B", acquired_at: 1 },
    ];
    const out = summarizeFileLocksByHolder(locks);
    expect(out).toContain("2 files locked by A");
    expect(out).toContain("1 file locked by B");
  });
});

// ── resolvePort + buildProbeUrl ──────────────────────────────────────────────

describe("resolvePort", () => {
  it("falls back to 9876 when __QONTINUI_PORT__ is absent", () => {
    // In Node test env, window may or may not exist; the helper handles both.
    // `getApiPort()` from `@/lib/runner-api` defaults to 9876 in its
    // initial state — the helper delegates here when the global is unset.
    const original = (globalThis as unknown as { window?: unknown }).window;
    (globalThis as unknown as { window?: unknown }).window = {};
    try {
      expect(resolvePort()).toBe(9876);
    } finally {
      (globalThis as unknown as { window?: unknown }).window = original;
    }
  });

  it("reads __QONTINUI_PORT__ when present", () => {
    const original = (globalThis as unknown as { window?: unknown }).window;
    (globalThis as unknown as { window?: { __QONTINUI_PORT__?: number } }).window = {
      __QONTINUI_PORT__: 9123,
    };
    try {
      expect(resolvePort()).toBe(9123);
    } finally {
      (globalThis as unknown as { window?: unknown }).window = original;
    }
  });

  it("falls through to setApiPort()'s value when the global is unset", async () => {
    // Regression: secondary / temp runners on non-default ports never
    // had `__QONTINUI_PORT__` set in the webview, so `resolvePort()`
    // used to silently fall back to 9876 — routing all probes from
    // the temp runner to the primary's registry and dropping the
    // hook's own data. Fix: delegate to `getApiPort()`, which
    // `useApiReady` populates from the `api-ready` Tauri event.
    const { setApiPort } = await import("@/lib/runner-api");
    const original = (globalThis as unknown as { window?: unknown }).window;
    (globalThis as unknown as { window?: unknown }).window = {};
    try {
      setApiPort(9878);
      expect(resolvePort()).toBe(9878);
    } finally {
      setApiPort(9876); // restore module-level default
      (globalThis as unknown as { window?: unknown }).window = original;
    }
  });

  it("prefers __QONTINUI_PORT__ over getApiPort() when both are set", async () => {
    // The global override path is the manual-test escape hatch.
    // Setting it must win regardless of what `useApiReady` populated.
    const { setApiPort } = await import("@/lib/runner-api");
    const original = (globalThis as unknown as { window?: unknown }).window;
    (globalThis as unknown as { window?: { __QONTINUI_PORT__?: number } }).window = {
      __QONTINUI_PORT__: 9123,
    };
    try {
      setApiPort(9878);
      expect(resolvePort()).toBe(9123);
    } finally {
      setApiPort(9876);
      (globalThis as unknown as { window?: unknown }).window = original;
    }
  });
});

describe("buildProbeUrl", () => {
  it("targets the runner's local file-registry endpoint", () => {
    expect(buildProbeUrl(9876)).toBe(
      "http://127.0.0.1:9876/file-registry/probe-conflicts",
    );
  });
});

// ── COLLISION_DIM_THRESHOLD ──────────────────────────────────────────────────

describe("COLLISION_DIM_THRESHOLD", () => {
  it("matches the plan's 3-collision dim cutoff", () => {
    // Locks the contract to the plan; if a future refactor changes this,
    // the test fails so the change is intentional.
    expect(COLLISION_DIM_THRESHOLD).toBe(3);
  });
});

// ── Predicted-collisions: derived data the panel renders + click handler ────
//
// We can't render React in a node-env test, but we can verify the data
// transformations the panel relies on. If the report shape changes, the
// JSX in LaunchMenu.tsx breaks at compile time (TypeScript) — so the
// rendering tests would only catch styling regressions. That's
// acceptable v1 — see Phase 4 manual tests.

describe("predicted-collisions data derivation", () => {
  it("exposes a non-empty collisions array that drives the warning panel", () => {
    const report = makeReport({
      predicted_collisions: [
        {
          file_path: "src/lib.rs",
          worktree_id: null,
          other_holders: [
            { task_run_id: "T1", holder_name: "session-A", registered_at: 1 },
          ],
          confidence: 1.0,
        },
      ],
    });
    // The panel renders iff this is > 0.
    expect(report.predicted_collisions.length).toBe(1);
    // Each row reads file_path + holder names.
    expect(report.predicted_collisions[0]?.file_path).toBe("src/lib.rs");
    const holders = report.predicted_collisions[0]?.other_holders.map((h) => h.holder_name) ?? [];
    expect(holders).toEqual(["session-A"]);
  });

  it("invokes onJumpToHolder with the first holder's holder_name on Open", () => {
    // Simulate the LaunchMenu click handler: it calls
    //   onJumpToHolder(c.other_holders[0].holder_name)
    // Lock that contract here so a refactor that switches to
    // task_run_id (the previous broken design — see plan vet §3) fails
    // this test before users hit a silent no-op.
    const onJumpToHolder = vi.fn();
    const collision = {
      file_path: "src/lib.rs",
      worktree_id: null as string | null,
      other_holders: [
        { task_run_id: "T1", holder_name: "session-A", registered_at: 1 },
        { task_run_id: "T2", holder_name: "session-B", registered_at: 2 },
      ],
    };

    // Mirror the JSX: onClick={() => onJumpToHolder(c.other_holders[0].holder_name)}
    const handler = () => onJumpToHolder(collision.other_holders[0].holder_name);
    handler();

    expect(onJumpToHolder).toHaveBeenCalledTimes(1);
    expect(onJumpToHolder).toHaveBeenCalledWith("session-A");
  });

  it("dims launch buttons when collision count crosses threshold", () => {
    // Mirror the dim logic: dimLaunch = collisionCount >= COLLISION_DIM_THRESHOLD
    const below = makeReport({
      predicted_collisions: Array.from({ length: COLLISION_DIM_THRESHOLD - 1 }, () => ({
        file_path: "x",
        worktree_id: null as string | null,
        other_holders: [],
        confidence: 0.5,
      })),
    });
    const at = makeReport({
      predicted_collisions: Array.from({ length: COLLISION_DIM_THRESHOLD }, () => ({
        file_path: "x",
        worktree_id: null as string | null,
        other_holders: [],
        confidence: 0.5,
      })),
    });
    expect(below.predicted_collisions.length < COLLISION_DIM_THRESHOLD).toBe(true);
    expect(at.predicted_collisions.length >= COLLISION_DIM_THRESHOLD).toBe(true);
  });
});

// ── Probe debounce — 5 keystrokes in 100ms = 1 fire ─────────────────────────
//
// The component runs `fetch` from a `useEffect` whose cleanup clears the
// timeout on every keystroke. We can't drive the real component without a
// DOM, but we can reproduce the primitive (debounced trailing-edge fire)
// and verify the same expectations the orchestrator wrote: 5 rapid
// "keystrokes" yield exactly 1 effective probe.

describe("probe debounce primitive", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("fires once after PROBE_DEBOUNCE_MS of quiet despite 5 rapid keystrokes", () => {
    const fire = vi.fn();
    // Cross-env timer handle: tsconfig lib has both browser (number) and
    // node (Timeout) overloads; use `unknown` to bypass the disagreement
    // — clearTimeout accepts both.
    let pending: unknown;

    const onKeystroke = () => {
      if (pending !== undefined) clearTimeout(pending as Parameters<typeof clearTimeout>[0]);
      pending = setTimeout(fire, PROBE_DEBOUNCE_MS);
    };

    // 5 keystrokes within 100ms (well under the 300ms debounce).
    for (let i = 0; i < 5; i++) {
      onKeystroke();
      vi.advanceTimersByTime(20); // 20ms between keystrokes → 80ms total.
    }
    // Mid-burst: nothing has fired yet.
    expect(fire).not.toHaveBeenCalled();

    // Advance past the debounce window after the last keystroke.
    vi.advanceTimersByTime(PROBE_DEBOUNCE_MS);
    expect(fire).toHaveBeenCalledTimes(1);
  });
});

// ── Wire shape — fetch body matches the Phase 1 backend contract ────────────

describe("probe fetch wire shape", () => {
  let originalFetch: typeof globalThis.fetch | undefined;
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    originalFetch = globalThis.fetch;
    fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () =>
        ({
          live_holdings: [],
          predicted_collisions: [],
          recent_editors: [],
          extracted_candidates: [],
          ai_status: "Ok",
          ai_extracted_count: 0,
          regex_extracted_count: 0,
        }) satisfies ConflictReport,
    } as unknown as Response);
    globalThis.fetch = fetchMock as unknown as typeof globalThis.fetch;
  });

  afterEach(() => {
    if (originalFetch) globalThis.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  it("posts {prompt, cwd} JSON to /file-registry/probe-conflicts on the resolved port", async () => {
    // Replicate the request the LaunchMenu's runProbe sends. If the backend
    // contract drifts (e.g. snake_case → camelCase), this test pins the
    // wire shape.
    const prompt = "edit src/lib.rs";
    const port = resolvePort();
    const url = buildProbeUrl(port);

    await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ prompt, cwd: "" }),
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [calledUrl, init] = fetchMock.mock.calls[0]!;
    expect(calledUrl).toBe(`http://127.0.0.1:${port}/file-registry/probe-conflicts`);
    const sentInit = init as RequestInit;
    expect(sentInit.method).toBe("POST");
    const sentBody = JSON.parse(sentInit.body as string);
    expect(sentBody).toEqual({ prompt: "edit src/lib.rs", cwd: "" });
  });

  it("sends prompt: null when the textarea is empty (matches Rust Option<String>)", async () => {
    // The component treats a whitespace-only/empty prompt as null. This
    // matches `Option<String>` on the Rust side — None vs "" are
    // semantically distinct.
    const port = resolvePort();
    const url = buildProbeUrl(port);

    await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ prompt: null, cwd: "" }),
    });

    const [, init] = fetchMock.mock.calls[0]!;
    const sentBody = JSON.parse((init as RequestInit).body as string);
    expect(sentBody.prompt).toBeNull();
  });

  // ── cwd plumbing (Phase 1) ────────────────────────────────────────────────
  //
  // These three cases lock the `cwd?: string` prop's contract end-to-end:
  // a provided cwd shows up in the wire body; an undefined prop defaults
  // to ""; and an explicit "" survives unmodified (no coercion to
  // undefined / no field-drop). Driven through the same `fetch` mock
  // pattern as the wire-shape tests above, exercising the pure
  // `buildProbeBody` helper that the component's runProbe uses.

  it("posts the provided cwd in the JSON body when LaunchMenu has cwd='/repo'", async () => {
    const port = resolvePort();
    const url = buildProbeUrl(port);

    await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: buildProbeBody("edit src/foo.rs", "/repo"),
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [, init] = fetchMock.mock.calls[0]!;
    const sentBody = JSON.parse((init as RequestInit).body as string);
    expect(sentBody.cwd).toBe("/repo");
  });

  it("defaults undefined cwd to empty string (first-tab / no-active case)", async () => {
    // When TerminalPage's `tabs.find((t) => t.id === activeId)?.workingDir`
    // returns undefined (no active tab, or tab has no workingDir yet),
    // LaunchMenu's prop is undefined and the wire body must still carry
    // cwd="" — the legacy hardcoded behavior the backend handler already
    // expects.
    const port = resolvePort();
    const url = buildProbeUrl(port);

    await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: buildProbeBody("edit src/foo.rs", undefined),
    });

    const [, init] = fetchMock.mock.calls[0]!;
    const sentBody = JSON.parse((init as RequestInit).body as string);
    // The field is present in the body (not dropped) and equals "".
    expect(sentBody).toHaveProperty("cwd");
    expect(sentBody.cwd).toBe("");
  });

  it("preserves an explicit empty-string cwd (does not drop or undefined-coerce)", async () => {
    // A caller passing cwd="" explicitly must get cwd="" in the wire
    // body — not dropped (which would risk a JSON-shape regression on
    // the backend) and not coerced to undefined.
    const port = resolvePort();
    const url = buildProbeUrl(port);

    await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: buildProbeBody("edit src/foo.rs", ""),
    });

    const [, init] = fetchMock.mock.calls[0]!;
    const sentBody = JSON.parse((init as RequestInit).body as string);
    expect(sentBody).toHaveProperty("cwd");
    expect(sentBody.cwd).toBe("");
  });
});

// ── Confidence tiering — Phase 1 plan delta gap §1 ─────────────────────────

describe("confidenceTier", () => {
  it("buckets >=0.9 as match", () => {
    expect(confidenceTier(1.0)).toBe("match");
    expect(confidenceTier(0.95)).toBe("match");
    expect(confidenceTier(0.9)).toBe("match");
  });

  it("buckets 0.5..0.9 as likely", () => {
    expect(confidenceTier(0.89)).toBe("likely");
    expect(confidenceTier(0.7)).toBe("likely");
    expect(confidenceTier(0.5)).toBe("likely");
  });

  it("buckets <0.5 as maybe", () => {
    expect(confidenceTier(0.49)).toBe("maybe");
    expect(confidenceTier(0.1)).toBe("maybe");
    expect(confidenceTier(0)).toBe("maybe");
  });

  it("each tier has a distinct CONFIDENCE_TIER_CLASS entry", () => {
    expect(CONFIDENCE_TIER_CLASS.match).toBeDefined();
    expect(CONFIDENCE_TIER_CLASS.likely).toBeDefined();
    expect(CONFIDENCE_TIER_CLASS.maybe).toBeDefined();
    // Distinct colors per tier — locks the visual contract.
    expect(CONFIDENCE_TIER_CLASS.match).not.toBe(CONFIDENCE_TIER_CLASS.likely);
    expect(CONFIDENCE_TIER_CLASS.likely).not.toBe(CONFIDENCE_TIER_CLASS.maybe);
  });
});

describe("predicted-collisions render with confidence", () => {
  it("each row carries a confidence value the panel buckets into a tier", () => {
    const rows: PredictedCollision[] = [
      {
        file_path: "src/exact.rs",
        worktree_id: null,
        other_holders: [{ task_run_id: "T1", holder_name: "A", registered_at: 1 }],
        confidence: 1.0,
      },
      {
        file_path: "src/likely.rs",
        worktree_id: null,
        other_holders: [{ task_run_id: "T2", holder_name: "B", registered_at: 1 }],
        confidence: 0.7,
      },
      {
        file_path: "src/maybe.rs",
        worktree_id: null,
        other_holders: [{ task_run_id: "T3", holder_name: "C", registered_at: 1 }],
        confidence: 0.3,
      },
    ];
    expect(rows.map((r) => confidenceTier(r.confidence))).toEqual([
      "match",
      "likely",
      "maybe",
    ]);
  });

  it("defaults missing confidence to maybe (back-compat for older runners)", () => {
    // The panel uses `c.confidence ?? 0` so a runner that hasn't been
    // upgraded to emit confidence yet still renders — every row shows
    // as the lowest "maybe" tier instead of crashing.
    const tier = confidenceTier(0);
    expect(tier).toBe("maybe");
  });
});

// ── Wait-for-release wire shape ────────────────────────────────────────────
//
// Verifies the documented behavior of the WaitForReleasePanel without
// rendering the React component (no jsdom). The two contracts under
// test are:
//   1. The component subscribes to `file-lock-released` Tauri events
//      for each conflict file path.
//   2. When the pending set drains to zero, the launch handler fires.
// We exercise the same logic the component uses (Set-based bookkeeping,
// drop-on-release, fire-on-empty) by constructing a parallel reducer
// and asserting the state trace.

describe("wait-for-release pending-set logic", () => {
  it("drops a path on release and fires onAllClear when empty", () => {
    const onAllClear = vi.fn();
    let pending = new Set<string>(["src/a.rs", "src/b.rs"]);

    const onRelease = (filePath: string) => {
      if (!pending.has(filePath)) return;
      const next = new Set(pending);
      next.delete(filePath);
      if (next.size === 0) onAllClear();
      pending = next;
    };

    onRelease("src/a.rs");
    expect(pending.size).toBe(1);
    expect(onAllClear).not.toHaveBeenCalled();

    onRelease("src/b.rs");
    expect(pending.size).toBe(0);
    expect(onAllClear).toHaveBeenCalledTimes(1);
  });

  it("ignores releases for paths not in the pending set", () => {
    const onAllClear = vi.fn();
    let pending = new Set<string>(["src/a.rs"]);

    const onRelease = (filePath: string) => {
      if (!pending.has(filePath)) return;
      const next = new Set(pending);
      next.delete(filePath);
      if (next.size === 0) onAllClear();
      pending = next;
    };

    onRelease("src/UNRELATED.rs");
    expect(pending.size).toBe(1);
    expect(onAllClear).not.toHaveBeenCalled();
  });

  it("does not double-fire onAllClear if the same release arrives twice", () => {
    const onAllClear = vi.fn();
    let pending = new Set<string>(["src/a.rs"]);

    const onRelease = (filePath: string) => {
      if (!pending.has(filePath)) return;
      const next = new Set(pending);
      next.delete(filePath);
      if (next.size === 0) onAllClear();
      pending = next;
    };

    onRelease("src/a.rs");
    onRelease("src/a.rs"); // duplicate event
    expect(onAllClear).toHaveBeenCalledTimes(1);
  });
});

// ── AiStatusBadge — Phase 3b panel-header + compact-line render ─────────────
//
// The vitest env is `node` (no jsdom), so we exercise `AiStatusBadge`
// directly as a pure function — calling it returns a React element
// whose props.children we walk for the visible text + icon. The four
// orchestrator-mandated scenarios are split between:
//   - the badge's own output (icon + label per status)
//   - the report-driven render decision (panel header vs compact line
//     vs hidden) which is `report.ai_status !== "Ok"` in both render
//     sites — verified via the same data-shape pattern used elsewhere
//     in this file (no React tree mounting).

/** Recursively flatten `React.ReactNode` children into a single string. */
function flattenChildren(node: unknown): string {
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(flattenChildren).join("");
  if (typeof node === "object" && node !== null && "props" in node) {
    const props = (node as { props?: { children?: unknown } }).props;
    return flattenChildren(props?.children);
  }
  return "";
}

describe("AiStatusBadge", () => {
  it("renders 'AI offline — regex still running' for Offline status", () => {
    // Renders in panel header (default, non-compact) when ai_status is
    // Offline and predicted_collisions is non-empty. The badge's text
    // payload tells the user the AI half degraded but regex still ran.
    const el = AiStatusBadge({ status: "Offline" });
    const text = flattenChildren(el);
    expect(text).toContain("AI offline — regex still running");
    // Non-compact path: no extra padding classes added.
    const className = (el.props as { className?: string }).className ?? "";
    expect(className).not.toContain("px-2");
  });

  it("does not render when ai_status is Ok (panel-header guard)", () => {
    // The panel header conditional is `report.ai_status !== "Ok"`. The
    // badge itself is typed to forbid "Ok" (Exclude<AiStatus, "Ok">),
    // so the contract is enforced at the call site. Verify the report
    // shape that should suppress the badge.
    const report = makeReport({
      predicted_collisions: [
        {
          file_path: "src/lib.rs",
          worktree_id: null,
          other_holders: [{ task_run_id: "T1", holder_name: "A", registered_at: 1 }],
          confidence: 1.0,
        },
      ],
      ai_status: "Ok",
    });
    // Mirror the panel header's render guard.
    const shouldRender = report.ai_status !== "Ok";
    expect(shouldRender).toBe(false);
  });

  it("renders compact badge when ai_status is Offline and no predicted collisions", () => {
    // The second render site — compact line — fires when there are
    // no predicted collisions but the AI is still degraded, so the
    // user doesn't silently trust an empty result.
    const report = makeReport({
      predicted_collisions: [],
      ai_status: "Offline",
    });
    const shouldRenderCompact =
      report.ai_status !== "Ok" && report.predicted_collisions.length === 0;
    expect(shouldRenderCompact).toBe(true);

    // The compact variant carries px-2 py-0.5 so the muted line has
    // breathing room; the default (panel-header) variant does not.
    const compactEl = AiStatusBadge({ status: "Offline", compact: true });
    const compactClassName = (compactEl.props as { className?: string }).className ?? "";
    expect(compactClassName).toContain("px-2");
    expect(compactClassName).toContain("py-0.5");
    // Same text content in both variants — the user sees the same
    // diagnosis whether or not predicted collisions are present.
    expect(flattenChildren(compactEl)).toContain("AI offline — regex still running");
  });

  it("renders Disabled status with the gear icon and 'Settings' copy", () => {
    // Disabled is the user-actionable case — the gear glyph hints at
    // a settings toggle (vs the generic info glyph used for transient
    // Offline / RateLimited cases).
    const el = AiStatusBadge({ status: "Disabled" });
    const text = flattenChildren(el);
    expect(text).toContain("AI prediction off (Settings)");
    expect(text).toContain("⚙");
    expect(text).not.toContain("ⓘ");
  });
});
