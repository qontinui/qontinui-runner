/**
 * The fleet walk, exercised against a FAKE coord (plan
 * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
 * Phase 4).
 *
 * Until Phase 4 the walk lived inside a React hook and the runner's vitest is
 * `environment: "node"` (no DOM, no renderer), so its rules — cursor only when
 * advancing, a superseded page discarded, a scope mismatch restarted silently —
 * could only be pinned by grepping the hook's source. Lifting one tenant's walk
 * into `FleetTenantWalker` with an injected fetch makes them BEHAVIOUR, asserted
 * here; the merged view (`mergeFleetWalks`) is asserted the same way.
 */

import { describe, it, expect } from "vitest";

import {
  FleetTenantWalker,
  initialFleetWalkSnapshot,
  servedTenantOf,
  stampServedTenant,
  type FleetPageRequest,
  type FleetWalkSnapshot,
} from "./fleetWalker";
import { FLEET_CURSOR_STALLED_MESSAGE, fleetTruncation, type FleetScope } from "./fleetDiscovery";
import { mergeFleetWalks, type FleetSession, type FleetSessionsResponse } from "./useFleetSessions";

const A = "aaaaaaaa-0000-4000-8000-00000000000a";
const B = "bbbbbbbb-0000-4000-8000-00000000000b";
const DEVICE = "22222222-2222-4222-8222-222222222222";

function session(id: string, over: Partial<FleetSession> = {}): FleetSession {
  return {
    sessionId: id,
    deviceId: DEVICE,
    isCallerDevice: false,
    deviceHostname: "box",
    deviceDisplayName: null,
    claudeCodeSessionId: null,
    sessionKind: null,
    intent: null,
    state: "active",
    sessionStatus: null,
    workUnitSlug: null,
    repo: null,
    branch: null,
    provider: null,
    correlationTopic: null,
    startedAt: null,
    lastHeartbeatAt: null,
    closedAt: null,
    ...over,
  };
}

function envelope(over: Partial<FleetSessionsResponse> = {}): FleetSessionsResponse {
  return {
    tenantId: A,
    callerDeviceId: null,
    sessions: [],
    count: 0,
    limit: 100,
    nextCursor: null,
    sessionBridgeColumnPresent: true,
    workAxisColumnsPresent: true,
    deviceIdentityColumnsPresent: true,
    ...over,
  };
}

/** A fetch whose every call is held until the test resolves or rejects it. */
function fakeCoord() {
  const calls: {
    req: FleetPageRequest;
    resolve: (r: FleetSessionsResponse) => void;
    reject: (e: unknown) => void;
  }[] = [];
  const fetchPage = (req: FleetPageRequest) =>
    new Promise<FleetSessionsResponse>((resolve, reject) => {
      calls.push({ req, resolve, reject });
    });
  return { calls, fetchPage };
}

/** Let every queued promise continuation run. */
async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await Promise.resolve();
}

function walker(
  opts: {
    tenant?: string | null;
    fetchPage: (req: FleetPageRequest) => Promise<FleetSessionsResponse>;
    pageSize?: () => number;
    seed?: FleetWalkSnapshot;
    scope?: Partial<FleetScope>;
  },
  log: FleetWalkSnapshot[] = [],
) {
  return new FleetTenantWalker({
    scope: {
      deviceId: null,
      state: null,
      includeClosed: false,
      tenantId: opts.tenant ?? null,
      ...opts.scope,
    },
    fetchPage: opts.fetchPage,
    pageSize: opts.pageSize ?? (() => 100),
    seed: opts.seed,
    onChange: (s) => log.push(s),
  });
}

const scopeMismatch =
  'fleet_sessions_list: coord returned 400 — body: {"error":"cursor_scope_mismatch","detail":"x"}';
const malformed =
  'fleet_sessions_list: coord returned 400 — body: {"error":"cursor_malformed","detail":"x"}';

describe("FleetTenantWalker — the cursor goes on the wire only when advancing", () => {
  it("restarts with no cursor, then advances with coord's cursor verbatim, under its tenant", async () => {
    const coord = fakeCoord();
    const w = walker({ tenant: A, fetchPage: coord.fetchPage, scope: { state: "active" } });
    void w.restart();
    expect(coord.calls[0].req).toEqual({
      deviceId: null,
      state: "active",
      includeClosed: false,
      limit: 100,
      cursor: null,
      tenant: A,
    });
    coord.calls[0].resolve(envelope({ sessions: [session("s1")], nextCursor: "ck-1" }));
    await settle();
    expect(w.canAdvance).toBe(true);

    void w.loadMore();
    expect(coord.calls[1].req.cursor).toBe("ck-1");
    expect(coord.calls[1].req.tenant).toBe(A);
    coord.calls[1].resolve(envelope({ sessions: [session("s2")], nextCursor: null }));
    await settle();
    expect(w.snapshot.walk.sessions.map((s) => s.sessionId)).toEqual(["s1", "s2"]);
    expect(w.snapshot.walk.pages).toBe(2);
    expect(w.canAdvance).toBe(false);
  });

  it("refuses to 'advance' with no cursor instead of re-reading page one", async () => {
    const coord = fakeCoord();
    const w = walker({ fetchPage: coord.fetchPage });
    await w.loadMore();
    expect(coord.calls).toHaveLength(0);
  });

  it("reads the page size at CALL time, so a resize applies to the next page without a restart", async () => {
    const coord = fakeCoord();
    let size = 100;
    const w = walker({ fetchPage: coord.fetchPage, pageSize: () => size });
    void w.restart();
    coord.calls[0].resolve(envelope({ sessions: [session("s1")], nextCursor: "ck-1" }));
    await settle();
    size = 250;
    void w.loadMore();
    expect(coord.calls[1].req).toMatchObject({ limit: 250, cursor: "ck-1" });
  });
});

describe("FleetTenantWalker — a page from a superseded scope is never merged", () => {
  it("discards a response that lost the race to a newer restart", async () => {
    const coord = fakeCoord();
    const w = walker({ fetchPage: coord.fetchPage });
    void w.restart();
    void w.restart();
    coord.calls[1].resolve(envelope({ sessions: [session("new")] }));
    await settle();
    coord.calls[0].resolve(envelope({ sessions: [session("old")] }));
    await settle();
    expect(w.snapshot.walk.sessions.map((s) => s.sessionId)).toEqual(["new"]);
  });

  it("writes nothing after dispose", async () => {
    const coord = fakeCoord();
    const log: FleetWalkSnapshot[] = [];
    const w = walker({ fetchPage: coord.fetchPage }, log);
    void w.restart();
    const before = log.length;
    w.dispose();
    coord.calls[0].resolve(envelope({ sessions: [session("late")] }));
    await settle();
    expect(log).toHaveLength(before);
    await w.restart();
    expect(coord.calls).toHaveLength(1);
  });
});

describe("FleetTenantWalker — refusals", () => {
  it("restarts silently on a scope mismatch for a page that carried a cursor", async () => {
    const coord = fakeCoord();
    const w = walker({ fetchPage: coord.fetchPage });
    void w.restart();
    coord.calls[0].resolve(envelope({ sessions: [session("s1")], nextCursor: "ck-1" }));
    await settle();
    void w.loadMore();
    coord.calls[1].reject(scopeMismatch);
    await settle();
    expect(coord.calls).toHaveLength(3);
    expect(coord.calls[2].req.cursor).toBeNull();
    expect(w.snapshot.error).toBeNull();
  });

  it("reports a scope mismatch on a request with NO cursor rather than restarting for ever", async () => {
    const coord = fakeCoord();
    const w = walker({ fetchPage: coord.fetchPage });
    void w.restart();
    coord.calls[0].reject(scopeMismatch);
    await settle();
    expect(coord.calls).toHaveLength(1);
    expect(w.snapshot.errorCode).toBe("cursor_scope_mismatch");
  });

  it("keeps the rows and drops the cursor when coord refuses it as malformed", async () => {
    const coord = fakeCoord();
    const w = walker({ fetchPage: coord.fetchPage });
    void w.restart();
    coord.calls[0].resolve(envelope({ sessions: [session("s1")], nextCursor: "ck-1" }));
    await settle();
    void w.loadMore();
    coord.calls[1].reject(malformed);
    await settle();
    expect(w.snapshot.walk.sessions).toHaveLength(1);
    expect(w.snapshot.walk.nextCursor).toBeNull();
    expect(w.snapshot.errorCode).toBe("cursor_malformed");
    expect(w.canAdvance).toBe(false);
  });

  it("calls a cursor handed back unchanged a STALLED walk, not a failed read", async () => {
    const coord = fakeCoord();
    const w = walker({ fetchPage: coord.fetchPage });
    void w.restart();
    coord.calls[0].resolve(envelope({ sessions: [session("s1")], nextCursor: "ck-1" }));
    await settle();
    void w.loadMore();
    coord.calls[1].resolve(envelope({ sessions: [session("s2")], nextCursor: "ck-1" }));
    await settle();
    expect(w.snapshot.walkStalled).toBe(true);
    expect(w.snapshot.error).toBe(FLEET_CURSOR_STALLED_MESSAGE);
    expect(w.snapshot.walk.nextCursor).toBeNull();
    expect(w.snapshot.walk.sessions).toHaveLength(2);
  });

  it("keeps the previous read on a failed RESTART, without a cursor it can no longer use", async () => {
    const coord = fakeCoord();
    const seed: FleetWalkSnapshot = {
      ...initialFleetWalkSnapshot(A),
      walk: { sessions: [session("kept")], nextCursor: "old", pages: 1 },
      loaded: true,
    };
    const w = walker({ tenant: A, fetchPage: coord.fetchPage, seed });
    void w.restart();
    // The previous rows stay on screen while page one is read.
    expect(w.snapshot.loading).toBe(true);
    expect(w.snapshot.walk.sessions.map((s) => s.sessionId)).toEqual(["kept"]);
    coord.calls[0].reject("fleet_sessions_list: coord returned 401");
    await settle();
    expect(w.snapshot.walk.sessions.map((s) => s.sessionId)).toEqual(["kept"]);
    expect(w.snapshot.walk.nextCursor).toBeNull();
    expect(w.snapshot.error).toMatch(/401/);
  });

  it("seeds the previous rows WITHOUT their cursor, so no dead 'Load more' shows mid-read", () => {
    const coord = fakeCoord();
    const seed: FleetWalkSnapshot = {
      ...initialFleetWalkSnapshot(A),
      walk: { sessions: [session("kept")], nextCursor: "old-scope-cursor", pages: 1 },
      loaded: true,
    };
    const w = walker({ tenant: A, fetchPage: coord.fetchPage, seed });
    expect(w.snapshot.walk.sessions.map((s) => s.sessionId)).toEqual(["kept"]);
    expect(w.snapshot.walk.nextCursor).toBeNull();
    expect(w.canAdvance).toBe(false);
    void w.restart();
    // Still null while page one is in flight.
    expect(w.snapshot.loading).toBe(true);
    expect(w.snapshot.walk.nextCursor).toBeNull();
  });

  it("ignores a seed from a different tenant", () => {
    const seed: FleetWalkSnapshot = {
      ...initialFleetWalkSnapshot(B),
      walk: { sessions: [session("b")], nextCursor: null, pages: 1 },
    };
    const w = walker({ tenant: A, fetchPage: fakeCoord().fetchPage, seed });
    expect(w.snapshot.walk.sessions).toHaveLength(0);
  });
});

describe("FleetTenantWalker — what a page says about itself", () => {
  it("publishes the applied query in the SAME snapshot as the response, with coord's own limit", async () => {
    const coord = fakeCoord();
    const log: FleetWalkSnapshot[] = [];
    const w = walker(
      { fetchPage: coord.fetchPage, scope: { deviceId: DEVICE, includeClosed: true } },
      log,
    );
    void w.restart();
    coord.calls[0].resolve(envelope({ limit: 500 }));
    await settle();
    const withResponse = log.filter((s) => s.response !== null);
    expect(withResponse.length).toBeGreaterThan(0);
    for (const s of withResponse) {
      expect(s.appliedQuery).toEqual({
        deviceId: DEVICE,
        state: null,
        includeClosed: true,
        limit: 500,
      });
    }
  });

  it("stamps every row with the tenant its page was served under", async () => {
    const coord = fakeCoord();
    const w = walker({ tenant: null, fetchPage: coord.fetchPage });
    void w.restart();
    coord.calls[0].resolve(envelope({ tenantId: B, sessions: [session("s1"), session("s2")] }));
    await settle();
    expect(w.snapshot.walk.sessions.map((s) => s.servedTenantId)).toEqual([B, B]);
  });
});

describe("servedTenantOf / stampServedTenant", () => {
  it("prefers the envelope, falls back to the tenant ASKED for, else null", () => {
    expect(servedTenantOf(envelope({ tenantId: B }), A)).toBe(B);
    expect(servedTenantOf(envelope({ tenantId: "  " }), A)).toBe(A);
    expect(servedTenantOf(envelope({ tenantId: "" }), null)).toBeNull();
    expect(servedTenantOf(null, null)).toBeNull();
  });

  it("does not mutate coord's rows", () => {
    const row = session("s1");
    const out = stampServedTenant(envelope({ sessions: [row] }), null);
    expect(out.sessions[0].servedTenantId).toBe(A);
    expect(row.servedTenantId).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// The union
// ---------------------------------------------------------------------------

function answered(
  tenant: string | null,
  rows: FleetSession[],
  over: Partial<FleetWalkSnapshot> = {},
  env: Partial<FleetSessionsResponse> = {},
): FleetWalkSnapshot {
  const response = stampServedTenant(
    envelope({ tenantId: tenant ?? A, sessions: rows, ...env }),
    tenant,
  );
  return {
    ...initialFleetWalkSnapshot(tenant),
    walk: {
      sessions: response.sessions,
      nextCursor: (env.nextCursor as string | null | undefined) ?? null,
      pages: 1,
    },
    response,
    loaded: true,
    appliedQuery: { deviceId: null, state: null, includeClosed: false, limit: 100 },
    ...over,
  };
}

function failed(tenant: string | null, error = "coord returned 401"): FleetWalkSnapshot {
  return { ...initialFleetWalkSnapshot(tenant), error };
}

describe("mergeFleetWalks — a single walk reads exactly as it always did", () => {
  it("folds to the walk's own error, completeness and empty reason", () => {
    const one = answered(A, [session("s1")], {}, { nextCursor: "ck" });
    const m = mergeFleetWalks([one]);
    expect(m.merged).toBe(false);
    expect(m.sessions).toBe(one.walk.sessions);
    expect(m.truncation).toEqual(fleetTruncation(one.response, 1, true));
    expect(m.servedTenants).toEqual([A]);
    expect(m.readTenants).toEqual({ answered: [A], failed: [] });

    const broken = mergeFleetWalks([{ ...failed(null), errorCode: "unknown_state" }]);
    expect(broken.error).toBe("coord returned 401");
    expect(broken.errorCode).toBe("unknown_state");
    expect(broken.emptyReason).toBe("error");
  });
});

describe("mergeFleetWalks — one tenant's failure never blanks or hides the others", () => {
  it("keeps the answering tenant's rows, names the failed one, and shows no blanket error", () => {
    const m = mergeFleetWalks([failed(A), answered(B, [session("b1")])]);
    expect(m.merged).toBe(true);
    expect(m.sessions.map((s) => s.sessionId)).toEqual(["b1"]);
    expect(m.sessions[0].servedTenantId).toBe(B);
    expect(m.error).toBeNull();
    expect(m.errorCode).toBeNull();
    expect(m.failures).toEqual([
      { requestedTenant: A, error: "coord returned 401", errorCode: null, walkStalled: false },
    ]);
    expect(m.readTenants).toEqual({ answered: [B], failed: [A] });
    expect(m.servedTenants).toEqual([B]);
  });

  it("is never observed-empty while a tenant is unanswered — it is `partial`", () => {
    const m = mergeFleetWalks([failed(A), answered(B, [])]);
    expect(m.sessions).toHaveLength(0);
    expect(m.emptyReason).toBe("partial");
    // An unanswered tenant is not a complete one.
    expect(m.truncation.kind).toBe("unknown");
  });

  it("puts the failure in place of the list only when EVERY tenant failed", () => {
    const m = mergeFleetWalks([failed(A), failed(B)]);
    expect(m.error).toMatch(/No tenant's fleet read succeeded/);
    expect(m.emptyReason).toBe("error");
    expect(m.failures).toHaveLength(2);
  });

  it("waits for every tenant before calling an empty union observed-empty", () => {
    expect(mergeFleetWalks([answered(A, []), initialFleetWalkSnapshot(B)]).emptyReason).toBe(
      "not-loaded",
    );
    expect(mergeFleetWalks([answered(A, []), answered(B, [])]).emptyReason).toBe("observed-empty");
  });

  it("lists a stalled walk as a failure entry without calling it a failed read", () => {
    const stalled = answered(A, [session("a1")], {
      error: FLEET_CURSOR_STALLED_MESSAGE,
      walkStalled: true,
    });
    const m = mergeFleetWalks([stalled, answered(B, [])]);
    expect(m.failures[0].walkStalled).toBe(true);
    expect(m.readTenants).toEqual({ answered: [A, B], failed: [] });
    expect(m.walkStalled).toBe(false);
  });
});

describe("mergeFleetWalks — every count is over the union", () => {
  it("unions rows, de-duplicated by session id, and sums the pages", () => {
    const m = mergeFleetWalks([
      answered(A, [session("s1"), session("s2")]),
      answered(B, [session("s2"), session("s3")]),
    ]);
    expect(m.sessions.map((s) => s.sessionId)).toEqual(["s1", "s2", "s3"]);
    expect(m.pagesLoaded).toBe(2);
    expect(m.servedTenants).toEqual([A, B]);
  });

  it("offers more while ANY walk can advance, counting the union", () => {
    const m = mergeFleetWalks([
      answered(A, [session("a1")], {}, { nextCursor: "ck" }),
      answered(B, [session("b1"), session("b2")]),
    ]);
    expect(m.hasMore).toBe(true);
    expect(m.truncation).toMatchObject({ kind: "more-available", shown: 3 });
  });

  it("is complete only when every walk said last page", () => {
    expect(mergeFleetWalks([answered(A, []), answered(B, [])]).truncation.kind).toBe("none");
  });

  it("is never complete while a walk holding an OLD last-page envelope failed or is re-reading", () => {
    // Each walk still holds a previous read's `nextCursor: null` envelope.
    const refreshFailed = answered(A, [], { error: "coord returned 401" });
    expect(mergeFleetWalks([refreshFailed, answered(B, [])]).truncation.kind).toBe("unknown");
    const reloading = answered(A, [], { loading: true });
    expect(mergeFleetWalks([reloading, answered(B, [])]).truncation.kind).toBe("unknown");
    // A single walk that failed and is NOT re-reading keeps its own envelope.
    expect(mergeFleetWalks([refreshFailed]).truncation.kind).toBe("none");
    // A single walk that IS re-reading has no current envelope either.
    expect(mergeFleetWalks([reloading]).truncation.kind).toBe("unknown");
  });

  it("reads `unknown`, not `unreachable`, while a seeded single walk is loading", () => {
    // A walker seeded across a scope change: the old scope's envelope said
    // "more", but its cursor was dropped, so the walk cannot advance. Until
    // the new scope's first page lands, that is no statement about THIS read.
    const seeded = answered(A, [session("a1")], { loading: true }, { nextCursor: "old-scope" });
    seeded.walk = { ...seeded.walk, nextCursor: null };
    const m = mergeFleetWalks([seeded]);
    expect(m.truncation.kind).toBe("unknown");
    // Once the read settles (not loading), the same snapshot is classified by
    // its envelope as before.
    expect(mergeFleetWalks([{ ...seeded, loading: false }]).truncation.kind).toBe("unreachable");
  });

  it("reads each row's degraded flags off its OWN tenant's envelope", () => {
    const m = mergeFleetWalks([
      answered(A, [session("a1")], {}, { deviceIdentityColumnsPresent: false }),
      answered(B, [session("b1")]),
    ]);
    expect(m.degraded).toBe(true);
    expect(m.envelopes.get(A)?.deviceIdentityColumnsPresent).toBe(false);
    expect(m.envelopes.get(B)?.deviceIdentityColumnsPresent).toBe(true);
  });
});
