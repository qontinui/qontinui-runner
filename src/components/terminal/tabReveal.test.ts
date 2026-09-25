import { describe, it, expect } from "vitest";
import { planTabReveal } from "./tabReveal";
import { liveTabIdForSession } from "./useSessionManager";

describe("planTabReveal", () => {
  it("focuses the zone already showing the tab without reassigning", () => {
    expect(planTabReveal({ 0: "a", 1: "b", 2: "c" }, 4, 0, "c")).toEqual({
      zone: 2,
      assign: false,
    });
  });

  it("places a hidden tab in the first empty zone, displacing nothing", () => {
    expect(planTabReveal({ 0: "a", 2: "c" }, 4, 0, "z")).toEqual({ zone: 1, assign: true });
  });

  it("places a hidden tab in the focused zone when every zone is full", () => {
    expect(planTabReveal({ 0: "a", 1: "b" }, 2, 1, "z")).toEqual({ zone: 1, assign: true });
  });

  it("ignores a stale assignment beyond the current layout's zones", () => {
    // Layout shrank from 4 zones to 2; zone 3 still lists the tab.
    expect(planTabReveal({ 0: "a", 1: "b", 3: "z" }, 2, 0, "z")).toEqual({
      zone: 0,
      assign: true,
    });
  });

  it("clamps an out-of-range focused zone to zone 0", () => {
    expect(planTabReveal({ 0: "a" }, 1, 5, "z")).toEqual({ zone: 0, assign: true });
  });

  it("returns null when the layout has no zones", () => {
    expect(planTabReveal({}, 0, 0, "z")).toBeNull();
  });
});

describe("liveTabIdForSession — which card clicks go to a terminal", () => {
  const real = { injected_live_status: null, injected_tab: null };
  const tabs = [
    { id: "t-live", isAlive: true },
    { id: "t-dead", isAlive: false },
  ];

  it("routes a session with a live tab to that tab", () => {
    expect(liveTabIdForSession({ zoneTabId: "t-live", _transcript: real }, tabs)).toBe("t-live");
  });

  it("keeps the transcript for a session whose tab has exited", () => {
    expect(liveTabIdForSession({ zoneTabId: "t-dead", _transcript: real }, tabs)).toBeNull();
  });

  it("keeps the transcript for a session with no tab", () => {
    expect(liveTabIdForSession({ zoneTabId: null, _transcript: real }, tabs)).toBeNull();
  });

  it("keeps the transcript for a tab id this window does not render", () => {
    expect(liveTabIdForSession({ zoneTabId: "t-gone", _transcript: real }, tabs)).toBeNull();
  });

  it("keeps the transcript for an injected fixture even when its tab id matches", () => {
    expect(
      liveTabIdForSession(
        {
          zoneTabId: "t-live",
          _transcript: { injected_live_status: "frozen", injected_tab: null },
        },
        tabs,
      ),
    ).toBeNull();
  });
});
