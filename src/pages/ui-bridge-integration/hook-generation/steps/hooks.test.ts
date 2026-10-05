import { describe, it, expect, vi, beforeEach } from "vitest";

const readFileMock = vi.fn();
vi.mock("../../integrationApi", () => ({ readFile: (...a: unknown[]) => readFileMock(...a) }));

import { hooksStep } from "./hooks";
import {
  aiMessage,
  appliedStatuses,
  contentOf,
  flushAsync,
  stubContext,
  type StubStepContext,
} from "./stubContext.testutil";
import type { GeneratedFile } from "../parse";

const HOOKS_TURN = aiMessage(
  [
    "Here are your hooks.",
    "```tsx",
    "// FILE: src/lib/ui-bridge/UIBridgeHooks.tsx",
    "export function useRoute() {}",
    "```",
    "```ts",
    "// FILE: src/lib/ui-bridge/state.ts",
    "export const s = 1;",
    "```",
  ].join("\n"),
);

/** Mirror the useStepMachineRefs effect: generatedFiles state -> allGeneratedFilesRef. */
function mirrorGeneratedFiles(ctx: StubStepContext) {
  ctx.setGeneratedFiles.mockImplementation((v: GeneratedFile[]) => {
    ctx.allGeneratedFilesRef.current = v;
  });
}

beforeEach(() => readFileMock.mockReset());

describe("hooksStep", () => {
  it("extracts the // FILE: files and, without the arch spec, goes to preview", () => {
    const ctx = stubContext({ includeArchSpec: false });
    mirrorGeneratedFiles(ctx);
    // Mutate refs AFTER ctx is built: the handler must act on the live refs.
    ctx.pendingStepRef.current = "hooks";
    ctx.allGeneratedFilesRef.current = [{ filePath: "stale.ts", content: "" }];

    hooksStep(ctx, contentOf([HOOKS_TURN]));

    expect(ctx.allGeneratedFilesRef.current.map((f) => f.filePath)).toEqual([
      "src/lib/ui-bridge/UIBridgeHooks.tsx",
      "src/lib/ui-bridge/state.ts",
    ]);
    expect(ctx.pendingStepRef.current).toBeNull();
    expect(ctx.setExpandedFiles).toHaveBeenCalledWith(
      new Set(["src/lib/ui-bridge/UIBridgeHooks.tsx", "src/lib/ui-bridge/state.ts"]),
    );
    expect(ctx.setPhase).toHaveBeenLastCalledWith("preview");
    expect(appliedStatuses(ctx, [{ state: "active", label: "Generate Hook files" }])).toEqual([
      { state: "done", label: "Generate Hook files" },
    ]);
    expect(ctx.sendMessage).not.toHaveBeenCalled();
  });

  it("with the arch spec, moves to architecture-spec and sends the spec prompt", async () => {
    const ctx = stubContext({ includeArchSpec: true, isRegenSpec: false });
    mirrorGeneratedFiles(ctx);
    ctx.pendingStepRef.current = "hooks";

    hooksStep(ctx, contentOf([HOOKS_TURN]));
    await flushAsync();

    expect(ctx.allGeneratedFilesRef.current).toHaveLength(2);
    expect(ctx.pendingStepRef.current).toBe("architecture-spec");
    expect(ctx.setPhase).toHaveBeenCalledWith("generating-spec");
    expect(ctx.setPhase).not.toHaveBeenCalledWith("preview");
    expect(
      appliedStatuses(ctx, [
        { state: "active", label: "Generate Hook files" },
        { state: "pending", label: "Architecture spec" },
      ]),
    ).toEqual([
      { state: "done", label: "Generate Hook files" },
      { state: "active", label: "Architecture spec" },
    ]);
    expect(readFileMock).not.toHaveBeenCalled();
    expect(ctx.sendMessage).toHaveBeenCalledTimes(1);
    expect(ctx.sendMessage.mock.calls[0][0]).toMatch(
      /^Now generate an architecture spec for this project\./,
    );
  });

  it("in regen mode reads the existing spec through integrationApi.readFile first", async () => {
    readFileMock.mockResolvedValue({ success: true, data: '{"projectName":"Old"}' });
    const ctx = stubContext({ includeArchSpec: true, isRegenSpec: true, projectPath: "/p2" });

    hooksStep(ctx, contentOf([HOOKS_TURN]));
    await flushAsync();

    expect(readFileMock).toHaveBeenCalledWith(
      "/p2",
      "project.architecture.uibridge.json",
      ctx.controller.signal,
    );
    expect(ctx.sendMessage).toHaveBeenCalledTimes(1);
  });

  it("with no // FILE: markers, errors back to idle", () => {
    const ctx = stubContext();
    ctx.pendingStepRef.current = "hooks";

    hooksStep(ctx, contentOf([aiMessage("I could not do it.")]));

    expect(ctx.pendingStepRef.current).toBeNull();
    expect(ctx.setGeneratedFiles).not.toHaveBeenCalled();
    expect(ctx.setError).toHaveBeenCalledWith(
      "AI did not produce any files with // FILE: markers. Try regenerating.",
    );
    expect(ctx.setPhase).toHaveBeenLastCalledWith("idle");
    expect(appliedStatuses(ctx, [{ state: "active", label: "Hook files" }])).toEqual([
      { state: "error", label: "Hook files" },
    ]);
  });
});
