/**
 * The two launch-failure IPC reads against the REAL wire shape (plan
 * 2026-11-20-terminal-launch-failure-analyze-button, Phase 2).
 *
 * `capture_launch_evidence` returns `Result<LaunchEvidence, String>` and
 * `get_terminal_launch_failure` returns `LaunchFailureView`
 * (`src-tauri/src/commands/terminal.rs`). Tauri serializes both BARE — the
 * struct itself, no `CommandResponse` envelope. The fixtures below are the
 * exact JSON the Rust tests `launch_evidence_serializes_camel_case` and
 * `launch_failure_view_serializes_recorded_and_not`
 * (`src-tauri/src/terminal/launch_evidence.rs`) pin, so a change on either
 * side breaks one of the two suites.
 *
 * The analyzer tests inject `captureEvidence` and never reach this parse,
 * which is how a `.data` read here once failed every click unnoticed.
 */

import { describe, it, expect, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { captureLaunchEvidence, readTerminalLaunchFailure } from "./launchFailureApi";

const PARAMS = {
  terminalId: "t1",
  launchCommand: "claude",
  configDir: "/h/.claude-x",
  pinnedSessionId: "abc",
};

describe("captureLaunchEvidence (bare LaunchEvidence)", () => {
  it("reads the bare payload the Tauri command serializes", async () => {
    const invoker = vi.fn(async () => ({
      path: "/h/.qontinui/launch-evidence/t1-20261120T101500Z.md",
      cwd: "/work/repo",
      commandsProvisioned: true,
    }));
    await expect(captureLaunchEvidence(PARAMS, invoker)).resolves.toEqual({
      path: "/h/.qontinui/launch-evidence/t1-20261120T101500Z.md",
      cwd: "/work/repo",
      commandsProvisioned: true,
    });
    expect(invoker).toHaveBeenCalledWith("capture_launch_evidence", PARAMS);
  });

  it("reads a null cwd and an unprovisioned capture exactly as the Rust test pins them", async () => {
    await expect(
      captureLaunchEvidence(PARAMS, async () => ({
        path: "/p.md",
        cwd: null,
        commandsProvisioned: false,
      })),
    ).resolves.toEqual({ path: "/p.md", cwd: null, commandsProvisioned: false });
  });

  it("throws the backend's refusal, which arrives as the invoke rejection", async () => {
    await expect(
      captureLaunchEvidence(PARAMS, async () => Promise.reject("Terminal not found: t1")),
    ).rejects.toBe("Terminal not found: t1");
  });

  it("throws on a payload with no evidence path (including a CommandResponse envelope)", async () => {
    await expect(captureLaunchEvidence(PARAMS, async () => null)).rejects.toThrow(
      "no evidence path",
    );
    await expect(
      captureLaunchEvidence(PARAMS, async () => ({
        success: true,
        data: { path: "/p.md", cwd: null, commandsProvisioned: false },
      })),
    ).rejects.toThrow("no evidence path");
  });
});

describe("readTerminalLaunchFailure (bare LaunchFailureView)", () => {
  it("maps the bare not-recorded view", async () => {
    const invoker = vi.fn(async () => ({ recorded: false }));
    expect(await readTerminalLaunchFailure("t", invoker)).toEqual({ state: "not-recorded" });
    expect(invoker).toHaveBeenCalledWith("get_terminal_launch_failure", { terminalId: "t" });
  });

  it("maps the bare recorded view", async () => {
    expect(
      await readTerminalLaunchFailure("t", async () => ({
        recorded: true,
        kind: "resolve-failed",
        detail: "claude not found after 20 tries settings=true",
        settings: true,
        userChoseSession: false,
        atMs: 1_795_169_695_000,
      })),
    ).toEqual({
      state: "recorded",
      kind: "resolve-failed",
      detail: "claude not found after 20 tries settings=true",
    });
  });

  it("is unread for a rejection or a payload that is not a LaunchFailureView", async () => {
    expect(
      await readTerminalLaunchFailure("t", async () => Promise.reject(new Error("x"))),
    ).toEqual({ state: "unread" });
    expect(await readTerminalLaunchFailure("t", async () => ({ success: false }))).toEqual({
      state: "unread",
    });
    expect(
      await readTerminalLaunchFailure("t", async () => ({ data: { recorded: false } })),
    ).toEqual({ state: "unread" });
  });
});
