/**
 * Pre-start restart capability for the Orchestration Loop panels
 * (plan 2026-09-22-orchestration-loop-restart-modes-depend-on-the-dev-only-supervisor,
 * Phase 3).
 *
 * The runner's vitest config is `environment: "node"` and the workspace has no
 * DOM implementation (no jsdom / happy-dom), so — following the repo precedent
 * (`SessionCountBanner.test.tsx`, `CommitTrafficLight.test.tsx`) — the panels'
 * behaviour is pinned through the module both panels delegate to:
 *   - the capability request (over a mocked `invoke`), its debounce and its
 *     stale-answer drop;
 *   - the Start gate and the inline reason, rendered with `renderToStaticMarkup`
 *     through the same `GatedStartButton` / `RestartCapabilityNotice` the
 *     panels render;
 *   - the fresh-form default and the restore rule both panels call.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  CAPABILITY_DEBOUNCE_MS,
  GatedStartButton,
  RestartCapabilityNotice,
  betweenToWire,
  capabilityConfig,
  createCapabilityRequester,
  defaultBetween,
  fetchRestartCapability,
  multiStartBlockedReason,
  normalizeRestartCapability,
  restoreBetween,
  startBlockedReason,
  type CapabilityResult,
  type RestartCapabilityProbe,
  type RestartCapabilityState,
} from "./restartCapability";

const ORCHESTRATOR_REASON =
  "the loop runs inside this runner; restarting it would end the loop — target a secondary instance";

/** Self target (null port) in the given mode, as the single panel builds it. */
const selfProbe = (between: string): RestartCapabilityProbe => ({
  target_runner_port: null,
  target_runner_id: null,
  supervisor_port: 9875,
  between_iterations: betweenToWire(between),
});

const secondaryProbe = (between: string): RestartCapabilityProbe => ({
  target_runner_port: 9877,
  target_runner_id: "runner-2",
  supervisor_port: 9875,
  between_iterations: betweenToWire(between),
});

/** Wire answers exactly as the schemas type serializes them (camelCase, None omitted). */
const refusedSelf = {
  supported: false,
  code: "target_is_orchestrator",
  reason: ORCHESTRATOR_REASON,
  targetPort: 9876,
};
const supportedSecondary = {
  supported: true,
  path: "instance_manager",
  targetPort: 9877,
  instanceId: "runner-2",
};

/** Drive one debounced request to completion and return what it delivered. */
async function requestOnce(probes: RestartCapabilityProbe[]): Promise<CapabilityResult[]> {
  const requester = createCapabilityRequester();
  const delivered: CapabilityResult[][] = [];
  requester.request(probes, (r) => delivered.push(r));
  await vi.advanceTimersByTimeAsync(CAPABILITY_DEBOUNCE_MS);
  expect(delivered).toHaveLength(1);
  return delivered[0];
}

beforeEach(() => {
  mockInvoke.mockReset();
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("single panel: self target + restart mode", () => {
  it("shows the target_is_orchestrator reason and disables Start", async () => {
    mockInvoke.mockResolvedValue(refusedSelf);
    const [state] = await requestOnce([selfProbe("restart_runner_no_rebuild")]);

    // The command is asked with the same config shape a start sends.
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    const [cmd, args] = mockInvoke.mock.calls[0];
    expect(cmd).toBe("orchestration_loop_restart_capability");
    expect(args).toEqual({ config: capabilityConfig(selfProbe("restart_runner_no_rebuild")) });
    expect((args as { config: { between_iterations: unknown } }).config.between_iterations).toEqual(
      {
        type: "restart_runner",
        rebuild: false,
      },
    );

    expect(state.status).toBe("ready");
    const reason = startBlockedReason(state);
    expect(reason).toBe(ORCHESTRATOR_REASON);

    const notice = renderToStaticMarkup(<RestartCapabilityNotice state={state} />);
    expect(notice).toContain('data-restart-code="target_is_orchestrator"');
    expect(notice).toContain("Unsupported here:");
    expect(notice).toContain("restarting it would end the loop");

    const button = renderToStaticMarkup(
      <GatedStartButton blockedReason={reason} onClick={() => {}} label="Run Loop" />,
    );
    expect(button).toMatch(/<button[^>]*disabled=""/);
    // The reason is visible text AND the button's title — never a silent grey button.
    expect(button).toContain("Start blocked:");
    expect(button).toMatch(/title="Unsupported here: the loop runs inside this runner/);
  });

  it("falls back to a code-specific sentence when the verdict carries no reason", () => {
    const state: RestartCapabilityState = {
      status: "ready",
      capability: { supported: false, code: "rebuild_needs_dev_supervisor", targetPort: 9877 },
    };
    expect(startBlockedReason(state)).toMatch(/dev supervisor/);
  });
});

describe("a supported capability enables Start", () => {
  it("single panel: runner-managed secondary with Always (no rebuild)", async () => {
    mockInvoke.mockResolvedValue(supportedSecondary);
    const [state] = await requestOnce([secondaryProbe("restart_runner_no_rebuild")]);

    expect(state.status).toBe("ready");
    expect(startBlockedReason(state)).toBeNull();

    const button = renderToStaticMarkup(
      <GatedStartButton blockedReason={null} onClick={() => {}} label="Run Loop" />,
    );
    expect(button).not.toContain('disabled=""');
    expect(button).not.toContain("Start blocked");

    const notice = renderToStaticMarkup(<RestartCapabilityNotice state={state} />);
    expect(notice).toContain("Target :9877 restarts in-process");
  });

  it("multi panel: every assignment supported → Start All enabled", async () => {
    mockInvoke.mockResolvedValue(supportedSecondary);
    const states = await requestOnce([
      secondaryProbe("restart_on_signal_no_rebuild"),
      { ...secondaryProbe("restart_on_signal_no_rebuild"), target_runner_port: 9878 },
    ]);
    expect(mockInvoke).toHaveBeenCalledTimes(2);
    expect(multiStartBlockedReason(states, ["a", "b"])).toBeNull();
  });

  it("multi panel: one refused assignment blocks Start All, naming that loop", async () => {
    mockInvoke.mockImplementation(
      (_cmd: string, args: { config: { target_runner_port: number } }) =>
        Promise.resolve(args.config.target_runner_port === 9876 ? refusedSelf : supportedSecondary),
    );
    const states = await requestOnce([
      secondaryProbe("restart_runner_no_rebuild"),
      { ...secondaryProbe("restart_runner_no_rebuild"), target_runner_port: 9876 },
    ]);
    const reason = multiStartBlockedReason(states, ["secondary", "primary"]);
    expect(reason).toBe(`primary: ${ORCHESTRATOR_REASON}`);

    const button = renderToStaticMarkup(
      <GatedStartButton
        blockedReason={reason}
        extraDisabled={false}
        onClick={() => {}}
        label="Start All"
      />,
    );
    expect(button).toMatch(/<button[^>]*disabled=""/);
    expect(button).toContain("Start blocked: primary:");
  });

  it("an UNKNOWN answer does not block Start (the backend re-checks at start)", async () => {
    mockInvoke.mockRejectedValue("unknown command orchestration_loop_restart_capability");
    const [state] = await requestOnce([selfProbe("restart_runner")]);
    expect(state.status).toBe("unknown");
    expect(startBlockedReason(state)).toBeNull();
    expect(renderToStaticMarkup(<RestartCapabilityNotice state={state} />)).toContain(
      "Restart capability unknown",
    );
  });
});

describe("fresh panel default", () => {
  it("targeting self defaults to wait_healthy (both panels call defaultBetween(true))", () => {
    expect(defaultBetween(true)).toBe("wait_healthy");
    expect(betweenToWire(defaultBetween(true))).toEqual({ type: "wait_healthy" });
  });

  it("the self default is a mode the capability check reports as not needing a restart", async () => {
    mockInvoke.mockResolvedValue({ supported: true, path: "not_needed", targetPort: 9876 });
    const [state] = await requestOnce([selfProbe(defaultBetween(true))]);
    expect(startBlockedReason(state)).toBeNull();
    // `not_needed` renders no notice at all.
    expect(renderToStaticMarkup(<RestartCapabilityNotice state={state} />)).toBe("");
  });
});

describe("restored saved config", () => {
  it("keeps a saved restart_on_signal verbatim and shows why it is refused", async () => {
    const restored = restoreBetween("restart_on_signal", true);
    expect(restored).toBe("restart_on_signal"); // never silently rewritten

    mockInvoke.mockResolvedValue(refusedSelf);
    const [state] = await requestOnce([selfProbe(restored)]);
    expect(
      (mockInvoke.mock.calls[0][1] as { config: { between_iterations: unknown } }).config
        .between_iterations,
    ).toEqual({ type: "restart_on_signal", rebuild: true });
    const notice = renderToStaticMarkup(<RestartCapabilityNotice state={state} />);
    expect(notice).toContain("restarting it would end the loop");
    expect(startBlockedReason(state)).toBe(ORCHESTRATOR_REASON);
  });

  it("only a MISSING saved value falls back to the default for the restored target", () => {
    expect(restoreBetween(undefined, true)).toBe("wait_healthy");
    expect(restoreBetween("", true)).toBe("wait_healthy");
    expect(restoreBetween(undefined, false)).toBe("restart_on_signal");
    expect(restoreBetween("none", false)).toBe("none");
  });
});

describe("request lifecycle", () => {
  it("debounces rapid target/mode changes into one invoke for the latest probe", async () => {
    mockInvoke.mockResolvedValue(supportedSecondary);
    const requester = createCapabilityRequester();
    const delivered: CapabilityResult[][] = [];
    requester.request([selfProbe("restart_runner")], (r) => delivered.push(r));
    await vi.advanceTimersByTimeAsync(CAPABILITY_DEBOUNCE_MS / 2);
    requester.request([secondaryProbe("restart_runner_no_rebuild")], (r) => delivered.push(r));
    await vi.advanceTimersByTimeAsync(CAPABILITY_DEBOUNCE_MS);

    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(
      (mockInvoke.mock.calls[0][1] as { config: { target_runner_port: number } }).config
        .target_runner_port,
    ).toBe(9877);
    expect(delivered).toHaveLength(1);
  });

  it("drops a stale answer that lands after a newer request", async () => {
    let resolveFirst: (v: unknown) => void = () => {};
    mockInvoke
      .mockImplementationOnce(() => new Promise((res) => (resolveFirst = res)))
      .mockResolvedValueOnce(supportedSecondary);

    const requester = createCapabilityRequester();
    const delivered: CapabilityResult[][] = [];
    requester.request([selfProbe("restart_runner")], (r) => delivered.push(r));
    await vi.advanceTimersByTimeAsync(CAPABILITY_DEBOUNCE_MS); // first invoke in flight
    requester.request([secondaryProbe("restart_runner_no_rebuild")], (r) => delivered.push(r));
    await vi.advanceTimersByTimeAsync(CAPABILITY_DEBOUNCE_MS); // second resolves

    resolveFirst(refusedSelf); // the stale refusal arrives last
    await vi.advanceTimersByTimeAsync(0);

    expect(delivered).toHaveLength(1);
    expect(startBlockedReason(delivered[0][0])).toBeNull();
  });

  it("cancel drops an in-flight answer (panel unmounted or probe changed)", async () => {
    mockInvoke.mockResolvedValue(refusedSelf);
    const requester = createCapabilityRequester();
    const delivered: CapabilityResult[][] = [];
    requester.request([selfProbe("restart_runner")], (r) => delivered.push(r));
    requester.cancel();
    await vi.advanceTimersByTimeAsync(CAPABILITY_DEBOUNCE_MS * 2);
    expect(mockInvoke).not.toHaveBeenCalled();
    expect(delivered).toHaveLength(0);
  });
});

describe("wire normalization", () => {
  it("reads the schemas camelCase shape with omitted optionals", () => {
    expect(normalizeRestartCapability(supportedSecondary)).toEqual({
      supported: true,
      path: "instance_manager",
      code: null,
      reason: null,
      targetPort: 9877,
      instanceId: "runner-2",
    });
  });

  it("folds the snake_case deserialize aliases onto the camelCase keys", () => {
    expect(
      normalizeRestartCapability({ supported: true, target_port: 9878, instance_id: "x" }),
    ).toMatchObject({ targetPort: 9878, instanceId: "x" });
  });

  it("an answer without a verdict is UNKNOWN, never supported", async () => {
    expect(normalizeRestartCapability({ targetPort: 9876 })).toBeNull();
    mockInvoke.mockResolvedValue({});
    const state = await fetchRestartCapability(selfProbe("restart_runner"));
    expect(state.status).toBe("unknown");
  });
});
