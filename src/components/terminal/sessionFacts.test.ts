import { describe, expect, it } from "vitest";

import {
  buildSessionFacts,
  prCiKey,
  prsNeedingCi,
  repoOfPath,
  type PrCiRead,
  type SessionFactsRow,
  type SessionFactsSources,
  type SessionInfoListView,
} from "./sessionFacts";
import type { SessionOrg } from "./sessionOrgTypes";
import type { SessionInfoBody, SessionInfoState, SessionPrs } from "./useSessionInfo";
import { indexPrCi, normalizeInfoList } from "./useSessionFacts";
import { textSearchResultFrom, type TranscriptSearchResponse } from "./transcriptSearch";

const SID = "11111111-1111-1111-1111-111111111111";
const DIR = "/work/agent-worktrees/abc/qontinui-runner";

function row(over: Partial<SessionFactsRow> = {}): SessionFactsRow {
  return {
    sessionId: SID,
    source: "transcript",
    displayName: "first prompt",
    resumeName: "Runner fixes",
    registryName: null,
    firstMessagePreview: "please fix",
    lastMessagePreview: "done",
    workSummaryHint: "Task: fix — Last: done",
    projectPath: "/work/qontinui-coord",
    projectLabel: "qontinui-coord",
    accountLabel: "paktis",
    messageCount: 40,
    startedAt: "2026-10-13T10:00:00Z",
    lastModified: "2026-10-13T11:00:00Z",
    lastMessageTimestamp: "2026-10-13T11:05:00Z",
    status: {
      location: "on-screen",
      activity: "working",
      reason: "r",
      since: null,
      exitCode: null,
    },
    remoteDeviceLabel: null,
    ...over,
  };
}

function prs(over: Partial<SessionPrs> = {}): SessionPrs {
  return {
    status: "ok",
    reason: null,
    opened: [],
    landed: [],
    unknown: [],
    openCount: 0,
    landedCount: 0,
    unknownCount: 0,
    scanned: true,
    scannedRepos: [DIR],
    ...over,
  };
}

function body(
  over: { finishedAt?: number | null; prs?: SessionPrs; workingDir?: string | null } = {},
): SessionInfoBody {
  return {
    identity: {
      claudeSessionId: SID,
      terminalId: "t",
      fleetSessionHandle: null,
      tenantId: null,
      taskRunId: null,
    },
    name: { value: "Runner fixes", source: "operator" },
    account: { label: "paktis", wrapper: "clp", configDir: "/home/x/.claude-paktis" },
    placement: {
      pageId: "p1",
      zoneIndex: 0,
      workingDir: over.workingDir === undefined ? DIR : over.workingDir,
    },
    lifecycle: {
      state: "open",
      provider: "claude",
      origin: "authoritative",
      openedAt: 1_000,
      lastSeenAt: 2_000,
      closedAt: null,
      closeReason: null,
      confirmed: true,
      transcriptExists: true,
      restorable: true,
      restoredFromBootAt: null,
      restoreTier: null,
      bypassPermissions: null,
      finishedAt: over.finishedAt ?? null,
      finishReason: null,
    },
    prs: over.prs ?? prs(),
    tenancy: null,
  };
}

function infoOk(
  b: SessionInfoBody | null = body(),
  branch: "ok" | "detached" | "unknown" = "ok",
): SessionInfoListView {
  const byId = new Map<string, SessionInfoState>();
  if (b) byId.set(SID, { status: "ok", reason: null, body: b });
  return {
    phase: "ok",
    byId,
    notFound: new Set(b ? [] : [SID]),
    branches: new Map([
      [
        DIR,
        {
          workingDir: DIR,
          status: branch,
          branch: branch === "ok" ? "agent/feature" : null,
          reason: branch === "unknown" ? "timeout" : null,
        },
      ],
    ]),
  };
}

const ORG: SessionOrg = {
  version: 1,
  revision: 3,
  groups: [
    { id: "g1", title: "Coord work", colorRole: "info", collapsed: false, order: 0, createdAt: 1 },
  ],
  members: {
    [SID]: {
      groupId: "g1",
      order: 0,
      pinned: true,
      tags: ["urgent"],
      muted: false,
      snoozedUntil: null,
      lastSeenAt: 500,
    },
  },
  recentlyClosed: [],
  recentlyClosedTotal: 0,
  savedViews: [],
  dismissedSuggestions: [],
};

function sources(over: Partial<SessionFactsSources> = {}): SessionFactsSources {
  return {
    row: row(),
    sessionKey: SID,
    claudeSessionId: SID,
    org: { phase: "known", org: ORG },
    info: infoOk(),
    prCi: new Map(),
    fleet: null,
    page: { id: "p1", name: "Main" },
    ...over,
  };
}

function ci(n: number, state: PrCiRead["state"], authorActs: boolean | null = false): PrCiRead {
  return {
    repo: "qontinui/qontinui-runner",
    number: n,
    state,
    authorActs,
    reason: null,
    confidence: "fresh",
    observedAt: 1,
  };
}

describe("buildSessionFacts", () => {
  it("takes text, status, org and info facts from their sources", () => {
    const f = buildSessionFacts(sources());
    expect(f.title).toBe("Runner fixes");
    expect(f.topic).toBe("Task: fix — Last: done");
    expect(f.preview).toBe("please fix done");
    expect(f.location).toEqual({ known: true, value: "on-screen" });
    expect(f.activity).toEqual({ known: true, value: "working" });
    expect(f.pinned).toEqual({ known: true, value: true });
    expect(f.group).toEqual({
      known: true,
      value: { id: "g1", title: "Coord work", colorRole: "info" },
    });
    expect(f.tags).toEqual({ known: true, value: ["urgent"] });
    expect(f.lastSeenAt).toEqual({ known: true, value: 500 });
    expect(f.account).toEqual({ known: true, value: "paktis" });
    // The working dir's repo wins over the transcript project path.
    expect(f.repo).toEqual({ known: true, value: "qontinui-runner" });
    expect(f.branch).toEqual({ known: true, value: "agent/feature" });
    expect(f.finished).toEqual({ known: true, value: false });
    expect(f.page).toEqual({ known: true, value: { id: "p1", name: "Main" } });
    expect(f.messageCount).toEqual({ known: true, value: 40 });
    expect(f.startedAt).toEqual({ known: true, value: Date.parse("2026-10-13T10:00:00Z") });
    expect(f.lastActivityAt).toEqual({ known: true, value: Date.parse("2026-10-13T11:05:00Z") });
  });

  it("an unknown status axis is an unknown fact carrying the classifier's reason", () => {
    const f = buildSessionFacts(
      sources({
        row: row({
          status: {
            location: "unknown",
            activity: "unknown",
            reason: "scan degraded",
            since: null,
            exitCode: null,
          },
        }),
      }),
    );
    expect(f.location).toEqual({ known: false, reason: "scan degraded" });
    expect(f.activity.known).toBe(false);
  });

  it("an unread or unreadable org is unknown for every org fact, never 'ungrouped'", () => {
    for (const org of [
      { phase: "loading" as const },
      { phase: "unknown" as const, reason: "corrupt file" },
    ]) {
      const f = buildSessionFacts(sources({ org }));
      for (const fact of [f.pinned, f.muted, f.snoozedUntil, f.lastSeenAt, f.group, f.tags]) {
        expect(fact.known).toBe(false);
      }
    }
  });

  it("a session with no member row is known-default (ungrouped, unpinned)", () => {
    const f = buildSessionFacts(sources({ sessionKey: "other" }));
    expect(f.group).toEqual({ known: true, value: null });
    expect(f.pinned).toEqual({ known: true, value: false });
    expect(f.tags).toEqual({ known: true, value: [] });
  });

  it("info loading / unavailable / not-found make info facts unknown with the reason", () => {
    for (const [info, reason] of [
      [{ phase: "loading" }, "session info not read yet"],
      [
        { phase: "unavailable", reason: "lifecycle_store_unavailable" },
        "session info unavailable: lifecycle_store_unavailable",
      ],
      [infoOk(null), "no lifecycle record"],
    ] as [SessionInfoListView, string][]) {
      const f = buildSessionFacts(sources({ info }));
      expect(f.finished).toEqual({ known: false, reason });
      expect(f.prs).toEqual({ known: false, reason });
      // Repo still resolves from the transcript's project path.
      expect(f.repo).toEqual({ known: true, value: "qontinui-coord" });
    }
  });

  it("the finished marker comes from the lifecycle record", () => {
    expect(buildSessionFacts(sources({ info: infoOk(body({ finishedAt: 5 })) })).finished).toEqual({
      known: true,
      value: true,
    });
  });

  it("branch: detached is a known null; an unprobed or failed dir is unknown", () => {
    expect(buildSessionFacts(sources({ info: infoOk(body(), "detached") })).branch).toEqual({
      known: true,
      value: null,
    });
    expect(buildSessionFacts(sources({ info: infoOk(body(), "unknown") })).branch.known).toBe(
      false,
    );
    expect(buildSessionFacts(sources({ info: infoOk(body({ workingDir: null })) })).branch).toEqual(
      {
        known: false,
        reason: "no working dir",
      },
    );
  });

  it("PR CI joins the ledger with coord: the ledger's terminal states stand", () => {
    const ledger = prs({
      opened: [
        {
          repo: "qontinui/qontinui-runner",
          prNumber: 1,
          branch: null,
          openedAt: null,
          prState: "open",
        },
        {
          repo: "qontinui/qontinui-runner",
          prNumber: 2,
          branch: null,
          openedAt: null,
          prState: "merged",
        },
        {
          repo: "qontinui/qontinui-runner",
          prNumber: 3,
          branch: null,
          openedAt: null,
          prState: "open",
        },
      ],
      landed: [
        { repo: "qontinui/qontinui-runner", prNumber: 4, landedAt: null, landSignal: "ff-land" },
      ],
    });
    const prCi = new Map([[prCiKey("qontinui/qontinui-runner", 1), ci(1, "green", true)]]);
    const f = buildSessionFacts(sources({ info: infoOk(body({ prs: ledger })), prCi }));
    expect(f.prs.known).toBe(true);
    if (!f.prs.known) return;
    const byN = new Map(f.prs.value.map((p) => [p.number, p]));
    expect(byN.get(1)).toMatchObject({ ci: "green", authorActs: true });
    expect(byN.get(2)).toMatchObject({ ci: "landed" });
    expect(byN.get(3)).toMatchObject({ ci: "unknown", reason: "PR CI not read" });
    expect(byN.get(4)).toMatchObject({ ci: "landed" });
  });

  it("CI not yet fetched is unknown per PR", () => {
    const ledger = prs({
      opened: [{ repo: "r", prNumber: 9, branch: null, openedAt: null, prState: "open" }],
    });
    const f = buildSessionFacts(sources({ info: infoOk(body({ prs: ledger })), prCi: null }));
    expect(f.prs.known && f.prs.value[0]).toMatchObject({
      ci: "unknown",
      reason: "PR CI not read yet",
    });
  });

  it("an unscanned empty ledger asserts nothing; an unavailable one is unknown", () => {
    expect(
      buildSessionFacts(sources({ info: infoOk(body({ prs: prs({ scanned: false }) })) })).prs
        .known,
    ).toBe(false);
    expect(
      buildSessionFacts(
        sources({
          info: infoOk(body({ prs: prs({ status: "unavailable", reason: "db_error" }) })),
        }),
      ).prs,
    ).toEqual({ known: false, reason: "PR ledger unavailable: db_error" });
    expect(buildSessionFacts(sources()).prs).toEqual({ known: true, value: [] });
  });

  it("fleet rows supply device, work unit, repo and branch", () => {
    const f = buildSessionFacts(
      sources({
        fleet: {
          deviceLabel: "laptop",
          sessionStatus: "working",
          workUnitSlug: "plan-x",
          repo: "qontinui-web",
          branch: "b",
        },
      }),
    );
    expect(f.device).toEqual({ known: true, value: "laptop" });
    expect(f.workUnit).toEqual({ known: true, value: "plan-x" });
    expect(f.repo).toEqual({ known: true, value: "qontinui-web" });
    expect(f.branch).toEqual({ known: true, value: "b" });
  });

  it("without a fleet row the work unit is unknown and the device is this device when known", () => {
    const f = buildSessionFacts(sources());
    expect(f.workUnit.known).toBe(false);
    expect(f.device.known).toBe(false);
    expect(buildSessionFacts(sources({ thisDeviceLabel: "spaceship" })).device).toEqual({
      known: true,
      value: "spaceship",
    });
  });

  it("a tab row has no message count; no tab here is a known null page", () => {
    const f = buildSessionFacts(sources({ row: row({ source: "tab" }), page: null }));
    expect(f.messageCount.known).toBe(false);
    expect(f.page).toEqual({ known: true, value: null });
  });

  it("the tab's last output can be the latest activity", () => {
    const at = Date.parse("2026-10-13T12:00:00Z");
    expect(buildSessionFacts(sources({ lastOutputAt: at })).lastActivityAt).toEqual({
      known: true,
      value: at,
    });
  });
});

describe("repoOfPath", () => {
  it("takes the last segment on either separator", () => {
    expect(repoOfPath("C:\\work\\qontinui-runner\\")).toBe("qontinui-runner");
    expect(repoOfPath("/a/b")).toBe("b");
    expect(repoOfPath("")).toBeNull();
    expect(repoOfPath(null)).toBeNull();
  });
});

describe("prsNeedingCi", () => {
  it("asks coord only about PRs the ledger has not settled", () => {
    const ledger = prs({
      opened: [
        { repo: "r", prNumber: 1, branch: null, openedAt: null, prState: "open" },
        { repo: "r", prNumber: 2, branch: null, openedAt: null, prState: "merged" },
        { repo: "r", prNumber: 3, branch: null, openedAt: null, prState: null },
        { repo: "r", prNumber: 4, branch: null, openedAt: null, prState: "open" },
      ],
      landed: [{ repo: "r", prNumber: 4, landedAt: null, landSignal: "github-merge" }],
      unknown: [{ repo: "r", prNumber: 5, reason: "ref_stale" }],
    });
    expect(prsNeedingCi(infoOk(body({ prs: ledger })))).toEqual([
      { repo: "r", number: 1 },
      { repo: "r", number: 3 },
      { repo: "r", number: 5 },
    ]);
    expect(prsNeedingCi({ phase: "loading" })).toEqual([]);
  });
});

describe("normalizeInfoList / indexPrCi", () => {
  it("indexes envelopes by session id and keeps notFound and branches", () => {
    const raw = {
      status: "ok",
      reason: null,
      sessions: [{ available: true, ...body() }],
      notFound: ["ghost"],
      branches: [{ workingDir: DIR, status: "ok", branch: "main", reason: null }],
    };
    const v = normalizeInfoList(raw);
    expect(v.phase).toBe("ok");
    if (v.phase !== "ok") return;
    expect(v.byId.get(SID)?.status).toBe("ok");
    expect(v.byId.get(SID)?.body?.lifecycle.finishedAt).toBeNull();
    expect([...v.notFound]).toEqual(["ghost"]);
    expect(v.branches.get(DIR)?.branch).toBe("main");
  });

  it("a degraded or malformed listing is unavailable with a reason, never empty-ok", () => {
    expect(
      normalizeInfoList({
        status: "unavailable",
        reason: "lifecycle_store_unavailable",
        sessions: [],
      }),
    ).toEqual({
      phase: "unavailable",
      reason: "lifecycle_store_unavailable",
    });
    expect(normalizeInfoList(null)).toEqual({ phase: "unavailable", reason: "malformed_listing" });
    expect(normalizeInfoList({ status: "ok" })).toEqual({
      phase: "unavailable",
      reason: "malformed_listing",
    });
  });

  it("indexes CI reads by repo#number", () => {
    const m = indexPrCi({ prs: [ci(7, "red")] });
    expect(m.get(prCiKey("qontinui/qontinui-runner", 7))?.state).toBe("red");
    expect(indexPrCi(null).size).toBe(0);
  });
});

describe("textSearchResultFrom", () => {
  const resp = (over: Partial<TranscriptSearchResponse> = {}): TranscriptSearchResponse => ({
    terms: ["x"],
    hits: [{ sessionId: "a", configDir: "c", path: "p", hitCount: 1, snippet: "x", mtimeMs: 1 }],
    matched: 1,
    scanned: 2,
    total: 2,
    failed: 0,
    truncated: false,
    truncation: [],
    notScanned: [],
    missing: [],
    elapsedMs: 3,
    ...over,
  });

  it("carries matches, the not-scanned set and the limit flag", () => {
    const r = textSearchResultFrom(
      resp({ truncated: true, truncation: ["time_budget"], notScanned: ["b"] }),
      true,
    );
    expect(r).toMatchObject({ status: "done", limitHit: false, incomplete: false });
    if (r.status !== "done") return;
    expect([...r.matched]).toEqual(["a"]);
    expect([...r.notScanned]).toEqual(["b"]);
  });

  it("an unscoped scan cut short is incomplete; a limit alone is not", () => {
    expect(
      textSearchResultFrom(resp({ truncated: true, truncation: ["time_budget"] }), false),
    ).toMatchObject({
      incomplete: true,
    });
    expect(
      textSearchResultFrom(resp({ truncated: true, truncation: ["limit"] }), false),
    ).toMatchObject({
      incomplete: false,
      limitHit: true,
    });
  });
});
