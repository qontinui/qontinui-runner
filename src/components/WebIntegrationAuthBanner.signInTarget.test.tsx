// @vitest-environment jsdom
/**
 * The Authorize button's sign-in target, entered through the REAL
 * WebIntegrationAuthBanner: the component is rendered (React 19 `createRoot` +
 * `act`, the `StatusIndicator.errorBanner.test.tsx` pattern) with `invoke` and
 * the Tauri event bus mocked, Authorize is clicked, and the `cognito_sign_in`
 * payload's `backendUrl` is asserted. The banner is an "operator" surface: a
 * machine-local saved URL IS its target when this build dials it.
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
vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: () => ({ ref: () => {} }),
}));
vi.mock("@/hooks/useRunnerTier", () => ({
  useRunnerTier: () => ({ tier: "qontinui_account" }),
}));
vi.mock("@/contexts/TenantContext", () => ({
  useTenant: () => ({
    showSwitcher: false,
    candidates: [],
    defaultTenantIdForNewSessions: null,
    setDefaultTenantForNewSessions: vi.fn(),
  }),
}));

import { WebIntegrationAuthBanner } from "./WebIntegrationAuthBanner";

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

let host: HTMLDivElement;
let root: Root;
/** What the next `get_web_integration_status` read answers: a status, an Error to reject, or a pending Promise. */
let statusReply: unknown;
/** What `get_coord_credential_posture` answers; `null` (no concluded pass) keeps the credential-dark banner out. */
let postureReply: unknown;

/** A concluded posture pass whose credential cannot answer coord and whose remedy is a sign-in. */
const DARK_SIGN_IN_POSTURE = {
  posture: "expired",
  canAnswer: false,
  cta: "sign_in",
  reason: "The coord device credential expired.",
  since: null,
};

function emit(event: string, payload: unknown = null) {
  for (const h of listeners.get(event) ?? []) h({ payload });
}

beforeEach(() => {
  listeners.clear();
  sessionStorage.clear();
  postureReply = null;
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "get_web_integration_status":
        if (statusReply instanceof Promise) return statusReply;
        return statusReply instanceof Error
          ? Promise.reject(statusReply)
          : Promise.resolve(statusReply);
      // Unpaired: the "Connect this runner" banner with Authorize is shown.
      case "device_jwt_present":
        return Promise.resolve(false);
      // Default: no concluded posture pass — the credential-dark banner stays out.
      case "get_coord_credential_posture":
        return Promise.resolve(postureReply);
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
    root.render(<WebIntegrationAuthBanner />);
  });
  await act(async () => {});
}

/** Authorize is disabled and a click starts no sign-in: the target is unknown. */
async function expectAuthorizePaused(): Promise<void> {
  const button = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent === "Authorize",
  );
  expect(button).toBeDefined();
  expect(button!.disabled).toBe(true);
  const before = invokeMock.mock.calls.filter(([cmd]) => cmd === "cognito_sign_in").length;
  await act(async () => {
    button!.click();
  });
  await act(async () => {});
  expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "cognito_sign_in").length).toBe(before);
}

async function clickAuthorize(): Promise<string | undefined> {
  const button = Array.from(host.querySelectorAll("button")).find(
    (b) => b.textContent === "Authorize",
  );
  expect(button).toBeDefined();
  await act(async () => {
    button!.click();
  });
  await act(async () => {});
  const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "cognito_sign_in");
  expect(calls.length).toBeGreaterThan(0);
  return (calls[calls.length - 1][1] as { backendUrl?: string }).backendUrl;
}

describe("WebIntegrationAuthBanner Authorize sign-in target (operator surface)", () => {
  it("signs in against a saved loopback backend in a debug build (machine-local, not refused)", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    expect(await clickAuthorize()).toBe("http://127.0.0.1:8000");
  });

  it("signs in against production when this release build refuses the saved loopback backend", async () => {
    statusReply = RELEASE_LOOPBACK;
    await mount();
    expect(await clickAuthorize()).toBe("https://api.qontinui.io");
  });

  it("pauses Authorize after a failed status re-read instead of guessing production, though the banner still shows", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    await expectAuthorizePaused();
  });

  it("ignores an older failed re-read that settles after a newer successful one", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    let rejectOlder!: (e: Error) => void;
    statusReply = new Promise((_resolve, reject) => {
      rejectOlder = reject;
    });
    await act(async () => {
      emit("web-integration-changed");
    });
    statusReply = DEBUG_LOOPBACK;
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {
      rejectOlder(new Error("slow read failed late"));
    });
    await act(async () => {});
    expect(await clickAuthorize()).toBe("http://127.0.0.1:8000");
  });

  it("ignores an older successful re-read that settles after a newer failed one", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    let resolveOlder!: (v: unknown) => void;
    statusReply = new Promise((resolve) => {
      resolveOlder = resolve;
    });
    await act(async () => {
      emit("web-integration-changed");
    });
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {
      resolveOlder(DEBUG_LOOPBACK);
    });
    await act(async () => {});
    await expectAuthorizePaused();
  });

  it("targets the saved loopback backend again once a later re-read succeeds", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    // The failing re-read has taken effect before the next one is issued.
    await expectAuthorizePaused();
    statusReply = DEBUG_LOOPBACK;
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    expect(await clickAuthorize()).toBe("http://127.0.0.1:8000");
  });
});

const SIGN_IN_TARGET_UNKNOWN_TEXT =
  "Could not read the saved backend settings, so sign-in is paused until the next successful read.";

function credentialCta(): HTMLButtonElement {
  // The credential-dark banner (role=alert) is shown, not the "Connect this runner" one.
  const alert = host.querySelector('[role="alert"]');
  expect(alert).not.toBeNull();
  expect(alert!.textContent).toContain("The coord device credential expired.");
  const buttons = Array.from(alert!.querySelectorAll("button"));
  expect(buttons.some((b) => b.textContent === "Authorize")).toBe(false);
  const cta = buttons.find((b) => b.textContent === "Sign in" || b.textContent === "Working…");
  expect(cta).toBeDefined();
  return cta!;
}

function signInCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "cognito_sign_in");
}

describe("WebIntegrationAuthBanner credential-dark sign-in CTA target (operator surface)", () => {
  it("signs in against the saved loopback backend from the credential-dark CTA in a debug build", async () => {
    statusReply = DEBUG_LOOPBACK;
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    const cta = credentialCta();
    expect(cta.disabled).toBe(false);
    await act(async () => {
      cta.click();
    });
    await act(async () => {});
    const calls = signInCalls();
    expect(calls.length).toBe(1);
    expect((calls[0][1] as { backendUrl?: string }).backendUrl).toBe("http://127.0.0.1:8000");
  });

  it("pauses the credential-dark CTA after a failed status re-read and says why, not 'Working…'", async () => {
    statusReply = DEBUG_LOOPBACK;
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    expect(credentialCta().disabled).toBe(false);
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    const cta = credentialCta();
    expect(cta.disabled).toBe(true);
    expect(cta.textContent).toBe("Sign in");
    expect(cta.textContent).not.toBe("Working…");
    expect(cta.title).toBe(SIGN_IN_TARGET_UNKNOWN_TEXT);
    expect(host.querySelector('[role="alert"]')!.textContent).toContain(
      SIGN_IN_TARGET_UNKNOWN_TEXT,
    );
    await act(async () => {
      cta.click();
    });
    await act(async () => {});
    expect(signInCalls().length).toBe(0);
  });
});
