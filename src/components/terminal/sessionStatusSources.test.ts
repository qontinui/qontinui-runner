/**
 * The pure readers behind the Session Manager's status sources: the external
 * scan, the finished marker (from the batched session-info listing), and the
 * by-id digest request (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 1). Each one's UNKNOWN arm is asserted as UNKNOWN, never as zero or
 * "not finished".
 */

import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { readExternalScan } from "./useExternalClaudeScan";
import { finishedMarker, type SessionInfoListView } from "./sessionFacts";
import type { SessionInfoState } from "./useSessionInfo";
import {
  DIGEST_CHUNK_SIZE,
  RECENT_ROWS_DIGESTED,
  DIGEST_MAX_AGE_MS,
  chunkIds,
  digestRequestIds,
  mergeDigestRound,
  type SessionDigest,
} from "./useSessionDigests";

describe("readExternalScan", () => {
  it("reads pids, and a stale answer as stale", () => {
    expect(
      readExternalScan({
        success: true,
        data: { processes: [{ pid: 9 }, { pid: 3 }], stale: false },
      }),
    ).toEqual({ state: "ok", pids: [3, 9] });
    expect(readExternalScan({ success: true, data: { processes: [], stale: true } }).state).toBe(
      "stale",
    );
  });

  it("is degraded — never an empty ok — on failure or an unreadable shape", () => {
    expect(readExternalScan({ success: false })).toEqual({ state: "degraded", pids: [] });
    expect(readExternalScan({ success: true, data: {} }).state).toBe("degraded");
    expect(readExternalScan(null).state).toBe("degraded");
  });
});

describe("finishedMarker — the live row's marker from the session-info listing", () => {
  /** A listed session whose lifecycle record carries `finishedAt` as given. */
  const listed = (finishedAt: unknown): SessionInfoState =>
    ({
      status: "ok",
      reason: null,
      body: { lifecycle: { finishedAt } },
    }) as unknown as SessionInfoState;
  const ok: SessionInfoListView = {
    phase: "ok",
    byId: new Map([
      ["done", listed(1_700_000_000_000)],
      ["open", listed(null)],
      // A runner build whose record omits the field.
      ["old-build", listed(undefined)],
      ["no-body", { status: "unavailable", reason: "x", body: null }],
    ]),
    notFound: new Set(["gone"]),
    branches: new Map(),
  };

  it("reads finished / not finished from the lifecycle record", () => {
    expect(finishedMarker(ok, "done")).toBe(true);
    expect(finishedMarker(ok, "open")).toBe(false);
  });

  it("is UNKNOWN — never 'not finished' — wherever the record does not say", () => {
    expect(finishedMarker(ok, "old-build")).toBeNull();
    expect(finishedMarker(ok, "no-body")).toBeNull();
    expect(finishedMarker(ok, "gone")).toBeNull();
    expect(finishedMarker(ok, "never-listed")).toBeNull();
    expect(finishedMarker(ok, null)).toBeNull();
    expect(finishedMarker({ phase: "loading" }, "done")).toBeNull();
    expect(finishedMarker({ phase: "unavailable", reason: "ipc_error: x" }, "done")).toBeNull();
  });
});

describe("digest request by id", () => {
  it("chunks at the command's per-call bound", () => {
    const ids = Array.from({ length: 250 }, (_, i) => `s-${i}`);
    const chunks = chunkIds(ids);
    expect(chunks.map((c) => c.length)).toEqual([DIGEST_CHUNK_SIZE, DIGEST_CHUNK_SIZE, 50]);
    expect(chunks.flat()).toEqual(ids);
  });

  it("requests every possibly-live session however many, plus the newest rows, and no other", () => {
    const live = Array.from({ length: 200 }, (_, i) => ({
      sessionId: `live-${i}`,
      location: "outside-runner" as const,
      lastModified: "2026-01-01T00:00:00Z",
    }));
    const gone = Array.from({ length: 300 }, (_, i) => ({
      sessionId: `gone-${i}`,
      location: "not-running" as const,
      lastModified: `2026-10-${String((i % 28) + 1).padStart(2, "0")}T00:00:${String(i % 60).padStart(2, "0")}Z`,
    }));
    const ids = digestRequestIds([...live, ...gone]);
    expect(ids).toContain("live-50");
    expect(ids).toContain("live-199");
    expect(ids.filter((id) => id.startsWith("gone-"))).toHaveLength(RECENT_ROWS_DIGESTED);
    // An Unknown placement is requested too: it may be running.
    expect(
      digestRequestIds([
        { sessionId: "u", location: "unknown", lastModified: "2000-01-01T00:00:00Z" },
      ]),
    ).toEqual(["u"]);
  });
});

describe("mergeDigestRound — a digest cannot outlive its refreshes", () => {
  const dg = (id: string, preview = "p"): SessionDigest => ({
    session_id: id,
    config_dir: "c",
    project_path: "/p",
    last_message_type: "assistant",
    last_message_timestamp: "2026-10-14T00:00:00Z",
    last_message_preview: preview,
    last_assistant_had_tool_use: false,
    likely_frozen: false,
    work_summary_hint: "",
  });

  it("applies a successful chunk even when another chunk of the round failed", () => {
    const prev = { b: { digest: dg("b", "old"), fetchedAt: 0 } };
    const next = mergeDigestRound(
      prev,
      ["a", "b"],
      [
        { ids: ["a"], data: [dg("a", "new")] },
        { ids: ["b"], data: null },
      ],
      1_000,
    );
    expect(next.a).toEqual({ digest: dg("a", "new"), fetchedAt: 1_000 });
    // The failed chunk keeps b's previous digest with its OLD timestamp.
    expect(next.b).toEqual({ digest: dg("b", "old"), fetchedAt: 0 });
  });

  it("drops a digest whose chunk keeps failing past the max age, so the row reads Unknown", () => {
    let entries = mergeDigestRound({}, ["a"], [{ ids: ["a"], data: [dg("a")] }], 0);
    const failed = [{ ids: ["a"], data: null }];
    entries = mergeDigestRound(entries, ["a"], failed, DIGEST_MAX_AGE_MS);
    expect(entries.a).toBeDefined(); // exactly at the bound: still kept
    entries = mergeDigestRound(entries, ["a"], failed, DIGEST_MAX_AGE_MS + 1);
    expect(entries.a).toBeUndefined();
  });

  it("a successful chunk is authoritative: an answered id with no digest is dropped", () => {
    const prev = { a: { digest: dg("a"), fetchedAt: 0 } };
    expect(mergeDigestRound(prev, ["a"], [{ ids: ["a"], data: [] }], 10)).toEqual({});
  });

  it("drops ids no longer requested", () => {
    const prev = { a: { digest: dg("a"), fetchedAt: 0 } };
    expect(mergeDigestRound(prev, ["b"], [{ ids: ["b"], data: null }], 10)).toEqual({});
  });

  it("is three poll periods", () => {
    expect(DIGEST_MAX_AGE_MS).toBe(90_000);
  });
});
