// @vitest-environment jsdom
/**
 * `useLiveRef` against a real React render (jsdom): the ref it returns keeps
 * one identity across renders and holds the value of the LATEST committed
 * render. The terminal page's effect signatures rely on both. `useUIComponent`
 * keeps the signature an action was first registered with, so that first
 * closure must keep reading current state through the same ref object
 * (`src/lib/ui-bridge/action-effect-coverage.test.ts` checks that the
 * registrations pass such refs; this file checks that the refs are live).
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { MutableRefObject } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useLiveRef } from "./useLiveRef";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

describe("useLiveRef", () => {
  it("holds the latest committed value, through the ref captured at mount", () => {
    const seen: MutableRefObject<number>[] = [];
    function Probe({ value }: { value: number }) {
      seen.push(useLiveRef(value));
      return null;
    }

    act(() => root?.render(<Probe value={1} />));
    // What a signature built on the first render closes over.
    const firstRef = seen[0];
    expect(firstRef.current).toBe(1);

    act(() => root?.render(<Probe value={2} />));
    act(() => root?.render(<Probe value={3} />));

    expect(seen.every((r) => r === firstRef)).toBe(true);
    expect(firstRef.current).toBe(3);
  });

  it("an object value is replaced, not frozen at the mount-time object", () => {
    const seen: MutableRefObject<{ profiles: string[]; liveTabCount: number }>[] = [];
    function Probe({ profiles, liveTabCount }: { profiles: string[]; liveTabCount: number }) {
      seen.push(useLiveRef({ profiles, liveTabCount }));
      return null;
    }

    act(() => root?.render(<Probe profiles={[]} liveTabCount={0} />));
    act(() => root?.render(<Probe profiles={["dev"]} liveTabCount={2} />));

    expect(seen[0].current).toEqual({ profiles: ["dev"], liveTabCount: 2 });
  });
});
