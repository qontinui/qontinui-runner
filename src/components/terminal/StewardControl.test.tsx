/**
 * Tests for `StewardControl`'s pure helpers.
 *
 * The runner's vitest config uses `environment: "node"` — no jsdom and no
 * `@testing-library/react` (see `LaunchMenu.test.tsx` for precedent). The
 * component's fetch/poll/render logic is exercised manually via UI Bridge
 * (it lives in `SessionManagerPanel`, mounted in the live component tree);
 * here we lock down the wire-shape helpers.
 */

import { describe, it, expect } from "vitest";
import type { StewardStatus } from "./StewardControl";
import {
  buildStewardUrl,
  buildStewardsUrl,
  formatRunningSummary,
  formatUptime,
  readErrorDetail,
} from "./StewardControl";

/** A running steward row as `GET /stewards` returns one. */
function running(overrides: Partial<StewardStatus> = {}): StewardStatus {
  return {
    kind: "merge-train",
    label: "Merge-train steward",
    skill: "merge-train-steward",
    default_mode: "autonomous",
    default_interval: "5m",
    running: true,
    session_id: "term-a",
    mode: "autonomous",
    interval: "5m",
    started_at: 1_000_000,
    ...overrides,
  };
}

describe("buildStewardsUrl", () => {
  it("targets the runner's local /stewards roster endpoint", () => {
    expect(buildStewardsUrl(9876)).toBe("http://127.0.0.1:9876/stewards");
  });
});

describe("buildStewardUrl", () => {
  it("targets the runner's local per-kind /status endpoint", () => {
    expect(buildStewardUrl(9876, "merge-train", "status")).toBe(
      "http://127.0.0.1:9876/steward/merge-train/status",
    );
  });

  it("targets the runner's local per-kind /start endpoint", () => {
    expect(buildStewardUrl(9876, "dev-ops", "start")).toBe(
      "http://127.0.0.1:9876/steward/dev-ops/start",
    );
  });

  it("targets the runner's local per-kind /stop endpoint", () => {
    expect(buildStewardUrl(9876, "cleanup", "stop")).toBe(
      "http://127.0.0.1:9876/steward/cleanup/stop",
    );
  });

  it("keeps each kind on its own path, so one steward's button cannot hit another", () => {
    // The kind must actually reach the URL. A helper that ignored its `kind`
    // argument — or interpolated a constant — would collapse these to one
    // string and let a second steward's button act on the first.
    const start = (kind: string) => buildStewardUrl(9876, kind, "start");
    expect(start("merge-train")).not.toBe(start("dev-ops"));
    expect(start("dev-ops")).not.toBe(start("cleanup"));
  });

  it("distinguishes the three actions for a single kind", () => {
    // Likewise `path` must reach the URL: a helper that dropped it would
    // make Stop call Start.
    const urls = new Set([
      buildStewardUrl(9876, "dev-ops", "status"),
      buildStewardUrl(9876, "dev-ops", "start"),
      buildStewardUrl(9876, "dev-ops", "stop"),
    ]);
    expect(urls.size).toBe(3);
  });
});

describe("readErrorDetail", () => {
  it("prefers the backend's own message over the status code", async () => {
    // The 400/409/resource-floor bodies are the whole reason both callers
    // read the body instead of rendering the status.
    const detail = await readErrorDetail({
      status: 409,
      json: async () => ({ success: false, error: "merge-train-steward is already running" }),
    });
    expect(detail).toBe("merge-train-steward is already running");
  });

  it("falls back to the status code when the body is not JSON", async () => {
    const detail = await readErrorDetail({
      status: 502,
      json: async () => {
        throw new SyntaxError("Unexpected token < in JSON");
      },
    });
    expect(detail).toBe("HTTP 502");
  });

  it("falls back to the status code when the envelope carries no error", async () => {
    const detail = await readErrorDetail({ status: 500, json: async () => ({ success: false }) });
    expect(detail).toBe("HTTP 500");
  });
});

describe("formatUptime", () => {
  it("refuses to invent an uptime from a missing or zero timestamp", () => {
    // `TerminalInfo::created_at` defaults to 0. Rendering that as "up 20941d"
    // — or as "0m" — would answer the operator's first question wrongly, so
    // the caller must be able to omit the part entirely.
    expect(formatUptime(undefined, 5_000_000)).toBeNull();
    expect(formatUptime(0, 5_000_000)).toBeNull();
    expect(formatUptime(Number.NaN, 5_000_000)).toBeNull();
    expect(formatUptime(Number.POSITIVE_INFINITY, 5_000_000)).toBeNull();
  });

  it("reports minutes, then hours, then days", () => {
    const start = 1_000_000_000_000;
    expect(formatUptime(start, start + 90_000)).toBe("1m");
    expect(formatUptime(start, start + 59 * 60_000)).toBe("59m");
    expect(formatUptime(start, start + 60 * 60_000)).toBe("1h 0m");
    expect(formatUptime(start, start + (3 * 60 + 25) * 60_000)).toBe("3h 25m");
    expect(formatUptime(start, start + 26 * 60 * 60_000)).toBe("1d 2h");
  });

  it("treats a start time slightly in the future as just-started", () => {
    // The runner stamps `created_at`; the renderer reads its own clock. Skew
    // must not blank out the uptime of a steward that genuinely just started.
    expect(formatUptime(1_000_000_000_500, 1_000_000_000_000)).toBe("0m");
  });
});

describe("formatRunningSummary", () => {
  it("shows the cadence the stopped row shows, plus how long it has been up", () => {
    // The stopped row renders `default_mode · default_interval`. Before this,
    // the running row dropped the interval entirely, so the cadence vanished
    // at exactly the moment it became a live fact rather than a default.
    const summary = formatRunningSummary(
      running({ started_at: 1_000_000 }),
      1_000_000 + 12 * 60_000,
    );
    expect(summary).toBe("autonomous · 5m · up 12m");
  });

  it("omits parts the runner did not report rather than rendering blanks", () => {
    // A runner that reports less must still produce a well-formed line — no
    // leading separator, no empty parentheses.
    expect(formatRunningSummary(running({ interval: undefined, started_at: 0 }), 2_000_000)).toBe(
      "autonomous",
    );
    expect(
      formatRunningSummary(
        running({ mode: undefined, interval: undefined, started_at: 0 }),
        2_000_000,
      ),
    ).toBe("");
  });

  it("uses the live mode/interval, not the roster defaults", () => {
    // An observe re-soak launched over the API reports mode=observe while
    // default_mode stays autonomous; showing the default would misreport what
    // is actually running.
    const summary = formatRunningSummary(
      running({ mode: "observe", interval: "30m", started_at: 0 }),
      2_000_000,
    );
    expect(summary).toBe("observe · 30m");
  });
});
