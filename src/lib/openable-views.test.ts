import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  registerOpenableView,
  listOpenableViews,
  openView,
  __resetOpenableViewsForTest,
} from "./openable-views";

describe("openable-views registry", () => {
  beforeEach(() => {
    __resetOpenableViewsForTest();
  });

  it("lists registered views without their opener callbacks", () => {
    registerOpenableView({
      name: "setup-wizard",
      description: "First-launch setup wizard",
      preconditions: "none",
      open: () => {},
    });

    const views = listOpenableViews();
    expect(views).toEqual([
      {
        name: "setup-wizard",
        description: "First-launch setup wizard",
        preconditions: "none",
      },
    ]);
    // The opener is a callback — it must never be serialized over the bridge.
    expect(views[0]).not.toHaveProperty("open");
  });

  it("sorts views by name so the listing is stable", () => {
    const noop = { description: "d", preconditions: "p", open: () => {} };
    registerOpenableView({ name: "zebra", ...noop });
    registerOpenableView({ name: "alpha", ...noop });

    expect(listOpenableViews().map((v) => v.name)).toEqual(["alpha", "zebra"]);
  });

  it("invokes the registered opener and reports success", async () => {
    const open = vi.fn();
    registerOpenableView({
      name: "setup-wizard",
      description: "d",
      preconditions: "p",
      open,
    });

    await expect(openView("setup-wizard")).resolves.toBe(true);
    expect(open).toHaveBeenCalledOnce();
  });

  it("awaits an async opener before resolving", async () => {
    let settled = false;
    registerOpenableView({
      name: "slow",
      description: "d",
      preconditions: "p",
      open: async () => {
        await new Promise((r) => setTimeout(r, 5));
        settled = true;
      },
    });

    await openView("slow");
    expect(settled).toBe(true);
  });

  it("reports false for an unknown view rather than throwing", async () => {
    await expect(openView("nope")).resolves.toBe(false);
  });

  it("unregisters on cleanup", async () => {
    const unregister = registerOpenableView({
      name: "setup-wizard",
      description: "d",
      preconditions: "p",
      open: () => {},
    });

    expect(listOpenableViews()).toHaveLength(1);
    unregister();
    expect(listOpenableViews()).toHaveLength(0);
    await expect(openView("setup-wizard")).resolves.toBe(false);
  });

  it("replaces on re-register so StrictMode double-effects don't wedge it", async () => {
    const first = vi.fn();
    const second = vi.fn();
    registerOpenableView({ name: "v", description: "d", preconditions: "p", open: first });
    registerOpenableView({ name: "v", description: "d", preconditions: "p", open: second });

    expect(listOpenableViews()).toHaveLength(1);
    await openView("v");
    // The live registration wins; the stale one is not invoked.
    expect(second).toHaveBeenCalledOnce();
    expect(first).not.toHaveBeenCalled();
  });

  it("a stale unregister does not remove the live re-registration", async () => {
    const stale = registerOpenableView({
      name: "v",
      description: "d",
      preconditions: "p",
      open: () => {},
    });
    const live = vi.fn();
    registerOpenableView({ name: "v", description: "d", preconditions: "p", open: live });

    // Simulates StrictMode: mount -> remount(replace) -> cleanup of the FIRST mount.
    stale();

    expect(listOpenableViews()).toHaveLength(1);
    await expect(openView("v")).resolves.toBe(true);
    expect(live).toHaveBeenCalledOnce();
  });
});
