import { describe, expect, it } from "vitest";

import type { FanoutCapOutcome, FanoutRunView } from "./fanoutApi";
import {
  alreadyCreatedReason,
  buildCreateFanoutRequest,
  collisionProbeLabel,
  defaultTitleTemplate,
  fanoutGate,
  detectPathPlatform,
  isAbsolutePath,
  judgeFanoutCreate,
  planPreview,
  probeKey,
  promptsToProbe,
  rowIsBlocked,
  runWithConcurrency,
  sharedCwdWarning,
} from "./fanoutPlan";
import { MAX_PROMPT_ARGV_COST, type FanoutRow } from "./promptMatrix";
import type { PromptParameter } from "./promptLibraryApi";
import type { ConflictReport } from "./useSessionManager";

const TEMPLATE = {
  body: "Port {{feature}} to {{platform}} in {{lang}}",
  parameters: [
    {
      name: "feature",
      type: "string",
      label: "Feature",
      description: "",
      required: true,
    } satisfies PromptParameter,
  ],
};

function okPreview(matrixText: string, fixed: Record<string, string> = { feature: "login" }) {
  const p = planPreview({
    template: TEMPLATE,
    fixedValues: fixed,
    matrixText,
    mode: "zip",
    titleTemplate: "{{platform}}",
  });
  if (p.kind !== "ok") throw new Error(`expected ok, got ${p.message}`);
  return p.rows;
}

function row(index: number, overrides: Partial<FanoutRow> = {}): FanoutRow {
  return {
    index,
    values: {},
    title: `t${index}`,
    prompt: `p${index}`,
    missing: [],
    errors: [],
    warnings: [],
    ...overrides,
  };
}

const ALL = (n: number) => new Set(Array.from({ length: n }, (_, i) => i));

describe("planPreview", () => {
  it("renders one row per zip member", () => {
    const rows = okPreview("platform:iOS,Android;lang:swift,kotlin");
    expect(rows.map((r) => r.prompt)).toEqual([
      "Port login to iOS in swift",
      "Port login to Android in kotlin",
    ]);
    expect(rows.map((r) => r.title)).toEqual(["iOS", "Android"]);
  });

  it("names a parse error and an unequal zip instead of returning rows", () => {
    const bad = planPreview({
      template: TEMPLATE,
      fixedValues: {},
      matrixText: "platform iOS",
      mode: "zip",
      titleTemplate: "t",
    });
    expect(bad).toEqual({ kind: "error", message: expect.stringContaining('no ":"') });

    const unequal = planPreview({
      template: TEMPLATE,
      fixedValues: {},
      matrixText: "platform:iOS,Android;lang:swift",
      mode: "zip",
      titleTemplate: "t",
    });
    expect(unequal.kind === "error" && unequal.message).toContain("platform=2, lang=1");
  });

  it("product mode multiplies the axes", () => {
    const p = planPreview({
      template: TEMPLATE,
      fixedValues: { feature: "x" },
      matrixText: "platform:iOS,Android;lang:swift,kotlin",
      mode: "product",
      titleTemplate: "{{platform}}-{{lang}}",
    });
    expect(p.kind === "ok" && p.rows.map((r) => r.title)).toEqual([
      "iOS-swift",
      "iOS-kotlin",
      "Android-swift",
      "Android-kotlin",
    ]);
  });

  it("carries a missing required parameter on every row", () => {
    const rows = okPreview("platform:iOS;lang:swift", {});
    expect(rows[0].missing).toEqual(["feature"]);
    expect(rowIsBlocked(rows[0])).toBe(true);
  });
});

describe("defaultTitleTemplate", () => {
  it("names the slug and every axis", () => {
    expect(
      defaultTitleTemplate("port", [
        { name: "platform", values: ["a"] },
        { name: "lang", values: ["b"] },
      ]),
    ).toBe("port — {{platform}} · {{lang}}");
    expect(defaultTitleTemplate("port", [])).toBe("port");
  });
});

describe("fanoutGate", () => {
  const base = {
    workingDir: "/repo",
    maxConcurrent: 3,
    policy: { kind: "bestHeadroom" } as const,
    platform: "posix" as const,
  };

  it("enables Create when every ticked row is clean", () => {
    const g = fanoutGate({ ...base, rows: [row(0), row(1)], ticked: ALL(2) });
    expect(g).toEqual({ canCreate: true, tickedCount: 2, blockedIndices: [], reason: null });
  });

  it("disables Create while a ticked row has an error OR a missing parameter", () => {
    const rows = [
      row(0, { errors: [{ kind: "empty_prompt" }] }),
      row(1, { missing: ["feature"] }),
      row(2),
    ];
    const g = fanoutGate({ ...base, rows, ticked: ALL(3) });
    expect(g.canCreate).toBe(false);
    expect(g.blockedIndices).toEqual([0, 1]);
    expect(g.reason).toContain("#1, #2");
  });

  it("an unticked blocked row does not block Create", () => {
    const rows = [row(0, { missing: ["feature"] }), row(1)];
    const g = fanoutGate({ ...base, rows, ticked: new Set([1]) });
    expect(g.canCreate).toBe(true);
    expect(g.tickedCount).toBe(1);
  });

  it("a warning alone does not block Create", () => {
    const rows = [row(0, { warnings: [{ kind: "identical_prompts", count: 2, message: "same" }] })];
    expect(fanoutGate({ ...base, rows, ticked: ALL(1) }).canCreate).toBe(true);
  });

  it("refuses with nothing ticked, a relative dir, a bad cap, or an empty fixed account", () => {
    const rows = [row(0)];
    expect(fanoutGate({ ...base, rows, ticked: new Set() }).reason).toBe(
      "Tick at least one member",
    );
    expect(fanoutGate({ ...base, rows, ticked: ALL(1), workingDir: " " }).reason).toBe(
      "Set a working directory",
    );
    expect(fanoutGate({ ...base, rows, ticked: ALL(1), workingDir: "rel/dir" }).reason).toContain(
      "absolute",
    );
    expect(fanoutGate({ ...base, rows, ticked: ALL(1), maxConcurrent: 0 }).canCreate).toBe(false);
    expect(fanoutGate({ ...base, rows, ticked: ALL(1), maxConcurrent: 1.5 }).canCreate).toBe(false);
    expect(fanoutGate({ ...base, rows, ticked: ALL(1), maxConcurrent: 2 ** 32 }).reason).toContain(
      "too large",
    );
    expect(fanoutGate({ ...base, rows, ticked: ALL(1), workingDir: "C:\\repo" }).reason).toContain(
      "absolute",
    );
    expect(
      fanoutGate({ ...base, rows, ticked: ALL(1), policy: { kind: "fixed", configDir: "" } })
        .canCreate,
    ).toBe(false);
  });

  it("refuses a row over the UTF-8 byte bound that UTF-16 .length would pass", () => {
    const rows = okPreview("platform:iOS;lang:swift", { feature: "é".repeat(13_000) });
    expect(rows[0].prompt.length).toBeLessThan(MAX_PROMPT_ARGV_COST);
    expect(fanoutGate({ ...base, rows, ticked: ALL(1) }).canCreate).toBe(false);
  });
});

describe("isAbsolutePath", () => {
  it("follows the runner's own platform, as Path::is_absolute does", () => {
    expect(isAbsolutePath("/home/x", "posix")).toBe(true);
    expect(isAbsolutePath("D:\\qontinui-root", "posix")).toBe(false);
    expect(isAbsolutePath("repo", "posix")).toBe(false);

    expect(isAbsolutePath("D:\\qontinui-root", "windows")).toBe(true);
    expect(isAbsolutePath("D:/qontinui-root", "windows")).toBe(true);
    expect(isAbsolutePath("\\\\server\\share", "windows")).toBe(true);
    expect(isAbsolutePath("/home/x", "windows")).toBe(false);
    expect(isAbsolutePath("D:repo", "windows")).toBe(false);
  });

  it("reads the platform from the webview", () => {
    expect(detectPathPlatform("Win32")).toBe("windows");
    expect(detectPathPlatform("Linux x86_64")).toBe("posix");
    expect(detectPathPlatform(undefined)).toBe("posix");
  });
});

describe("buildCreateFanoutRequest", () => {
  it("posts exactly the ticked rows, in index order, titles trimmed, prompts verbatim", () => {
    const rows = [
      row(0, { title: " a ", prompt: "  first\nline " }),
      row(1, { title: "b" }),
      row(2, { title: "c", prompt: "third" }),
    ];
    const req = buildCreateFanoutRequest({
      rows,
      ticked: new Set([2, 0]),
      templateSlug: "port",
      templateVersion: 4,
      maxConcurrent: 1,
      policy: { kind: "fixed", configDir: "/home/u/.claude-a" },
      workingDir: " /repo ",
      tenantId: "t-1",
    });
    expect(req).toEqual({
      tenantId: "t-1",
      templateSlug: "port",
      templateVersion: 4,
      maxConcurrent: 1,
      configDirPolicy: { kind: "fixed", configDir: "/home/u/.claude-a" },
      workingDir: "/repo",
      members: [
        { title: "a", prompt: "  first\nline ", previewIndex: 0 },
        { title: "c", prompt: "third", previewIndex: 2 },
      ],
    });
  });

  it("omits tenantId when none is pinned", () => {
    const req = buildCreateFanoutRequest({
      rows: [row(0)],
      ticked: ALL(1),
      templateSlug: "s",
      templateVersion: 1,
      maxConcurrent: 3,
      policy: { kind: "bestHeadroom" },
      workingDir: "/r",
      tenantId: null,
    });
    expect("tenantId" in req).toBe(false);
  });
});

describe("judgeFanoutCreate", () => {
  function outcome(members: number, clampedFrom: number | null, cap = 3): FanoutCapOutcome {
    const run = {
      id: "abcdef12-0000-0000-0000-000000000000",
      maxConcurrent: cap,
      members: Array.from({ length: members }, (_, i) => ({ index: i })),
    } as unknown as FanoutRunView;
    return { run, fanoutBound: 15, clampedFrom };
  }

  it("is a success only when the run holds every posted member", () => {
    const v = judgeFanoutCreate(2, outcome(2, null));
    expect(v).toEqual({
      ok: true,
      runId: "abcdef12-0000-0000-0000-000000000000",
      message: "Queued 2 members — up to 3 run at once",
      clampNote: null,
    });
  });

  it("reports a short create as a failure naming the shortfall", () => {
    const v = judgeFanoutCreate(3, outcome(2, null));
    expect(v.ok).toBe(false);
    expect(!v.ok && v.message).toContain("2 of 3");
  });

  it("surfaces the server's clamp", () => {
    const v = judgeFanoutCreate(1, outcome(1, 40, 15));
    expect(v.ok && v.clampNote).toBe(
      "Max concurrent clamped from 40 to 15 (this runner's fan-out bound is 15)",
    );
  });
});

describe("sharedCwdWarning", () => {
  it("warns (isolation UNKNOWN) only when more than one member would run", () => {
    expect(sharedCwdWarning(1, "/repo")).toBeNull();
    const w = sharedCwdWarning(3, "/repo");
    expect(w).toContain("UNKNOWN");
    expect(w).toContain("all 3 members share");
    expect(w).toContain("/repo");
  });
});

describe("collisionProbeLabel", () => {
  const report = (n: number, ai: ConflictReport["ai_status"] = "Ok"): ConflictReport => ({
    live_holdings: [],
    predicted_collisions: Array.from({ length: n }, (_, i) => ({
      file_path: `f${i}.rs`,
      other_holders: [],
      confidence: 0.9,
    })),
    recent_editors: [],
    extracted_candidates: [],
    ai_status: ai,
    ai_extracted_count: 0,
    regex_extracted_count: 0,
  });

  it("distinguishes pending, UNKNOWN, collisions and none", () => {
    expect(collisionProbeLabel(undefined).text).toBe("probing…");
    expect(collisionProbeLabel({ kind: "unknown", error: "HTTP 500" })).toMatchObject({
      text: "UNKNOWN",
      title: "Collision probe failed: HTTP 500",
    });
    expect(collisionProbeLabel({ kind: "ok", report: report(2) })).toMatchObject({
      text: "2 collisions",
      tone: "warn",
    });
    expect(collisionProbeLabel({ kind: "ok", report: report(0) }).text).toBe("none");
  });

  it("does not read a degraded extractor's empty answer as a clean none", () => {
    expect(collisionProbeLabel({ kind: "ok", report: report(0, "Offline") }).text).toBe(
      "none (regex only)",
    );
  });
});

describe("alreadyCreatedReason", () => {
  it("blocks a body created earlier even after edits away and back", () => {
    const created = new Map([["body-A", "0123456789abcdef"]]);
    // Un-tick a row (body-B), then re-tick it (body-A again).
    expect(alreadyCreatedReason(created, "body-B")).toBeNull();
    expect(alreadyCreatedReason(created, "body-A")).toMatch(
      /^This exact fan-out was already created \(run 01234567\)/,
    );
  });
});

describe("collision probes", () => {
  it("probes only prompts with no answer and none in flight", () => {
    const answered = { [probeKey("/w", "a")]: { kind: "ok" } };
    const inFlight = new Set([probeKey("/w", "b")]);
    expect(promptsToProbe(["a", "b", "c"], "/w", answered, inFlight)).toEqual(["c"]);
    // Another directory is another probe.
    expect(promptsToProbe(["a"], "/other", answered, inFlight)).toEqual(["a"]);
  });

  it("re-probes a prompt whose last probe failed", () => {
    const answered = {
      [probeKey("/w", "a")]: { kind: "ok" },
      [probeKey("/w", "b")]: { kind: "unknown", error: "HTTP 503" },
    };
    expect(promptsToProbe(["a", "b"], "/w", answered, new Set())).toEqual(["b"]);
    // …unless a retry is already in flight.
    expect(promptsToProbe(["a", "b"], "/w", answered, new Set([probeKey("/w", "b")]))).toEqual([]);
  });

  it("runs at most `limit` at once and every item once", async () => {
    let live = 0;
    let peak = 0;
    const seen: number[] = [];
    await runWithConcurrency([1, 2, 3, 4, 5, 6, 7, 8, 9], 4, async (n) => {
      live++;
      peak = Math.max(peak, live);
      await new Promise((r) => setTimeout(r, 1));
      seen.push(n);
      live--;
    });
    expect(peak).toBe(4);
    expect(seen.sort((a, b) => a - b)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9]);
  });

  it("starts nothing more once aborted", async () => {
    const controller = new AbortController();
    const started: number[] = [];
    await runWithConcurrency(
      [1, 2, 3, 4, 5, 6],
      2,
      async (n) => {
        started.push(n);
        if (n === 2) controller.abort();
        await Promise.resolve();
      },
      controller.signal,
    );
    expect(started).toEqual([1, 2]);
  });
});
