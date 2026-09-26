/**
 * The ONE home for "is this wrapper running?" in the frontend.
 *
 * "Running" is two different questions, and the backend answers them
 * differently for a `degraded` wrapper (see Rust `wrappers::manager`):
 *
 * - **Is the process alive?** ({@link isWrapperProcessAlive}) — `running` or
 *   `degraded`. A degraded wrapper still owns its subprocess, so
 *   `POST /wrappers/:id/stop` has something to stop. This governs the
 *   lifecycle control: Stop vs Start.
 * - **Will dispatch route to it?** ({@link isWrapperRoutable}) — `running`
 *   ONLY, mirroring `WrapperManager::port_for`
 *   (`.filter(|rt| rt.state == WrapperState::Running)`) and the idle reaper.
 *   A degraded wrapper is not routable, and the UI must say so rather than
 *   imply it (plan 2026-08-23-single-source-derived-facts item 10).
 *
 * Never compare a {@link WrapperStatus} against these literals inline — a new
 * state (e.g. `starting`) then has to be taught to every copy.
 */

import type { WrapperStatus, WrapperStatusInfo } from "./types";

/** The subprocess exists (running or degraded) — Stop is the right control. */
export function isWrapperProcessAlive(status: WrapperStatus): boolean {
  return status === "running" || status === "degraded";
}

/** Dispatch will route to this wrapper — mirrors Rust `port_for`. */
export function isWrapperRoutable(status: WrapperStatus): boolean {
  return status === "running";
}

/** The lifecycle control a wrapper offers: Stop a live process, else Start. */
export type WrapperLifecycleControl = "start" | "stop";

/** Which lifecycle control to render for `status`. */
export function wrapperLifecycleControl(status: WrapperStatus): WrapperLifecycleControl {
  return isWrapperProcessAlive(status) ? "stop" : "start";
}

/** Human label for a status, stating non-routability where it applies. */
export function wrapperStatusLabel(status: WrapperStatus): string {
  switch (status) {
    case "running":
      return "Running";
    case "degraded":
      return "Degraded — not routable";
    case "stopped":
      return "Stopped";
    case "unknown":
      return "Unknown";
  }
}

/**
 * Project a `GET /wrappers/:id/status` payload onto the UI status. A payload
 * with no recognisable `state` is `unknown`, never a guessed state.
 */
export function wrapperStatusFromInfo(info: WrapperStatusInfo | null | undefined): WrapperStatus {
  switch (info?.state) {
    case "running":
    case "degraded":
    case "stopped":
      return info.state;
    default:
      return "unknown";
  }
}
