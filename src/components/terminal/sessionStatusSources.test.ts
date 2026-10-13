/**
 * The pure readers behind the Session Manager's status sources: the external
 * scan, the finished marker, and the by-id digest request (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 1). Each one's UNKNOWN arm is asserted as UNKNOWN, never as zero or
 * "not finished".
 */

import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { readExternalScan } from "./useExternalClaudeScan";
import { finishedFor, readFinishedMarkers } from "./useFinishedMarkers";
import {
  DIGEST_CHUNK_SIZE,
  RECENT_ROWS_DIGESTED,
  chunkIds,
  digestRequestIds,
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

describe("readFinishedMarkers / finishedFor", () => {
  const ok = readFinishedMarkers({
    success: true,
    data: {
      sessions: [
        { claudeSessionId: "a", finished: true },
        { claudeSessionId: "b", finished: false },
        { claudeSessionId: "c" },
        { claudeSessionId: "a", finished: false },
      ],
    },
  });

  it("reads the marker where a record carries it; a finish on any record of a session wins", () => {
    expect(finishedFor(ok, "a")).toBe(true);
    expect(finishedFor(ok, "b")).toBe(false);
  });

  it("is UNKNOWN for a record without the field, an unknown session, and a failed read", () => {
    expect(finishedFor(ok, "c")).toBeNull();
    expect(finishedFor(ok, "zzz")).toBeNull();
    const failed = readFinishedMarkers({ success: false });
    expect(failed.read).toBe("failed");
    expect(finishedFor(failed, "a")).toBeNull();
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
