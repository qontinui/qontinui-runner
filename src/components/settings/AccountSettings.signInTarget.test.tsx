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

/** A read that SUCCEEDED but reports settings.json unreadable: `backendUrl` is a placeholder. */
const FAULTED = {
  ...BASE,
  backendUrl: "https://backend.example.com",
  settingsFault: { kind: "unreadable" },
  backendUrlMachineLocal: false,
  backendUrlRefused: false,
};

const COULD_NOT_READ = "Could not read the saved backend URL";

/** A `get_web_integration_status` reply held in flight until the test settles it. */
function deferredStatus(): {
  promise: Promise<unknown>;
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
} {
  let resolve!: (v: unknown) => void;
  let reject!: (e: Error) => void;
  const promise = new Promise<unknown>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

let host: HTMLDivElement;
let root: Root;
/** What the next `get_web_integration_status` read answers: a status, an Error to reject, or a pending Promise. */
let statusReply: unknown;
/** What `cognito_sign_in` answers: a pending Promise holds the sign-in in flight; anything else resolves. */
let signInReply: unknown;
// Stable across renders: the status effect depends on `onLog`.
const onLog = vi.fn();
const onOpenBackendConnection = vi.fn();

function emit(event: string, payload: unknown = null) {
  for (const h of listeners.get(event) ?? []) h({ payload });
}

beforeEach(() => {
  listeners.clear();
  onLog.mockReset();
  signInReply = undefined;
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "get_web_integration_status":
        if (statusReply instanceof Promise) return statusReply;
        return statusReply instanceof Error
          ? Promise.reject(statusReply)
          : Promise.resolve(statusReply);
      case "cognito_sign_in":
        if (signInReply instanceof Promise) return signInReply;
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

function buttonLabelled(label: string): HTMLButtonElement | undefined {
  return Array.from(host.querySelectorAll("button")).find((b) => b.textContent === label);
}

function statusReads(): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "get_web_integration_status").length;
}

/** After a failed read the sign-in button is an ENABLED "Retry": a click re-reads the status once and starts no sign-in. */
async function expectSignInRetry(): Promise<void> {
  expect(buttonLabelled("Sign in with Qontinui")).toBeUndefined();
  const button = buttonLabelled("Retry");
  expect(button).toBeDefined();
  expect(button!.disabled).toBe(false);
  expect(targetText()).toBe(COULD_NOT_READ);
  const readsBefore = statusReads();
  const signInsBefore = signInCalls().length;
  await act(async () => {
    button!.click();
  });
  await act(async () => {});
  expect(statusReads()).toBe(readsBefore + 1);
  expect(signInCalls().length).toBe(signInsBefore);
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

  it("turns sign-in into an enabled Retry and says the URL could not be read after a failed re-read, instead of guessing production", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    expect(targetText()).toBe("http://127.0.0.1:8000");
    statusReply = new Error("get_web_integration_status failed");
    await rereadStatus();
    await act(async () => {});
    expect(targetText()).toBe(COULD_NOT_READ);
    expect(reasonElement()).toBeNull();
    await expectSignInRetry();
    // The re-read failed again: still Retry, still no sign-in.
    expect(buttonLabelled("Retry")).toBeDefined();
    expect(targetText()).toBe(COULD_NOT_READ);
    expect(signInCalls().length).toBe(0);
  });

  it("Retry re-reads the status, and once that read succeeds sign-in targets the saved backend", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await rereadStatus();
    await act(async () => {});
    statusReply = DEBUG_REMOTE;
    await expectSignInRetry();
    expect(buttonLabelled("Retry")).toBeUndefined();
    expect(targetText()).toBe("https://backend.example.test");
    expect(await clickSignIn()).toBe("https://backend.example.test");
    expect(signInCalls().length).toBe(1);
  });

  it("offers Retry when the FIRST status read fails, and signs in once a retried read lands", async () => {
    statusReply = new Error("get_web_integration_status failed");
    await mount();
    await expectSignInRetry();
    statusReply = DEBUG_LOOPBACK;
    await expectSignInRetry();
    expect(targetText()).toBe("http://127.0.0.1:8000");
    expect(await clickSignIn()).toBe("http://127.0.0.1:8000");
  });

  it("keeps the sign-in button disabled (not Retry) while the first status read is still loading", async () => {
    statusReply = new Promise(() => {});
    await mount();
    expect(targetText()).toBe("loading…");
    const button = signInButton();
    expect(button.disabled).toBe(true);
    expect(buttonLabelled("Retry")).toBeUndefined();
  });

  it("keeps 'Waiting for browser…' (busy beats Retry) when a status re-read fails during an in-flight sign-in", async () => {
    statusReply = DEBUG_LOOPBACK;
    signInReply = new Promise(() => {});
    await mount();
    await act(async () => {
      signInButton().click();
    });
    await act(async () => {});
    statusReply = new Error("get_web_integration_status failed");
    await rereadStatus();
    await act(async () => {});
    const waiting = buttonLabelled("Waiting for browser…");
    expect(waiting).toBeDefined();
    expect(waiting!.disabled).toBe(true);
    expect(buttonLabelled("Retry")).toBeUndefined();
    expect(signInCalls().length).toBe(1);
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
    await expectSignInRetry();
  });

  it("offers Retry instead of signing in when the read succeeds with a settingsFault, and re-reads on click", async () => {
    statusReply = FAULTED;
    await mount();
    expect(targetText()).toBe(COULD_NOT_READ);
    expect(reasonElement()).toBeNull();
    await expectSignInRetry();
    // Still faulted: still Retry, never a sign-in to production.
    expect(buttonLabelled("Retry")).toBeDefined();
    expect(signInCalls().length).toBe(0);
    // The fault clears on the next re-read: sign-in targets the saved backend.
    statusReply = DEBUG_REMOTE;
    await expectSignInRetry();
    expect(targetText()).toBe("https://backend.example.test");
    expect(await clickSignIn()).toBe("https://backend.example.test");
  });

  it("shows 'Reading…' (disabled) while a Retry re-read is in flight, then the sign-in button once it succeeds", async () => {
    statusReply = new Error("get_web_integration_status failed");
    await mount();
    const pending = deferredStatus();
    statusReply = pending.promise;
    await expectSignInRetry();
    const reading = buttonLabelled("Reading…");
    expect(reading).toBeDefined();
    expect(reading!.disabled).toBe(true);
    expect(buttonLabelled("Retry")).toBeUndefined();
    await act(async () => {
      pending.resolve(DEBUG_LOOPBACK);
    });
    await act(async () => {});
    expect(buttonLabelled("Reading…")).toBeUndefined();
    expect(signInButton().disabled).toBe(false);
    expect(signInCalls().length).toBe(0);
  });

  it("returns 'Reading…' to Retry when the retried re-read fails", async () => {
    statusReply = FAULTED;
    await mount();
    const pending = deferredStatus();
    statusReply = pending.promise;
    await expectSignInRetry();
    expect(buttonLabelled("Reading…")!.disabled).toBe(true);
    await act(async () => {
      pending.reject(new Error("get_web_integration_status failed"));
    });
    await act(async () => {});
    expect(buttonLabelled("Reading…")).toBeUndefined();
    expect(buttonLabelled("Retry")!.disabled).toBe(false);
    expect(signInCalls().length).toBe(0);
  });
});

/**
 * Call a button's React `onClick` DIRECTLY. jsdom never dispatches a click to
 * a disabled button, so clicking one tests nothing about the handler's guard.
 */
async function callOnClickDirectly(button: HTMLButtonElement): Promise<void> {
  const key = Object.keys(button).find((k) => k.startsWith("__reactProps$"));
  expect(key).toBeDefined();
  const props = (button as unknown as Record<string, { onClick?: (e?: unknown) => unknown }>)[key!];
  expect(typeof props.onClick).toBe("function");
  await act(async () => {
    await props.onClick!();
  });
  await act(async () => {});
}

describe("AccountSettings sign-in handler guards (called directly, not clicked)", () => {
  it("while the first status read is loading, the handler signs in nowhere, reads nothing, and says it is waiting", async () => {
    statusReply = new Promise(() => {});
    await mount();
    const button = signInButton();
    expect(button.disabled).toBe(true);
    const readsBefore = statusReads();
    await callOnClickDirectly(button);
    expect(statusReads()).toBe(readsBefore);
    expect(signInCalls().length).toBe(0);
    expect(host.textContent).toContain(
      "Waiting for the saved backend settings to load before signing in.",
    );
  });

  it("while a Retry's re-read is in flight, the handler signs in nowhere and issues no second read", async () => {
    statusReply = new Error("get_web_integration_status failed");
    await mount();
    statusReply = deferredStatus().promise;
    await expectSignInRetry();
    const reading = buttonLabelled("Reading…");
    expect(reading!.disabled).toBe(true);
    const readsBefore = statusReads();
    await callOnClickDirectly(reading!);
    expect(statusReads()).toBe(readsBefore);
    expect(signInCalls().length).toBe(0);
    expect(host.textContent).not.toContain("Waiting for the saved backend settings");
  });
});
