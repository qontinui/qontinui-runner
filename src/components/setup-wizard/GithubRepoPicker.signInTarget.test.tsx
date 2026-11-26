// @vitest-environment jsdom
/**
 * The setup wizard's GitHub step "Sign in to Qontinui", entered through the
 * REAL picker in its signed_out view (React 19 `createRoot` + `act`, the
 * `LoginScreen.signInTarget.test.tsx` pattern) with `invoke`, the event bus and
 * the dialog/opener plugins mocked. The picker is a "gate" surface: an
 * UNBOUNDED sign-in target starts no `cognito_sign_in`; `SignInButton` renders
 * the SignInFallbackNotice, and only its production button sends production.
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
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

import { GithubRepoPicker } from "./GithubRepoPicker";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const REMOTE = {
  backendUrl: "https://backend.example.com",
  settingsFault: null,
  backendUrlMachineLocal: false,
  backendUrlRefused: false,
};
const FAULTED = { ...REMOTE, settingsFault: { path: "settings.json", error: "bad" } };
const NO_VERDICT = { backendUrl: "https://backend.example.com", settingsFault: null };

/** A pending sign-in whose browser failed to launch: PendingSignInNotice shows
 *  it at once WHILE active, so a stuck `signingIn` would be visible. */
const PENDING_NO_BROWSER = {
  attemptId: 1,
  authorizeUrl: "https://auth.example.test/authorize",
  browserLaunchError: "no browser",
  callbackPort: 53682,
  startedAtUnix: 0,
};

let host: HTMLDivElement;
let root: Root;
/** Per-read replies for `get_web_integration_status`, consumed in order; an
 *  Error rejects, a Promise is returned as-is, and an empty queue answers REMOTE. */
let statusReplies: unknown[];

beforeEach(() => {
  statusReplies = [];
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "github_list_repos":
        return Promise.resolve({ signed_in: false, connected: false, repos: [] });
      case "get_web_integration_status": {
        const reply = statusReplies.length > 0 ? statusReplies.shift() : REMOTE;
        if (reply instanceof Promise) return reply;
        return reply instanceof Error ? Promise.reject(reply) : Promise.resolve(reply);
      }
      case "cognito_sign_in":
        return Promise.resolve({ userId: "u", email: null, tenantId: null, deviceId: "d" });
      case "cognito_pending_sign_in":
        return Promise.resolve(PENDING_NO_BROWSER);
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

function calls(cmd: string) {
  return invokeMock.mock.calls.filter(([c]) => c === cmd);
}

const q = <T extends Element = HTMLElement>(sel: string) => host.querySelector<T>(sel);

async function settle() {
  for (let i = 0; i < 4; i++) await act(async () => {});
}

async function mount() {
  await act(async () => {
    root.render(<GithubRepoPicker onProjectsCloned={() => {}} />);
  });
  await settle();
  expect(q('[data-testid="wizard-github-sign-in"]')).not.toBeNull();
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

const SIGN_IN = '[data-testid="wizard-github-sign-in"]';

describe("GithubRepoPicker sign-in target (gate surface)", () => {
  it("signs in to a saved remote backend the build dials", async () => {
    await mount();
    await click(SIGN_IN);
    expect(calls("get_web_integration_status")).toHaveLength(1);
    expect(calls("cognito_sign_in")).toEqual([
      ["cognito_sign_in", { backendUrl: "https://backend.example.com" }],
    ]);
    expect(q("#signin-fallback-notice")).toBeNull();
  });

  it("withholds after a rejected read and its re-read: notice shown, no cognito_sign_in, no 'Opening browser…' and no pending notice", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await mount();
    await click(SIGN_IN);
    expect(calls("get_web_integration_status")).toHaveLength(2);
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-notice")).not.toBeNull();
    expect(q("#signin-fallback-production")!.textContent).toBe("Sign in to api.qontinui.io");
    expect(q("#signin-fallback-effect")!.textContent).toBe(
      "This signs in to production and, if the runner's settings are readable, sets its backend to production.",
    );
    expect(q("#signin-fallback-retry")!.textContent).toBe("Retry and sign in");
    // `signingIn` was cleared on the withhold arm.
    expect(host.textContent).not.toContain("Opening browser…");
    expect(host.textContent).not.toContain("No browser to use?");
    // Rendered once, from inside SignInButton.
    expect(host.querySelectorAll("#signin-fallback-notice")).toHaveLength(1);
  });

  it("'Sign in to api.qontinui.io' signs in against production", async () => {
    statusReplies = [new Error("ipc down"), new Error("ipc down")];
    await mount();
    await click(SIGN_IN);
    await click("#signin-fallback-production");
    expect(calls("cognito_sign_in")).toEqual([
      ["cognito_sign_in", { backendUrl: "https://api.qontinui.io" }],
    ]);
    expect(calls("get_web_integration_status")).toHaveLength(2);
  });

  it("a rejected read whose automatic re-read succeeds shows no notice and signs in to the saved remote", async () => {
    statusReplies = [new Error("ipc down"), REMOTE];
    await mount();
    await click(SIGN_IN);
    expect(q("#signin-fallback-notice")).toBeNull();
    expect(calls("cognito_sign_in")).toEqual([
      ["cognito_sign_in", { backendUrl: "https://backend.example.com" }],
    ]);
  });

  it("a persistent settingsFault shows the notice with the fault's effect line", async () => {
    statusReplies = [FAULTED, FAULTED];
    await mount();
    await click(SIGN_IN);
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-effect")!.textContent).toBe(
      "This signs in to production. The runner's saved backend can't be changed while its settings are unreadable.",
    );
    expect(q("#signin-fallback-retry")).not.toBeNull();
  });

  it("a missing verdict shows the notice with no Retry, after exactly one status read", async () => {
    statusReplies = [NO_VERDICT];
    await mount();
    await click(SIGN_IN);
    expect(calls("get_web_integration_status")).toHaveLength(1);
    expect(calls("cognito_sign_in")).toHaveLength(0);
    expect(q("#signin-fallback-notice")).not.toBeNull();
    expect(q("#signin-fallback-retry")).toBeNull();
  });

  it("'Retry and sign in' issues ONE read and, now bounded, signs in to the saved target", async () => {
    let resolveRead!: (v: unknown) => void;
    statusReplies = [
      new Error("ipc down"),
      new Error("ipc down"),
      new Promise((r) => {
        resolveRead = r;
      }),
    ];
    await mount();
    await click(SIGN_IN);
    await click("#signin-fallback-retry");
    expect(q("#signin-fallback-retry")!.textContent).toBe("Reading…");
    expect(q<HTMLButtonElement>("#signin-fallback-retry")!.disabled).toBe(true);
    await act(async () => resolveRead(REMOTE));
    await settle();
    expect(calls("get_web_integration_status")).toHaveLength(3);
    expect(calls("cognito_sign_in")).toEqual([
      ["cognito_sign_in", { backendUrl: "https://backend.example.com" }],
    ]);
    expect(q("#signin-fallback-notice")).toBeNull();
  });
});
