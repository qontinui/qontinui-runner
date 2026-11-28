/**
 * Which local panes would NOT survive a runner upgrade, and why (plan
 * `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
 * D7). The runner records what backs each pane's PTY — a PTY holder, or an
 * in-process PTY — and serves it from the `terminal_holder_states` command.
 * A tab whose pane is not upgrade-safe gets a "not upgrade-safe" marker.
 *
 * Pure + dependency-free (vitest runs it in `node`).
 */

/** Why a pane is not upgrade-safe — the runner's `NotUpgradeSafe`. */
export type NotUpgradeSafe =
  | { reason: "holder_disabled" }
  | { reason: "holder_spawn_failed"; kind: string; detail: string }
  | { reason: "unprotected_cgroup"; detail: string }
  | { reason: "cgroup_unknown"; detail: string };

/** The runner's `HolderState` for one pane. */
export interface HolderState {
  holder: boolean;
  upgradeSafe: boolean;
  notUpgradeSafe: NotUpgradeSafe | null;
}

/** Short inline label for the marker. */
export const NOT_UPGRADE_SAFE_LABEL = "not upgrade-safe";

/**
 * The marker's tooltip, or `null` when the pane needs no marker: an
 * upgrade-safe pane, a remote mirror (`null` state: its holder question is
 * its own device's), or a pane whose state was never read (`undefined` —
 * absence of a claim, not a claim).
 */
export function notUpgradeSafeTooltip(state: HolderState | null | undefined): string | null {
  if (!state || state.upgradeSafe) return null;
  const why = state.notUpgradeSafe;
  if (!why) {
    return "This pane is not upgrade-safe: a runner upgrade or restart ends it.";
  }
  switch (why.reason) {
    case "holder_disabled":
      return (
        "This pane runs inside the runner (terminal.pty_holder is off): a runner " +
        "upgrade or restart ends it."
      );
    case "holder_spawn_failed":
      return (
        `This pane's PTY holder could not be started (${why.kind}), so it fell back to ` +
        "an in-process PTY: a runner upgrade or restart ends it. " +
        `Detail: ${why.detail}`
      );
    case "unprotected_cgroup":
      return (
        "This pane's PTY holder shares the runner's systemd unit: stopping or " +
        `restarting that unit ends it (${why.detail}).`
      );
    case "cgroup_unknown":
      return (
        "This pane's PTY holder was adopted, but its cgroup could not be read: whether " +
        "a runner upgrade or restart ends it is unknown."
      );
  }
}
