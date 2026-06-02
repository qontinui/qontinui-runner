/**
 * Wiring test for `useTabSessionIdCapture` — proves the capture-poll
 * SUCCESS path durably persists the session id via `rememberSessionId`.
 *
 * Why a dedicated file: the sibling `useTabSessionIdCapture.test.ts` only
 * exercises a *mirror* of the claim decision (`decideClaim`), so it never
 * runs the hook's real `tick` body and can't catch a regression where the
 * `rememberSessionId(...)` call is missing. That blind spot is exactly the
 * defect this test closes: the durable store read back `null` in live
 * verification because nothing on the capture path actually wrote to it.
 *
 * The runner's vitest env is `node` (no DOM, no @testing-library), so we
 * drive the *real* hook by stubbing React's primitive hooks with synchronous
 * stand-ins. `startCapture` then runs the genuine polling loop in
 * `useTabSessionIdCapture.ts` — including the success branch that calls
 * `updateTab` AND `rememberSessionId`. If that line were removed, the
 * `rememberSessionId` spy assertion below would fail.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type { TerminalTab } from "./useTerminalManager";

// ── IPC mock: transcript_get_latest returns a fresh, unclaimed payload ──────
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

// ── Durable-store mock: spy on rememberSessionId ────────────────────────────
const rememberSessionId = vi.fn();
vi.mock("./lastKnownSessionIds", () => ({
  rememberSessionId: (...args: unknown[]) => rememberSessionId(...args),
}));

// ── React primitive stand-ins so the hook can run outside a renderer ────────
// useRef → stable mutable box; useCallback → identity; useEffect → run now.
vi.mock("react", () => {
  return {
    useRef: <T>(initial: T) => ({ current: initial }),
    useCallback: <T>(fn: T) => fn,
    useEffect: (fn: () => void | (() => void)) => {
      fn();
    },
  };
});

import { useTabSessionIdCapture } from "./useTabSessionIdCapture";

const tab = (id: string, workingDir: string, claudeSessionId?: string): TerminalTab => ({
  id,
  title: id,
  pid: 1234,
  isAlive: true,
  exitCode: null,
  workingDir,
  claudeSessionId,
});

// The hook's tick loop is async + self-scheduling; let microtasks drain.
const flush = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
};

beforeEach(() => {
  mockInvoke.mockReset();
  rememberSessionId.mockReset();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("useTabSessionIdCapture — capture success durably persists the id", () => {
  it("calls rememberSessionId(tabId, sessionId, configDir) on a successful capture", async () => {
    mockInvoke.mockResolvedValue({
      success: true,
      data: {
        session_id: "session-CAPTURED",
        config_dir: "C:/claude/.claude-hotmail",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
    });

    const updateTab = vi.fn();
    const tabs = [tab("tab-1", "D:/repo")];

    const { startCapture } = useTabSessionIdCapture({ updateTab, tabs });
    startCapture("tab-1", "D:/repo", Date.now(), "C:/claude/.claude-hotmail");

    await flush();

    // Live binding fires…
    expect(updateTab).toHaveBeenCalledWith("tab-1", {
      claudeSessionId: "session-CAPTURED",
      claudeConfigDir: "C:/claude/.claude-hotmail",
    });
    // …and — the actual fix — the durable store is written.
    expect(rememberSessionId).toHaveBeenCalledWith(
      "tab-1",
      "session-CAPTURED",
      "C:/claude/.claude-hotmail",
    );
  });

  it("does NOT persist when the probe returns null (nothing captured yet)", async () => {
    mockInvoke.mockResolvedValue({ success: true, data: null });

    const updateTab = vi.fn();
    const tabs = [tab("tab-1", "D:/repo")];

    const { startCapture } = useTabSessionIdCapture({ updateTab, tabs });
    startCapture("tab-1", "D:/repo", Date.now());

    await flush();

    expect(updateTab).not.toHaveBeenCalled();
    expect(rememberSessionId).not.toHaveBeenCalled();
  });
});
