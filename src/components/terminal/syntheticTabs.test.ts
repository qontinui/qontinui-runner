/**
 * Unit tests for `deriveSyntheticTabs` — the pure derivation that turns a
 * tab-backed injected fake's `injected_tab` spec into a minimal `TerminalTab`
 * (+ pre-age seed) so it flows through the real `useSessionManager` /
 * `useSessionStateTracking` correlation path.
 *
 * These tabs are how the debug-gated test-fixtures seam reaches the `idle`
 * (pre-aged stale tab) and tab-backed `error` / `completed` (dead-tab
 * exit-code) StatusStrip buckets that the `injected_live_status` short-circuit
 * cannot.
 */

import { describe, it, expect, vi, afterEach } from "vitest";
import { deriveSyntheticTabs } from "./syntheticTabs";
import type { TranscriptSession } from "./useTranscriptSessions";

// Minimal transcript-session builder — only the fields the derivation reads
// matter; the rest are inert defaults.
function session(
  partial: Partial<TranscriptSession> & Pick<TranscriptSession, "session_id">,
): TranscriptSession {
  return {
    project_path: "<test-fixture>",
    config_dir: "C:\\claude\\.claude-test-fixture",
    message_count: 1,
    last_modified: "2026-06-05T00:00:00Z",
    started_at: "2026-06-05T00:00:00Z",
    first_message_preview: "preview",
    has_plans: false,
    display_name: "Fixture",
    ...partial,
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("deriveSyntheticTabs", () => {
  it("ignores sessions without an injected_tab (the production path)", () => {
    const result = deriveSyntheticTabs([
      session({ session_id: "real-1" }),
      session({ session_id: "real-2", injected_live_status: "active-in-zone" }),
    ]);
    expect(result.tabs).toEqual([]);
    expect(result.seedLastOutput).toEqual({});
  });

  it("derives a live, pre-aged tab for an idle fake and seeds lastOutput", () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);

    const result = deriveSyntheticTabs([
      session({
        session_id: "idle-1",
        injected_tab: { is_alive: true, exit_code: null, quiet_ms: 61_000 },
      }),
    ]);

    expect(result.tabs).toHaveLength(1);
    const tab = result.tabs[0];
    expect(tab.id).toBe("synthetic-idle-1");
    expect(tab.claudeSessionId).toBe("idle-1");
    expect(tab.isAlive).toBe(true);
    expect(tab.exitCode).toBeNull();
    expect(tab.pid).toBeNull();
    expect(tab.__synthetic).toBe(true);

    // Seed is now - quiet_ms, i.e. pre-aged into the past so the 60s sweep
    // marks it stale.
    expect(result.seedLastOutput["synthetic-idle-1"]).toBe(1_000_000 - 61_000);
  });

  it("derives a dead tab with exit code 1 for an error fake, no seed", () => {
    const result = deriveSyntheticTabs([
      session({
        session_id: "err-1",
        injected_tab: { is_alive: false, exit_code: 1, quiet_ms: null },
      }),
    ]);

    expect(result.tabs).toHaveLength(1);
    expect(result.tabs[0].isAlive).toBe(false);
    expect(result.tabs[0].exitCode).toBe(1);
    expect(result.tabs[0].__synthetic).toBe(true);
    // Dead tabs are not pre-aged — the dead-tab sweep branch handles them.
    expect(result.seedLastOutput).toEqual({});
  });

  it("derives a dead tab with exit code 0 for a completed fake, no seed", () => {
    const result = deriveSyntheticTabs([
      session({
        session_id: "done-1",
        injected_tab: { is_alive: false, exit_code: 0, quiet_ms: null },
      }),
    ]);

    expect(result.tabs).toHaveLength(1);
    expect(result.tabs[0].isAlive).toBe(false);
    expect(result.tabs[0].exitCode).toBe(0);
    expect(result.seedLastOutput).toEqual({});
  });

  it("does not seed a live tab that carries no quiet_ms", () => {
    const result = deriveSyntheticTabs([
      session({
        session_id: "live-no-quiet",
        injected_tab: { is_alive: true, exit_code: null, quiet_ms: null },
      }),
    ]);
    expect(result.tabs).toHaveLength(1);
    expect(result.seedLastOutput).toEqual({});
  });

  it("handles a mix: real + idle + error in one pass", () => {
    const result = deriveSyntheticTabs([
      session({ session_id: "real" }),
      session({
        session_id: "idle",
        injected_tab: { is_alive: true, exit_code: null, quiet_ms: 90_000 },
      }),
      session({
        session_id: "err",
        injected_tab: { is_alive: false, exit_code: 1, quiet_ms: null },
      }),
    ]);

    expect(result.tabs.map((t) => t.id).sort()).toEqual([
      "synthetic-err",
      "synthetic-idle",
    ]);
    expect(Object.keys(result.seedLastOutput)).toEqual(["synthetic-idle"]);
  });
});
