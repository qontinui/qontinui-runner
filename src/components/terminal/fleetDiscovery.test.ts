/**
 * Unit tests for the Fleet picker's discovery logic (plan
 * `2026-09-11-headless-runner-parity-from-a-headed-runner`, Phases 2 and 5a).
 *
 * The properties under test are the ones the defect turned on: an incomplete
 * list must be reachable, a filtered list must never read as a total, and the
 * client-side text filter must never be mistaken for one that reaches coord.
 *
 * Phase 5a repaired a live contract break. coord's fleet route retired
 * `truncated` for keyset pagination, and the classifier still opened with
 * `if (!response.truncated) return { kind: "none" }` — which reads `!undefined`
 * as "complete", so the banner vanished and the picker silently claimed a full
 * list again. The `nextCursor` tests below are written to FAIL if that
 * semantics is ever restored.
 */

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_FLEET_SERVER_FILTER,
  EMPTY_FLEET_WALK,
  FLEET_COMPLETE_AS_OF_NOW,
  FLEET_CURSOR_STALLED_MESSAGE,
  FLEET_DEFAULT_LIMIT,
  FLEET_MAX_LIMIT,
  FLEET_STATE_VOCABULARY,
  accumulateFleetSessions,
  fleetCountSummary,
  fleetCursorStalled,
  fleetEmptyReadMessage,
  fleetErrorCode,
  fleetErrorInvalidatesCursor,
  fleetErrorIsRestart,
  fleetErrorMessage,
  fleetFilterConflict,
  fleetFilteredOutMessage,
  fleetScopeKey,
  fleetScopeOf,
  fleetScopesEqual,
  fleetSearchTerms,
  fleetSessionHaystack,
  fleetSessionMatchesTerms,
  fleetStateOptions,
  fleetTruncation,
  fleetWalkAccept,
  fleetWalkDropCursor,
  filterFleetSessions,
  hasActiveFleetFilter,
  hasNarrowingFleetFilter,
  hasNarrowingServerFilter,
  isLikelyDeviceId,
  mergeDeviceCatalog,
  mergeStateCatalog,
  normalizeFleetCursor,
  statesSeenIn,
  type FleetDeviceOption,
} from "./fleetDiscovery";
import { devicesSeenIn, type FleetSession, type FleetSessionsResponse } from "./useFleetSessions";

function session(over: Partial<FleetSession> = {}): FleetSession {
  return {
    sessionId: "11111111-1111-1111-1111-111111111111",
    deviceId: "22222222-2222-2222-2222-222222222222",
    isCallerDevice: false,
    deviceHostname: null,
    deviceDisplayName: null,
    claudeCodeSessionId: null,
    sessionKind: null,
    intent: null,
    state: null,
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

function response(over: Partial<FleetSessionsResponse> = {}): FleetSessionsResponse {
  const sessions = over.sessions ?? [];
  return {
    tenantId: "33333333-3333-3333-3333-333333333333",
    callerDeviceId: "22222222-2222-2222-2222-222222222222",
    count: sessions.length,
    limit: FLEET_DEFAULT_LIMIT,
    // Always present as a KEY, null on the last page — exactly as coord serves
    // it, so no test here can accidentally exercise an "absent field" shape the
    // wire never produces.
    nextCursor: null,
    sessionBridgeColumnPresent: true,
    workAxisColumnsPresent: true,
    deviceIdentityColumnsPresent: true,
    ...over,
    sessions,
  };
}

function page(n: number, offset = 0): FleetSession[] {
  return Array.from({ length: n }, (_, i) =>
    session({ sessionId: `s-${offset + i}`, workUnitSlug: `plan-${offset + i}` }),
  );
}

describe("fleetTruncation — completeness is `nextCursor`, and nothing else", () => {
  it("is UNKNOWN before any read completes — not 'none'", () => {
    // The distinction Phase 2 exists for: no answer is not a complete answer.
    expect(fleetTruncation(null, 0)).toEqual({ kind: "unknown" });
  });

  it("is 'none' only when coord handed back no cursor", () => {
    expect(fleetTruncation(response({ sessions: [session()], nextCursor: null }), 1)).toEqual({
      kind: "none",
    });
  });

  it("offers the next page whenever coord handed back a cursor", () => {
    const t = fleetTruncation(response({ sessions: page(100), nextCursor: "ck-1" }), 100);

    expect(t.kind).toBe("more-available");
    if (t.kind !== "more-available") throw new Error("unreachable");
    expect(t.shown).toBe(100);
    expect(t.pageSize).toBe(FLEET_DEFAULT_LIMIT);
    expect(t.message).toContain("100 loaded so far");
    expect(t.message).toMatch(/reachable/i);
  });

  /**
   * THE DISCRIMINATING TEST for the break Phase 5a repaired.
   *
   * coord retired `truncated`, so the field is simply absent from the body the
   * route now serves. The old classifier opened with
   * `if (!response.truncated) return { kind: "none" }`, which reads `!undefined`
   * as `true` — every read classified as complete, the banner never rendered,
   * and the picker went back to silently claiming a full list. This asserts on
   * a response with NO `truncated` key at all (cast, because the interface no
   * longer declares one) and a cursor present: it fails the instant anything
   * consults `truncated` again, and it fails if `nextCursor` is ignored.
   */
  it("classifies a body with NO `truncated` key at all from the cursor alone", () => {
    const wire = response({ sessions: page(100), nextCursor: "ck-1" });
    expect("truncated" in wire).toBe(false);
    expect(fleetTruncation(wire, 100).kind).toBe("more-available");
  });

  it("ignores a stale `truncated` field in BOTH directions", () => {
    // A server, proxy or fixture still carrying the retired flag must not be
    // able to move this classification either way — `nextCursor` is the only
    // completeness signal on the wire, so a `truncated: true` beside a null
    // cursor is complete and a `truncated: false` beside a cursor is not.
    const stale = (truncated: boolean, nextCursor: string | null) =>
      ({ ...response({ sessions: page(3), nextCursor }), truncated }) as FleetSessionsResponse;

    expect(fleetTruncation(stale(true, null), 3).kind).toBe("none");
    expect(fleetTruncation(stale(false, "ck-9"), 3).kind).toBe("more-available");
  });

  it("an empty-string cursor is the LAST page, never a next one", () => {
    // coord's walk protocol says an empty `cursor` is page one, so re-sending
    // one would silently restart the walk while the UI claimed to advance it.
    expect(fleetTruncation(response({ sessions: page(3), nextCursor: "" }), 3).kind).toBe("none");
    expect(normalizeFleetCursor("")).toBeNull();
    expect(normalizeFleetCursor(undefined)).toBeNull();
    expect(normalizeFleetCursor("ck-1")).toBe("ck-1");
  });

  it("a FULL page with no cursor is complete — the row count never decides", () => {
    // A tenant with exactly 100 live sessions and a tenant with a next page
    // look identical by count alone.
    expect(
      fleetTruncation(response({ sessions: page(FLEET_DEFAULT_LIMIT), nextCursor: null }), 100)
        .kind,
    ).toBe("none");
  });

  it("counts what the WALK has loaded, not what the last page held", () => {
    // Three pages in, `response.sessions` holds the last 100 while the list
    // holds 300 — a banner reading "100 loaded so far" over a 300-row list is
    // the same class of false on-screen claim this phase removes.
    const t = fleetTruncation(response({ sessions: page(100, 200), nextCursor: "ck-3" }), 300);
    if (t.kind !== "more-available") throw new Error("unreachable");
    expect(t.shown).toBe(300);
    expect(t.message).toContain("300 loaded so far");
  });

  it("names coord's OWN effective page size, clamp included", () => {
    // `limit` on the response is post-clamp, so a caller that asked for 9999
    // must see the 500 coord actually served rather than what it requested.
    const t = fleetTruncation(
      response({ sessions: page(500), limit: FLEET_MAX_LIMIT, nextCursor: "ck-1" }),
      500,
    );
    if (t.kind !== "more-available") throw new Error("unreachable");
    expect(t.pageSize).toBe(FLEET_MAX_LIMIT);
    expect(t.message).toContain(`next ${FLEET_MAX_LIMIT}`);
  });

  it("falls back to the default page size when coord served no usable limit", () => {
    const t = fleetTruncation(
      { ...response({ sessions: page(5), nextCursor: "ck-1" }), limit: 0 },
      5,
    );
    if (t.kind !== "more-available") throw new Error("unreachable");
    expect(t.pageSize).toBe(FLEET_DEFAULT_LIMIT);
  });

  it("never claims some rows are unreachable — the ceiling copy is retired", () => {
    // The at-the-ceiling arm said 500 sessions in one bucket "cannot be paged
    // further at all". Keyset pagination makes that FALSE, and a false warning
    // is a worse `ux-priorities` gate-4 failure than the silence it replaced.
    const t = fleetTruncation(
      response({ sessions: page(500), limit: FLEET_MAX_LIMIT, nextCursor: "ck-1" }),
      500,
    );
    if (t.kind !== "more-available") throw new Error("unreachable");
    expect(t.message).not.toMatch(/cannot be paged further/i);
    expect(t.message).not.toMatch(/ceiling/i);
    expect(t.message).not.toMatch(/no larger read/i);
  });

  it("says out loud that a complete walk is complete only AS OF NOW", () => {
    expect(FLEET_COMPLETE_AS_OF_NOW).toMatch(/as of its last read/i);
    expect(FLEET_COMPLETE_AS_OF_NOW).toMatch(/sessions started since/i);
  });
});

describe("the cursor is OPAQUE — re-sent verbatim, never interpreted", () => {
  const SOURCES = ["./fleetDiscovery.ts", "./useFleetSessions.ts"].map((rel) => ({
    rel,
    text: readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8"),
  }));

  it("no cursor-carrying module decodes, splits or parses a cursor", () => {
    // coord's contract makes the cursor opaque: a client that learns to read it
    // pins a format coord is free to change, and every such client then breaks
    // silently on the next version bump — which is what
    // `cursor_version_unsupported` exists to report rather than to survive.
    for (const { rel, text } of SOURCES) {
      for (const forbidden of [/atob\s*\(/, /Buffer\.from/, /JSON\.parse/, /decodeURIComponent/]) {
        expect(text, `${rel} must not interpret the cursor: ${forbidden}`).not.toMatch(forbidden);
      }
      // Nor may it slice, split or regex a value named like a cursor.
      expect(text, rel).not.toMatch(/cursor[A-Za-z]*\.(split|slice|substring|match|replace)\(/i);
    }
  });

  it("compares cursors for EQUALITY only, to spot one that never reached coord", () => {
    // A keyset cursor encodes the page just served, so coord returning the
    // cursor it was GIVEN cannot happen under the contract — it is the
    // signature of a parameter dropped in transit, which would otherwise show
    // up as a "load more" that re-serves page one for ever.
    expect(fleetCursorStalled("ck-1", "ck-1")).toBe(true);
    expect(fleetCursorStalled("ck-1", "ck-2")).toBe(false);
    expect(fleetCursorStalled("ck-1", null)).toBe(false);
    // Page one sends no cursor, so there is nothing to have stalled.
    expect(fleetCursorStalled(null, "ck-1")).toBe(false);
    expect(fleetCursorStalled(null, null)).toBe(false);
  });
});

describe("fleetScopeKey — what a cursor is valid within", () => {
  const base = { deviceId: null, state: null, includeClosed: false };

  it.each([
    ["device", { ...base, deviceId: "a" }],
    ["state", { ...base, state: "active" }],
    ["closed", { ...base, includeClosed: true }],
  ])("a changed %s invalidates the cursor", (_name, changed) => {
    expect(fleetScopesEqual(base, changed)).toBe(false);
  });

  it("a changed LIMIT does NOT invalidate the cursor", () => {
    // coord leaves `limit` out of the scope fingerprint on purpose: resizing a
    // page changes the slice, not the sequence. Including it here would restart
    // a walk that did not need restarting and silently re-fetch every page.
    const small = fleetScopeOf({ ...DEFAULT_FLEET_SERVER_FILTER, limit: FLEET_DEFAULT_LIMIT });
    const large = fleetScopeOf({ ...DEFAULT_FLEET_SERVER_FILTER, limit: FLEET_MAX_LIMIT });
    expect(fleetScopesEqual(small, large)).toBe(true);
    expect(fleetScopeKey(small)).not.toMatch(String(FLEET_MAX_LIMIT));
    expect(fleetScopeKey(small)).not.toMatch(String(FLEET_DEFAULT_LIMIT));
  });

  it("distinguishes a null filter from the empty string", () => {
    expect(fleetScopesEqual(base, { ...base, state: "" })).toBe(false);
  });
});

describe("the walk — pages accumulate, a restart replaces", () => {
  it("appends page two onto page one and carries the new cursor", () => {
    const one = fleetWalkAccept(
      EMPTY_FLEET_WALK,
      response({ sessions: page(2), nextCursor: "ck-1" }),
      "restart",
    );
    expect(one.sessions.map((s) => s.sessionId)).toEqual(["s-0", "s-1"]);
    expect(one.nextCursor).toBe("ck-1");
    expect(one.pages).toBe(1);

    const two = fleetWalkAccept(
      one,
      response({ sessions: page(2, 2), nextCursor: "ck-2" }),
      "more",
    );
    expect(two.sessions.map((s) => s.sessionId)).toEqual(["s-0", "s-1", "s-2", "s-3"]);
    expect(two.nextCursor).toBe("ck-2");
    expect(two.pages).toBe(2);
  });

  it("ends the walk on the last page, keeping every row it accumulated", () => {
    let walk = fleetWalkAccept(
      EMPTY_FLEET_WALK,
      response({ sessions: page(2), nextCursor: "ck-1" }),
      "restart",
    );
    walk = fleetWalkAccept(walk, response({ sessions: page(1, 2), nextCursor: null }), "more");
    expect(walk.nextCursor).toBeNull();
    expect(walk.sessions).toHaveLength(3);
    expect(walk.pages).toBe(2);
  });

  it("a RESTART replaces the accumulation — two scopes never share a list", () => {
    // The trap: a device filter narrows the walk, so carrying the previous
    // scope's rows over would build a list that no filter set on screen
    // describes, and the count beside it would be a claim about neither.
    const first = fleetWalkAccept(
      EMPTY_FLEET_WALK,
      response({ sessions: page(3), nextCursor: "ck-1" }),
      "restart",
    );
    const restarted = fleetWalkAccept(
      first,
      response({ sessions: [session({ sessionId: "other" })], nextCursor: null }),
      "restart",
    );
    expect(restarted.sessions.map((s) => s.sessionId)).toEqual(["other"]);
    expect(restarted.pages).toBe(1);
    expect(restarted.nextCursor).toBeNull();
  });

  it("deduplicates by sessionId, keeping the later read of a repeated row", () => {
    const one = fleetWalkAccept(
      EMPTY_FLEET_WALK,
      response({ sessions: [session({ sessionId: "a", state: "active" })], nextCursor: "ck-1" }),
      "restart",
    );
    const two = fleetWalkAccept(
      one,
      response({
        sessions: [session({ sessionId: "a", state: "stale" }), session({ sessionId: "b" })],
        nextCursor: null,
      }),
      "more",
    );
    expect(two.sessions).toHaveLength(2);
    // In place: moving the row would reorder a list the operator is reading.
    expect(two.sessions[0]?.sessionId).toBe("a");
    expect(two.sessions[0]?.state).toBe("stale");
  });

  it("an empty page keeps the array identity, so a consumer's memo survives", () => {
    const one = fleetWalkAccept(
      EMPTY_FLEET_WALK,
      response({ sessions: page(2), nextCursor: "ck-1" }),
      "restart",
    );
    const two = fleetWalkAccept(one, response({ sessions: [], nextCursor: null }), "more");
    expect(two.sessions).toBe(one.sessions);
    expect(accumulateFleetSessions(one.sessions, [])).toBe(one.sessions);
  });

  it("drops the cursor without touching the rows", () => {
    const one = fleetWalkAccept(
      EMPTY_FLEET_WALK,
      response({ sessions: page(2), nextCursor: "ck-1" }),
      "restart",
    );
    const stalled = fleetWalkDropCursor(one);
    expect(stalled.nextCursor).toBeNull();
    expect(stalled.sessions).toBe(one.sessions);
    // And it is a no-op when there was no cursor, preserving identity.
    expect(fleetWalkDropCursor(stalled)).toBe(stalled);
  });

  it("the empty walk is 'unknown', never 'none'", () => {
    expect(EMPTY_FLEET_WALK.pages).toBe(0);
    expect(EMPTY_FLEET_WALK.sessions).toHaveLength(0);
    expect(EMPTY_FLEET_WALK.nextCursor).toBeNull();
    expect(fleetTruncation(null, EMPTY_FLEET_WALK.sessions.length)).toEqual({ kind: "unknown" });
  });
});

describe("a whole walk, driven the way the hook drives it", () => {
  /**
   * The sequence the hook performs, run over the same exported primitives it
   * calls — not a re-implementation of the rules, which is the failure mode
   * `fleet_sessions.rs`'s own tests record: a test that restates a predicate
   * passes happily while the real path diverges. What is NOT reachable here is
   * React's part (the generation guard and the restart effect); those are
   * asserted against the hook's source in `FleetSessionPicker.wiring.test.ts`,
   * because the runner's vitest environment is `node` with no DOM.
   */
  function walkPages(pages: { rows: FleetSession[]; nextCursor: string | null }[]) {
    let walk = EMPTY_FLEET_WALK;
    let sent: string | null = null;
    const cursorsSent: (string | null)[] = [];
    for (const [i, p] of pages.entries()) {
      cursorsSent.push(sent);
      const r = response({ sessions: p.rows, nextCursor: p.nextCursor });
      walk = fleetWalkAccept(walk, r, i === 0 ? "restart" : "more");
      sent = fleetCursorStalled(sent, walk.nextCursor) ? null : walk.nextCursor;
      if (sent === null) walk = fleetWalkDropCursor(walk);
    }
    return { walk, cursorsSent };
  }

  it("accumulates three pages, sends each cursor verbatim, and stops on the last", () => {
    const { walk, cursorsSent } = walkPages([
      { rows: page(2, 0), nextCursor: "ck-1" },
      { rows: page(2, 2), nextCursor: "ck-2" },
      { rows: page(1, 4), nextCursor: null },
    ]);
    expect(cursorsSent).toEqual([null, "ck-1", "ck-2"]);
    expect(walk.sessions.map((s) => s.sessionId)).toEqual(["s-0", "s-1", "s-2", "s-3", "s-4"]);
    expect(walk.pages).toBe(3);
    expect(fleetTruncation(response({ nextCursor: null }), walk.sessions.length).kind).toBe("none");
  });

  it("stops instead of looping when the cursor never reaches coord", () => {
    // The shape of a dropped parameter: page one comes back for ever, each time
    // with the same cursor. The dedup keeps the list honest and the stall check
    // ends the walk rather than offering a control that cannot advance.
    const { walk, cursorsSent } = walkPages([
      { rows: page(2, 0), nextCursor: "ck-1" },
      { rows: page(2, 0), nextCursor: "ck-1" },
    ]);
    expect(cursorsSent).toEqual([null, "ck-1"]);
    expect(walk.sessions).toHaveLength(2);
    expect(walk.nextCursor).toBeNull();
  });

  it("a scope change restarts the walk instead of appending across filters", () => {
    // Coord answers `cursor_scope_mismatch` to a cursor carried across a filter
    // change; the hook drops the cursor and restarts, which here is the
    // `restart` fold — the previous scope's rows do not survive it.
    const first = walkPages([
      { rows: page(3, 0), nextCursor: "ck-1" },
      { rows: page(3, 3), nextCursor: "ck-2" },
    ]).walk;
    expect(first.sessions).toHaveLength(6);

    const code = fleetErrorCode(
      'GET /coord/sessions/fleet returned 400 — body: {"error":"cursor_scope_mismatch","detail":"x"}',
    );
    expect(fleetErrorIsRestart(code)).toBe(true);

    const restarted = fleetWalkAccept(
      first,
      response({ sessions: [session({ sessionId: "narrowed" })], nextCursor: null }),
      "restart",
    );
    expect(restarted.sessions.map((s) => s.sessionId)).toEqual(["narrowed"]);
    expect(restarted.pages).toBe(1);
  });
});

describe("coord's 400 codes — the code is the contract, the prose is not", () => {
  const body = (code: string) =>
    `GET /coord/sessions/fleet returned 400 — body: {"error":"${code}","detail":"static prose"}`;

  it.each([
    "cursor_scope_mismatch",
    "cursor_malformed",
    "cursor_version_unsupported",
    "limit_not_positive",
    "unknown_state",
  ])("recovers %s from the wrapped body", (code) => {
    expect(fleetErrorCode(body(code))).toBe(code);
  });

  it("reads the `error` KEY, not a loose occurrence of the token", () => {
    // A code name inside `detail`'s prose, or in a url, is not the verdict.
    expect(
      fleetErrorCode(
        'returned 400 — body: {"error":"limit_not_positive","detail":"not a cursor_malformed"}',
      ),
    ).toBe("limit_not_positive");
    expect(fleetErrorCode("GET /coord/sessions/fleet?cursor_malformed=1: timed out")).toBeNull();
  });

  it("is null for a transport failure, which is UNKNOWN rather than a code", () => {
    expect(fleetErrorCode("GET https://coord/coord/sessions/fleet: connection refused")).toBeNull();
    expect(fleetErrorCode(new Error("boom"))).toBeNull();
    expect(fleetErrorCode(undefined)).toBeNull();
  });

  it("does not invent a code for an unknown one coord might add", () => {
    expect(fleetErrorCode(body("cursor_expired"))).toBeNull();
  });

  it("never matches the RETIRED free-text limit body", () => {
    // The non-positive-limit body changed shape from `{"error":"limit must be
    // positive"}` to a machine code — a deliberate break, and the reason
    // nothing here matches on prose.
    expect(fleetErrorCode('returned 400 — body: {"error":"limit must be positive"}')).toBeNull();
  });

  it("routes a scope mismatch to a RESTART, never to the operator's screen", () => {
    // Changing a filter mid-walk is ordinary use; showing it as an error would
    // blame the operator for the UI's own race.
    expect(fleetErrorIsRestart("cursor_scope_mismatch")).toBe(true);
    for (const other of [
      "cursor_malformed",
      "cursor_version_unsupported",
      "limit_not_positive",
      "unknown_state",
    ]) {
      expect(fleetErrorIsRestart(other as never)).toBe(false);
    }
    expect(fleetErrorIsRestart(null)).toBe(false);
  });

  it("invalidates the cursor for exactly the two codes that mean it is unusable", () => {
    expect(fleetErrorInvalidatesCursor("cursor_malformed")).toBe(true);
    expect(fleetErrorInvalidatesCursor("cursor_version_unsupported")).toBe(true);
    expect(fleetErrorInvalidatesCursor("limit_not_positive")).toBe(false);
    expect(fleetErrorInvalidatesCursor("cursor_scope_mismatch")).toBe(false);
    expect(fleetErrorInvalidatesCursor("unknown_state")).toBe(false);
    expect(fleetErrorInvalidatesCursor(null)).toBe(false);
  });

  it("says the list may be INCOMPLETE when a bad cursor stops the walk", () => {
    // Dropping the cursor silently would turn an incomplete list into one that
    // claims to be complete — the exact defect this phase repaired.
    for (const code of ["cursor_malformed", "cursor_version_unsupported"] as const) {
      const msg = fleetErrorMessage(code, "raw");
      expect(msg).toMatch(/there may be more/i);
      expect(msg).toMatch(/refresh/i);
    }
  });

  it("surfaces an uncoded failure raw rather than guessing at it", () => {
    expect(fleetErrorMessage(null, "connection refused")).toBe(
      "Failed to load fleet sessions: connection refused",
    );
  });

  it("says the page size must be positive, without echoing coord's prose", () => {
    expect(fleetErrorMessage("limit_not_positive", "raw")).toMatch(/positive/i);
  });

  it("names an unknown state as a FILTER problem with a way out, not a raw failure", () => {
    // Before qontinui-coord#2085 an unknown `?state=` answered 200 with an empty
    // page. Now it is a typed 400, and falling through to the raw-failure arm
    // would read as coord being down rather than as a filter it cannot match.
    const msg = fleetErrorMessage("unknown_state", "raw");
    expect(msg).not.toMatch(/^Failed to load fleet sessions/);
    expect(msg).toMatch(/state filter/i);
    expect(msg).toMatch(/clear/i);
  });

  it("the stalled-cursor message admits the list may be incomplete", () => {
    expect(FLEET_CURSOR_STALLED_MESSAGE).toMatch(/cannot advance/i);
    expect(FLEET_CURSOR_STALLED_MESSAGE).toMatch(/may be incomplete/i);
  });
});

describe("text filtering — AND over every searchable field", () => {
  const rows = [
    session({
      sessionId: "aaaa1111-0000-0000-0000-000000000001",
      workUnitSlug: "coord-merge-train-fix",
      repo: "qontinui-coord",
      branch: "main",
      state: "active",
    }),
    session({
      sessionId: "bbbb2222-0000-0000-0000-000000000002",
      intent: "rewrite the fleet picker",
      repo: "qontinui-web",
      branch: "feat/picker",
      state: "stale",
      provider: "claude",
    }),
    session({
      sessionId: "cccc3333-0000-0000-0000-000000000003",
      repo: "qontinui-web",
      branch: "main",
      deviceHostname: "merytshost",
      state: "active",
    }),
  ];

  it("an empty query keeps every row (and the same array)", () => {
    expect(filterFleetSessions(rows, "")).toBe(rows);
    expect(filterFleetSessions(rows, "   ")).toBe(rows);
  });

  it("matches case-insensitively across fields", () => {
    expect(filterFleetSessions(rows, "MERGE-TRAIN")).toHaveLength(1);
    expect(filterFleetSessions(rows, "merytshost")).toHaveLength(1);
  });

  it("ANDs terms, so every added word narrows", () => {
    expect(filterFleetSessions(rows, "qontinui-web")).toHaveLength(2);
    expect(filterFleetSessions(rows, "qontinui-web main")).toHaveLength(1);
    expect(filterFleetSessions(rows, "qontinui-web main nonsense")).toHaveLength(0);
  });

  it("finds a row by a pasted session id", () => {
    const hit = filterFleetSessions(rows, "bbbb2222-0000-0000-0000-000000000002");
    expect(hit).toHaveLength(1);
    expect(hit[0]?.repo).toBe("qontinui-web");
  });

  it("never matches the literal string 'null' for an absent field", () => {
    // coord's nulls are UNKNOWN; inventing the word "null" for them would let a
    // search match rows on a value they do not have.
    const bare = session();
    expect(fleetSessionHaystack(bare)).not.toContain("null");
    expect(fleetSessionMatchesTerms(bare, ["null"])).toBe(false);
  });

  it("splits a query on any run of whitespace", () => {
    expect(fleetSearchTerms("  a\t b  c ")).toEqual(["a", "b", "c"]);
    expect(fleetSearchTerms("")).toEqual([]);
  });
});

describe("mergeDeviceCatalog — the device list only ever grows", () => {
  const a: FleetDeviceOption = {
    deviceId: "a",
    label: "alpha",
    isCallerDevice: false,
    labelIsFallback: false,
  };
  const b: FleetDeviceOption = {
    deviceId: "b",
    label: "bravo",
    isCallerDevice: true,
    labelIsFallback: false,
  };

  it("keeps devices a narrowed read no longer returns", () => {
    // This is the trap the merge exists for: after selecting device `a`, coord's
    // next response holds only `a`, and recomputing would leave no way back.
    const merged = mergeDeviceCatalog([a, b], [a]);
    expect(merged.map((d) => d.deviceId).sort()).toEqual(["a", "b"]);
  });

  it("puts the caller's own device first, then sorts by label", () => {
    const merged = mergeDeviceCatalog(
      [],
      [{ deviceId: "z", label: "zulu", isCallerDevice: false, labelIsFallback: false }, a, b],
    );
    expect(merged.map((d) => d.deviceId)).toEqual(["b", "a", "z"]);
  });

  it("lets a real label replace the degraded-read id placeholder", () => {
    const placeholder: FleetDeviceOption = {
      deviceId: "a",
      label: "device abcd1234",
      isCallerDevice: false,
      labelIsFallback: true,
    };
    const merged = mergeDeviceCatalog([placeholder], [a]);
    expect(merged[0]?.label).toBe("alpha");
    expect(merged[0]?.labelIsFallback).toBe(false);
  });

  it("decides by the FLAG, not by the label's text", () => {
    // An operator may legitimately name a device "device farm 2", which a
    // startsWith("device ") sniff would misread as the id placeholder and
    // happily overwrite.
    const namedLikeAPlaceholder: FleetDeviceOption = {
      deviceId: "a",
      label: "device farm 2",
      isCallerDevice: false,
      labelIsFallback: false,
    };
    const realPlaceholder: FleetDeviceOption = {
      deviceId: "a",
      label: "device abcd1234",
      isCallerDevice: false,
      labelIsFallback: true,
    };
    expect(mergeDeviceCatalog([namedLikeAPlaceholder], [realPlaceholder])[0]?.label).toBe(
      "device farm 2",
    );
  });

  it("returns the SAME array when a read adds nothing", () => {
    const once = mergeDeviceCatalog([], [a, b]);
    expect(mergeDeviceCatalog(once, [a, b])).toBe(once);
  });

  it("does not let a placeholder overwrite a real label", () => {
    const placeholder: FleetDeviceOption = {
      deviceId: "a",
      label: "device abcd1234",
      isCallerDevice: false,
      labelIsFallback: true,
    };
    const merged = mergeDeviceCatalog([a], [placeholder]);
    expect(merged[0]?.label).toBe("alpha");
  });

  it("is idempotent", () => {
    const once = mergeDeviceCatalog([], [a, b]);
    expect(mergeDeviceCatalog(once, [a, b])).toEqual(once);
  });
});

describe("devicesSeenIn — the filter options a page of rows supports", () => {
  it("returns one entry per device, labelled", () => {
    const seen = devicesSeenIn([
      session({ deviceId: "a", deviceDisplayName: "alpha" }),
      session({ deviceId: "a", deviceDisplayName: "alpha" }),
      session({ deviceId: "b", deviceHostname: "bravo-host", isCallerDevice: true }),
    ]);
    expect(seen).toEqual([
      { deviceId: "a", label: "alpha", isCallerDevice: false, labelIsFallback: false },
      { deviceId: "b", label: "bravo-host", isCallerDevice: true, labelIsFallback: false },
    ]);
  });

  it("is empty for an empty page — which the caller must not merge as fact", () => {
    expect(devicesSeenIn([])).toEqual([]);
  });

  it("feeds mergeDeviceCatalog without losing a previously seen device", () => {
    // The end-to-end shape of the trap: read everything, then narrow to one
    // device, and the dropdown must still offer the other.
    const all = devicesSeenIn([
      session({ deviceId: "a", deviceDisplayName: "alpha" }),
      session({ deviceId: "b", deviceDisplayName: "bravo" }),
    ]);
    const narrowed = devicesSeenIn([session({ deviceId: "a", deviceDisplayName: "alpha" })]);
    const catalog = mergeDeviceCatalog(mergeDeviceCatalog([], all), narrowed);
    expect(catalog.map((d) => d.deviceId)).toEqual(["a", "b"]);
  });
});

describe("fleetStateOptions — the known vocabulary, plus whatever coord served", () => {
  it("offers coord's whole vocabulary even when nothing has been seen", () => {
    expect(fleetStateOptions([], null)).toEqual([...FLEET_STATE_VOCABULARY]);
  });

  it("appends a state coord emitted that this build does not know about", () => {
    // The vocabulary is enforced in Rust, not by a DB constraint, so it can
    // evolve without a migration — a pinned list would silently hide the new
    // value, which is the same quiet omission this phase is fixing.
    const opts = fleetStateOptions(["quiescing", "active"], null);
    expect(opts).toContain("quiescing");
    expect(opts.filter((v) => v === "active")).toHaveLength(1);
  });

  it("ALWAYS offers the selected value, even when no row carries it", () => {
    // Otherwise a controlled <select> whose value matches no <option> renders
    // as the first one — "Any state" — while the request in flight still
    // carries the filter: the control showing one query, coord answering
    // another.
    expect(fleetStateOptions([], "quiescing")).toContain("quiescing");
  });

  it("ignores blank values rather than offering an unselectable option", () => {
    expect(fleetStateOptions(["  "], null)).toEqual([...FLEET_STATE_VOCABULARY]);
  });
});

describe("hasActiveFleetFilter — drives whether a reset is offered at all", () => {
  it("is false for the picker's default state", () => {
    expect(hasActiveFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "")).toBe(false);
  });

  it.each([
    ["device", { ...DEFAULT_FLEET_SERVER_FILTER, deviceId: "a" }, ""],
    ["state", { ...DEFAULT_FLEET_SERVER_FILTER, state: "active" }, ""],
    ["closed", { ...DEFAULT_FLEET_SERVER_FILTER, includeClosed: true }, ""],
    ["limit", { ...DEFAULT_FLEET_SERVER_FILTER, limit: 250 }, ""],
    ["text", DEFAULT_FLEET_SERVER_FILTER, "web"],
  ])("is true when %s is set", (_name, server, text) => {
    expect(hasActiveFleetFilter(server, text)).toBe(true);
  });

  it("treats a whitespace-only query as no filter", () => {
    expect(hasActiveFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "   ")).toBe(false);
  });
});

describe("fleetCountSummary — a filtered subset is never shown as a total", () => {
  it("reports a plain count when nothing is filtered out", () => {
    expect(
      fleetCountSummary({ matched: 3, loaded: 3, devices: 2, devicesLoaded: 2, remote: 1 }),
    ).toBe("3 sessions on 2 devices · 1 remote");
  });

  it("says 'of N loaded' the moment a filter hides anything", () => {
    expect(
      fleetCountSummary({ matched: 2, loaded: 47, devices: 1, devicesLoaded: 5, remote: 0 }),
    ).toBe("2 of 47 loaded on 1 of 5 devices");
  });

  it("gets the singular right", () => {
    expect(
      fleetCountSummary({ matched: 1, loaded: 1, devices: 1, devicesLoaded: 1, remote: 0 }),
    ).toBe("1 session on 1 device");
  });

  it("omits the remote clause when every row is local", () => {
    expect(
      fleetCountSummary({ matched: 2, loaded: 2, devices: 1, devicesLoaded: 1, remote: 0 }),
    ).not.toContain("remote");
  });
});

describe("fleetFilteredOutMessage — 'your filter matched nothing' ≠ 'the fleet is empty'", () => {
  it("explains a text filter that hid every loaded row", () => {
    const msg = fleetFilteredOutMessage(42, 0, "zzz");
    expect(msg).toContain("“zzz”");
    expect(msg).toContain("42 sessions are loaded");
    expect(msg).toMatch(/filters only those/i);
  });

  it("is silent when rows are visible", () => {
    expect(fleetFilteredOutMessage(42, 5, "zzz")).toBeNull();
  });

  it("is silent when the read itself was empty — that answer belongs elsewhere", () => {
    // `emptyReasonFor` owns that case: it is the only thing that can tell a
    // failed read from an observed-empty fleet.
    expect(fleetFilteredOutMessage(0, 0, "zzz")).toBeNull();
  });

  it("is silent when there is no text query to blame", () => {
    expect(fleetFilteredOutMessage(42, 0, "  ")).toBeNull();
  });
});

describe("fleetFilterConflict — a self-starving filter pair is named, not silently obeyed", () => {
  it("flags state=closed while closed sessions are excluded", () => {
    // coord appends `AND s.closed_at IS NULL` for include_closed=false, so the
    // pair very nearly excludes itself and the empty list reads as a fact about
    // the fleet rather than about the filters.
    const msg = fleetFilterConflict({
      ...DEFAULT_FLEET_SERVER_FILTER,
      state: "closed",
    });
    expect(msg).toMatch(/closed/i);
    expect(msg).toMatch(/turn on/i);
  });

  it("is silent once closed sessions are included", () => {
    expect(
      fleetFilterConflict({
        ...DEFAULT_FLEET_SERVER_FILTER,
        state: "closed",
        includeClosed: true,
      }),
    ).toBeNull();
  });

  it("is silent for every other state", () => {
    for (const state of ["active", "stale", "expected", "pending_resolution", null]) {
      expect(fleetFilterConflict({ ...DEFAULT_FLEET_SERVER_FILTER, state })).toBeNull();
    }
  });
});

describe("hasNarrowingFleetFilter — only a narrowing filter can explain an empty list", () => {
  it("is false for the default state", () => {
    expect(hasNarrowingFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "")).toBe(false);
  });

  it.each([
    ["device", { ...DEFAULT_FLEET_SERVER_FILTER, deviceId: "a" }, ""],
    ["state", { ...DEFAULT_FLEET_SERVER_FILTER, state: "active" }, ""],
    ["text", DEFAULT_FLEET_SERVER_FILTER, "web"],
  ])("is true when %s narrows the read", (_name, server, text) => {
    expect(hasNarrowingFleetFilter(server, text)).toBe(true);
  });

  it.each([
    ["a raised limit", { ...DEFAULT_FLEET_SERVER_FILTER, limit: 250 }],
    ["including closed sessions", { ...DEFAULT_FLEET_SERVER_FILTER, includeClosed: true }],
  ])("is false for %s, which WIDENS the read", (_name, server) => {
    // Blaming an empty result on a filter that widened the query would be a
    // false explanation of an honest zero.
    expect(hasNarrowingFleetFilter(server, "")).toBe(false);
    expect(hasActiveFleetFilter(server, "")).toBe(true);
  });
});

describe("a term never spans two haystack fields", () => {
  it("does not match across the join between repo and branch", () => {
    // The haystack is a space-joined string, so a naive reader might expect
    // "web feat" to match repo="qontinui-web" branch="feat/x" as one run. It
    // must not: each TERM has to land inside some field, or the box would
    // match on an artefact of field ORDER, which nothing guarantees. The
    // newline join in `fleetSessionHaystack` is what makes it impossible.
    const s = session({ repo: "qontinui-web", branch: "feat/picker" });
    expect(fleetSessionMatchesTerms(s, ["web feat"])).toBe(false);
    // The same two words as separate terms DO match, because each lands in a
    // field of its own.
    expect(fleetSessionMatchesTerms(s, ["web", "feat"])).toBe(true);
  });

  it("no term the search box produces can contain whitespace at all", () => {
    // Which is why the newline join above closes the hole for every query a
    // user can actually type.
    for (const term of fleetSearchTerms("  qontinui-web   feat/picker\tmain ")) {
      expect(term).not.toMatch(/\s/);
    }
  });
});

describe("statesSeenIn / mergeStateCatalog — the state list accumulates too", () => {
  it("collects distinct non-blank states", () => {
    expect(
      statesSeenIn([
        session({ state: "active" }),
        session({ state: "active" }),
        session({ state: " " }),
        session({ state: null }),
        session({ state: "quiescing" }),
      ]).sort(),
    ).toEqual(["active", "quiescing"]);
  });

  it("keeps a state a narrowed read no longer returns", () => {
    // Same trap as the device catalogue: filtering to state=active makes
    // coord's next response carry only that state.
    const merged = mergeStateCatalog(["active", "quiescing"], ["active"]);
    expect(merged).toEqual(["active", "quiescing"]);
  });

  it("returns the SAME array when nothing is new", () => {
    const once = mergeStateCatalog([], ["active"]);
    expect(mergeStateCatalog(once, ["active"])).toBe(once);
  });
});

describe("hasNarrowingServerFilter — only what coord SAW can explain coord's zero", () => {
  it("is false for the default filter", () => {
    expect(hasNarrowingServerFilter(DEFAULT_FLEET_SERVER_FILTER)).toBe(false);
  });

  it("is false for a text query, which is never sent to coord", () => {
    // The distinction finding 3 turned on: `hasNarrowingFleetFilter` counts the
    // text box (correct for the client-side plane), and using THAT to explain a
    // zero-row RESPONSE would conceal a genuinely empty fleet behind an
    // invented cause.
    expect(hasNarrowingFleetFilter(DEFAULT_FLEET_SERVER_FILTER, "foo")).toBe(true);
    expect(hasNarrowingServerFilter(DEFAULT_FLEET_SERVER_FILTER)).toBe(false);
  });

  it.each([
    ["device", { ...DEFAULT_FLEET_SERVER_FILTER, deviceId: "a" }],
    ["state", { ...DEFAULT_FLEET_SERVER_FILTER, state: "active" }],
  ])("is true for a %s filter", (_name, server) => {
    expect(hasNarrowingServerFilter(server)).toBe(true);
  });
});

describe("fleetEmptyReadMessage — three different facts, never merged", () => {
  it("blames the filters only when coord actually had some", () => {
    const r = fleetEmptyReadMessage({ ...DEFAULT_FLEET_SERVER_FILTER, state: "active" }, "");
    expect(r.message).toMatch(/what coord returned for them/i);
    expect(r.offerClear).toBe(true);
  });

  it("reports a real empty fleet when coord was asked with no narrowing filter", () => {
    const r = fleetEmptyReadMessage(DEFAULT_FLEET_SERVER_FILTER, "");
    expect(r.message).toBe("No open sessions anywhere on the fleet.");
    expect(r.offerClear).toBe(false);
  });

  it("clears the text box of suspicion instead of blaming it", () => {
    const r = fleetEmptyReadMessage(DEFAULT_FLEET_SERVER_FILTER, "foo");
    expect(r.message).toMatch(/No open sessions anywhere on the fleet/);
    expect(r.message).toMatch(/not sent to coord/i);
    expect(r.message).not.toMatch(/what coord returned for them/i);
  });

  it("a raised limit alone never makes the list read as filtered", () => {
    const r = fleetEmptyReadMessage({ ...DEFAULT_FLEET_SERVER_FILTER, limit: 500 }, "");
    expect(r.message).toBe("No open sessions anywhere on the fleet.");
  });
});

describe("isLikelyDeviceId — a value coord's Uuid extractor will accept", () => {
  it("accepts a uuid, trimmed and in either case", () => {
    expect(isLikelyDeviceId("22222222-2222-2222-2222-222222222222")).toBe(true);
    expect(isLikelyDeviceId("  AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE  ")).toBe(true);
  });

  it("rejects what coord would 400 on", () => {
    for (const bad of ["", "merytshost", "2222", "22222222-2222-2222-2222-22222222222", "zzzz"]) {
      expect(isLikelyDeviceId(bad)).toBe(false);
    }
  });
});

describe("fleetFilteredOutMessage under a walk — 'not loaded' is not 'not there'", () => {
  it("points at the next page when one exists, instead of stopping at the loaded set", () => {
    // Under the ladder this message was the end of the road once the page was
    // at the ceiling. With a cursor it is not: the way to widen what the box
    // sees is one click away, and saying so is what keeps it true.
    const msg = fleetFilteredOutMessage(100, 0, "zzz", true);
    expect(msg).toMatch(/coord has more/i);
    expect(msg).toMatch(/load another page/i);
  });

  it("does NOT promise a next page when coord said there is none", () => {
    const msg = fleetFilteredOutMessage(100, 0, "zzz", false);
    expect(msg).not.toMatch(/coord has more/i);
    expect(msg).toMatch(/filters only those/i);
  });
});
