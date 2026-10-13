/**
 * The one-time localStorage → runner-store import of Session Manager pins and
 * labels. "Once" is enforced by removing each key only after the runner
 * confirmed it, so these tests drive repeated runs against one storage.
 */

import { describe, it, expect, vi } from "vitest";

vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: { getItem: () => null, removeItem: () => undefined },
}));

import {
  LEGACY_LABELS_KEY,
  LEGACY_PINNED_KEY,
  runSessionOrgImport,
  type LegacyStorage,
} from "./sessionOrgImport";
import type { SessionOrgApplyResponse, SessionOrgOp } from "./sessionOrgTypes";

function memoryStorage(initial: Record<string, string>): LegacyStorage & {
  data: Record<string, string>;
} {
  const data = { ...initial };
  return {
    data,
    getItem: (k) => (k in data ? data[k] : null),
    removeItem: (k) => {
      delete data[k];
    },
  };
}

function okApply() {
  return vi.fn(
    async (ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse> => ({
      results: ops.map(() => ({ ok: true, error: null, groupId: null })),
      snapshot: {
        status: "known",
        org: {
          version: 1,
          revision: 1,
          groups: [],
          members: {},
          recentlyClosed: [],
          recentlyClosedTotal: 0,
          savedViews: [],
          dismissedSuggestions: [],
        },
        reason: null,
        preservedAt: null,
      },
    }),
  );
}

describe("runSessionOrgImport", () => {
  it("imports pins and each label as a tag, then deletes both keys — and runs once", async () => {
    const storage = memoryStorage({
      [LEGACY_PINNED_KEY]: JSON.stringify(["s1", "s2"]),
      [LEGACY_LABELS_KEY]: JSON.stringify({ s1: "review", s3: "  blocked " }),
    });
    const apply = okApply();
    const first = await runSessionOrgImport({ apply, storage });
    expect(first).toEqual({ status: "imported", pinned: 2, tagged: 2, unreadable: [] });
    expect(apply).toHaveBeenCalledTimes(1);
    expect(apply.mock.calls[0][0]).toEqual([
      { op: "setPinned", sessionKeys: ["s1", "s2"], pinned: true },
      { op: "addTags", sessionKeys: ["s1"], tags: ["review"] },
      { op: "addTags", sessionKeys: ["s3"], tags: ["blocked"] },
    ]);
    expect(storage.data).toEqual({});

    const second = await runSessionOrgImport({ apply, storage });
    expect(second.status).toBe("nothing-to-import");
    expect(apply).toHaveBeenCalledTimes(1);
  });

  it("keeps the keys when the runner cannot be asked, so the next run retries", async () => {
    const storage = memoryStorage({ [LEGACY_PINNED_KEY]: JSON.stringify(["s1"]) });
    const apply = vi.fn(async () => {
      throw new Error("invoke failed");
    });
    const outcome = await runSessionOrgImport({ apply, storage });
    expect(outcome).toEqual({ status: "failed", reason: "invoke failed", unreadable: [] });
    expect(storage.data[LEGACY_PINNED_KEY]).toBe('["s1"]');

    const retry = await runSessionOrgImport({ apply: okApply(), storage });
    expect(retry.status).toBe("imported");
    expect(storage.data).toEqual({});
  });

  it("keeps the keys when any op is refused or the org is unknown", async () => {
    const storage = memoryStorage({ [LEGACY_LABELS_KEY]: JSON.stringify({ s1: "x" }) });
    const refused = vi.fn(async () => ({
      results: [{ ok: false, error: "org_unknown", groupId: null }],
      snapshot: { status: "unknown" as const, org: null, reason: "unreadable", preservedAt: null },
    }));
    const outcome = await runSessionOrgImport({ apply: refused, storage });
    expect(outcome.status).toBe("failed");
    expect(storage.data[LEGACY_LABELS_KEY]).toBeDefined();
  });

  it("leaves an unparseable key in place and still imports the readable one", async () => {
    const storage = memoryStorage({
      [LEGACY_PINNED_KEY]: "{not json",
      [LEGACY_LABELS_KEY]: JSON.stringify({ s1: "keep" }),
    });
    const outcome = await runSessionOrgImport({ apply: okApply(), storage });
    expect(outcome).toEqual({
      status: "imported",
      pinned: 0,
      tagged: 1,
      unreadable: [LEGACY_PINNED_KEY],
    });
    expect(storage.data).toEqual({ [LEGACY_PINNED_KEY]: "{not json" });
  });

  it("removes empty legacy keys without calling the runner", async () => {
    const storage = memoryStorage({
      [LEGACY_PINNED_KEY]: "[]",
      [LEGACY_LABELS_KEY]: "{}",
    });
    const apply = okApply();
    const outcome = await runSessionOrgImport({ apply, storage });
    expect(outcome.status).toBe("imported");
    expect(apply).not.toHaveBeenCalled();
    expect(storage.data).toEqual({});
  });

  it("reports nothing to import when neither key exists", async () => {
    const apply = okApply();
    const outcome = await runSessionOrgImport({ apply, storage: memoryStorage({}) });
    expect(outcome).toEqual({ status: "nothing-to-import", unreadable: [] });
    expect(apply).not.toHaveBeenCalled();
  });
});
