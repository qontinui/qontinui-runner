import { describe, it, expect } from "vitest";

import {
  ACCEPT_REMOTE_CREATE_DEFAULT,
  ACCEPT_REMOTE_CREATE_OPTIONS,
  describeCoordMirror,
  isAcceptRemoteCreate,
} from "./remoteCreateSettingsHelpers";

describe("accept_remote_create options", () => {
  it("defaults to off, matching the Rust default", () => {
    expect(ACCEPT_REMOTE_CREATE_DEFAULT).toBe("off");
  });
  it("offers exactly the three wire values the runner accepts", () => {
    expect(ACCEPT_REMOTE_CREATE_OPTIONS.map((o) => o.value)).toEqual([
      "off",
      "same_user",
      "tenant",
    ]);
  });
  it("validates wire values strictly", () => {
    expect(isAcceptRemoteCreate("tenant")).toBe(true);
    expect(isAcceptRemoteCreate("Tenant")).toBe(false);
    expect(isAcceptRemoteCreate("")).toBe(false);
    expect(isAcceptRemoteCreate(undefined)).toBe(false);
  });
});

describe("describeCoordMirror", () => {
  it("reports agreement only for the agreed state", () => {
    expect(describeCoordMirror({ state: "agreed", consistent: true, value: "tenant" }).tone).toBe(
      "ok",
    );
  });
  it("names both values on divergence", () => {
    const d = describeCoordMirror({
      state: "diverged",
      consistent: false,
      local: "off",
      coord: "tenant",
    });
    expect(d.tone).toBe("warn");
    expect(d.text).toContain("tenant");
    expect(d.text).toContain("off");
  });
  it("treats not_stored and unreachable as warnings, not agreement", () => {
    expect(describeCoordMirror({ state: "not_stored", consistent: false }).tone).toBe("warn");
    expect(
      describeCoordMirror({ state: "unreachable", consistent: false, detail: "timeout" }).text,
    ).toContain("timeout");
  });
  it("treats a missing or never-run mirror as unknown", () => {
    expect(describeCoordMirror(null).tone).toBe("unknown");
    expect(describeCoordMirror({ state: "unknown", consistent: false }).tone).toBe("unknown");
  });
});
