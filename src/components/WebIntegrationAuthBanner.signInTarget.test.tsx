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
/** A read that SUCCEEDED but reports settings.json unreadable: `backendUrl` is a placeholder. */
const FAULTED = {
  ...BASE,
  backendUrl: "https://backend.example.com",
  settingsFault: { kind: "unreadable" },
  backendUrlMachineLocal: false,
  backendUrlRefused: false,
};

let host: HTMLDivElement;
let root: Root;
/** What the next `get_web_integration_status` read answers: a status, an Error to reject, or a pending Promise. */
let statusReply: unknown;
/** What `get_coord_credential_posture` answers; `null` (no concluded pass) keeps the credential-dark banner out. */
let postureReply: unknown;
/** What `cognito_sign_in` answers: a pending Promise holds the sign-in in flight; anything else resolves. */
let signInReply: unknown;

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
  signInReply = undefined;
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
      case "cognito_sign_in":
        return signInReply instanceof Promise ? signInReply : Promise.resolve(undefined);
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

const SIGN_IN_TARGET_UNKNOWN_TEXT =
  "Could not read the saved backend settings. Retry reads them again before signing in.";
const COULD_NOT_READ_PREFIX = "Could not read";
const RELEASE_REASON_FRAGMENT = "this release build does not dial machine-local backends";

function reasonElement(): Element | null {
  return host.querySelector("#web-integration-banner-sign-in-target-reason");
}

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

function buttonLabelled(label: string, scope: ParentNode = host): HTMLButtonElement | undefined {
  return Array.from(scope.querySelectorAll("button")).find((b) => b.textContent === label);
}

function statusReads(): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "get_web_integration_status").length;
}

function signInCalls() {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === "cognito_sign_in");
}

/** Click an ENABLED "Retry": it re-reads the status once and starts no sign-in. */
async function clickRetry(button: HTMLButtonElement | undefined): Promise<void> {
  expect(button).toBeDefined();
  expect(button!.textContent).toBe("Retry");
  expect(button!.disabled).toBe(false);
  const readsBefore = statusReads();
  const signInsBefore = signInCalls().length;
  await act(async () => {
    button!.click();
  });
  await act(async () => {});
  expect(statusReads()).toBe(readsBefore + 1);
  expect(signInCalls().length).toBe(signInsBefore);
}

/** The target is unknown: Authorize is an ENABLED "Retry" with the reason shown inline, and a click re-reads instead of signing in. */
async function expectAuthorizeRetry(): Promise<void> {
  expect(buttonLabelled("Authorize")).toBeUndefined();
  expect(host.querySelector('[role="status"]')!.textContent).toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
  await clickRetry(buttonLabelled("Retry"));
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

  it("turns Authorize into an enabled Retry after a failed status re-read instead of guessing production, though the banner still shows", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    await expectAuthorizeRetry();
    // The re-read failed again: still Retry, still no sign-in.
    expect(buttonLabelled("Retry")).toBeDefined();
    expect(signInCalls().length).toBe(0);
  });

  it("Retry re-reads the status, and once that read succeeds Authorize signs in to the saved backend", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    statusReply = DEBUG_LOOPBACK;
    await clickRetry(buttonLabelled("Retry"));
    expect(buttonLabelled("Retry")).toBeUndefined();
    expect(host.querySelector('[role="status"]')!.textContent).not.toContain(
      SIGN_IN_TARGET_UNKNOWN_TEXT,
    );
    expect(await clickAuthorize()).toBe("http://127.0.0.1:8000");
    expect(signInCalls().length).toBe(1);
  });

  it("keeps 'Opening…' (busy beats Retry) when a status re-read fails during an in-flight sign-in", async () => {
    statusReply = DEBUG_LOOPBACK;
    signInReply = new Promise(() => {});
    await mount();
    await act(async () => {
      buttonLabelled("Authorize")!.click();
    });
    await act(async () => {});
    expect(buttonLabelled("Opening…")).toBeDefined();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    expect(buttonLabelled("Opening…")!.disabled).toBe(true);
    expect(buttonLabelled("Retry")).toBeUndefined();
    expect(host.textContent).not.toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    expect(signInCalls().length).toBe(1);
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
    await expectAuthorizeRetry();
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
    await expectAuthorizeRetry();
    statusReply = DEBUG_LOOPBACK;
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    expect(await clickAuthorize()).toBe("http://127.0.0.1:8000");
  });

  it("says beside Authorize why it targets production when this release build refuses the saved loopback backend", async () => {
    statusReply = RELEASE_LOOPBACK;
    await mount();
    const banner = host.querySelector('[role="status"]')!;
    expect(reasonElement()).not.toBeNull();
    expect(banner.contains(reasonElement())).toBe(true);
    expect(reasonElement()!.textContent).toContain(RELEASE_REASON_FRAGMENT);
    expect(banner.textContent).not.toContain(COULD_NOT_READ_PREFIX);
    expect(await clickAuthorize()).toBe("https://api.qontinui.io");
  });

  it("shows no reason line when Authorize targets the saved backend", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    expect(buttonLabelled("Authorize")).toBeDefined();
    expect(reasonElement()).toBeNull();
  });

  it("offers Retry instead of signing in when the read succeeds with a settingsFault, and re-reads on click", async () => {
    statusReply = FAULTED;
    await mount();
    await expectAuthorizeRetry();
    // Still faulted: still Retry, never a sign-in to production.
    expect(buttonLabelled("Retry")).toBeDefined();
    expect(reasonElement()).toBeNull();
    expect(signInCalls().length).toBe(0);
    // The fault clears on the next re-read: Authorize signs in to the saved backend.
    statusReply = DEBUG_LOOPBACK;
    await clickRetry(buttonLabelled("Retry"));
    expect(await clickAuthorize()).toBe("http://127.0.0.1:8000");
    expect(signInCalls().length).toBe(1);
  });

  it("shows 'Reading…' (disabled) while a Retry re-read is in flight, then Authorize once it succeeds", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    const pending = deferredStatus();
    statusReply = pending.promise;
    await clickRetry(buttonLabelled("Retry"));
    const reading = buttonLabelled("Reading…");
    expect(reading).toBeDefined();
    expect(reading!.disabled).toBe(true);
    expect(buttonLabelled("Retry")).toBeUndefined();
    await act(async () => {
      pending.resolve(DEBUG_LOOPBACK);
    });
    await act(async () => {});
    expect(buttonLabelled("Reading…")).toBeUndefined();
    expect(buttonLabelled("Authorize")!.disabled).toBe(false);
    expect(signInCalls().length).toBe(0);
  });

  it("returns 'Reading…' to Retry when the retried re-read fails", async () => {
    statusReply = FAULTED;
    await mount();
    const pending = deferredStatus();
    statusReply = pending.promise;
    await clickRetry(buttonLabelled("Retry"));
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

function credentialCta(): HTMLButtonElement {
  // The credential-dark banner (role=alert) is shown, not the "Connect this runner" one.
  const alert = host.querySelector('[role="alert"]');
  expect(alert).not.toBeNull();
  expect(alert!.textContent).toContain("The coord device credential expired.");
  const buttons = Array.from(alert!.querySelectorAll("button"));
  expect(buttons.some((b) => b.textContent === "Authorize")).toBe(false);
  const cta = buttons.find((b) =>
    ["Sign in", "Working…", "Retry", "Reading…"].includes(b.textContent ?? ""),
  );
  expect(cta).toBeDefined();
  return cta!;
}

function alertText(): string {
  return host.querySelector('[role="alert"]')!.textContent ?? "";
}

async function clickCta(): Promise<void> {
  const cta = credentialCta();
  await act(async () => {
    cta.click();
  });
  await act(async () => {});
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

  it("turns the credential-dark CTA into an enabled Retry after a failed status re-read, says why, and re-reads on click", async () => {
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
    expect(cta.textContent).toBe("Retry");
    expect(cta.disabled).toBe(false);
    expect(alertText()).toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    await clickRetry(cta);
    expect(credentialCta().textContent).toBe("Retry");
    expect(signInCalls().length).toBe(0);
  });

  it("returns the credential-dark CTA to 'Sign in' once Retry's re-read succeeds, then signs in to the saved backend", async () => {
    statusReply = DEBUG_LOOPBACK;
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    statusReply = DEBUG_LOOPBACK;
    await clickRetry(credentialCta());
    expect(credentialCta().textContent).toBe("Sign in");
    expect(alertText()).not.toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    await clickCta();
    const calls = signInCalls();
    expect(calls.length).toBe(1);
    expect((calls[0][1] as { backendUrl?: string }).backendUrl).toBe("http://127.0.0.1:8000");
  });

  it("offers Retry on the credential-dark CTA when the FIRST status read fails, and signs in once a retried read lands", async () => {
    statusReply = new Error("get_web_integration_status failed");
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    const cta = credentialCta();
    expect(cta.textContent).toBe("Retry");
    expect(cta.disabled).toBe(false);
    expect(alertText()).toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    await clickRetry(cta);
    expect(credentialCta().textContent).toBe("Retry");
    statusReply = DEBUG_LOOPBACK;
    await clickRetry(credentialCta());
    expect(credentialCta().textContent).toBe("Sign in");
    await clickCta();
    const calls = signInCalls();
    expect(calls.length).toBe(1);
    expect((calls[0][1] as { backendUrl?: string }).backendUrl).toBe("http://127.0.0.1:8000");
  });

  it("keeps 'Working…' on the credential-dark CTA (busy beats Retry) when a status re-read fails during an in-flight sign-in", async () => {
    statusReply = DEBUG_LOOPBACK;
    postureReply = DARK_SIGN_IN_POSTURE;
    signInReply = new Promise(() => {});
    await mount();
    await clickCta();
    expect(credentialCta().textContent).toBe("Working…");
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    const cta = credentialCta();
    expect(cta.textContent).toBe("Working…");
    expect(cta.disabled).toBe(true);
    expect(alertText()).not.toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    expect(signInCalls().length).toBe(1);
  });

  it("keeps the credential-dark CTA disabled under its normal label, with no error, while the FIRST status read is pending", async () => {
    statusReply = new Promise(() => {});
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    const cta = credentialCta();
    expect(cta.textContent).toBe("Sign in");
    expect(cta.disabled).toBe(true);
    expect(alertText()).not.toContain(COULD_NOT_READ_PREFIX);
    expect(reasonElement()).toBeNull();
    const readsBefore = statusReads();
    await act(async () => {
      cta.click();
    });
    await act(async () => {});
    expect(signInCalls().length).toBe(0);
    expect(statusReads()).toBe(readsBefore);
  });

  it("says in the credential-dark banner why its sign-in targets production for a release-refused loopback backend", async () => {
    statusReply = RELEASE_LOOPBACK;
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    expect(host.querySelector('[role="alert"]')!.contains(reasonElement())).toBe(true);
    expect(reasonElement()!.textContent).toContain(RELEASE_REASON_FRAGMENT);
    await clickCta();
    const calls = signInCalls();
    expect(calls.length).toBe(1);
    expect((calls[0][1] as { backendUrl?: string }).backendUrl).toBe("https://api.qontinui.io");
  });

  it("offers Retry on the credential-dark CTA when the read succeeds with a settingsFault, re-reads on click, and never signs in", async () => {
    statusReply = FAULTED;
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    const cta = credentialCta();
    expect(cta.textContent).toBe("Retry");
    expect(cta.disabled).toBe(false);
    expect(alertText()).toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    expect(reasonElement()).toBeNull();
    await clickRetry(cta);
    expect(credentialCta().textContent).toBe("Retry");
    expect(signInCalls().length).toBe(0);
  });

  it("shows 'Reading…' (disabled) on the credential-dark CTA while a Retry re-read is in flight, then 'Sign in' once it succeeds", async () => {
    statusReply = new Error("get_web_integration_status failed");
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    const pending = deferredStatus();
    statusReply = pending.promise;
    await clickRetry(credentialCta());
    const reading = credentialCta();
    expect(reading.textContent).toBe("Reading…");
    expect(reading.disabled).toBe(true);
    expect(alertText()).not.toContain(SIGN_IN_TARGET_UNKNOWN_TEXT);
    await act(async () => {
      pending.resolve(DEBUG_LOOPBACK);
    });
    await act(async () => {});
    const settled = credentialCta();
    expect(settled.textContent).toBe("Sign in");
    expect(settled.disabled).toBe(false);
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

describe("WebIntegrationAuthBanner sign-in handler guards (called directly, not clicked)", () => {
  it("Authorize's handler starts nothing while a Retry's re-read is in flight", async () => {
    statusReply = DEBUG_LOOPBACK;
    await mount();
    statusReply = new Error("get_web_integration_status failed");
    await act(async () => {
      emit("web-integration-changed");
    });
    await act(async () => {});
    statusReply = deferredStatus().promise;
    await clickRetry(buttonLabelled("Retry"));
    const reading = buttonLabelled("Reading…");
    expect(reading!.disabled).toBe(true);
    const readsBefore = statusReads();
    await callOnClickDirectly(reading!);
    expect(statusReads()).toBe(readsBefore);
    expect(signInCalls().length).toBe(0);
  });

  it("the credential-dark CTA's handler starts nothing while the FIRST status read is pending", async () => {
    statusReply = new Promise(() => {});
    postureReply = DARK_SIGN_IN_POSTURE;
    await mount();
    const cta = credentialCta();
    expect(cta.disabled).toBe(true);
    const readsBefore = statusReads();
    await callOnClickDirectly(cta);
    expect(statusReads()).toBe(readsBefore);
    expect(signInCalls().length).toBe(0);
  });
});
