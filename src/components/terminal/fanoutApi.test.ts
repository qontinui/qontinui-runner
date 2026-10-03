import { describe, expect, it } from "vitest";

import {
  isFanoutCapOutcome,
  isFanoutRunList,
  isFanoutRunView,
  parseFanoutEnvelope,
  type FanoutRunView,
} from "./fanoutApi";

function runFixture(overrides: Partial<FanoutRunView> = {}): FanoutRunView {
  return {
    id: "11111111-2222-3333-4444-555555555555",
    tenantId: null,
    templateSlug: "port-feature",
    templateVersion: 3,
    maxConcurrent: 2,
    configDirPolicy: { kind: "bestHeadroom" },
    workingDir: "/repo",
    createdAt: "2026-10-03T10:00:00Z",
    state: "active",
    counts: { queued: 0, admitted: 0, released: 0, cancelled: 0, refused: 0 },
    members: [],
    ...overrides,
  };
}

describe("parseFanoutEnvelope", () => {
  it("returns data for a 2xx success envelope", () => {
    const run = runFixture();
    const r = parseFanoutEnvelope(200, { success: true, data: [run] }, isFanoutRunList, "GET");
    expect(r).toEqual({ ok: true, data: [run] });
  });

  it("an empty list is a real answer only inside a success envelope", () => {
    expect(parseFanoutEnvelope(200, { success: true, data: [] }, isFanoutRunList, "GET")).toEqual({
      ok: true,
      data: [],
    });
  });

  it("a non-2xx carries the server's own error text and status", () => {
    const r = parseFanoutEnvelope(
      400,
      { success: false, error: 'workingDir: "rel" is not an absolute path' },
      isFanoutRunList,
      "POST /fanout",
    );
    expect(r).toEqual({
      ok: false,
      status: 400,
      error: 'workingDir: "rel" is not an absolute path',
    });
  });

  it("a non-2xx with no envelope names the route and status", () => {
    expect(parseFanoutEnvelope(503, null, isFanoutRunList, "GET /fanout")).toEqual({
      ok: false,
      status: 503,
      error: "GET /fanout: HTTP 503",
    });
  });

  it("a 200 that says success:false is a failure, not an empty answer", () => {
    const r = parseFanoutEnvelope(200, { success: false, error: "nope" }, isFanoutRunList, "GET");
    expect(r).toEqual({ ok: false, status: 200, error: "nope" });
  });

  it("a 200 success with no data, or the wrong shape, is a failure", () => {
    expect(parseFanoutEnvelope(200, { success: true }, isFanoutRunList, "GET").ok).toBe(false);
    expect(
      parseFanoutEnvelope(200, { success: true, data: { runs: [] } }, isFanoutRunList, "GET").ok,
    ).toBe(false);
  });
});

describe("shape guards", () => {
  it("accept a full run and reject a run missing members", () => {
    expect(isFanoutRunView(runFixture())).toBe(true);
    const { members: _m, ...noMembers } = runFixture();
    expect(isFanoutRunView(noMembers)).toBe(false);
    expect(isFanoutRunView(runFixture({ state: "weird" as never }))).toBe(false);
  });

  it("accept a cap outcome with or without a clamp", () => {
    expect(isFanoutCapOutcome({ run: runFixture(), fanoutBound: 15, clampedFrom: null })).toBe(
      true,
    );
    expect(isFanoutCapOutcome({ run: runFixture(), fanoutBound: 15, clampedFrom: 40 })).toBe(true);
    expect(isFanoutCapOutcome({ run: runFixture(), fanoutBound: "15", clampedFrom: null })).toBe(
      false,
    );
  });
});
