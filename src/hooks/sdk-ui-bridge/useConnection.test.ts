/**
 * The connections map `useConnection` rebuilds from a runner listing
 * (`GET /ui-bridge/sdk/status` `allConnections`). The runner never dispatches
 * through a connection whose client would send the operator's bearer off this
 * machine, so such a connection is not offered as a switch target.
 */

import { describe, expect, it } from "vitest";

import type { SdkAppInfo, SdkConnectionInfo } from "./types";
import { normalizeUrl, switchableConnections } from "./useConnection";

const app = (appId: string): SdkAppInfo => ({
  appId,
  appName: appId,
  appType: "web",
  framework: null,
  version: null,
  capabilities: [],
  port: 3000,
});

const listed = (
  url: string,
  sendsBearerOffMachine: boolean,
  bearer: SdkConnectionInfo["bearer"] = "sent",
): SdkConnectionInfo => ({
  url,
  app: app(url),
  connectedAt: 0,
  isActive: false,
  verifiedOrigin: null,
  principalClass: "operator_trust",
  bearer,
  sendsBearerOffMachine,
});

describe("switchableConnections", () => {
  it("leaves out a connection that would send the bearer off this machine", () => {
    const map = switchableConnections([
      listed("http://127.0.0.1:3001", false),
      listed("http://127.0.0.1:8100", true),
      listed("http://127.0.0.1:8200", false, "withheld_from_target"),
    ]);
    expect([...map.keys()]).toEqual(["http://127.0.0.1:3001", "http://127.0.0.1:8200"]);
  });

  it("keys entries the way connect and disconnect do", () => {
    const map = switchableConnections([listed("http://127.0.0.1:3001//", false)]);
    expect(map.has(normalizeUrl("http://127.0.0.1:3001/"))).toBe(true);
    expect(map.get("http://127.0.0.1:3001")?.appId).toBe("http://127.0.0.1:3001//");
  });

  it("is empty for an empty listing", () => {
    expect(switchableConnections([]).size).toBe(0);
  });
});
