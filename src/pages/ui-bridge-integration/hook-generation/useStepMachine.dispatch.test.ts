/**
 * useStepMachine's dispatcher: `handleSessionTransition` must run the
 * `STEP_HANDLERS` entry for `pendingStepRef.current` only on a
 * processing → ready transition, and bridge `chainToDemoScriptRef` only when
 * it dispatched.
 *
 * The hook is driven for real: `renderToStaticMarkup` runs the hook body once
 * (vitest is `environment: "node"`, no DOM, and `@testing-library/react` is not
 * a dependency) and the captured `handleSessionTransition` closure is then
 * invoked outside render, the way HookGenerationPanel's session-state effect
 * calls it. `./steps` is mocked so each handler is a spy.
 */

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

const handlers = vi.hoisted(() => ({
  hooks: vi.fn(),
  "architecture-spec": vi.fn(),
  "page-registrations": vi.fn(),
  "page-spec": vi.fn(),
  "page-tutorial": vi.fn(),
  "page-architecture-diagram": vi.fn(),
  "page-demo-script": vi.fn(),
  "explainer-index": vi.fn(),
  "explainer-cluster": vi.fn(),
  "explainer-page": vi.fn(),
}));
vi.mock("./steps", () => ({ STEP_HANDLERS: handlers }));

import { useStepMachine, type SessionTransitionParams } from "./useStepMachine";
import { aiMessage, userMessage } from "./steps/stubContext.testutil";
import type { AiSession, StepContent, StepContext } from "./steps/types";
import type { ProjectAnalysis } from "../types";

type Machine = ReturnType<typeof useStepMachine>;

function mountMachine(): Machine {
  let machine: Machine | null = null;
  function Harness() {
    machine = useStepMachine({
      session: {} as AiSession,
      projectPath: "/proj",
      analysis: { framework: "react", project_path: "/proj" } as unknown as ProjectAnalysis,
      isRegenSpec: false,
      includeArchSpec: false,
      generatedFiles: [],
      setPhase: vi.fn(),
      setGeneratedFiles: vi.fn(),
      setStepStatuses: vi.fn(),
      setExpandedFiles: vi.fn(),
      setError: vi.fn(),
      setCurrentPageName: vi.fn(),
    });
    return null;
  }
  renderToStaticMarkup(createElement(Harness));
  if (!machine) throw new Error("Harness did not render");
  return machine;
}

function transition(overrides: Partial<SessionTransitionParams> = {}): SessionTransitionParams {
  return {
    controller: new AbortController(),
    setRetryTimer: vi.fn(),
    prevState: "processing",
    sessionState: "ready",
    messages: [],
    streamingContent: "",
    taskRunId: undefined,
    sendMessage: vi.fn(async () => undefined) as unknown as AiSession["sendMessage"],
    ...overrides,
  };
}

function calledHandlers(): string[] {
  return Object.entries(handlers)
    .filter(([, fn]) => fn.mock.calls.length > 0)
    .map(([step]) => step);
}

beforeEach(() => {
  for (const fn of Object.values(handlers)) fn.mockReset();
});

describe("useStepMachine handleSessionTransition", () => {
  it("on processing → ready, runs the pending step's handler and bridges chainToDemoScript", () => {
    const machine = mountMachine();
    machine.pendingStepRef.current = "page-spec";
    machine.processedMessageCountRef.current = 1;
    const params = transition({
      messages: [aiMessage("old reply"), userMessage("next prompt"), aiMessage("new reply")],
      streamingContent: "tail",
    });

    machine.handleSessionTransition(params);

    expect(calledHandlers()).toEqual(["page-spec"]);
    const [ctx, content] = handlers["page-spec"].mock.calls[0] as [StepContext, StepContent];
    expect(ctx.controller).toBe(params.controller);
    expect(ctx.pendingStepRef).toBe(machine.pendingStepRef);
    // Per-page step: only AI messages past the processed cursor, plus streaming tail.
    expect(content.aiMessages.map((m) => m.content)).toEqual(["old reply", "new reply"]);
    expect(content.fullContent).toBe("new reply\n\ntail");
    expect(machine.chainToDemoScriptRef.current).toBe(ctx.flow.chainToDemoScript);
  });

  it.each([
    ["ready → ready", "ready", "ready"],
    ["idle → ready", "idle", "ready"],
    ["ready → processing", "ready", "processing"],
  ])("on %s, runs no handler and does not bridge chainToDemoScript", (_label, prev, next) => {
    const machine = mountMachine();
    machine.pendingStepRef.current = "page-spec";

    machine.handleSessionTransition(transition({ prevState: prev, sessionState: next }));

    expect(calledHandlers()).toEqual([]);
    expect(machine.chainToDemoScriptRef.current).toBeNull();
  });

  it("on processing → ready with no pending step, runs no handler and does not bridge", () => {
    const machine = mountMachine();

    machine.handleSessionTransition(transition());

    expect(calledHandlers()).toEqual([]);
    expect(machine.chainToDemoScriptRef.current).toBeNull();
  });
});
