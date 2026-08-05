/**
 * Tests for the zone-profile restore QUEUE — the piece that makes §7.2 step 3
 * survive the one case a bare window event cannot: the target page's tree is
 * not mounted yet when the activation fires, so the event has no listener.
 *
 * `environment: "node"`, so the hook itself is not renderable; the queue is
 * module-level state with two exported accessors, which is exactly the shape
 * the mount-drain and the event handler both go through.
 */

import { describe, it, expect, beforeEach } from "vitest";

import {
  cancelZoneProfileRestore,
  requestZoneProfileRestore,
  takePendingZoneProfileRestore,
} from "./useZoneProfileRestore";

beforeEach(() => {
  // `requestZoneProfileRestore` also dispatches the window event for pages
  // that ARE mounted. vitest's environment is "node", so stub the globals the
  // webview provides — same approach `useTerminalPages.test.ts` takes for
  // `localStorage`. Only the queue half is under test here; the dispatch is
  // recorded and discarded.
  (globalThis as Record<string, unknown>).window = {
    dispatchEvent: () => true,
  };
  if (typeof (globalThis as Record<string, unknown>).CustomEvent === "undefined") {
    (globalThis as Record<string, unknown>).CustomEvent = class {
      detail: unknown;
      constructor(_type: string, init?: { detail?: unknown }) {
        this.detail = init?.detail;
      }
    };
  }
  cancelZoneProfileRestore();
});

describe("zone-profile restore queue", () => {
  it("holds a request until the target page asks for it", () => {
    requestZoneProfileRestore({ profileName: "focus", pageId: "page-a" });
    expect(takePendingZoneProfileRestore("page-a")).toEqual({
      profileName: "focus",
      pageId: "page-a",
    });
  });

  it("is one-shot: a consumed request is not replayed on the next page switch", () => {
    requestZoneProfileRestore({ profileName: "focus", pageId: "page-a" });
    takePendingZoneProfileRestore("page-a");
    expect(takePendingZoneProfileRestore("page-a")).toBeNull();
  });

  it("does not hand a project's profile to a DIFFERENT page", () => {
    // The damage this guard prevents: `useApplyZoneProfile` rewrites the
    // layout of whatever page is active, so delivering to the wrong page
    // reshapes a page the operator never activated.
    requestZoneProfileRestore({ profileName: "focus", pageId: "page-a" });
    expect(takePendingZoneProfileRestore("page-b")).toBeNull();
    expect(takePendingZoneProfileRestore("page-a")).not.toBeNull();
  });

  it("delivers a page-less request to whichever page drains first", () => {
    requestZoneProfileRestore({ profileName: "focus" });
    expect(takePendingZoneProfileRestore("page-b")?.profileName).toBe("focus");
  });

  it("keeps only the newest request when two activations race", () => {
    requestZoneProfileRestore({ profileName: "old", pageId: "page-a" });
    requestZoneProfileRestore({ profileName: "new", pageId: "page-a" });
    expect(takePendingZoneProfileRestore("page-a")?.profileName).toBe("new");
    expect(takePendingZoneProfileRestore("page-a")).toBeNull();
  });

  it("cancel drops an undelivered request so it cannot surface later", () => {
    // Activating a project with no `zone_profile` cancels: otherwise the
    // previous project's queued restore would fire on an unrelated switch to
    // its page, minutes later.
    requestZoneProfileRestore({ profileName: "focus", pageId: "page-a" });
    cancelZoneProfileRestore();
    expect(takePendingZoneProfileRestore("page-a")).toBeNull();
  });

  it("reports nothing queued when nothing was requested", () => {
    expect(takePendingZoneProfileRestore("page-a")).toBeNull();
  });
});
