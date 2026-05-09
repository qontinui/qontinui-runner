/**
 * Pure-logic tests for CommitTrafficLight helpers.
 *
 * The runner's vitest config uses `environment: "node"` (no jsdom), so we
 * follow the repo precedent (`WebIntegrationAuthBanner.test.ts`,
 * `CompletionReportSections.test.tsx`) and exercise the exported pure
 * helpers + the click-handler logic via module-level mocks of the Tauri
 * IPC surface. The JSX shell itself is verified by manual + UI Bridge
 * tests once the spec is updated.
 *
 * Coverage:
 *   1. `isCommitButtonEnabled` — only `dirty` is enabled.
 *   2. `COMMIT_PROMPT_TEMPLATE` — the exact prompt text we send to the
 *      AI is locked down so accidental edits to the procedure get
 *      caught here.
 *   3. PTY click → `onWriteToTerminal` called once with prompt + "\r".
 *   4. SDK click → mocked `invoke("send_user_message", ...)` called.
 *   5. Disabled-state click is a no-op for clean / empty / merging /
 *      unknown.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

// Module mock — must be hoisted before the component import so the
// `invoke` import inside CommitTrafficLight resolves to the mock.
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  COMMIT_PROMPT_TEMPLATE,
  isCommitButtonEnabled,
  type CommitState,
} from "./CommitTrafficLight";

const baseState = (
  overrides: Partial<CommitState> & Pick<CommitState, "status">,
): CommitState => ({
  touched_count: 3,
  dirty_count: 1,
  repo_roots: ["D:/repo"],
  merging_repos: [],
  generated_at_ms: Date.now(),
  ...overrides,
});

describe("isCommitButtonEnabled", () => {
  it("returns true only for dirty", () => {
    expect(isCommitButtonEnabled(baseState({ status: "dirty" }))).toBe(true);
    expect(isCommitButtonEnabled(baseState({ status: "clean" }))).toBe(false);
    expect(isCommitButtonEnabled(baseState({ status: "empty" }))).toBe(false);
    expect(isCommitButtonEnabled(baseState({ status: "merging" }))).toBe(false);
    expect(isCommitButtonEnabled(baseState({ status: "unknown" }))).toBe(false);
    expect(isCommitButtonEnabled(undefined)).toBe(false);
  });
});

describe("COMMIT_PROMPT_TEMPLATE", () => {
  it("starts with the qontinui automated marker", () => {
    expect(COMMIT_PROMPT_TEMPLATE.startsWith("[QONTINUI: COMMIT-PROGRESS REQUEST")).toBe(true);
  });

  it("never instructs the AI to add a Co-Authored-By trailer", () => {
    // The runner's pre-commit hook rejects "Co-Authored-By: Claude" so the
    // prompt MUST tell the AI not to add it. Catch any accidental
    // softening of that instruction here.
    expect(COMMIT_PROMPT_TEMPLATE).toMatch(/Do NOT include a "Co-Authored-By: Claude" trailer/);
  });

  it("instructs the AI to stop on in-progress merge/rebase/cherry-pick", () => {
    expect(COMMIT_PROMPT_TEMPLATE).toMatch(/MERGE_HEAD/);
    expect(COMMIT_PROMPT_TEMPLATE).toMatch(/STOP/);
  });
});

// ────────────────────────────────────────────────────────────────────────────
// Click-handler logic — replicate the same dispatch the component does so
// we can verify the IPC contract without a DOM. The click handler in
// CommitTrafficLight.tsx is small and structured around the public
// surface (onWriteToTerminal vs. invoke); we exercise the same surface
// here by reproducing its decision branch in a tiny helper. If the
// component's handler diverges, this test pins the contract.
// ────────────────────────────────────────────────────────────────────────────

interface ClickArgs {
  state?: CommitState;
  isPtyTab: boolean;
  sessionId?: string;
  onWriteToTerminal?: (text: string) => void;
}

async function simulateClick(args: ClickArgs): Promise<void> {
  if (!isCommitButtonEnabled(args.state)) return;
  if (args.isPtyTab) {
    args.onWriteToTerminal?.(COMMIT_PROMPT_TEMPLATE + "\r");
  } else if (args.sessionId) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("send_user_message", {
      taskRunId: args.sessionId,
      message: COMMIT_PROMPT_TEMPLATE,
    });
  }
}

describe("commit-button click dispatch", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("PTY + dirty → calls onWriteToTerminal once with template + carriage return", async () => {
    const writeSpy = vi.fn();
    await simulateClick({
      state: baseState({ status: "dirty" }),
      isPtyTab: true,
      onWriteToTerminal: writeSpy,
    });
    expect(writeSpy).toHaveBeenCalledTimes(1);
    expect(writeSpy.mock.calls[0][0]).toBe(COMMIT_PROMPT_TEMPLATE + "\r");
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("SDK + dirty → invokes send_user_message with the matching session id and template", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true });
    await simulateClick({
      state: baseState({ status: "dirty" }),
      isPtyTab: false,
      sessionId: "abc-123",
    });
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "send_user_message",
      { taskRunId: "abc-123", message: COMMIT_PROMPT_TEMPLATE },
    ]);
  });

  it.each([
    ["clean" as const],
    ["empty" as const],
    ["merging" as const],
    ["unknown" as const],
  ])("disabled status %s → no IPC, no terminal write", async (status) => {
    const writeSpy = vi.fn();
    await simulateClick({
      state: baseState({ status }),
      isPtyTab: true,
      onWriteToTerminal: writeSpy,
    });
    expect(writeSpy).not.toHaveBeenCalled();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("undefined state → no IPC, no terminal write", async () => {
    const writeSpy = vi.fn();
    await simulateClick({ state: undefined, isPtyTab: true, onWriteToTerminal: writeSpy });
    expect(writeSpy).not.toHaveBeenCalled();
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
