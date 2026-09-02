/**
 * Regression tests for the `registeredAt` backfill.
 *
 * `environment: "node"` here (no jsdom, no `@testing-library/react`), so the
 * exported helper is exercised directly — same precedent as
 * `useTerminalsEvents.test.ts`.
 */

import { describe, it, expect } from "vitest";

import { backfillRegisteredAt } from "./useDiscoveryEvents";

function bridgeWith(registry: Record<string, { registeredAt?: number }>) {
  return {
    registry: {
      getElement: (id: string) => registry[id] ?? null,
    },
  };
}

describe("backfillRegisteredAt", () => {
  /**
   * WHY THIS MATTERS. The runner folds every discover payload into a snapshot
   * signature whose `generation` half hashes each element's `registeredAt` —
   * the only field that says WHICH MOUNT an element belongs to. A payload
   * without it folds ids alone, so `remounted` can never be anything but
   * `false` and the pre-action `fromSnapshotId` gate inherits a permanent
   * false negative.
   */
  it("copies registeredAt out of the live registry onto the payload", () => {
    const payload = { elements: [{ id: "btn_save" }, { id: "inp_name" }] };
    backfillRegisteredAt(
      bridgeWith({ btn_save: { registeredAt: 1724500000000 }, inp_name: { registeredAt: 7 } }),
      payload,
    );
    expect(payload.elements).toEqual([
      { id: "btn_save", registeredAt: 1724500000000 },
      { id: "inp_name", registeredAt: 7 },
    ]);
  });

  /**
   * Unregistered DOM hits — discover synthesizes ids for those — have no
   * registry record. They must contribute NOTHING rather than a spurious
   * constant: per the spec-v1 fold, an absent field folds no bytes, and a
   * fabricated one would move the generation hash for a mount that never
   * happened.
   */
  it("leaves an unregistered element untouched rather than inventing a time", () => {
    const payload = { elements: [{ id: "synthesized_1" }] };
    backfillRegisteredAt(bridgeWith({}), payload);
    expect(payload.elements[0]).toEqual({ id: "synthesized_1" });
    expect(payload.elements[0]).not.toHaveProperty("registeredAt");
  });

  /** A non-numeric registry value is not evidence of a mount time. */
  it("ignores a registry record whose registeredAt is not a number", () => {
    const payload = { elements: [{ id: "btn" }] };
    backfillRegisteredAt(bridgeWith({ btn: { registeredAt: undefined } }), payload);
    expect(payload.elements[0]).not.toHaveProperty("registeredAt");
  });

  /**
   * Deliberately independent of the `stableRef` enrichment that follows it —
   * that one is behind a dynamic import that may legitimately fail, and the
   * backfill must not fail with it, nor throw on a bridge with no registry.
   */
  it("is a no-op, not a throw, when there is nothing to read from", () => {
    expect(() => backfillRegisteredAt(undefined, { elements: [{ id: "a" }] })).not.toThrow();
    expect(() => backfillRegisteredAt({}, { elements: [{ id: "a" }] })).not.toThrow();
    expect(() => backfillRegisteredAt(bridgeWith({}), undefined)).not.toThrow();
    expect(() => backfillRegisteredAt(bridgeWith({}), {})).not.toThrow();
  });

  /** Idempotent: it writes the same registry value the SDK producer will. */
  it("is idempotent, so it stays harmless once the SDK emits the field itself", () => {
    const payload = { elements: [{ id: "btn", registeredAt: 7 }] };
    backfillRegisteredAt(bridgeWith({ btn: { registeredAt: 7 } }), payload);
    backfillRegisteredAt(bridgeWith({ btn: { registeredAt: 7 } }), payload);
    expect(payload.elements[0]).toEqual({ id: "btn", registeredAt: 7 });
  });
});
