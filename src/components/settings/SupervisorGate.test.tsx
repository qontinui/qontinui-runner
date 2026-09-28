/**
 * The dev-only settings panels hide every supervisor control unless the
 * runner OBSERVES a supervisor (plan
 * 2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists, B2).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * panels are rendered with `renderToStaticMarkup` — effects do not run, which
 * is exactly what makes the first-render verdict observable here. The
 * observation hook is mocked; its pure helpers stay real.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { SupervisorObservationState } from "@/hooks/useSupervisorObservation";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/contexts/TenantContext", () => ({
  useTenant: () => ({ defaultTenantIdForNewSessions: null, candidates: [] }),
}));

let mockState: SupervisorObservationState = { kind: "loading" };
vi.mock("@/hooks/useSupervisorObservation", async (importActual) => {
  const actual = await importActual<typeof import("@/hooks/useSupervisorObservation")>();
  return { ...actual, useSupervisorObservation: () => mockState };
});

import { CiRunnerSettings } from "./CiRunnerSettings";
import { DevLoopSettings } from "./DevLoopSettings";
import { DiscoverySettings } from "./DiscoverySettings";
import { NOT_AVAILABLE_TEXT, SupervisorGateView } from "./SupervisorGate";
import {
  nextObservationState,
  observedSupervisorBase,
  observedSupervisorPort,
  withObservedPort,
} from "@/hooks/useSupervisorObservation";

const onLog = () => {};

function read(observed: boolean | null, port: number | null = 4242): SupervisorObservationState {
  return {
    kind: "read",
    observation: {
      observed,
      probed_at: "2026-09-28T00:00:00Z",
      port: observed === null ? null : port,
      base_url: observed === null ? null : `http://127.0.0.1:${port}`,
    },
  };
}

const NOT_SHOWN: Array<[string, SupervisorObservationState]> = [
  ["not observed", read(false)],
  ["unknown (address did not parse)", read(null)],
  ["read failed", { kind: "error", message: "HTTP 404" }],
  ["loading", { kind: "loading" }],
];

/** Anything a supervisor-dependent control would put in the tree. */
function assertNoSupervisorSurface(html: string) {
  expect(html.toLowerCase()).not.toContain("supervisor");
  expect(html).not.toContain("9875");
  expect(html).not.toContain("4242");
  expect(html).not.toContain("CI Runner");
  expect(html).not.toContain("Test My Change");
}

describe("dev-only panels render nothing supervisor-related unless observed", () => {
  beforeEach(() => {
    mockState = { kind: "loading" };
  });

  for (const [label, state] of NOT_SHOWN) {
    it(`CiRunnerSettings — ${label}`, () => {
      mockState = state;
      assertNoSupervisorSurface(renderToStaticMarkup(<CiRunnerSettings onLog={onLog} />));
    });
    it(`DevLoopSettings — ${label}`, () => {
      mockState = state;
      assertNoSupervisorSurface(renderToStaticMarkup(<DevLoopSettings onLog={onLog} />));
    });
  }

  it("not observed renders the neutral line, not a blank page", () => {
    mockState = read(false);
    expect(renderToStaticMarkup(<CiRunnerSettings onLog={onLog} />)).toContain(NOT_AVAILABLE_TEXT);
  });

  it("observed renders the panels, at the observed address (no literal port)", () => {
    mockState = read(true, 4242);
    const ci = renderToStaticMarkup(<CiRunnerSettings onLog={onLog} />);
    expect(ci).not.toContain(NOT_AVAILABLE_TEXT);
    const dev = renderToStaticMarkup(<DevLoopSettings onLog={onLog} />);
    expect(dev).toContain("Test My Change");
    expect(dev).toContain("http://127.0.0.1:4242");
    expect(dev).not.toContain("9875");
  });
});

describe("SupervisorGateView", () => {
  it("hands the child the observed base URL, trailing slash stripped", () => {
    const state: SupervisorObservationState = {
      kind: "read",
      observation: {
        observed: true,
        probed_at: "x",
        port: 7,
        base_url: "http://127.0.0.1:7/",
      },
    };
    const html = renderToStaticMarkup(
      <SupervisorGateView state={state}>
        {(base) => <span>{`child:${base}`}</span>}
      </SupervisorGateView>,
    );
    expect(html).toContain("child:http://127.0.0.1:7<");
  });

  it("never calls the child when not observed", () => {
    const child = vi.fn(() => <span>child</span>);
    for (const [, state] of NOT_SHOWN) {
      renderToStaticMarkup(<SupervisorGateView state={state}>{child}</SupervisorGateView>);
    }
    expect(child).not.toHaveBeenCalled();
  });
});

describe("DiscoverySettings always-scanned list", () => {
  it("names no supervisor port unless one is observed", () => {
    mockState = read(false);
    expect(renderToStaticMarkup(<DiscoverySettings onLog={onLog} />)).not.toContain(
      "hardcoded-port:9875",
    );
    mockState = read(true, 4242);
    expect(renderToStaticMarkup(<DiscoverySettings onLog={onLog} />)).toContain(
      "hardcoded-port:4242",
    );
  });
});

describe("observation helpers", () => {
  it("only an observed listener yields a base URL or port", () => {
    expect(observedSupervisorBase(read(true))).toBe("http://127.0.0.1:4242");
    expect(observedSupervisorBase(read(false))).toBeNull();
    expect(observedSupervisorBase(read(null))).toBeNull();
    expect(observedSupervisorPort(read(true))).toBe(4242);
    expect(observedSupervisorPort(read(false))).toBeNull();
    expect(observedSupervisorPort({ kind: "error", message: "x" })).toBeNull();
  });

  it("withObservedPort appends once, keeps order", () => {
    expect(withObservedPort([1, 2], 3)).toEqual([1, 2, 3]);
    expect(withObservedPort([1, 2], 2)).toEqual([1, 2]);
    expect(withObservedPort([1, 2], null)).toEqual([1, 2]);
  });
});

describe("nextObservationState — a transient failure keeps the last read", () => {
  it("keeps an observed read across a failed re-read", () => {
    const prev = read(true);
    expect(nextObservationState(prev, { ok: false, message: "timeout" })).toBe(prev);
  });
  it("reports error only when there was no read yet", () => {
    expect(nextObservationState({ kind: "loading" }, { ok: false, message: "x" })).toEqual({
      kind: "error",
      message: "x",
    });
  });
  it("an answering read always moves the verdict", () => {
    const next = read(false);
    expect(
      nextObservationState(read(true), {
        ok: true,
        observation: (next as Extract<SupervisorObservationState, { kind: "read" }>).observation,
      }),
    ).toEqual(next);
  });
});
