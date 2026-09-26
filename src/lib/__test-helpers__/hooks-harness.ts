/**
 * Minimal React hooks harness for hook-level tests.
 *
 * vitest runs `environment: "node"` with no DOM and no React test renderer, so
 * a test drives a hook by mocking `react` with this harness's slot-indexed
 * `useState` / `useRef` / `useEffect` (and an identity `useCallback`). Setters
 * really update state, and each effect runs after the render that queued it
 * when its deps changed. So a hook's effects act on a fixture as they would in
 * React. `renderSettled` re-renders until state stops changing, which is what
 * React does after a commit.
 *
 * One module-level instance, shared by the test file and its `react` mock:
 *
 *   vi.mock("react", async () => {
 *     const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
 *     return hooksHarness.reactMock;
 *   });
 *   import { hooksHarness } from "@/lib/__test-helpers__/hooks-harness";
 */

function createHooksHarness() {
  const slots: unknown[] = [];
  let cursor = 0;
  let dirty = false;
  let pendingEffects: Array<() => void> = [];
  const depsChanged = (prev: unknown[] | undefined, next: unknown[] | undefined) =>
    !prev || !next || prev.length !== next.length || prev.some((d, k) => !Object.is(d, next[k]));

  function useState<T>(init: T | (() => T)) {
    const slot = cursor++;
    if (!(slot in slots)) {
      slots[slot] = typeof init === "function" ? (init as () => T)() : init;
    }
    const set = (v: T | ((prev: T) => T)) => {
      const next = typeof v === "function" ? (v as (p: T) => T)(slots[slot] as T) : v;
      if (!Object.is(next, slots[slot])) dirty = true;
      slots[slot] = next;
    };
    return [slots[slot] as T, set] as const;
  }

  function useRef<T>(init: T) {
    const slot = cursor++;
    if (!(slot in slots)) slots[slot] = { current: init };
    return slots[slot] as { current: T };
  }

  function useEffect(effect: () => void, deps?: unknown[]) {
    const slot = cursor++;
    const prev = slots[slot] as unknown[] | undefined;
    if (depsChanged(prev, deps)) {
      slots[slot] = deps;
      pendingEffects.push(effect);
    }
  }

  return {
    /** Forget every slot: the next render mounts the hook afresh. */
    reset() {
      slots.length = 0;
      cursor = 0;
      dirty = false;
      pendingEffects = [];
    },
    beginRender() {
      cursor = 0;
      dirty = false;
      pendingEffects = [];
    },
    /** Run the effects queued by the last render; true if any state changed. */
    flushEffects(): boolean {
      const effects = pendingEffects;
      pendingEffects = [];
      for (const run of effects) run();
      return dirty;
    },
    /**
     * One settled render of `render` (which must call `beginRender` first):
     * render, run the effects that render queued, and re-render until state
     * stops changing.
     */
    renderSettled<R>(render: () => R, maxPasses = 10): R {
      for (let pass = 0; pass < maxPasses; pass++) {
        const result = render();
        if (!this.flushEffects()) return result;
      }
      throw new Error(`hook did not settle within ${maxPasses} renders`);
    },
    useState,
    useRef,
    useEffect,
    /** The `react` module surface the mock exposes. */
    reactMock: {
      useState,
      useCallback: <F>(fn: F) => fn,
      useRef,
      useEffect,
    },
  };
}

export const hooksHarness = createHooksHarness();
