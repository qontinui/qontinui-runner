// @vitest-environment jsdom
/**
 * The first-run LoginScreen's sign-in target, entered through the REAL screen:
 * the component is rendered (React 19 `createRoot` + `act`, the
 * `StatusIndicator.errorBanner.test.tsx` pattern), `invoke` is mocked, the
 * "Sign in with Qontinui" button is clicked, and the `cognito_sign_in` payload's
 * `backendUrl` is asserted. LoginScreen is a "gate" surface: a machine-local
 * saved URL is never its target, a remote one the build dials is.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

import { LoginScreen } from "./LoginScreen";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;
let status: unknown;

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "get_web_integration_status") return Promise.resolve(status);
    if (cmd === "cognito_sign_in") {
      return Promise.resolve({ userId: "u", email: null, tenantId: null, deviceId: "d" });
    }
    // No sign-in pending in the browser — the pending notice stays quiet.
    if (cmd === "cognito_pending_sign_in") return Promise.resolve(null);
    return Promise.resolve(undefined);
  });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function clickSignIn(): Promise<string | undefined> {
  await act(async () => {
    root.render(<LoginScreen />);
  });
  const button = host.querySelector<HTMLButtonElement>('[data-testid="cognito-sign-in"]');
  expect(button).not.toBeNull();
  await act(async () => {
    button!.click();
  });
  await act(async () => {});
  const call = invokeMock.mock.calls.find(([cmd]) => cmd === "cognito_sign_in");
  expect(call).toBeDefined();
  return (call![1] as { backendUrl?: string }).backendUrl;
}

describe("LoginScreen sign-in target (gate surface)", () => {
  it("signs in against production when the saved URL is machine-local, even though this build would dial it", async () => {
    status = {
      backendUrl: "http://127.0.0.1:8000",
      settingsFault: null,
      backendUrlMachineLocal: true,
      backendUrlRefused: false,
    };
    expect(await clickSignIn()).toBe("https://api.qontinui.io");
  });

  it("signs in against production for a saved bind-all [::] backend the runner classes machine-local", async () => {
    // The runner's verdict, not a TS host check, decides: the IPv6 unspecified
    // host is machine-local, so a first-run screen does not dial it.
    status = {
      backendUrl: "http://[::]:8000",
      settingsFault: null,
      backendUrlMachineLocal: true,
      backendUrlRefused: false,
    };
    expect(await clickSignIn()).toBe("https://api.qontinui.io");
  });

  it("signs in against a saved remote backend the build dials", async () => {
    status = {
      backendUrl: "https://backend.example.com",
      settingsFault: null,
      backendUrlMachineLocal: false,
      backendUrlRefused: false,
    };
    expect(await clickSignIn()).toBe("https://backend.example.com");
  });
});
