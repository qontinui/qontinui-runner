/**
 * Tests for the durable per-tab "last-known Claude session id" store
 * (`lastKnownSessionIds`). This is what keeps a restored tab resumable
 * across a close→reopen even when the live tab object transiently loses its
 * `claudeSessionId` (capture lag / state churn).
 *
 * The runner's vitest env is `node` (no `localStorage`), so we mock
 * `@/lib/instance-storage` with an in-memory JSON store mirroring its
 * `getJSON`/`setJSON` contract.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

// In-memory backing store for the mocked instanceStorage.
const mem = new Map<string, unknown>();

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON<T>(key: string, fallback: T): T {
      return mem.has(key) ? (mem.get(key) as T) : fallback;
    },
    setJSON(key: string, value: unknown): void {
      mem.set(key, value);
    },
  },
}));

import { rememberSessionId, recallSessionId, pruneSessionIds } from "./lastKnownSessionIds";

beforeEach(() => {
  mem.clear();
  vi.useRealTimers();
});

describe("rememberSessionId / recallSessionId", () => {
  it("round-trips a remembered id for a tab", () => {
    rememberSessionId("tab-1", "sess-A", "C:/claude/.claude-gmail");
    const recalled = recallSessionId("tab-1");
    expect(recalled?.claudeSessionId).toBe("sess-A");
    expect(recalled?.claudeConfigDir).toBe("C:/claude/.claude-gmail");
  });

  it("returns null for an unknown tab", () => {
    expect(recallSessionId("nope")).toBeNull();
  });

  it("is a no-op for an empty/undefined id (never stores blanks)", () => {
    rememberSessionId("tab-1", undefined, "C:/claude/.claude-gmail");
    expect(recallSessionId("tab-1")).toBeNull();
  });

  it("keeps entries isolated per tab id", () => {
    rememberSessionId("tab-1", "sess-A", undefined);
    rememberSessionId("tab-2", "sess-B", undefined);
    expect(recallSessionId("tab-1")?.claudeSessionId).toBe("sess-A");
    expect(recallSessionId("tab-2")?.claudeSessionId).toBe("sess-B");
  });

  it("refreshes (overwrites) an existing entry", () => {
    rememberSessionId("tab-1", "sess-OLD", undefined);
    rememberSessionId("tab-1", "sess-NEW", "C:/claude/.claude-hotmail");
    const recalled = recallSessionId("tab-1");
    expect(recalled?.claudeSessionId).toBe("sess-NEW");
    expect(recalled?.claudeConfigDir).toBe("C:/claude/.claude-hotmail");
  });
});

describe("staleness", () => {
  it("does not recall an entry older than the max age", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    rememberSessionId("tab-1", "sess-A", undefined);

    // Advance 8 days (> 7-day ceiling).
    vi.setSystemTime(new Date("2026-01-09T00:00:01Z"));
    expect(recallSessionId("tab-1")).toBeNull();
  });

  it("prunes stale entries from the store", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    rememberSessionId("old", "sess-OLD", undefined);

    vi.setSystemTime(new Date("2026-01-08T00:00:01Z"));
    rememberSessionId("fresh", "sess-FRESH", undefined);

    pruneSessionIds();

    expect(recallSessionId("old")).toBeNull();
    expect(recallSessionId("fresh")?.claudeSessionId).toBe("sess-FRESH");
  });
});
