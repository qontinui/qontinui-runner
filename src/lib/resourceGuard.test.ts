import { afterEach, describe, expect, it, vi } from "vitest";

import {
  CRITICAL_REFUSAL_PREFIX,
  GRANT_IDLE_MS,
  GRANT_MAX_MS,
  GRANT_SPAWN_LIMIT,
  getGrantsSnapshot,
  getSnapshot,
  parseResourceGuardRefusal,
  resolvePendingResourceBlock,
  revokeResourceGuardGrant,
  spawnWithResourceGuard,
  subscribe,
} from "./resourceGuard";
import wire from "./resourceGuardWire.fixture.json";

/**
 * The attended half of the spawn-time resource gate (plan
 * `2026-08-07-runner-resource-guard-and-session-protection` §Part D).
 *
 * Two properties matter more than any other here and both are pinned below:
 * a NON-refusal error must never be offered an override (that would let a
 * dead-account or bad-cwd failure be "started anyway", retrying something that
 * cannot work), and declining must re-throw rather than resolve (a spawn the
 * operator refused must not be reported to its caller as a success).
 *
 * vitest runs `environment: "node"` with no React Testing Library, so these
 * drive the store contract (`subscribe`/`getSnapshot`) that
 * `usePendingResourceBlock` hands to `useSyncExternalStore`, rather than
 * rendering the hook — same precedent as `authOverlayStore.test.ts`.
 */

/**
 * The refusals Rust actually emits, read from the fixture its own tests pin
 * (`resource_guard::tests::the_refusal_wire_matches_the_shared_fixture`). A
 * Rust rewording that drops the lane token fails there; a parser here that
 * cannot read these exact bytes fails below.
 */
const REFUSAL = wire.refusals.free_commit_bytes;
const THREAD_REFUSAL = wire.refusals.thread_count;
/** What a runner build from before the lane token sends. */
const LEGACY_REFUSAL =
  `${CRITICAL_REFUSAL_PREFIX} Not starting a new terminal session: the host lane has ` +
  `1.00 GiB of free commit, below the 1.50 GiB critical floor.`;

// The queue and the grants are module-level singletons — drain both between tests.
afterEach(() => {
  while (getSnapshot() !== null) resolvePendingResourceBlock(false);
  for (const g of getGrantsSnapshot()) revokeResourceGuardGrant(g.metric);
  vi.useRealTimers();
});

describe("parseResourceGuardRefusal", () => {
  it("pins the prefix to the Rust constant via the shared fixture", () => {
    expect(wire.prefix).toBe(CRITICAL_REFUSAL_PREFIX);
  });

  it("reads the memory lane token and strips it with the prefix", () => {
    expect(parseResourceGuardRefusal(REFUSAL)).toEqual({
      metric: "free_commit_bytes",
      message:
        "Not starting a new terminal session: the host lane has 1.00 GiB of free commit, " +
        "below the 1.50 GiB critical floor. Free memory (close a build or a session) and try " +
        "again, or start anyway to override. The limits live in Settings > Resource Guard.",
    });
  });

  it("reads the thread lane token", () => {
    const parsed = parseResourceGuardRefusal(THREAD_REFUSAL);
    expect(parsed?.metric).toBe("thread_count");
    expect(parsed?.message.startsWith("Not starting a new terminal session:")).toBe(true);
    expect(parsed?.message).toContain("400-thread critical ceiling");
  });

  it("reads a refusal with no token (an older runner) as unknown, keeping all its text", () => {
    expect(parseResourceGuardRefusal(LEGACY_REFUSAL)).toEqual({
      metric: "unknown",
      message:
        "Not starting a new terminal session: the host lane has 1.00 GiB of free commit, " +
        "below the 1.50 GiB critical floor.",
    });
  });

  it("reads a lane this build does not know as unknown, still stripping the token", () => {
    expect(parseResourceGuardRefusal(`${CRITICAL_REFUSAL_PREFIX}fd_count: Too many.`)).toEqual({
      metric: "unknown",
      message: "Too many.",
    });
    expect(parseResourceGuardRefusal(`${CRITICAL_REFUSAL_PREFIX}gpu0_mem: Full.`)).toEqual({
      metric: "unknown",
      message: "Full.",
    });
  });

  it("unwraps an Error, since invoke can reject with either shape", () => {
    expect(parseResourceGuardRefusal(new Error(THREAD_REFUSAL))?.metric).toBe("thread_count");
  });

  it("returns null for anything that is not a resource-guard refusal", () => {
    for (const other of [
      "Failed to open PTY: os error 5",
      "terminal:tenant_invalid: nope is not a tenant uuid",
      "spawn_blocked (trust gate)",
      new Error("boom"),
      null,
      undefined,
      42,
    ]) {
      expect(parseResourceGuardRefusal(other)).toBeNull();
    }
  });
});

describe("spawnWithResourceGuard", () => {
  it("passes through with no override when the spawn succeeds", async () => {
    const attempt = vi.fn().mockResolvedValue("ok");
    await expect(spawnWithResourceGuard(attempt)).resolves.toBe("ok");
    expect(attempt).toHaveBeenCalledExactlyOnceWith(false);
    expect(getSnapshot()).toBeNull();
  });

  it("never prompts for a non-refusal failure", async () => {
    const attempt = vi.fn().mockRejectedValue("Failed to open PTY: os error 5");
    await expect(spawnWithResourceGuard(attempt)).rejects.toBe("Failed to open PTY: os error 5");
    expect(attempt).toHaveBeenCalledTimes(1);
    expect(getSnapshot()).toBeNull();
  });

  it("prompts on a typed refusal and retries WITH the override on confirm", async () => {
    const attempt = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValueOnce("started anyway");
    const listener = vi.fn();
    const unsubscribe = subscribe(listener);

    const pending = spawnWithResourceGuard(attempt);
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    expect(getSnapshot()?.message).toContain("1.50 GiB critical floor");
    expect(getSnapshot()?.metric).toBe("free_commit_bytes");
    expect(getSnapshot()?.pending).toBe(1);
    expect(listener).toHaveBeenCalled();

    resolvePendingResourceBlock(true);
    await expect(pending).resolves.toBe("started anyway");
    expect(attempt).toHaveBeenNthCalledWith(1, false);
    expect(attempt).toHaveBeenNthCalledWith(2, true);
    expect(getSnapshot()).toBeNull();

    unsubscribe();
  });

  it("re-throws the ORIGINAL refusal when the operator declines", async () => {
    const attempt = vi.fn().mockRejectedValue(REFUSAL);

    const pending = spawnWithResourceGuard(attempt);
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(false);

    // Rejecting (not resolving) is what keeps the caller's existing catch block
    // running — `createTerminal` returns null and no tab is added.
    await expect(pending).rejects.toBe(REFUSAL);
    expect(attempt).toHaveBeenCalledTimes(1);
  });

  it("queues concurrent refusals of DIFFERENT lanes instead of stranding or merging them", async () => {
    const first = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValueOnce("first");
    const second = vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("second");

    const a = spawnWithResourceGuard(first);
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    const b = spawnWithResourceGuard(second);
    await vi.waitFor(() => expect(second).toHaveBeenCalledTimes(1));

    // Head answered first; the other lane's refusal is still waiting, not discarded.
    resolvePendingResourceBlock(true);
    await expect(a).resolves.toBe("first");
    expect(getSnapshot()?.metric).toBe("thread_count");

    resolvePendingResourceBlock(false);
    await expect(b).rejects.toBe(THREAD_REFUSAL);
    expect(getSnapshot()).toBeNull();
  });

  it("ignores a resolve when nothing is pending", () => {
    expect(() => resolvePendingResourceBlock(true)).not.toThrow();
    expect(getSnapshot()).toBeNull();
  });
});

describe("one decision per burst", () => {
  it("coalesces N concurrent same-lane refusals into one dialog that one answer resolves", async () => {
    const attempts = [1, 2, 3].map((n) =>
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce(`t${n}`),
    );
    const spawns = attempts.map((a) => spawnWithResourceGuard(a, { label: "new terminal" }));
    await vi.waitFor(() => expect(getSnapshot()?.pending).toBe(3));
    expect(getSnapshot()?.metric).toBe("thread_count");
    expect(getSnapshot()?.source).toEqual({ label: "new terminal" });

    resolvePendingResourceBlock(true);
    await expect(Promise.all(spawns)).resolves.toEqual(["t1", "t2", "t3"]);
    for (const a of attempts) expect(a).toHaveBeenNthCalledWith(2, true);
    expect(getSnapshot()).toBeNull();
  });

  it("'Start none' rejects every coalesced spawn with its own refusal", async () => {
    const errors = [new Error(THREAD_REFUSAL), new Error(THREAD_REFUSAL)];
    const spawns = errors.map((e) => spawnWithResourceGuard(vi.fn().mockRejectedValue(e)));
    await vi.waitFor(() => expect(getSnapshot()?.pending).toBe(2));

    resolvePendingResourceBlock(false);
    await expect(spawns[0]).rejects.toBe(errors[0]);
    await expect(spawns[1]).rejects.toBe(errors[1]);
    expect(getGrantsSnapshot()).toEqual([]);
  });

  it("joins a waiting group of its lane even when another lane is at the head", async () => {
    const mem = spawnWithResourceGuard(vi.fn().mockRejectedValue(REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    const t1 = vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("t1");
    const t2 = vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("t2");
    const s1 = spawnWithResourceGuard(t1);
    await vi.waitFor(() => expect(t1).toHaveBeenCalledTimes(1));
    const s2 = spawnWithResourceGuard(t2);
    await vi.waitFor(() => expect(t2).toHaveBeenCalledTimes(1));

    resolvePendingResourceBlock(false); // the memory question
    await expect(mem).rejects.toBe(REFUSAL);
    expect(getSnapshot()).toMatchObject({ metric: "thread_count", pending: 2 });
    resolvePendingResourceBlock(true);
    await expect(Promise.all([s1, s2])).resolves.toEqual(["t1", "t2"]);
  });

  it("never merges refusals that named no lane, even with each other", async () => {
    const a = spawnWithResourceGuard(vi.fn().mockRejectedValue(LEGACY_REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    const second = vi.fn().mockRejectedValue(LEGACY_REFUSAL);
    const b = spawnWithResourceGuard(second);
    await vi.waitFor(() => expect(second).toHaveBeenCalledTimes(1));
    expect(getSnapshot()).toMatchObject({ metric: "unknown", pending: 1 });

    resolvePendingResourceBlock(true);
    // An unknown lane gets no grant — a grant scoped to an unnamed lane is not scoped.
    expect(getGrantsSnapshot()).toEqual([]);
    resolvePendingResourceBlock(false);
    await expect(a).rejects.toBe(LEGACY_REFUSAL);
    await expect(b).rejects.toBe(LEGACY_REFUSAL);
  });

  it("a SERIAL burst (refuse → grant → refuse) shows exactly one dialog", async () => {
    vi.useFakeTimers();
    const listener = vi.fn();
    const unsubscribe = subscribe(listener);
    let dialogs = 0;
    let prev = getSnapshot();
    const countDialogs = subscribe(() => {
      const now = getSnapshot();
      if (now !== null && prev === null) dialogs += 1;
      prev = now;
    });

    const source = { label: "session restore", queued: 5 };
    const first = vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("r0");
    const p0 = spawnWithResourceGuard(first, source);
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await expect(p0).resolves.toBe("r0");
    expect(getGrantsSnapshot()).toMatchObject([
      { metric: "thread_count", remaining: GRANT_SPAWN_LIMIT, sourceLabel: "session restore" },
    ]);

    // The next four records are refused one at a time, each after the previous
    // answer — the shape coalescing alone never merges.
    for (let k = 1; k <= 4; k++) {
      const next = vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce(`r${k}`);
      await expect(spawnWithResourceGuard(next, source)).resolves.toBe(`r${k}`);
      // Still audited: the grant re-invokes WITH the override, so Rust's
      // OVERRIDDEN arm logs and notifies exactly as a click would.
      expect(next).toHaveBeenNthCalledWith(1, false);
      expect(next).toHaveBeenNthCalledWith(2, true);
      expect(getSnapshot()).toBeNull();
    }
    expect(dialogs).toBe(1);
    expect(getGrantsSnapshot()[0].remaining).toBe(GRANT_SPAWN_LIMIT - 4);

    countDialogs();
    unsubscribe();
  });

  it("a spawn the guard admits does not spend the grant", async () => {
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await p;
    await spawnWithResourceGuard(vi.fn().mockResolvedValue("fine"));
    expect(getGrantsSnapshot()[0].remaining).toBe(GRANT_SPAWN_LIMIT);
  });

  it("a refusal that named no lane never spends a live named grant", async () => {
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await p;
    expect(getGrantsSnapshot()[0].remaining).toBe(GRANT_SPAWN_LIMIT);

    // An unknown-lane refusal must ask, not ride the thread grant.
    const legacy = spawnWithResourceGuard(vi.fn().mockRejectedValue(LEGACY_REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).toMatchObject({ metric: "unknown" }));
    expect(getGrantsSnapshot()[0].remaining).toBe(GRANT_SPAWN_LIMIT);
    resolvePendingResourceBlock(false);
    await expect(legacy).rejects.toBe(LEGACY_REFUSAL);
    expect(getGrantsSnapshot()[0].remaining).toBe(GRANT_SPAWN_LIMIT);
  });

  it("a grant is lane-scoped: it never answers another lane's refusal", async () => {
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await p;

    const mem = spawnWithResourceGuard(vi.fn().mockRejectedValue(REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()?.metric).toBe("free_commit_bytes"));
    resolvePendingResourceBlock(false);
    await expect(mem).rejects.toBe(REFUSAL);
  });

  it(`is COUNT-bounded: after ${GRANT_SPAWN_LIMIT} admitted spawns the next refusal asks again`, async () => {
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await p;

    for (let k = 0; k < GRANT_SPAWN_LIMIT; k++) {
      await spawnWithResourceGuard(
        vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce(k),
      );
      expect(getSnapshot()).toBeNull();
    }
    expect(getGrantsSnapshot()).toEqual([]);

    const asked = spawnWithResourceGuard(vi.fn().mockRejectedValue(THREAD_REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(false);
    await expect(asked).rejects.toBe(THREAD_REFUSAL);
  });

  it("lapses after the idle window, and a refusal after expiry asks again", async () => {
    vi.useFakeTimers();
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await p;

    vi.advanceTimersByTime(GRANT_IDLE_MS);
    expect(getGrantsSnapshot()).toEqual([]);

    const asked = spawnWithResourceGuard(vi.fn().mockRejectedValue(THREAD_REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(false);
    await expect(asked).rejects.toBe(THREAD_REFUSAL);
  });

  it("each use slides the idle window, but never past the absolute cap", async () => {
    vi.useFakeTimers();
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    const grantedAt = Date.now();
    resolvePendingResourceBlock(true);
    await p;

    // Use it just before each idle deadline: it survives past one idle window…
    const step = GRANT_IDLE_MS - 1_000;
    let uses = 0;
    while (Date.now() + step < grantedAt + GRANT_MAX_MS && uses < GRANT_SPAWN_LIMIT - 1) {
      vi.advanceTimersByTime(step);
      await spawnWithResourceGuard(
        vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce(uses),
      );
      uses += 1;
      expect(getGrantsSnapshot()[0].expiresAt).toBeLessThanOrEqual(grantedAt + GRANT_MAX_MS);
    }
    expect(Date.now() - grantedAt).toBeGreaterThan(GRANT_IDLE_MS);
    expect(getGrantsSnapshot()).toHaveLength(1);

    // …and is gone AT the absolute cap, although its last use was recent enough
    // that the idle deadline alone would keep it alive well past that moment.
    const idleDeadline = Date.now() + GRANT_IDLE_MS;
    expect(idleDeadline).toBeGreaterThan(grantedAt + GRANT_MAX_MS);
    vi.advanceTimersByTime(grantedAt + GRANT_MAX_MS - Date.now() - 1);
    expect(getGrantsSnapshot()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(getGrantsSnapshot()).toEqual([]);

    const asked = spawnWithResourceGuard(vi.fn().mockRejectedValue(THREAD_REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(false);
    await expect(asked).rejects.toBe(THREAD_REFUSAL);
  });

  it("Revoke ends the grant at once", async () => {
    const p = spawnWithResourceGuard(
      vi.fn().mockRejectedValueOnce(THREAD_REFUSAL).mockResolvedValueOnce("x"),
    );
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(true);
    await p;

    revokeResourceGuardGrant("thread_count");
    expect(getGrantsSnapshot()).toEqual([]);
    const asked = spawnWithResourceGuard(vi.fn().mockRejectedValue(THREAD_REFUSAL));
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    resolvePendingResourceBlock(false);
    await expect(asked).rejects.toBe(THREAD_REFUSAL);
  });
});
