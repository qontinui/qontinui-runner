import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import { resolveSignInBackendUrl, signInBackendUrlFrom } from "./savedBackendUrl";

const PROD = "https://api.qontinui.io";

describe("signInBackendUrlFrom", () => {
  it("honours a saved custom remote backend (the clobber this fixes)", () => {
    expect(signInBackendUrlFrom({ backendUrl: "  https://backend.example.test/  " })).toBe(
      "https://backend.example.test/",
    );
  });

  it.each([
    ["http://127.0.0.1:8000"],
    ["http://localhost:8000"],
    ["http://127.1.2.3"],
    ["http://[::1]:8000"],
    ["http://0.0.0.0:8000"],
  ])("keeps production for a loopback saved URL %s (debug-build default)", (backendUrl) => {
    expect(signInBackendUrlFrom({ backendUrl })).toBe(PROD);
  });

  it("keeps production when settings were unreadable (placeholder, not a choice)", () => {
    expect(
      signInBackendUrlFrom({
        backendUrl: "https://backend.example.test",
        settingsFault: { kind: "unreadable" },
      }),
    ).toBe(PROD);
  });

  it.each([[""], ["   "], [null], [undefined], ["not a url"], ["ftp://backend.example.test"]])(
    "keeps production for a blank or invalid saved URL %j",
    (backendUrl) => {
      expect(signInBackendUrlFrom({ backendUrl })).toBe(PROD);
    },
  );

  it("keeps production when the status read failed", () => {
    expect(signInBackendUrlFrom(null)).toBe(PROD);
  });
});

describe("resolveSignInBackendUrl", () => {
  beforeEach(() => mockInvoke.mockReset());

  it("reads get_web_integration_status and returns the saved remote URL", async () => {
    mockInvoke.mockResolvedValueOnce({
      backendUrl: "https://backend.example.test",
      settingsFault: null,
    });
    await expect(resolveSignInBackendUrl()).resolves.toBe("https://backend.example.test");
    expect(mockInvoke).toHaveBeenCalledWith("get_web_integration_status");
  });

  it("falls back to production when the read rejects", async () => {
    mockInvoke.mockRejectedValueOnce("ipc down");
    await expect(resolveSignInBackendUrl()).resolves.toBe(PROD);
  });
});
