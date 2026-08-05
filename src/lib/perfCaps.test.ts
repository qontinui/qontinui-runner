/**
 * Tests for the performance-caps store (many-sessions plan Phase 8).
 *
 * The runner's vitest config is `environment: "node"`, so this exercises the
 * store + normalizer directly rather than through React (same precedent as
 * `authOverlayStore`'s exported `subscribe`/`getSnapshot`).
 *
 * The load path is covered by mocking `@tauri-apps/api/core`, which is the
 * only IPC this module touches.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import {
  DEFAULT_PERF_CAPS,
  getPerfCaps,
  getPerfCapsLoadState,
  loadPerfCaps,
  normalizePerfCaps,
  setPerfCaps,
  subscribePerfCaps,
  __resetPerfCapsForTest,
  type PerfCaps,
} from "./perfCaps";

beforeEach(() => {
  __resetPerfCapsForTest();
  invokeMock.mockReset();
});

describe("defaults", () => {
  /**
   * These MUST equal `settings::PerformanceSettings::default()` in Rust —
   * they are what the UI and the banner use before IPC resolves, so a drift
   * here shows the operator a value the runner is not using.
   * `src-tauri/src/settings.rs::performance_settings_tests` pins the Rust
   * side to the same numbers.
   */
  it("match the Rust defaults", () => {
    expect(DEFAULT_PERF_CAPS).toEqual({
      max_webgl_panes: 8,
      background_flush_interval_ms: 250,
      unwatched_flush_interval_ms: 0,
      scrollback_capacity_bytes: 1_048_576,
      grid_scan_interval_ms: 1500,
      share_terminal_output: true,
      redact_terminal_secrets: null,
      max_sessions_warn: 30,
    });
  });

  it("are what the store serves before any load", () => {
    expect(getPerfCaps()).toEqual(DEFAULT_PERF_CAPS);
  });
});

describe("normalizePerfCaps", () => {
  it("fills every field from an empty payload", () => {
    expect(normalizePerfCaps({})).toEqual(DEFAULT_PERF_CAPS);
  });

  it("survives a non-object payload", () => {
    expect(normalizePerfCaps(null)).toEqual(DEFAULT_PERF_CAPS);
    expect(normalizePerfCaps("nope")).toEqual(DEFAULT_PERF_CAPS);
  });

  it("keeps good values and defaults only the bad ones", () => {
    const out = normalizePerfCaps({
      max_webgl_panes: 4,
      background_flush_interval_ms: Number.NaN,
      scrollback_capacity_bytes: -1,
      max_sessions_warn: 12,
      share_terminal_output: false,
    });
    expect(out.max_webgl_panes).toBe(4);
    expect(out.max_sessions_warn).toBe(12);
    expect(out.share_terminal_output).toBe(false);
    // One unusable key must not discard the rest.
    expect(out.background_flush_interval_ms).toBe(DEFAULT_PERF_CAPS.background_flush_interval_ms);
    expect(out.scrollback_capacity_bytes).toBe(DEFAULT_PERF_CAPS.scrollback_capacity_bytes);
  });

  it("keeps the tri-state redaction flag tri-state", () => {
    expect(normalizePerfCaps({ redact_terminal_secrets: true }).redact_terminal_secrets).toBe(true);
    expect(normalizePerfCaps({ redact_terminal_secrets: false }).redact_terminal_secrets).toBe(
      false,
    );
    // Missing / junk collapses to "follow share_output", never to a pin.
    expect(normalizePerfCaps({}).redact_terminal_secrets).toBeNull();
    expect(
      normalizePerfCaps({ redact_terminal_secrets: "yes" }).redact_terminal_secrets,
    ).toBeNull();
  });

  it("accepts 0 for the unwatched tier (its documented 'no emit' value)", () => {
    expect(normalizePerfCaps({ unwatched_flush_interval_ms: 0 }).unwatched_flush_interval_ms).toBe(
      0,
    );
  });
});

describe("store", () => {
  it("notifies subscribers on publish", () => {
    const seen: PerfCaps[] = [];
    const unsub = subscribePerfCaps(() => seen.push(getPerfCaps()));
    setPerfCaps({ ...DEFAULT_PERF_CAPS, max_sessions_warn: 7 });
    unsub();
    setPerfCaps({ ...DEFAULT_PERF_CAPS, max_sessions_warn: 9 });
    expect(seen).toHaveLength(1);
    expect(seen[0].max_sessions_warn).toBe(7);
    expect(getPerfCaps().max_sessions_warn).toBe(9);
  });
});

describe("loadPerfCaps", () => {
  it("loads once and shares the in-flight promise", async () => {
    invokeMock.mockResolvedValue({ max_sessions_warn: 11 });
    const [a, b] = await Promise.all([loadPerfCaps(), loadPerfCaps()]);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("get_performance_settings");
    expect(a.max_sessions_warn).toBe(11);
    expect(b.max_sessions_warn).toBe(11);
    expect(getPerfCaps().max_sessions_warn).toBe(11);
  });

  /**
   * A failed load must leave STOCK caps in place, not zeros — a WebGL cap of
   * 0 would disable acceleration outright and a session threshold of 0 would
   * warn from the first tab. And it must stay retryable.
   */
  it("keeps defaults on failure and allows a retry", async () => {
    invokeMock.mockRejectedValueOnce(new Error("no such command"));
    const first = await loadPerfCaps();
    expect(first).toEqual(DEFAULT_PERF_CAPS);
    expect(getPerfCaps()).toEqual(DEFAULT_PERF_CAPS);

    invokeMock.mockResolvedValueOnce({ max_webgl_panes: 2 });
    const second = await loadPerfCaps();
    expect(second.max_webgl_panes).toBe(2);
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });
});

describe("load state", () => {
  /**
   * The settings panel keys its "these are just the defaults" notice off this,
   * so a failed read must be distinguishable from a successful one that
   * happened to return the defaults.
   */
  it("distinguishes unloaded / loaded / failed", async () => {
    expect(getPerfCapsLoadState()).toBe("unloaded");

    invokeMock.mockRejectedValueOnce(new Error("boom"));
    await loadPerfCaps();
    expect(getPerfCapsLoadState()).toBe("failed");

    invokeMock.mockResolvedValueOnce({});
    await loadPerfCaps();
    expect(getPerfCapsLoadState()).toBe("loaded");
  });

  it("treats a publish (i.e. a successful save) as loaded", () => {
    expect(getPerfCapsLoadState()).toBe("unloaded");
    setPerfCaps({ ...DEFAULT_PERF_CAPS, max_sessions_warn: 5 });
    expect(getPerfCapsLoadState()).toBe("loaded");
  });
});
