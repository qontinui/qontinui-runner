import { describe, it, expect, vi } from "vitest";
import { attachSubordinateBridgeInput } from "./subordinateBridgeRegistration";
import {
  attachBridgeInputRegistration,
  type BridgeInputRegistryLike,
} from "./bridgeInputRegistration";

/** Injected interval timers — the watcher is driven by the test, not the clock. */
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
 * Stand-in for the real `UIBridgeRegistry`, faithful in the two ways that
 * matter: `registerElement` returns a fresh per-call `RegisteredElement` (object
 * identity is the ownership key) and overwrites any prior entry for the id.
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

const ID = "terminal-input-abc";

describe("attachSubordinateBridgeInput", () => {
  it("registers a pane that NEVER mounts a TerminalInstance", () => {
    // The iteration-17 case: a flow-grid `assigned-virtual` zone paints its
    // header and close button but mounts zero instances, so nothing ever calls
    // `attachBridgeInputRegistration` for this id.
    const { registry, byId } = fakeRegistry();
    const proxy = { tag: "proxy" };
    const timers = fakeTimers();

    attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => proxy,
      buildDescriptor: () => ({ type: "textarea" }),
      timers: timers.api,
    });

    expect(byId.get(ID)?.element).toBe(proxy);
  });

  it("yields to a mounted instance and never evicts it", () => {
    const { registry, byId } = fakeRegistry();
    const proxy = { tag: "proxy" };
    const real = { tag: "xterm-textarea" };
    const timers = fakeTimers();

    const releaseProxy = attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => proxy,
      buildDescriptor: () => ({}),
      timers: timers.api,
    });
    expect(byId.get(ID)?.element).toBe(proxy);

    // A pane scrolls into view: `TerminalInstance` mounts and registers the
    // real helper textarea, overwriting the proxy entry.
    const releaseInstance = attachBridgeInputRegistration({
      elementId: ID,
      getRegistry: () => registry,
      getInputElement: () => real,
      buildDescriptor: () => ({}),
      timers: timers.api,
    });
    expect(byId.get(ID)?.element).toBe(real);

    // The proxy must NOT take it back, and must not unregister it on teardown.
    timers.tick(5);
    expect(byId.get(ID)?.element).toBe(real);
    releaseProxy();
    expect(byId.get(ID)?.element).toBe(real);

    releaseInstance();
  });

  it("reclaims the id when the instance unmounts (pane scrolled out of view)", () => {
    const { registry, byId } = fakeRegistry();
    const proxy = { tag: "proxy" };
    const real = { tag: "xterm-textarea" };
    const timers = fakeTimers();

    attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => proxy,
      buildDescriptor: () => ({}),
      timers: timers.api,
    });
    const releaseInstance = attachBridgeInputRegistration({
      elementId: ID,
      getRegistry: () => registry,
      getInputElement: () => real,
      buildDescriptor: () => ({}),
      timers: timers.api,
    });
    expect(byId.get(ID)?.element).toBe(real);

    // Virtualization drops the instance. Its instance-keyed cleanup owns the
    // entry, so it unregisters — leaving the id unowned.
    releaseInstance();
    expect(byId.has(ID)).toBe(false);

    // One poll later the proxy has it again: no window in which the pane is
    // invisible to `writeToTerminal`.
    timers.tick(1);
    expect(byId.get(ID)?.element).toBe(proxy);
  });

  it("registers as soon as a late registry appears", () => {
    const { registry, byId } = fakeRegistry();
    let live: BridgeInputRegistryLike | null = null;
    const timers = fakeTimers();

    attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => live,
      getElement: () => ({ tag: "proxy" }),
      buildDescriptor: () => ({}),
      timers: timers.api,
    });
    expect(byId.has(ID)).toBe(false);

    live = registry;
    timers.tick(1);
    expect(byId.has(ID)).toBe(true);
  });

  it("reports an id that stays unowned for the whole budget — the give-up made reachable", () => {
    const onUnowned = vi.fn();
    const timers = fakeTimers();

    attachSubordinateBridgeInput({
      elementId: ID,
      // Never any registry: the pane can never register, and no mounted
      // component exists to run a ladder that could warn.
      getRegistry: () => null,
      getElement: () => ({}),
      buildDescriptor: () => ({}),
      pollMs: 100,
      unownedTimeoutMs: 500,
      onUnowned,
      timers: timers.api,
    });

    timers.tick(4);
    expect(onUnowned).not.toHaveBeenCalled();
    timers.tick(1);
    expect(onUnowned).toHaveBeenCalledTimes(1);
    expect(onUnowned.mock.calls[0][0]).toBeGreaterThanOrEqual(500);

    // Once, not once per tick — a wedged pane must not flood the runner log.
    timers.tick(20);
    expect(onUnowned).toHaveBeenCalledTimes(1);
  });

  it("carries a throwing registry's error into the report instead of calling it a timeout", () => {
    const onUnowned = vi.fn();
    const timers = fakeTimers();
    const boom = new Error("registry exploded");
    const registry: BridgeInputRegistryLike = {
      registerElement() {
        throw boom;
      },
      unregisterElement: () => false,
      getElement: () => undefined,
    };

    attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => ({}),
      buildDescriptor: () => ({}),
      pollMs: 100,
      unownedTimeoutMs: 300,
      onUnowned,
      timers: timers.api,
    });

    timers.tick(3);
    expect(onUnowned).toHaveBeenCalledTimes(1);
    expect(onUnowned.mock.calls[0][1]).toBe(boom);
  });

  it("does not report while it owns the element", () => {
    const onUnowned = vi.fn();
    const { registry } = fakeRegistry();
    const timers = fakeTimers();

    attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => ({}),
      buildDescriptor: () => ({}),
      pollMs: 100,
      unownedTimeoutMs: 200,
      onUnowned,
      timers: timers.api,
    });

    timers.tick(50);
    expect(onUnowned).not.toHaveBeenCalled();
  });

  it("does not report while a mounted instance owns the element", () => {
    const onUnowned = vi.fn();
    const { registry, byId } = fakeRegistry();
    const timers = fakeTimers();
    const real = { tag: "xterm-textarea" };
    registry.registerElement(ID, real, {});

    attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => ({ tag: "proxy" }),
      buildDescriptor: () => ({}),
      pollMs: 100,
      unownedTimeoutMs: 200,
      onUnowned,
      timers: timers.api,
    });

    timers.tick(50);
    expect(onUnowned).not.toHaveBeenCalled();
    expect(byId.get(ID)?.element).toBe(real);
  });

  it("releases its own registration on teardown and stops polling", () => {
    const { registry, byId } = fakeRegistry();
    const timers = fakeTimers();

    const release = attachSubordinateBridgeInput({
      elementId: ID,
      getRegistry: () => registry,
      getElement: () => ({}),
      buildDescriptor: () => ({}),
      timers: timers.api,
    });
    expect(byId.has(ID)).toBe(true);

    release();
    expect(byId.has(ID)).toBe(false);
    expect(timers.liveCount).toBe(0);
    release(); // idempotent
    expect(timers.liveCount).toBe(0);
  });
});
