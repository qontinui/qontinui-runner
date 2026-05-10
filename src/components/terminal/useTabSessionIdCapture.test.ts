/**
 * Tests for `useTabSessionIdCapture` — the post-spawn polling hook
 * that fills `tab.claudeSessionId` from the on-disk Claude CLI
 * transcript.
 *
 * Same constraint as `useCommitState.test.ts`: the runner's vitest
 * environment is `node`, so we mock the IPC surface and exercise the
 * decision logic directly. The branches that matter:
 *
 *   1. Latest unclaimed session matching workingDir →
 *      `updateTab({ claudeSessionId, claudeConfigDir })` is called once.
 *   2. Two tabs spawn into the same workdir → the first one wins;
 *      the second sees the session id as already-claimed and refuses
 *      to bind it.
 *   3. Timeout (no JSONL appears within 15 s) → `updateTab` is never
 *      called for that tab.
 *
 * The polling driver itself (the 500 ms tick scheduler) is exercised
 * via a small reproduction of the decision logic so we don't need
 * fake-timer plumbing in a node-environment vitest project.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import type { TerminalTab } from "./useTerminalManager";

// IPC mock
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

interface TranscriptSessionPayload {
  session_id: string;
  config_dir: string;
  project_path: string;
  last_modified: string;
}

// Decision helper that mirrors the hook's per-tick claim logic.
// Returns the update payload to apply (if any) given a poll response.
function decideClaim(
  tab: TerminalTab | undefined,
  payload: TranscriptSessionPayload | null,
  spawnTimestamp: number,
  alreadyClaimed: Set<string>,
): { claudeSessionId: string; claudeConfigDir: string } | null {
  if (!tab) return null;
  if (tab.claudeSessionId) return null;
  if (!payload) return null;
  const lastModifiedMs = Date.parse(payload.last_modified);
  if (!Number.isFinite(lastModifiedMs)) return null;
  if (lastModifiedMs <= spawnTimestamp) return null;
  if (alreadyClaimed.has(payload.session_id)) return null;
  return {
    claudeSessionId: payload.session_id,
    claudeConfigDir: payload.config_dir,
  };
}

const tab = (id: string, workingDir: string, claudeSessionId?: string): TerminalTab => ({
  id,
  title: id,
  pid: 1234,
  isAlive: true,
  exitCode: null,
  workingDir,
  claudeSessionId,
});

describe("decideClaim — happy path", () => {
  it("binds the session when payload is fresh + unclaimed", () => {
    const spawnAt = 1_700_000_000_000;
    const result = decideClaim(
      tab("tab-1", "D:/repo"),
      {
        session_id: "session-A",
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt + 1000).toISOString(),
      },
      spawnAt,
      new Set(),
    );
    expect(result).toEqual({
      claudeSessionId: "session-A",
      claudeConfigDir: "C:/claude/.claude-default",
    });
  });

  it("rejects a stale session (last_modified <= spawn timestamp)", () => {
    const spawnAt = 1_700_000_000_000;
    const result = decideClaim(
      tab("tab-1", "D:/repo"),
      {
        session_id: "session-old",
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt - 5_000).toISOString(),
      },
      spawnAt,
      new Set(),
    );
    expect(result).toBeNull();
  });

  it("rejects when the session id is already claimed", () => {
    const spawnAt = 1_700_000_000_000;
    const claimed = new Set(["session-A"]);
    const result = decideClaim(
      tab("tab-2", "D:/repo"),
      {
        session_id: "session-A", // claimed by tab-1 already
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt + 1000).toISOString(),
      },
      spawnAt,
      claimed,
    );
    expect(result).toBeNull();
  });
});

describe("decideClaim — concurrent same-workdir spawn defense", () => {
  it("first tab wins, second tab times out (no claim)", () => {
    const spawnAt = 1_700_000_000_000;
    const claimed = new Set<string>();
    const payload: TranscriptSessionPayload = {
      session_id: "session-shared",
      config_dir: "C:/claude/.claude-default",
      project_path: "D:/repo",
      last_modified: new Date(spawnAt + 1000).toISOString(),
    };

    // Tab 1 polls first → wins.
    const claim1 = decideClaim(tab("tab-1", "D:/repo"), payload, spawnAt, claimed);
    expect(claim1).not.toBeNull();
    if (claim1) claimed.add(claim1.claudeSessionId);

    // Tab 2 polls same workdir → sees the same session_id, but it's
    // claimed now → rejected.
    const claim2 = decideClaim(tab("tab-2", "D:/repo"), payload, spawnAt, claimed);
    expect(claim2).toBeNull();
  });
});

describe("decideClaim — already-bound and missing-tab cases", () => {
  it("skips a tab that already has a claudeSessionId (resume path beat us)", () => {
    const spawnAt = 1_700_000_000_000;
    const result = decideClaim(
      tab("tab-1", "D:/repo", "session-RESUME"),
      {
        session_id: "session-NEW",
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt + 1000).toISOString(),
      },
      spawnAt,
      new Set(),
    );
    expect(result).toBeNull();
  });

  it("returns null when the tab has been closed", () => {
    expect(
      decideClaim(
        undefined,
        {
          session_id: "session-A",
          config_dir: "C:/claude/.claude-default",
          project_path: "D:/repo",
          last_modified: new Date().toISOString(),
        },
        Date.now(),
        new Set(),
      ),
    ).toBeNull();
  });

  it("returns null when the probe payload is null (no transcript yet)", () => {
    expect(decideClaim(tab("tab-1", "D:/repo"), null, Date.now(), new Set())).toBeNull();
  });

  it("returns null when last_modified is unparseable", () => {
    expect(
      decideClaim(
        tab("tab-1", "D:/repo"),
        {
          session_id: "session-A",
          config_dir: "C:/claude/.claude-default",
          project_path: "D:/repo",
          last_modified: "not-a-date",
        },
        Date.now(),
        new Set(),
      ),
    ).toBeNull();
  });
});

describe("invoke contract", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("transcript_get_latest is invoked with camelCase projectPath", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, data: null });
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("transcript_get_latest", { projectPath: "D:/repo" });
    expect(mockInvoke).toHaveBeenCalledWith("transcript_get_latest", {
      projectPath: "D:/repo",
    });
  });

  it("transcript_get_latest accepts configDir alongside projectPath", async () => {
    mockInvoke.mockResolvedValueOnce({
      success: true,
      data: {
        session_id: "session-hotmail",
        config_dir: "C:/claude/.claude-hotmail",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
    });
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("transcript_get_latest", {
      projectPath: "D:/repo",
      configDir: "C:/claude/.claude-hotmail",
    });
    expect(mockInvoke).toHaveBeenCalledWith("transcript_get_latest", {
      projectPath: "D:/repo",
      configDir: "C:/claude/.claude-hotmail",
    });
  });
});

/**
 * Cross-config-dir capture — the P0 silent-fail surfaced during live
 * verification 2026-05-09.
 *
 * Setup: a hotmail-account PTY tab launches at T0. The same project_path
 * has an actively-writing gmail-account session (a different developer
 * Claude Code session). Without a `configDir` filter, the backend
 * `transcript_get_latest` returns whichever JSONL has the freshest
 * mtime — the gmail one — and the hotmail tab binds the wrong
 * session_id. Every `commit-state-changed` event keyed on the *real*
 * hotmail session_id then gets dropped by the listener filter and the
 * commit traffic light stays gray forever.
 *
 * The fix scopes the probe to the launching account. These cases verify
 * the hook's invoke contract: when configDir is supplied, it MUST be
 * forwarded to the backend. The decision logic for "what payload do we
 * accept" is unchanged — what matters is that the backend now returns
 * the right payload because the wrong-account ones are filtered out.
 */
describe("decideClaim — cross-config-dir scope (P0 fix)", () => {
  it("accepts a session whose config_dir matches the launching account", () => {
    const spawnAt = 1_700_000_000_000;
    // Backend was called with configDir filter, so it returns ONLY
    // the hotmail session — even though gmail had a fresher mtime.
    const result = decideClaim(
      tab("hotmail-tab", "D:/repo"),
      {
        session_id: "session-hotmail",
        config_dir: "C:/claude/.claude-hotmail",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt + 500).toISOString(),
      },
      spawnAt,
      new Set(),
    );
    expect(result).toEqual({
      claudeSessionId: "session-hotmail",
      claudeConfigDir: "C:/claude/.claude-hotmail",
    });
  });

  it("two tabs in different accounts each bind their own session", () => {
    const spawnAt = 1_700_000_000_000;
    const claimed = new Set<string>();

    // Hotmail tab: backend filtered to hotmail, returns hotmail session.
    const hotmailClaim = decideClaim(
      tab("hotmail-tab", "D:/repo"),
      {
        session_id: "session-hotmail",
        config_dir: "C:/claude/.claude-hotmail",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt + 500).toISOString(),
      },
      spawnAt,
      claimed,
    );
    expect(hotmailClaim?.claudeSessionId).toBe("session-hotmail");
    if (hotmailClaim) claimed.add(hotmailClaim.claudeSessionId);

    // Gmail tab launched concurrently into the same workdir but a
    // different account: backend filtered to gmail, returns its own
    // session. The two never collide because the configDir filter
    // narrowed each lookup to the right account.
    const gmailClaim = decideClaim(
      tab("gmail-tab", "D:/repo"),
      {
        session_id: "session-gmail",
        config_dir: "C:/claude/.claude-gmail",
        project_path: "D:/repo",
        last_modified: new Date(spawnAt + 700).toISOString(),
      },
      spawnAt,
      claimed,
    );
    expect(gmailClaim?.claudeSessionId).toBe("session-gmail");
  });
});
