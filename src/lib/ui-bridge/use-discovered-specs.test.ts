/**
 * Tests for the runtime spec loader's TARGET — which process it talks to.
 *
 * THE DEFECT these cover (manual-test-loop iteration 26, item 1): the spec-list
 * and spec-subscribe URLs were module constants hardcoding
 * `http://localhost:9876`. Every runner NOT on 9876 issued its spec fetch — and
 * opened a persistent `EventSource` — against whichever OTHER PROCESS owned
 * 9876. Measured live from an instance on 9893, `/ui-bridge/control/specs`
 * answered `400 {"code":"INTERNAL_ERROR","error":"GET
 * http://localhost:9876/apps/qontinui-runner/spec/list failed: HTTP 404 Not
 * Found"}` while the same instance's OWN `/apps/qontinui-runner/spec/list`
 * answered 200 with the full list. A 404 rather than a connection refusal is
 * the proof it crossed a process boundary — and the benign case: with
 * `qontinui-runner` registered on 9876 (which it is whenever the primary runner
 * is up) this instance would have loaded ANOTHER RUNNER'S SPECS with a 200.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mockResolvePort = vi.fn(() => 9876);
vi.mock("@/lib/runner-api", () => ({
  resolvePort: () => mockResolvePort(),
  getApiPort: () => mockResolvePort(),
}));

/** Every URL an `EventSource` was constructed with during a test. */
let eventSourceUrls: string[] = [];

class FakeEventSource {
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    eventSourceUrls.push(url);
  }
  addEventListener(): void {}
  close(): void {}
}

function okListResponse(): Response {
  return {
    ok: true,
    status: 200,
    statusText: "OK",
    json: async () => ({ ok: true, specs: [] }),
  } as unknown as Response;
}

/**
 * A fresh copy of the module under test. It holds a module-scoped cache and a
 * one-shot SSE latch, so every case needs its own.
 */
async function freshModule() {
  vi.resetModules();
  return import("./use-discovered-specs");
}

beforeEach(() => {
  eventSourceUrls = [];
  mockResolvePort.mockReturnValue(9876);
  // The loader guards on `window` before constructing an EventSource; the
  // runner's webview always has one, this "node" test environment does not.
  (globalThis as Record<string, unknown>).window = globalThis;
  (globalThis as Record<string, unknown>).EventSource = FakeEventSource;
});

afterEach(() => {
  delete (globalThis as Record<string, unknown>).window;
  delete (globalThis as Record<string, unknown>).EventSource;
  vi.restoreAllMocks();
});

describe("the spec loader talks to THIS process, on THIS process's port", () => {
  it("fetches the spec list from the resolved port — not a hardcoded 9876", async () => {
    mockResolvePort.mockReturnValue(9895);
    const fetchMock = vi.fn(async () => okListResponse());
    vi.stubGlobal("fetch", fetchMock);

    const { loadDiscoveredSpecs } = await freshModule();
    await loadDiscoveredSpecs();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const url = String(fetchMock.mock.calls[0]?.[0]);
    expect(url).toBe("http://127.0.0.1:9895/apps/qontinui-runner/spec/list");
    // The whole finding in one assertion: no runner may reach into 9876
    // because of a literal in this module.
    expect(url).not.toContain("9876");
  });

  it("subscribes the SSE stream to the resolved port too", async () => {
    // The `EventSource` is PERSISTENT — a hardcoded port there is a standing
    // connection into another process, not a one-off read.
    mockResolvePort.mockReturnValue(9895);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => okListResponse()),
    );

    const { loadDiscoveredSpecs } = await freshModule();
    await loadDiscoveredSpecs();

    expect(eventSourceUrls).toEqual(["http://127.0.0.1:9895/apps/qontinui-runner/spec/subscribe"]);
  });

  it("dials 127.0.0.1, never the name `localhost`", async () => {
    // Windows resolves `localhost` to `::1` first and the runner binds the
    // IPv4 loopback only, so the name costs a doomed IPv6 connect first.
    const fetchMock = vi.fn(async () => okListResponse());
    vi.stubGlobal("fetch", fetchMock);

    const { loadDiscoveredSpecs } = await freshModule();
    await loadDiscoveredSpecs();

    expect(String(fetchMock.mock.calls[0]?.[0])).toMatch(/^http:\/\/127\.0\.0\.1:/);
    expect(String(fetchMock.mock.calls[0]?.[0])).not.toContain("localhost");
    expect(eventSourceUrls[0]).not.toContain("localhost");
  });

  it("re-resolves the port per call rather than freezing it at module load", async () => {
    // `loadDiscoveredSpecs()` runs at App.tsx pre-mount, before `useApiReady`
    // has populated the centralized port. A value captured at module scope is
    // the 9876 default forever.
    const fetchMock = vi.fn(async () => okListResponse());
    vi.stubGlobal("fetch", fetchMock);

    const mod = await freshModule();
    mockResolvePort.mockReturnValue(9891);
    await mod.loadDiscoveredSpecs();
    expect(String(fetchMock.mock.calls[0]?.[0])).toContain(":9891/");
  });
});

describe("an upstream spec-fetch failure is TYPED", () => {
  it("carries the UPSTREAM_FETCH_FAILED prefix and .code on a non-2xx", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: false, status: 404, statusText: "Not Found" }) as Response),
    );

    const { loadDiscoveredSpecs, UPSTREAM_FETCH_FAILED } = await freshModule();
    await expect(loadDiscoveredSpecs()).rejects.toMatchObject({
      code: UPSTREAM_FETCH_FAILED,
    });
    expect(UPSTREAM_FETCH_FAILED).toBe("UPSTREAM_FETCH_FAILED");
  });

  it("types a refused connection the same way", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("Failed to fetch");
      }),
    );

    const { loadDiscoveredSpecs } = await freshModule();
    await expect(loadDiscoveredSpecs()).rejects.toThrow(/^UPSTREAM_FETCH_FAILED: /);
  });

  it("types an `ok:false` body the same way", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          ({
            ok: true,
            status: 200,
            statusText: "OK",
            json: async () => ({ ok: false, reason: "app-not-found" }),
          }) as unknown as Response,
      ),
    );

    const { loadDiscoveredSpecs } = await freshModule();
    await expect(loadDiscoveredSpecs()).rejects.toThrow(/^UPSTREAM_FETCH_FAILED: .*app-not-found/);
  });
});
