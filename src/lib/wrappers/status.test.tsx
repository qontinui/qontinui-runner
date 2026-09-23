/**
 * Wrapper "is running" — the single frontend home (plan
 * 2026-08-23-single-source-derived-facts item 10).
 *
 * Contract mirrored from Rust `wrappers::manager` (pinned there by
 * `degraded_is_alive_but_not_routable` / `running_is_routable` /
 * `status_wire_shape_matches_the_ts_mirror`):
 *   - `port_for` routes to `Running` ONLY;
 *   - a `Degraded` record still owns its subprocess, so `stop` acts on it.
 *
 * vitest runs with `environment: "node"` (no jsdom / testing-library), so the
 * render goes through `react-dom/server` like `StatusStrip.multiZone.test.tsx`.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { StatusBadge } from "@/components/wrappers/StatusBadge";
import { WrapperCard } from "@/components/wrappers/WrapperCard";
import type { InstalledWrapper, WrapperStatus, WrapperStatusInfo } from "./types";
import {
  isWrapperProcessAlive,
  isWrapperRoutable,
  wrapperLifecycleControl,
  wrapperStatusFromInfo,
  wrapperStatusLabel,
} from "./status";

/** A `GET /wrappers/:id/status` payload exactly as Rust serializes it. */
function statusInfo(state: WrapperStatusInfo["state"]): WrapperStatusInfo {
  return {
    id: "w",
    state,
    port: state === "stopped" ? null : 41234,
    pid: state === "stopped" ? null : 7,
    started_at_ms: state === "stopped" ? null : 1_000,
    last_dispatch_at_ms: state === "stopped" ? null : 1_000,
    consecutive_health_failures: state === "degraded" ? 3 : 0,
  };
}

const WRAPPER: InstalledWrapper = {
  id: "w",
  packageName: "@acme/w",
  version: "1.0.0",
  manifest: { manifestVersion: 1, id: "w", displayName: "W", transport: "api" },
  actions: [],
};

const noop = () => {};

function renderCard(status: WrapperStatus): string {
  return renderToStaticMarkup(
    <WrapperCard
      wrapper={WRAPPER}
      status={status}
      onOpen={noop}
      onStart={noop}
      onStop={noop}
      onUpdate={noop}
      onUninstall={noop}
    />,
  );
}

describe("a Degraded wrapper (the fixture the plan asks for)", () => {
  const status = wrapperStatusFromInfo(statusInfo("degraded"));

  it("is read from the wire's `state` field", () => {
    expect(status).toBe("degraded");
  });

  it("is NOT routable — matching `port_for`, which filters on Running", () => {
    expect(isWrapperRoutable(status)).toBe(false);
  });

  it("IS a live process — so the control offered is Stop, which `stop` honours", () => {
    expect(isWrapperProcessAlive(status)).toBe(true);
    expect(wrapperLifecycleControl(status)).toBe("stop");
  });

  it("renders a card whose badge says it is not routable", () => {
    const html = renderCard(status);
    expect(html).toContain("Degraded — not routable");
    expect(html).not.toContain(">Running<");
  });
});

describe("the full state table agrees with the Rust manager", () => {
  // [state, routable (port_for is Some), alive (stop has a process), control]
  const TABLE: Array<[WrapperStatus, boolean, boolean, "start" | "stop"]> = [
    ["running", true, true, "stop"],
    ["degraded", false, true, "stop"],
    ["stopped", false, false, "start"],
    ["unknown", false, false, "start"],
  ];

  it.each(TABLE)("%s → routable=%s alive=%s control=%s", (s, routable, alive, control) => {
    expect(isWrapperRoutable(s)).toBe(routable);
    expect(isWrapperProcessAlive(s)).toBe(alive);
    expect(wrapperLifecycleControl(s)).toBe(control);
  });

  it("routable implies alive (never route to a process that is gone)", () => {
    for (const [s] of TABLE) {
      if (isWrapperRoutable(s)) expect(isWrapperProcessAlive(s)).toBe(true);
    }
  });
});

describe("wrapperStatusFromInfo", () => {
  it("maps every backend state through unchanged", () => {
    expect(wrapperStatusFromInfo(statusInfo("running"))).toBe("running");
    expect(wrapperStatusFromInfo(statusInfo("stopped"))).toBe("stopped");
  });

  it("the pre-fix read (`.status`) is gone: a payload with no `state` is unknown, not guessed", () => {
    // The shape the old TS type claimed — the backend never sends it.
    const legacy = { status: "running" } as unknown as WrapperStatusInfo;
    expect(wrapperStatusFromInfo(legacy)).toBe("unknown");
    expect(wrapperStatusFromInfo(null)).toBe("unknown");
    expect(wrapperStatusFromInfo(undefined)).toBe("unknown");
  });
});

describe("StatusBadge labels", () => {
  it("uses the shared label for every status", () => {
    for (const s of ["running", "degraded", "stopped", "unknown"] as const) {
      expect(renderToStaticMarkup(<StatusBadge status={s} />)).toContain(wrapperStatusLabel(s));
    }
  });

  it("a running card does not claim degradation (negative control)", () => {
    expect(renderCard("running")).not.toContain("not routable");
  });
});
