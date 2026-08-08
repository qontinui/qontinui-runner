import { afterEach, describe, expect, it, vi } from "vitest";

import {
  CRITICAL_REFUSAL_PREFIX,
  getSnapshot,
  parseResourceGuardRefusal,
  resolvePendingResourceBlock,
  spawnWithResourceGuard,
  subscribe,
} from "./resourceGuard";

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

const REFUSAL =
  `${CRITICAL_REFUSAL_PREFIX} Not starting a new terminal session: the host lane has ` +
  `1.00 GiB of free commit, below the 1.50 GiB critical floor.`;

// The queue is a module-level singleton — drain it between tests.
afterEach(() => {
  while (getSnapshot() !== null) resolvePendingResourceBlock(false);
});

describe("parseResourceGuardRefusal", () => {
  it("recognises the typed refusal and strips the prefix", () => {
    expect(parseResourceGuardRefusal(REFUSAL)).toBe(
      "Not starting a new terminal session: the host lane has 1.00 GiB of free commit, " +
        "below the 1.50 GiB critical floor.",
    );
  });

  it("unwraps an Error, since invoke can reject with either shape", () => {
    expect(parseResourceGuardRefusal(new Error(REFUSAL))).toContain("critical floor");
  });

  it("returns null for anything that is not a resource-guard refusal", () => {
    for (const other of [
      "Failed to open PTY: os error 5",
      "terminal:tenant_invalid: nope is not a tenant uuid",
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

  it("queues concurrent refusals instead of stranding the first", async () => {
    const first = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValueOnce("first");
    const second = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValueOnce("second");

    const a = spawnWithResourceGuard(first);
    await vi.waitFor(() => expect(getSnapshot()).not.toBeNull());
    const b = spawnWithResourceGuard(second);
    await vi.waitFor(() => expect(second).toHaveBeenCalledTimes(1));

    // Head answered first; the second refusal is still waiting, not discarded.
    resolvePendingResourceBlock(true);
    await expect(a).resolves.toBe("first");
    expect(getSnapshot()).not.toBeNull();

    resolvePendingResourceBlock(false);
    await expect(b).rejects.toBe(REFUSAL);
    expect(getSnapshot()).toBeNull();
  });

  it("ignores a resolve when nothing is pending", () => {
    expect(() => resolvePendingResourceBlock(true)).not.toThrow();
    expect(getSnapshot()).toBeNull();
  });
});
