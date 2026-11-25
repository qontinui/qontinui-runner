/**
 * The Backend Connection status banner: one test per row of the state table.
 * The never-signed-in state must read "Not connected yet" with no spinner and
 * no "Registering" (a status must observe the state it names).
 *
 * vitest runs with `environment: "node"`, so the banner is rendered with
 * `renderToStaticMarkup`.
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { StatusBanner, type WebIntegrationStatus } from "./WebIntegrationSettings";
import { deriveStatusBanner } from "./webIntegrationBanner";

function status(over: Partial<WebIntegrationStatus>): WebIntegrationStatus {
  return {
    enabled: true,
    backendUrl: "https://api.qontinui.io",
    webBaseUrl: "",
    runnerTokenMasked: "",
    runnerId: null,
    lastHeartbeatAt: null,
    registrationError: null,
    wsConnected: false,
    ...over,
  };
}

const html = (s: WebIntegrationStatus | null) =>
  renderToStaticMarkup(<StatusBanner status={s} onRetry={() => {}} />);

describe("deriveStatusBanner", () => {
  it.each([
    ["disabled", status({ enabled: false, runnerId: "r1", wsConnected: true }), "none"],
    ["no status", null, "none"],
    ["connected", status({ runnerId: "r1", wsConnected: true }), "connected"],
    ["reconnecting", status({ runnerId: "r1" }), "reconnecting"],
    ["failed, never registered", status({ registrationError: "boom" }), "failed"],
    [
      "failed, registered but dropped",
      status({ runnerId: "r1", registrationError: "boom" }),
      "failed",
    ],
    ["not connected", status({}), "not-connected"],
    ["ws up, no runner id", status({ wsConnected: true }), "unknown"],
  ])("%s -> %s", (_n, s, kind) => {
    expect(deriveStatusBanner(s).kind).toBe(kind);
  });
});

describe("StatusBanner", () => {
  it("renders nothing when disabled", () => {
    expect(html(status({ enabled: false }))).toBe("");
  });

  it("connected shows the runner id, no spinner", () => {
    const out = html(status({ runnerId: "r1", wsConnected: true }));
    expect(out).toContain("Connected as runner");
    expect(out).toContain("r1");
    expect(out).not.toContain("animate-spin");
  });

  it("reconnecting shows the spinner", () => {
    const out = html(status({ runnerId: "r1" }));
    expect(out).toContain("Reconnecting");
    expect(out).toContain("animate-spin");
  });

  it("a registration error wins over reconnecting and offers Retry", () => {
    const out = html(status({ runnerId: "r1", registrationError: "boom" }));
    expect(out).toContain("Registration failed");
    expect(out).toContain("boom");
    expect(out).toContain("Retry");
    expect(out).not.toContain("Reconnecting");
  });

  it("failed without a runner id offers Retry", () => {
    const out = html(status({ registrationError: "boom" }));
    expect(out).toContain("Registration failed");
    expect(out).toContain("Retry");
  });

  it("never-signed-in reads Not connected yet with no spinner, Registering or Contacting", () => {
    const out = html(status({}));
    expect(out).toContain("Not connected yet");
    expect(out).not.toContain("animate-spin");
    expect(out).not.toContain("Registering");
    expect(out).not.toContain("Contacting");
  });

  it("socket up with no runner id reads Status unknown with no spinner", () => {
    const out = html(status({ wsConnected: true }));
    expect(out).toContain("Status unknown");
    expect(out).not.toContain("animate-spin");
  });
});
