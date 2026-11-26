// @vitest-environment jsdom
/**
 * The first-run LoginScreen's sign-in target, entered through the REAL screen:
 * the component is rendered (React 19 `createRoot` + `act`, the
 * `StatusIndicator.errorBanner.test.tsx` pattern), `invoke` is mocked, the
 * "Sign in with Qontinui" button is clicked, and the `cognito_sign_in` payload's
 * `backendUrl` is asserted. LoginScreen is a "gate" surface: a machine-local
 * saved URL is never its target, a remote one the build dials is. An
 * UNBOUNDED target (rejected read, settingsFault, missing verdict — after one
 * automatic re-read on the re-readable arms) starts no sign-in: the
 * SignInFallbackNotice asks first.
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
/** Per-read replies for `get_web_integration_status`, consumed in order before
 *  `status` applies; an Error rejects the read. */
let statusReplies: unknown[];

/** A pending sign-in whose browser failed to launch: PendingSignInNotice shows
 *  it at once WHILE active, so a stuck `pending` would be visible. */
const PENDING_NO_BROWSER = {
  attemptId: 1,
  authorizeUrl: "https://auth.example.test/authorize",
  browserLaunchError: "no browser",
  callbackPort: 53682,
  startedAtUnix: 0,
};

beforeEach(() => {
  statusReplies = [];
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "get_web_integration_status") {
      const reply = statusReplies.length > 0 ? statusReplies.shift() : status;
      return reply instanceof Error ? Promise.reject(reply) : Promise.resolve(reply);
    }
    if (cmd === "cognito_sign_in" || cmd === "cognito_sign_in_password") {
      return Promise.resolve({ userId: "u", email: null, tenantId: null, deviceId: "d" });
    }
    if (cmd === "cognito_pending_sign_in") return Promise.resolve(PENDING_NO_BROWSER);
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

  it("signs in against production for a saved IPv6-unspecified bind-all backend the runner classes machine-local", async () => {
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

// ---------------------------------------------------------------------------
// Unbounded target: the screen withholds and asks.
// ---------------------------------------------------------------------------

const REMOTE = {
  backendUrl: "https://backend.example.com",
  settingsFault: null,
  backendUrlMachineLocal: false,
  backendUrlRefused: false,
};
const FAULTED = { ...REMOTE, settingsFault: { path: "settings.json", error: "bad" } };
const NO_VERDICT = { backendUrl: "https://backend.example.com", settingsFault: null };

function calls(cmd: string) {
  return invokeMock.mock.calls.filter(([c]) => c === cmd);
}

const q = <T extends Element = HTMLElement>(sel: string) => host.querySelector<T>(sel);

async function settle() {
  for (let i = 0; i < 4; i++) await act(async () => {});
}

async function render() {
  await act(async () => {
    root.render(<LoginScreen />);
  });
}

async function click(sel: string) {
  const el = q<HTMLButtonElement>(sel);
  expect(el).not.toBeNull();
  expect(el!.disabled).toBe(false);
  await act(async () => {
    el!.click();
  });
  await settle();
}

function typeInto(sel: string, value: string) {
  const el = q<HTMLInputElement>(sel)!;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function submitPassword(email = "op@example.test", password = "hunter2") {
  typeInto("#email", email);
  typeInto("#password", password);
  await click("#button-sign-in-password");
}

describe("LoginScreen unbounded sign-in target (gate surface)", () => {
  it("withholds the sign-in after a rejected read and its re-read: notice shown, no cognito_sign_in, nothing claims a sign-in in flight", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await render();
    await click('[data-testid="cognito-sign-in"]');
    expect(calls("get_web_integration_status")).toHaveLength(2);
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-notice")).not.toBeNull();
    expect(q("#signin-fallback-notice")!.textContent).toContain(
      "Couldn't read this runner's saved backend settings (",
    );
    expect(q("#signin-fallback-production")!.textContent).toBe("Sign in to api.qontinui.io");
    expect(q("#signin-fallback-effect")!.textContent).toBe(
      "This signs in to production and, if the runner's settings are readable, sets its backend to production.",
    );
    expect(q("#signin-fallback-retry")!.textContent).toBe("Retry and sign in");
    // `loading` / `pending` were cleared: no spinner, no pending-sign-in notice.
    expect(host.textContent).not.toContain("Opening browser…");
    expect(host.textContent).not.toContain("No browser to use?");
    expect(q<HTMLButtonElement>('[data-testid="cognito-sign-in"]')!.disabled).toBe(false);
  });

  it("withholds the PASSWORD sign-in after a rejected read and its re-read: cognito_sign_in_password not invoked", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await render();
    await submitPassword();
    expect(calls("cognito_sign_in_password")).toHaveLength(0);
    expect(q("#signin-fallback-notice")).not.toBeNull();
    expect(host.textContent).not.toContain("Signing in…");
  });

  it("'Sign in to api.qontinui.io' runs the held hosted sign-in against production", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await render();
    await click('[data-testid="cognito-sign-in-github"]');
    await click("#signin-fallback-production");
    expect(calls("cognito_sign_in")).toHaveLength(1);
    expect(calls("cognito_sign_in")[0][1]).toEqual({
      backendUrl: "https://api.qontinui.io",
      identityProvider: "GitHub",
    });
    // The explicit choice issues no further status read.
    expect(calls("get_web_integration_status")).toHaveLength(2);
    expect(q("#signin-fallback-notice")).toBeNull();
  });

  it("editing the email or password while a password sign-in is held drops the held sign-in, so stale credentials are never sent", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await render();
    await submitPassword("op@example.test", "hunter2");
    expect(q("#signin-fallback-notice")).not.toBeNull();
    typeInto("#password", "hunter3");
    await settle();
    expect(q("#signin-fallback-notice")).toBeNull();
    expect(calls("cognito_sign_in_password")).toHaveLength(0);
  });

  it("the password path's held action keeps its email and password", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await render();
    await submitPassword("op@example.test", "hunter2");
    await click("#signin-fallback-production");
    expect(calls("cognito_sign_in_password")).toHaveLength(1);
    expect(calls("cognito_sign_in_password")[0][1]).toEqual({
      email: "op@example.test",
      password: "hunter2",
      backendUrl: "https://api.qontinui.io",
    });
  });

  it("a rejected read whose automatic re-read succeeds shows no notice and signs in to the saved remote", async () => {
    statusReplies = [new Error("ipc down"), REMOTE];
    await render();
    await click('[data-testid="cognito-sign-in"]');
    expect(q("#signin-fallback-notice")).toBeNull();
    expect(calls("cognito_sign_in")).toHaveLength(1);
    expect((calls("cognito_sign_in")[0][1] as { backendUrl: string }).backendUrl).toBe(
      "https://backend.example.com",
    );
  });

  it("a persistent settingsFault shows the notice with the fault's effect line", async () => {
    statusReplies = [FAULTED, FAULTED];
    await render();
    await click('[data-testid="cognito-sign-in"]');
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-effect")!.textContent).toBe(
      "This signs in to production. The runner's saved backend can't be changed while its settings are unreadable.",
    );
    expect(q("#signin-fallback-retry")).not.toBeNull();
  });

  it("a missing verdict shows the notice with no Retry, after exactly one status read", async () => {
    statusReplies = [NO_VERDICT, REMOTE];
    await render();
    await click('[data-testid="cognito-sign-in"]');
    expect(calls("get_web_integration_status")).toHaveLength(1);
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-notice")).not.toBeNull();
    expect(q("#signin-fallback-retry")).toBeNull();
    expect(q("#signin-fallback-effect")!.textContent).toBe(
      "This sets the runner's backend to production.",
    );
  });

  it("'Retry and sign in' issues ONE read and, now bounded, signs in to the saved target", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down"), REMOTE];
    await render();
    await click('[data-testid="cognito-sign-in-google"]');
    expect(calls("get_web_integration_status")).toHaveLength(2);
    await click("#signin-fallback-retry");
    expect(calls("get_web_integration_status")).toHaveLength(3);
    expect(calls("cognito_sign_in")).toHaveLength(1);
    expect(calls("cognito_sign_in")[0][1]).toEqual({
      backendUrl: "https://backend.example.com",
      identityProvider: "Google",
    });
    expect(q("#signin-fallback-notice")).toBeNull();
  });

  it("'Retry and sign in' that is still unbounded keeps the notice, with the new reason, and signs in nowhere", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down"), FAULTED];
    await render();
    await click('[data-testid="cognito-sign-in"]');
    await click("#signin-fallback-retry");
    expect(calls("get_web_integration_status")).toHaveLength(3);
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-effect")!.textContent).toContain("can't be changed");
  });

  it("'Retry and sign in' reads 'Reading…' and is disabled while its read is in flight", async () => {
    let resolveRead!: (v: unknown) => void;
    statusReplies = [
      new Error("ipc down"),
      new Error("ipc down"),
      new Promise((r) => {
        resolveRead = r;
      }),
    ];
    // A pending Promise is passed through as the reply.
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_web_integration_status") {
        const reply = statusReplies.shift();
        if (reply instanceof Promise) return reply;
        return reply instanceof Error ? Promise.reject(reply) : Promise.resolve(reply ?? REMOTE);
      }
      if (cmd === "cognito_sign_in") {
        return Promise.resolve({ userId: "u", email: null, tenantId: null, deviceId: "d" });
      }
      return Promise.resolve(null);
    });
    await render();
    await click('[data-testid="cognito-sign-in"]');
    await click("#signin-fallback-retry");
    const retry = q<HTMLButtonElement>("#signin-fallback-retry")!;
    expect(retry.textContent).toBe("Reading…");
    expect(retry.disabled).toBe(true);
    expect(q<HTMLButtonElement>("#signin-fallback-production")!.disabled).toBe(true);
    await act(async () => resolveRead(REMOTE));
    await settle();
    expect(calls("cognito_sign_in")).toHaveLength(1);
  });
  it("editing the password while 'Retry and sign in' is reading drops the retry's result: the stale credentials are never sent", async () => {
    let resolveRead!: (v: unknown) => void;
    statusReplies = [
      new Error("ipc down"),
      new Error("ipc down"),
      new Promise((r) => {
        resolveRead = r;
      }),
    ];
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_web_integration_status") {
        const reply = statusReplies.shift();
        if (reply instanceof Promise) return reply;
        return reply instanceof Error ? Promise.reject(reply) : Promise.resolve(reply ?? REMOTE);
      }
      return Promise.resolve(null);
    });
    await render();
    await submitPassword("op@example.test", "hunter2");
    await click("#signin-fallback-retry");
    expect(q("#signin-fallback-retry")!.textContent).toBe("Reading…");
    typeInto("#password", "hunter3");
    await act(async () => resolveRead(REMOTE));
    await settle();
    expect(calls("cognito_sign_in_password")).toHaveLength(0);
    expect(q("#signin-fallback-notice")).toBeNull();
  });
});
