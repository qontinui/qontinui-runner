/**
 * The runner API base names the address the runner binds — the IPv4 loopback —
 * never `localhost`, which Windows resolves to `::1` first (a failed connect
 * per new connection for any client without happy-eyeballs). Literal
 * expectations, so a regression reds here. Plan
 * 2026-10-25-runner-self-calls-dial-localhost-and-pay-a-doomed-ipv6-connect.
 */
import { describe, expect, it } from "vitest";
import {
  getApiBase,
  getWsBase,
  runnerBaseForPort,
  runnerWsBaseForPort,
  setApiPort,
} from "./runner-api";

describe("runner API base", () => {
  it("formats an explicit port on 127.0.0.1", () => {
    expect(runnerBaseForPort(9876)).toBe("http://127.0.0.1:9876");
    expect(runnerBaseForPort("9877")).toBe("http://127.0.0.1:9877");
    expect(runnerWsBaseForPort(9878)).toBe("ws://127.0.0.1:9878");
  });

  it("follows setApiPort for the bound-port getters", () => {
    setApiPort(9879);
    expect(getApiBase()).toBe("http://127.0.0.1:9879");
    expect(getWsBase()).toBe("ws://127.0.0.1:9879");
  });
});
