// @vitest-environment jsdom
/**
 * Settings → Account's sign-in target, entered through the REAL
 * AccountSettings panel in its signed-out state: the component is rendered
 * (React 19 `createRoot` + `act`, the `WebIntegrationAuthBanner.signInTarget`
 * pattern) with `invoke` and the Tauri event bus mocked, the "Sign in with
 * Qontinui" button is clicked, and both the rendered target
 * (`#account-saved-backend`, `#account-sign-in-target-reason`) and the
 * `cognito_sign_in` payload's `backendUrl` are asserted. The panel is an
 * "operator" surface: a machine-local saved URL IS its target when this build
 * dials it.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (e: { payload: unknown }) => void;
const listeners = new Map<string, Set<Handler>>();

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: Handler) => {
    let set = listeners.get(event);
    if (!set) {
      set = new Set();
      listeners.set(event, set);
    }
    set.add(handler);
    return Promise.resolve(() => {
      set.delete(handler);
    });
  }),
}));
// Signed out: a known, non-account tier, so the SignedOutPanel renders.
const refreshTier = vi.fn(() => Promise.resolve());
vi.mock("@/hooks/useRunnerTier", () => ({
  useRunnerTier: () => ({ tier: "local", loading: false, refresh: refreshTier }),
}));
vi.mock("@/components/AuthProvider", () => ({
  useAuth: () => ({ authStatus: null, signOutFull: vi.fn(), logout: vi.fn() }),
}));

import { AccountSettings } from "./AccountSettings";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const BASE = { enabled: true, runnerTokenMasked: "", registrationError: null };
const DEBUG_LOOPBACK = {
  ...BASE,
  backendUrl: "http://127.0.0.1:8000",
  settingsFault: null,
  backendUrlMachineLocal: true,
  backendUrlRefused: false,
};
const RELEASE_LOOPBACK = { ...DEBUG_LOOPBACK, backendUrlRefused: true };
const DEBUG_REMOTE = {
  ...DEBUG_LOOPBACK,
  backendUrl: "https://backend.example.test",
  backendUrlMachineLocal: false,
};

const COULD_NOT_READ = "Could not read the saved backend URL";

let host: HTMLDivElement;
let root: Root;
/** What the next `get_web_integration_status` read answers: a status, an Error to reject, or a pending Promise. */
let statusReply: unknown;
// Stable across renders: the status effect depends on `onLog`.
const onLog = vi.fn();
const onOpenBackendConnection = vi.fn();

function emit(event: string, payload: unknown = null) {
  for (const h of listeners.get(event) ?? []) h({ payload });
}

beforeEach(() => {
  listeners.clear();
  onLog.mockReset();
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "get_web_integration_status":
        if (statusReply instanceof Promise) return statusReply;
        return statusReply instanceof Error
          ? Promise.reject(statusReply)
          : Promise.resolve(statusReply);
      case "cognito_sign_in":
        return Promise.resolve({ userId: "u", email: null, tenantId: null, deviceId: "d" });
      // No sign-in pending in the browser — the pending notice stays quiet.
      case "cognito_pending_sign_in":
        return Promise.resolve(null);
      default:
        return Promise.resolve(undefined);
    }
  });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function mount() {
  await act(async () => {
    root.render(
      <AccountSettings onLog={onLog} onOpenBackendConnection={onOpenBackendConnection} />,
    );
  });
  await act(async () => {});
}

async function rereadStatus() {
  await act(async () => {
    emit("web-integration-changed");
  });
}

function signInButton(): HTMLButtonElement {
  const button = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent === "Sign in with Qontinui",
  );
  expect(button).toBeDefined();
  return button!;
}

function targetText(): string | null {
  return host.querySelector("#account-saved-backend")?.textContent ?? null;
}

function reasonElement(): Element | null {
  return host.querySelector("#account-sign-in-target-reason");
}

function signInCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "cognito_sign_in");
}

/** The sign-in button is disabled and a click starts no sign-in: the target is unknown. */
async function expectSignInPaused(): Promise<void> {
  const button = signInButton();
  expect(button.disabled).toBe(true);
  const before = signInCalls().length;
  await act(async () => {
    button.click();
  });
  await act(async () => {});
  expect(signInCalls().length).toBe(before);
}

async function clickSignIn(): Promise<string | undefined> {
  const button = signInButton();
  expect(button.disabled).toBe(false);
  await act(async () => {
    button.click();
  });
  await act(async () => {});
  const calls = signInCalls();
  expect(calls.length).toBeGreaterThan(0);
  return (calls[calls.length - 1][1] as { backendUrl?: string }).backendUrl;
}

describe("AccountSettings signed-out sign-in target (operator surface)", () => {
  it("shows and signs in against a saved loopback backend in a debug build (machine-local, not refused)", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    expect(targetText()).toBe("http://127.0.0.1:8000");
    expect(reasonElement()).toBeNull();
    expect(await clickSignIn()).toBe("http://127.0.0.1:8000");
  });

  it("shows production with a release-build reason when this build refuses the saved loopback backend", async () => {
    statusReply = RELEASE_LOOPBACK;
    await mount();
    expect(targetText()).toBe("https://api.qontinui.io");
    expect(reasonElement()).not.toBeNull();
    expect(reasonElement()!.textContent).toContain("release build");
    expect(await clickSignIn()).toBe("https://api.qontinui.io");
  });

  it("disables sign-in and says the URL could not be read after a failed re-read, instead of guessing production", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    expect(targetText()).toBe("http://127.0.0.1:8000");
    statusReply = new Error("get_web_integration_status failed");
    await rereadStatus();
    await act(async () => {});
    expect(targetText()).toBe(COULD_NOT_READ);
    expect(reasonElement()).toBeNull();
    await expectSignInPaused();
  });

  it("ignores an older failed re-read that settles after a newer successful one", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    let rejectOlder!: (e: Error) => void;
    statusReply = new Promise((_resolve, reject) => {
      rejectOlder = reject;
    });
    await rereadStatus();
    statusReply = DEBUG_REMOTE;
    await rereadStatus();
    await act(async () => {
      rejectOlder(new Error("slow read failed late"));
    });
    await act(async () => {});
    expect(targetText()).toBe("https://backend.example.test");
    expect(await clickSignIn()).toBe("https://backend.example.test");
  });

  it("ignores an older successful re-read that settles after a newer failed one", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    let resolveOlder!: (v: unknown) => void;
    statusReply = new Promise((resolve) => {
      resolveOlder = resolve;
    });
    await rereadStatus();
    statusReply = new Error("get_web_integration_status failed");
    await rereadStatus();
    await act(async () => {
      resolveOlder(DEBUG_REMOTE);
    });
    await act(async () => {});
    expect(targetText()).toBe(COULD_NOT_READ);
    await expectSignInPaused();
  });
});
