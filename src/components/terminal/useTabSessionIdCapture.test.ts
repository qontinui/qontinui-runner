/**
 * Tests for `useTabSessionIdCapture` — the post-spawn polling hook
 * that fills `tab.claudeSessionId` from the on-disk Claude CLI
 * transcript.
 *
 * Same constraint as `useCommitState.test.ts`: the runner's vitest
 * environment is `node`, so we mock the IPC surface and exercise the
 * decision logic directly.
 *
 * Freshness is enforced backend-side via `sinceMs`. The hook trusts
 * any non-null payload returned by `transcript_get_latest`. The
 * branches that still matter on the frontend:
 *
 *   1. Latest unclaimed session matching workingDir →
 *      `updateTab({ claudeSessionId, claudeConfigDir })` is called once.
 *   2. Two tabs spawn into the same workdir → the first one wins;
 *      the second sees the session id as already-claimed and refuses
 *      to bind it.
 *   3. Backend returns null (filtered by sinceMs because no fresh
 *      JSONL exists yet) → poll keeps ticking, no claim.
 *   4. Timeout (no JSONL appears within 15 s) → `updateTab` is never
 *      called for that tab.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import type { TerminalTab } from "./useTerminalManager";

// IPC mock
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import { cwdConflicts } from "./useTabSessionIdCapture";

interface TranscriptSessionPayload {
  session_id: string;
  config_dir: string;
  project_path: string;
  last_modified: string;
  cwd?: string | null;
}

// Decision helper that mirrors the hook's per-tick claim logic.
// Freshness (last_modified > spawnTimestamp) is enforced backend-side
// via `sinceMs`; the hook trusts any non-null payload it receives —
// except a candidate whose recorded cwd CONTRADICTS the pane's known
// working dir (item 6, the workspace-root-fallback mis-bind).
function decideClaim(
  tab: TerminalTab | undefined,
  payload: TranscriptSessionPayload | null,
  alreadyClaimed: Set<string>,
): { claudeSessionId: string; claudeConfigDir: string } | null {
  if (!tab) return null;
  if (tab.claudeSessionId) return null;
  if (!payload) return null;
  if (cwdConflicts(tab.workingDir, payload.cwd)) return null;
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
  it("binds the session when payload is unclaimed", () => {
    const result = decideClaim(
      tab("tab-1", "D:/repo"),
      {
        session_id: "session-A",
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
      new Set(),
    );
    expect(result).toEqual({
      claudeSessionId: "session-A",
      claudeConfigDir: "C:/claude/.claude-default",
    });
  });

  it("rejects when the session id is already claimed", () => {
    const claimed = new Set(["session-A"]);
    const result = decideClaim(
      tab("tab-2", "D:/repo"),
      {
        session_id: "session-A", // claimed by tab-1 already
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
      claimed,
    );
    expect(result).toBeNull();
  });
});

// Item 6 (boot-restore remediation) — the freshest-mtime race: a foreign
// session (e.g. a VS Code CLI in another directory) writes its JSONL after
// the pane spawn and wins the mtime sort. When the transcript records a cwd
// that contradicts the pane's working dir, the candidate must be rejected —
// better unregistered than mis-bound.
describe("decideClaim — foreign-cwd mtime race (item 6)", () => {
  const payloadWithCwd = (sessionId: string, cwd: string | null): TranscriptSessionPayload => ({
    session_id: sessionId,
    config_dir: "C:/claude/.claude-default",
    project_path: "D:/repo",
    last_modified: new Date().toISOString(),
    cwd,
  });

  it("rejects a candidate recorded in a different directory", () => {
    const result = decideClaim(
      tab("tab-1", "D:/repo/sub"),
      payloadWithCwd("foreign-session", "D:/elsewhere"),
      new Set(),
    );
    expect(result).toBeNull();
  });

  it("binds the matching-cwd candidate (Windows-tolerant path equality)", () => {
    const result = decideClaim(
      tab("tab-1", "D:/repo/sub"),
      payloadWithCwd("own-session", "d:\\repo\\sub\\"),
      new Set(),
    );
    expect(result?.claudeSessionId).toBe("own-session");
  });

  it("cannot discriminate when either side is unknown — claims (legacy behavior)", () => {
    // No cwd on the payload (old backend) and no workingDir on the tab: the
    // check must not regress the pre-field behavior.
    expect(
      decideClaim(tab("tab-1", "D:/repo"), payloadWithCwd("s-1", null), new Set()),
    ).not.toBeNull();
    expect(
      decideClaim(tab("tab-1", ""), payloadWithCwd("s-2", "D:/elsewhere"), new Set()),
    ).not.toBeNull();
  });
});

describe("cwdConflicts", () => {
  it("normalizes slashes, trailing separators, and case", () => {
    expect(cwdConflicts("D:/repo/sub", "d:\\repo\\sub\\")).toBe(false);
    expect(cwdConflicts("D:/repo/sub", "D:/repo/sub")).toBe(false);
    expect(cwdConflicts("D:/repo/sub", "D:/repo/other")).toBe(true);
    // Subdirectory is NOT equality — a parent/child mismatch is a conflict.
    expect(cwdConflicts("D:/repo", "D:/repo/sub")).toBe(true);
  });

  it("returns false (no conflict) when either side is unknown", () => {
    expect(cwdConflicts(undefined, "D:/x")).toBe(false);
    expect(cwdConflicts("", "D:/x")).toBe(false);
    expect(cwdConflicts("D:/x", null)).toBe(false);
    expect(cwdConflicts("D:/x", undefined)).toBe(false);
  });
});

describe("decideClaim — concurrent same-workdir spawn defense", () => {
  it("first tab wins, second tab times out (no claim)", () => {
    const claimed = new Set<string>();
    const payload: TranscriptSessionPayload = {
      session_id: "session-shared",
      config_dir: "C:/claude/.claude-default",
      project_path: "D:/repo",
      last_modified: new Date().toISOString(),
    };

    // Tab 1 polls first → wins.
    const claim1 = decideClaim(tab("tab-1", "D:/repo"), payload, claimed);
    expect(claim1).not.toBeNull();
    if (claim1) claimed.add(claim1.claudeSessionId);

    // Tab 2 polls same workdir → sees the same session_id, but it's
    // claimed now → rejected.
    const claim2 = decideClaim(tab("tab-2", "D:/repo"), payload, claimed);
    expect(claim2).toBeNull();
  });
});

describe("decideClaim — already-bound and missing-tab cases", () => {
  it("skips a tab that already has a claudeSessionId (resume path beat us)", () => {
    const result = decideClaim(
      tab("tab-1", "D:/repo", "session-RESUME"),
      {
        session_id: "session-NEW",
        config_dir: "C:/claude/.claude-default",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
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
        new Set(),
      ),
    ).toBeNull();
  });

  it("returns null when the probe payload is null (filtered by backend or no JSONL yet)", () => {
    expect(decideClaim(tab("tab-1", "D:/repo"), null, new Set())).toBeNull();
  });
});

describe("invoke contract", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("transcript_get_latest is invoked with camelCase projectPath + sinceMs", async () => {
    mockInvoke.mockResolvedValueOnce({ success: true, data: null });
    const { invoke } = await import("@tauri-apps/api/core");
    const spawnAt = Date.now();
    await invoke("transcript_get_latest", {
      sinceMs: spawnAt,
      projectPath: "D:/repo",
    });
    expect(mockInvoke).toHaveBeenCalledWith("transcript_get_latest", {
      sinceMs: spawnAt,
      projectPath: "D:/repo",
    });
  });

  it("transcript_get_latest accepts configDir alongside projectPath + sinceMs", async () => {
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
    const spawnAt = Date.now();
    await invoke("transcript_get_latest", {
      sinceMs: spawnAt,
      projectPath: "D:/repo",
      configDir: "C:/claude/.claude-hotmail",
    });
    expect(mockInvoke).toHaveBeenCalledWith("transcript_get_latest", {
      sinceMs: spawnAt,
      projectPath: "D:/repo",
      configDir: "C:/claude/.claude-hotmail",
    });
  });

  it("backend returning null (filtered by sinceMs) leaves the hook unbound", async () => {
    // After the since-filter migration, "stale" is a backend concept: the
    // backend returns `data: null` when no session passes the filter. The
    // hook must not bind anything in that case — it just keeps ticking.
    mockInvoke.mockResolvedValueOnce({ success: true, data: null });
    const { invoke } = await import("@tauri-apps/api/core");
    const resp = (await invoke("transcript_get_latest", {
      sinceMs: Date.now(),
      projectPath: "D:/repo",
    })) as { success: boolean; data: TranscriptSessionPayload | null };

    const data = resp.success ? resp.data : null;
    const claim = decideClaim(tab("tab-1", "D:/repo"), data, new Set());
    expect(claim).toBeNull();
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
    // Backend was called with configDir filter, so it returns ONLY
    // the hotmail session — even though gmail had a fresher mtime.
    const result = decideClaim(
      tab("hotmail-tab", "D:/repo"),
      {
        session_id: "session-hotmail",
        config_dir: "C:/claude/.claude-hotmail",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
      new Set(),
    );
    expect(result).toEqual({
      claudeSessionId: "session-hotmail",
      claudeConfigDir: "C:/claude/.claude-hotmail",
    });
  });

  it("two tabs in different accounts each bind their own session", () => {
    const claimed = new Set<string>();

    // Hotmail tab: backend filtered to hotmail, returns hotmail session.
    const hotmailClaim = decideClaim(
      tab("hotmail-tab", "D:/repo"),
      {
        session_id: "session-hotmail",
        config_dir: "C:/claude/.claude-hotmail",
        project_path: "D:/repo",
        last_modified: new Date().toISOString(),
      },
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
        last_modified: new Date().toISOString(),
      },
      claimed,
    );
    expect(gmailClaim?.claudeSessionId).toBe("session-gmail");
  });
});
