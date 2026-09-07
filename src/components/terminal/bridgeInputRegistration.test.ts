import { describe, it, expect, vi } from "vitest";
import {
  attachBridgeInputRegistration,
  type BridgeInputRegistryLike,
} from "./bridgeInputRegistration";

/**
 * Fake interval timers, injected — same rationale as `registerWhenReady.test.ts`:
 * the ladder is driven by the test, never by the wall clock or global state.
 */
function fakeTimers() {
  let next = 1;
  const live = new Map<number, () => void>();
  return {
    api: {
      setInterval: (fn: () => void, _ms: number) => {
        const handle = next++;
        live.set(handle, fn);
        return handle as unknown as ReturnType<typeof setInterval>;
      },
      clearInterval: (handle: ReturnType<typeof setInterval>) => {
        live.delete(handle as unknown as number);
      },
    },
    tick(times = 1) {
      for (let i = 0; i < times; i++) {
        for (const fn of [...live.values()]) fn();
      }
    },
    get liveCount() {
      return live.size;
    },
  };
}

/**
 * A minimal stand-in for the real `UIBridgeRegistry`, faithful in the two ways
 * that matter here: `registerElement` returns a fresh per-call
 * `RegisteredElement` (object identity is the instance key), and it overwrites
 * any prior entry for the same id.
 */
function fakeRegistry() {
  const byId = new Map<string, { id: string; element: object }>();
  const registry: BridgeInputRegistryLike = {
    registerElement(id, element) {
      const registered = { id, element };
      byId.set(id, registered);
      return registered;
    },
    unregisterElement(id) {
      return byId.delete(id);
    },
    getElement(id) {
      return byId.get(id);
    },
  };
  return { registry, byId };
}

describe("attachBridgeInputRegistration", () => {
  it("registers as soon as the registry and the element are both there", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    const element = { tag: "textarea" };

    attachBridgeInputRegistration({
      elementId: "terminal-input-a",
      getRegistry: () => registry,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    expect(byId.get("terminal-input-a")?.element).toBe(element);
    // Synchronous first try — the retry LADDER allocates no timer in the
    // common case. The one live timer is the reclaim watchdog (iter 24, item
    // 1), which starts only once something has landed and exists to take the
    // id back if another claimant overwrites it.
    expect(timers.liveCount).toBe(1);
  });

  /**
   * The rehydration case. A restored pane's backend is built asynchronously, so
   * `getInputElement()` reads null for a while — on the iteration-12 build the
   * registration rode inside that async init path and simply never ran for
   * those panes. Here the ladder is mount-driven, so it keeps polling and lands
   * the moment the renderer attaches the helper textarea.
   */
  it("registers a REHYDRATED pane whose input element attaches long after mount", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    let element: object | null = null;

    attachBridgeInputRegistration({
      elementId: "terminal-input-restored",
      getRegistry: () => registry,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    // Backend still building: nothing registered, but the ladder IS running —
    // the iteration-12 defect was that it was never started at all.
    expect(byId.has("terminal-input-restored")).toBe(false);
    expect(timers.liveCount).toBe(1);

    timers.tick(20);
    expect(byId.has("terminal-input-restored")).toBe(false);

    element = { tag: "textarea", restored: true };
    timers.tick();

    expect(byId.get("terminal-input-restored")?.element).toBe(element);
    // The ladder's timer is cleared; the remaining one is the reclaim
    // watchdog, started at the moment the registration landed.
    expect(timers.liveCount).toBe(1);
  });

  it("registers even when the bridge registry is the late half", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    let live: BridgeInputRegistryLike | null = null;
    const element = { tag: "textarea" };

    attachBridgeInputRegistration({
      elementId: "terminal-input-b",
      getRegistry: () => live,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    expect(byId.size).toBe(0);
    live = registry;
    timers.tick();
    expect(byId.get("terminal-input-b")?.element).toBe(element);
  });

  /**
   * The instance-keyed half of the fix. Cleanup used to be
   * `unregisterElement("terminal-input-" + id)` — keyed on the id, which is
   * per-terminal, not per-mount. A pane moving between the hidden mount and a
   * zone cell therefore tore down the element the NEW instance had just
   * registered, leaving a live pane answering ELEMENT_NOT_FOUND with nothing in
   * the log.
   */
  it("a stale instance cleanup does NOT evict a newer instance registration", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    const oldElement = { tag: "textarea", instance: "old" };
    const newElement = { tag: "textarea", instance: "new" };

    const releaseOld = attachBridgeInputRegistration({
      elementId: "terminal-input-shared",
      getRegistry: () => registry,
      getInputElement: () => oldElement,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    attachBridgeInputRegistration({
      elementId: "terminal-input-shared",
      getRegistry: () => registry,
      getInputElement: () => newElement,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });
    expect(byId.get("terminal-input-shared")?.element).toBe(newElement);

    // The old instance unmounts LAST — the ordering that caused the eviction.
    releaseOld();

    expect(byId.get("terminal-input-shared")?.element).toBe(newElement);
  });

  it("its own cleanup DOES unregister when it still owns the id", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();

    const release = attachBridgeInputRegistration({
      elementId: "terminal-input-c",
      getRegistry: () => registry,
      getInputElement: () => ({ tag: "textarea" }),
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    expect(byId.has("terminal-input-c")).toBe(true);
    release();
    expect(byId.has("terminal-input-c")).toBe(false);
    // Idempotent.
    release();
    expect(byId.has("terminal-input-c")).toBe(false);
  });

  it("cancels the pending ladder on unmount and never registers afterwards", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    let element: object | null = null;

    const release = attachBridgeInputRegistration({
      elementId: "terminal-input-d",
      getRegistry: () => registry,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    expect(timers.liveCount).toBe(1);
    release();
    expect(timers.liveCount).toBe(0);

    element = { tag: "textarea" };
    timers.tick(5);
    expect(byId.size).toBe(0);
  });

  it("warns once when the budget is exhausted", () => {
    const timers = fakeTimers();
    const onGiveUp = vi.fn();

    attachBridgeInputRegistration({
      elementId: "terminal-input-e",
      getRegistry: () => null,
      getInputElement: () => null,
      buildDescriptor: () => ({ type: "textarea" }),
      onGiveUp,
      intervalMs: 100,
      timeoutMs: 500,
      timers: timers.api,
    });

    timers.tick(5);
    expect(onGiveUp).toHaveBeenCalledTimes(1);
    expect(onGiveUp).toHaveBeenCalledWith(500, undefined);
    timers.tick(5);
    expect(onGiveUp).toHaveBeenCalledTimes(1);
  });

  /**
   * A registry that throws must not kill the ladder or the page. It keeps
   * retrying and the give-up report carries the error — loud, not fatal, and
   * never silent. (Iteration 12 threw `TypeError: Illegal invocation` out of a
   * caller that swallowed it, which is how a live pane ended up unregistered
   * with nothing at all in the log.)
   */
  it("survives a throwing attempt and reports the error when it gives up", () => {
    const timers = fakeTimers();
    const onGiveUp = vi.fn();
    const boom = new TypeError("Illegal invocation");

    const registry: BridgeInputRegistryLike = {
      registerElement() {
        throw boom;
      },
      unregisterElement: () => false,
      getElement: () => undefined,
    };

    expect(() =>
      attachBridgeInputRegistration({
        elementId: "terminal-input-f",
        getRegistry: () => registry,
        getInputElement: () => ({ tag: "textarea" }),
        buildDescriptor: () => ({ type: "textarea" }),
        onGiveUp,
        intervalMs: 100,
        timeoutMs: 300,
        timers: timers.api,
      }),
    ).not.toThrow();

    expect(() => timers.tick(3)).not.toThrow();
    expect(onGiveUp).toHaveBeenCalledTimes(1);
    expect(onGiveUp).toHaveBeenCalledWith(300, boom);
  });
});

/**
 * The RECLAIM watchdog — manual-test-loop iteration 24, item 1.
 *
 * `terminal-input-<id>` is a shared key space: `TerminalBridgeProxies` claims
 * the same id whenever it reads as unowned, and the registry is
 * last-write-wins. Landing the registration once therefore decides ownership
 * only until something else writes. Measured on the iteration-23 build: after
 * a soft-nav remount (`/terminal` → `/settings` → `/terminal`) the id pointed
 * at the proxy's hidden 1×1 textarea for the rest of the session, and a
 * visible, painted pane with a live xterm was served through it.
 */
describe("attachBridgeInputRegistration — reclaim watchdog", () => {
  it("takes the id back when another owner overwrites it", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    const element = { tag: "xterm-textarea" };

    attachBridgeInputRegistration({
      elementId: "terminal-input-a",
      getRegistry: () => registry,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      reclaimMs: 250,
      timers: timers.api,
    });

    const mine = byId.get("terminal-input-a");
    expect(mine?.element).toBe(element);

    // A proxy claims the id (this is `attachSubordinateBridgeInput` doing
    // exactly what it is designed to do when the entry reads as unowned).
    const proxyEl = { tag: "proxy-textarea" };
    registry.registerElement("terminal-input-a", proxyEl, {});
    expect(byId.get("terminal-input-a")?.element).toBe(proxyEl);

    // One watchdog tick and the MOUNTED node owns it again.
    timers.tick(1);
    expect(byId.get("terminal-input-a")?.element).toBe(element);
  });

  it("re-registers when the entry is dropped entirely", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();
    const element = { tag: "xterm-textarea" };

    attachBridgeInputRegistration({
      elementId: "terminal-input-b",
      getRegistry: () => registry,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });
    expect(byId.has("terminal-input-b")).toBe(true);

    // A stale instance's unmount, a registry reset, anything.
    registry.unregisterElement("terminal-input-b");
    expect(byId.has("terminal-input-b")).toBe(false);

    timers.tick(1);
    expect(byId.get("terminal-input-b")?.element).toBe(element);
  });

  it("does NOT churn the registry while it still owns the id", () => {
    const timers = fakeTimers();
    const { registry } = fakeRegistry();
    const spy = vi.spyOn(registry, "registerElement");

    attachBridgeInputRegistration({
      elementId: "terminal-input-c",
      getRegistry: () => registry,
      getInputElement: () => ({ tag: "textarea" }),
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });
    expect(spy).toHaveBeenCalledTimes(1);

    timers.tick(20);
    // Twenty ticks, still one registration: the watchdog is a Map lookup per
    // pane per tick, not a re-registration.
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("stops the watchdog on cleanup, leaving no immortal timer", () => {
    const timers = fakeTimers();
    const { registry, byId } = fakeRegistry();

    const detach = attachBridgeInputRegistration({
      elementId: "terminal-input-d",
      getRegistry: () => registry,
      getInputElement: () => ({ tag: "textarea" }),
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });
    expect(timers.liveCount).toBe(1);

    detach();
    expect(timers.liveCount).toBe(0);
    expect(byId.has("terminal-input-d")).toBe(false);

    // And a later foreign claim is NOT stolen back by a dead attachment.
    const proxyEl = { tag: "proxy" };
    registry.registerElement("terminal-input-d", proxyEl, {});
    timers.tick(5);
    expect(byId.get("terminal-input-d")?.element).toBe(proxyEl);
  });

  it("starts no watchdog until something has actually landed", () => {
    const timers = fakeTimers();
    const { registry } = fakeRegistry();
    let element: object | null = null;

    attachBridgeInputRegistration({
      elementId: "terminal-input-e",
      getRegistry: () => registry,
      getInputElement: () => element,
      buildDescriptor: () => ({ type: "textarea" }),
      intervalMs: 100,
      timeoutMs: 1000,
      timers: timers.api,
    });

    // Only the LADDER's timer is live; the watchdog would be a second one.
    expect(timers.liveCount).toBe(1);
    element = { tag: "textarea" };
    timers.tick(1);
    // Ladder satisfied and cleared, watchdog started — still exactly one.
    expect(timers.liveCount).toBe(1);
  });

  it("leaves the registry alone when it cannot answer who owns the id", () => {
    const timers = fakeTimers();
    const registered: object[] = [];
    // No `getElement` — the optional half of `BridgeInputRegistryLike`.
    const registry: BridgeInputRegistryLike = {
      registerElement(id, element) {
        const r = { id, element };
        registered.push(r);
        return r;
      },
      unregisterElement: () => true,
    };

    attachBridgeInputRegistration({
      elementId: "terminal-input-g",
      getRegistry: () => registry,
      getInputElement: () => ({ tag: "textarea" }),
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });
    expect(registered).toHaveLength(1);

    timers.tick(10);
    // Re-registering blindly every tick would churn the registry for no
    // information, so an unanswerable registry gets the old behaviour.
    expect(registered).toHaveLength(1);
  });
});
