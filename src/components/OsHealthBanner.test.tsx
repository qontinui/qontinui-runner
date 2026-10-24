/**
 * OsHealthBanner — the copy and the rendered banner (plan
 * `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`
 * Phase 5). Node environment: the banner is rendered with
 * `renderToStaticMarkup` after the module singleton has been driven on a fake
 * clock, so what is asserted is what the App root would render.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatCount, formatMs, OsHealthBanner, osHealthBannerCopy } from "./OsHealthBanner";
import {
  dismissOsHealthBanner,
  resetOsHealthForTests,
  subscribeOsHealth,
  type OsHealthPayload,
  type OsHealthReason,
} from "@/hooks/useOsHealth";

const mockInvoke = invoke as unknown as ReturnType<typeof vi.fn>;
const mockListen = listen as unknown as ReturnType<typeof vi.fn>;

function reason(
  axis: string,
  rule: string,
  band: OsHealthReason["band"],
  value: number | null,
): OsHealthReason {
  return { axis, rule, band, value, threshold: null, message: `${rule} ${band}` };
}

/** The incident as `/health` `osHealth` serves it (the Rust fixture). */
const INCIDENT: OsHealthPayload = {
  verdict: "breach",
  platform: "windows",
  reasons: [
    reason("S7", "token_objects", "breach", 4_365_962),
    reason("S8", "pool_total", "breach", 39_357_702_144),
    reason("S8", "pool_nonpaged", "warn", 15_075_627_008),
    reason("S9", "spawn_p50", "breach", 2_400),
  ],
  readings: {
    kernelPagedBytes: 24_282_075_136,
    kernelNonpagedBytes: 15_075_627_008,
    tokenObjects: 4_365_962,
    spawnProbe: { p50Ms: 2_400, maxMs: 30_000, count: 10, timeouts: 1, errors: 1, errorCode: 1450 },
  },
};

describe("osHealthBannerCopy", () => {
  it("names spawn latency, tokens and pool for the incident (the D5 sentence)", () => {
    expect(osHealthBannerCopy(INCIDENT)).toBe(
      "Windows on this computer is creating processes slowly (p50 2.4 s; kernel token " +
        "objects 4.4M, normally thousands; kernel memory pool 39.4 GB, normally 1–3 GB). " +
        "Restarting Windows clears it.",
    );
  });

  it("says processes are failing for spawn_failed, with the OS error", () => {
    const p: OsHealthPayload = {
      ...INCIDENT,
      reasons: [reason("S9", "spawn_failed", "breach", 10)],
    };
    expect(osHealthBannerCopy(p)).toBe(
      "Windows on this computer is failing to create processes (error 1450). Restarting Windows clears it.",
    );
  });

  it("leaves out readings whose rule did not fire, and an unknown rule", () => {
    const p: OsHealthPayload = {
      ...INCIDENT,
      reasons: [
        reason("S7", "token_objects", "breach", 4_365_962),
        reason("S9", "spawn_p50", "unknown", null),
      ],
    };
    expect(osHealthBannerCopy(p)).toBe(
      "Windows on this computer is running out of kernel resources (kernel token objects 4.4M, " +
        "normally thousands). Restarting Windows clears it.",
    );
  });

  it("does not tell a Linux user to restart Windows", () => {
    const p: OsHealthPayload = {
      ...INCIDENT,
      platform: "linux",
      reasons: [reason("S9", "spawn_p50", "breach", 1_500)],
      readings: {
        ...INCIDENT.readings!,
        spawnProbe: { ...INCIDENT.readings!.spawnProbe!, p50Ms: 1_500 },
      },
    };
    expect(osHealthBannerCopy(p)).toBe(
      "The operating system on this computer is creating processes slowly (p50 1.5 s). " +
        "Restarting the computer clears it.",
    );
  });

  it("formats", () => {
    expect(formatMs(640)).toBe("640 ms");
    expect(formatMs(2_400)).toBe("2.4 s");
    expect(formatCount(4_365_962)).toBe("4.4M");
    expect(formatCount(250_000)).toBe("250k");
    expect(formatCount(900)).toBe("900");
  });
});

describe("<OsHealthBanner />", () => {
  let current: OsHealthPayload = INCIDENT;
  let unsubscribe: () => void = () => {};

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-20T12:00:00Z"));
    resetOsHealthForTests();
    current = INCIDENT;
    mockInvoke.mockReset();
    mockListen.mockReset();
    mockInvoke.mockImplementation(async () => current);
    mockListen.mockImplementation(async () => () => {});
  });

  afterEach(() => {
    unsubscribe();
    resetOsHealthForTests();
    vi.useRealTimers();
  });

  it("renders nothing before the dwell, the sentence after it, and nothing once dismissed", async () => {
    unsubscribe = subscribeOsHealth(() => {});
    await vi.advanceTimersByTimeAsync(0);
    expect(renderToStaticMarkup(<OsHealthBanner />)).toBe("");

    await vi.advanceTimersByTimeAsync(10 * 60_000);
    const html = renderToStaticMarkup(<OsHealthBanner />);
    expect(html).toContain('data-ui-bridge-id="app.os-health-banner"');
    expect(html).toContain("creating processes slowly (p50 2.4 s");
    expect(html).toContain("Restarting Windows clears it.");

    dismissOsHealthBanner();
    expect(renderToStaticMarkup(<OsHealthBanner />)).toBe("");
  });

  it("renders nothing for an unknown verdict however long it lasts", async () => {
    current = {
      ...INCIDENT,
      verdict: "unknown",
      reasons: [reason("all", "no_snapshot", "unknown", null)],
    };
    unsubscribe = subscribeOsHealth(() => {});
    await vi.advanceTimersByTimeAsync(60 * 60_000);
    expect(renderToStaticMarkup(<OsHealthBanner />)).toBe("");
  });
});
