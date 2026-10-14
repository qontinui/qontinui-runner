/**
 * `useSessionOrg`'s client — optimistic apply, reconcile to the runner's
 * state, the cross-window change event, and the never-a-default states.
 * Exercised through `createSessionOrgClient` over a fake transport (the
 * vitest environment is `node`, no renderer); the hook is a thin
 * `useSyncExternalStore` over this client.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
const mockListen = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => mockListen(...args),
}));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: { getItem: () => null, removeItem: () => undefined },
}));

import {
  createSessionOrgClient,
  memberOf,
  tauriSessionOrgTransport,
  type SessionOrgTransport,
} from "./useSessionOrg";
import { applyOpsOptimistic } from "./sessionOrgOptimistic";
import type {
  SessionOrg,
  SessionOrgApplyResponse,
  SessionOrgOp,
  SessionOrgSnapshot,
} from "./sessionOrgTypes";

function emptyOrg(revision = 0): SessionOrg {
  return {
    version: 1,
    revision,
    groups: [],
    members: {},
    recentlyClosed: [],
    recentlyClosedTotal: 0,
    savedViews: [],
    dismissedSuggestions: [],
  };
}

function known(org: SessionOrg): SessionOrgSnapshot {
  return { status: "known", org, reason: null, preservedAt: null };
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve(v: T): void;
  reject(e: unknown): void;
}
function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** A fake runner: holds an org, applies ops through the optimistic mirror
 *  (good enough as a server stand-in), and lets a test emit events. */
function fakeTransport(initial: SessionOrgSnapshot) {
  let snapshot = initial;
  const emitters: Array<(s: SessionOrgSnapshot) => void> = [];
  const unsubscribed = { count: 0 };
  const transport: SessionOrgTransport & {
    applyGate: Deferred<void> | null;
    getFails: boolean;
  } = {
    applyGate: null,
    getFails: false,
    get: vi.fn(async () => {
      if (transport.getFails) throw new Error("ipc down");
      return snapshot;
    }),
    apply: vi.fn(async (ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse> => {
      if (transport.applyGate) await transport.applyGate.promise;
      const org = snapshot.org as SessionOrg;
      const next = applyOpsOptimistic(org, ops);
      next.revision = org.revision + 1;
      snapshot = known(next);
      return {
        results: ops.map((op) => ({
          ok: true,
          error: null,
          groupId: op.op === "createGroup" ? (op.groupId ?? null) : null,
        })),
        snapshot,
      };
    }),
    subscribe: vi.fn(async (onChange: (s: SessionOrgSnapshot) => void) => {
      emitters.push(onChange);
      return () => {
        unsubscribed.count += 1;
      };
    }),
  };
  return {
    transport,
    emit: (s: SessionOrgSnapshot) => emitters.forEach((e) => e(s)),
    unsubscribed,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
});

describe("useSessionOrg client", () => {
  it("is loading before the first read and known after it", async () => {
    const { transport } = fakeTransport(known(emptyOrg(3)));
    const client = createSessionOrgClient(transport);
    expect(client.getView()).toEqual({ phase: "loading" });
    client.retain();
    await flush();
    const view = client.getView();
    expect(view.phase).toBe("known");
    if (view.phase === "known") expect(view.org.revision).toBe(3);
  });

  it("a failed first read is unknown with its reason, never an empty org", async () => {
    const { transport } = fakeTransport(known(emptyOrg()));
    transport.getFails = true;
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    const view = client.getView();
    expect(view.phase).toBe("unknown");
    if (view.phase === "unknown") expect(view.reason).toContain("ipc down");
    expect(memberOf(view, "s1")).toBeNull();
  });

  it("an unreadable runner file renders unknown with where it was preserved", async () => {
    const { transport } = fakeTransport({
      status: "unknown",
      org: null,
      reason: "unreadable file: EOF",
      preservedAt: "/r/session-org.corrupt-1.json",
    });
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    expect(client.getView()).toEqual({
      phase: "unknown",
      reason: "unreadable file: EOF",
      preservedAt: "/r/session-org.corrupt-1.json",
    });
  });

  it("shows an apply at once, marks it in flight, then reconciles to the runner", async () => {
    const { transport } = fakeTransport(known(emptyOrg()));
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    transport.applyGate = deferred<void>();
    const done = client.apply([{ op: "createGroup", title: "Coord", sessionKeys: ["s1"] }]);

    const preview = client.getView();
    expect(preview.phase).toBe("known");
    if (preview.phase !== "known") return;
    expect(preview.pending).toBe(1);
    expect(preview.org.groups).toHaveLength(1);
    const previewId = preview.org.groups[0].id;
    expect(preview.org.members.s1.groupId).toBe(previewId);

    // The optimistic id is the id the runner was sent.
    const sent = (transport.apply as ReturnType<typeof vi.fn>).mock.calls[0][0] as SessionOrgOp[];
    expect(sent[0]).toMatchObject({ op: "createGroup", groupId: previewId });

    transport.applyGate.resolve();
    const response = await done;
    expect(response.results[0].groupId).toBe(previewId);
    const after = client.getView();
    expect(after.phase === "known" && after.pending).toBe(0);
    expect(after.phase === "known" && after.org.revision).toBe(1);
  });

  it("sends applies in the order they were made, even when the first is slow", async () => {
    const { transport } = fakeTransport(known(emptyOrg()));
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    transport.applyGate = deferred<void>();
    const gate = transport.applyGate;
    const pin = client.apply([{ op: "setPinned", sessionKeys: ["s1"], pinned: true }]);
    transport.applyGate = null;
    const unpin = client.apply([{ op: "setPinned", sessionKeys: ["s1"], pinned: false }]);
    await flush();
    // The unpin must not reach the store while the pin is still in flight.
    expect((transport.apply as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);
    gate.resolve();
    await Promise.all([pin, unpin]);
    const calls = (transport.apply as ReturnType<typeof vi.fn>).mock.calls;
    expect(calls).toHaveLength(2);
    expect((calls[1][0] as SessionOrgOp[])[0]).toMatchObject({ op: "setPinned", pinned: false });
    const view = client.getView();
    expect(view.phase === "known" && view.org.members.s1?.pinned).not.toBe(true);
  });

  it("a refused op vanishes from the view and is reported", async () => {
    const { transport } = fakeTransport(known(emptyOrg()));
    transport.apply = vi.fn(async () => ({
      results: [{ ok: false, error: "group_not_found", groupId: null }],
      snapshot: known(emptyOrg(0)),
    }));
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    await client.apply([{ op: "renameGroup", groupId: "nope", title: "x" }]);
    const view = client.getView();
    expect(view.phase === "known" && view.lastError).toBe("refused: group_not_found");
  });

  it("a transport failure withdraws the preview, re-reads, and rejects", async () => {
    const { transport } = fakeTransport(known(emptyOrg()));
    transport.apply = vi.fn(async () => {
      throw new Error("invoke failed");
    });
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    const getCalls = (transport.get as ReturnType<typeof vi.fn>).mock.calls.length;
    await expect(
      client.apply([{ op: "setPinned", sessionKeys: ["s1"], pinned: true }]),
    ).rejects.toThrow("invoke failed");
    const view = client.getView();
    expect(view.phase).toBe("known");
    if (view.phase !== "known") return;
    expect(view.org.members.s1).toBeUndefined();
    expect(view.lastError).toBe("not applied: invoke failed");
    expect((transport.get as ReturnType<typeof vi.fn>).mock.calls.length).toBe(getCalls + 1);
  });

  it("takes another window's change from the event and drops an older one", async () => {
    const { transport, emit } = fakeTransport(known(emptyOrg(5)));
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    const fromOtherWindow = applyOpsOptimistic(emptyOrg(6), [
      { op: "createGroup", groupId: "g1", title: "Elsewhere" },
    ]);
    emit(known(fromOtherWindow));
    let view = client.getView();
    expect(view.phase === "known" && view.org.groups.map((g) => g.title)).toEqual(["Elsewhere"]);

    emit(known(emptyOrg(4))); // late, older event
    view = client.getView();
    expect(view.phase === "known" && view.org.revision).toBe(6);
  });

  it("keeps the in-flight preview on top of an event from another window", async () => {
    const { transport, emit } = fakeTransport(known(emptyOrg(1)));
    const client = createSessionOrgClient(transport);
    client.retain();
    await flush();
    transport.applyGate = deferred<void>();
    const done = client.apply([{ op: "setPinned", sessionKeys: ["mine"], pinned: true }]);
    emit(
      known(
        applyOpsOptimistic(emptyOrg(2), [{ op: "setMuted", sessionKeys: ["theirs"], muted: true }]),
      ),
    );
    const view = client.getView();
    expect(view.phase).toBe("known");
    if (view.phase === "known") {
      expect(view.org.members.mine?.pinned).toBe(true);
      expect(view.org.members.theirs?.muted).toBe(true);
    }
    transport.applyGate.resolve();
    await done;
  });

  it("subscribes once across holders and unsubscribes at the last release", async () => {
    const { transport, unsubscribed } = fakeTransport(known(emptyOrg()));
    const client = createSessionOrgClient(transport);
    client.retain();
    client.retain();
    await flush();
    expect(transport.subscribe).toHaveBeenCalledTimes(1);
    client.release();
    expect(unsubscribed.count).toBe(0);
    client.release();
    expect(unsubscribed.count).toBe(1);
  });

  it("runs the legacy import at most once per client", async () => {
    const { transport } = fakeTransport(known(emptyOrg()));
    const client = createSessionOrgClient(transport);
    const a = client.importLegacyKeys();
    const b = client.importLegacyKeys();
    expect(a).toBe(b);
    expect((await a).status).toBe("nothing-to-import");
  });
});

describe("tauriSessionOrgTransport", () => {
  it("calls the two commands and the change event by their Rust names", async () => {
    mockInvoke.mockResolvedValue(known(emptyOrg()));
    mockListen.mockResolvedValue(() => undefined);
    await tauriSessionOrgTransport.get();
    expect(mockInvoke).toHaveBeenLastCalledWith("session_org_get");
    const ops: SessionOrgOp[] = [{ op: "markSeen", sessionKey: "s", at: 1 }];
    await tauriSessionOrgTransport.apply(ops);
    expect(mockInvoke).toHaveBeenLastCalledWith("session_org_apply", { ops });
    await tauriSessionOrgTransport.subscribe(() => undefined);
    expect(mockListen.mock.calls[0][0]).toBe("session-org-changed");
  });
});
