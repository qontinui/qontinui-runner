/**
 * `useShellIntegration`'s transcript-panel Resume, entered where the page
 * enters it: the hook's own `handleResumeSession`. The vitest project runs in
 * `node` with no React renderer, so `react` is replaced by identity hooks
 * (`useCallback` returns its callback, `useRef`/`useState` hold their initial
 * value) — the hook body itself runs unmodified.
 *
 * The contract pinned: a Resume opens its tab under the session's real
 * `--resume` name (`resume_name`, the `/rename` / ai-title label), falling
 * back to the `claude <id8>` label only when the transcript carries none.
 */
import { describe, expect, it, vi } from "vitest";

vi.mock("react", () => ({
  useCallback: <T>(fn: T) => fn,
  useRef: <T>(initial: T) => ({ current: initial }),
  useState: <T>(initial: T) => [initial, () => {}],
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
    getJSON: <T>(_k: string, fallback: T) => fallback,
    setJSON: () => {},
  },
}));

import { useShellIntegration } from "./useShellIntegration";
import type { TranscriptSession } from "./useTranscriptSessions";

const SESSION_ID = "0123abcd-4567-89ef-0123-456789abcdef";

function transcript(resumeName: string | null): TranscriptSession {
  return {
    session_id: SESSION_ID,
    project_path: "/repo",
    config_dir: "",
    message_count: 3,
    last_modified: "2026-10-04T00:00:00Z",
    started_at: null,
    first_message_preview: "first message preview",
    has_plans: false,
    display_name: "first message preview",
    resume_name: resumeName,
  };
}

function useShellIntegrationWith(newTab: ReturnType<typeof vi.fn>) {
  return useShellIntegration({
    tabs: [],
    updateTab: vi.fn(),
    renameTab: vi.fn(),
    createTerminal: newTab as never,
    setSessionStates: vi.fn() as never,
    terminalRefs: { current: new Map() },
    setRightPanelMode: vi.fn() as never,
    setSelectedTranscriptSessionId: vi.fn() as never,
    pageId: "default",
  });
}

describe("useShellIntegration handleResumeSession", () => {
  it("opens the resumed tab under the session's real --resume name", async () => {
    // A refused tab (null) ends the resume right after the tab request, so
    // nothing is typed and no verification loop is left running.
    const newTab = vi.fn(async () => null);
    const hook = useShellIntegrationWith(newTab);

    await hook.handleResumeSession(transcript("  my-renamed-session  "));

    expect(newTab).toHaveBeenCalledTimes(1);
    expect(newTab.mock.calls[0][0]).toBe("my-renamed-session");
    expect(newTab.mock.calls[0][1]).toBe("/repo");
  });
});
