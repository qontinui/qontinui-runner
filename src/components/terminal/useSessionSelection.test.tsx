// @vitest-environment jsdom

/**
 * The render-time prune in `useSessionSelection` is keyed on the visible
 * list's CONTENT. Keyed on identity, a caller that rebuilds an equal list each
 * render set state on every render and React aborted with "Too many
 * re-renders" (found by SessionManagerPanel.render.test.tsx in CI).
 */

import { act, createElement, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useSessionSelection, type SessionSelection } from "./useSessionSelection";

let container: HTMLDivElement;
let root: Root;
/** The hook's latest return, published after each commit. */
const probe: { latest: SessionSelection | null } = { latest: null };

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  probe.latest = null;
});

/** Rebuilds `ids` as a NEW array on every render, as an unmemoized caller would. */
function Probe({ ids }: { ids: readonly string[] }) {
  const selection = useSessionSelection({ visibleIds: [...ids], onOpen: () => {} });
  useEffect(() => {
    probe.latest = selection;
  });
  return null;
}

describe("useSessionSelection — pruning the selection to the rendered rows", () => {
  it("an equal list rebuilt on every render does not loop", () => {
    act(() => root.render(createElement(Probe, { ids: ["a", "b"] })));
    act(() => root.render(createElement(Probe, { ids: ["a", "b"] })));
    expect(probe.latest).not.toBeNull();
  });

  it("a row that leaves the rendered list leaves the selection", () => {
    act(() => root.render(createElement(Probe, { ids: ["a", "b"] })));
    act(() => probe.latest!.toggle("a"));
    act(() => probe.latest!.toggle("b"));
    expect([...probe.latest!.selectedIds].sort()).toEqual(["a", "b"]);
    act(() => root.render(createElement(Probe, { ids: ["b"] })));
    expect([...probe.latest!.selectedIds]).toEqual(["b"]);
  });
});
