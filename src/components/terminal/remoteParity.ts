/**
 * Local-vs-remote affordance parity matrix (plan
 * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * Phase C).
 *
 * One row per affordance a terminal tab offers. Each row names the LOCAL entry
 * point, the REMOTE path the same affordance takes for a remote tab (a
 * `TerminalSession` over `RemotePaneIo`), and a verdict:
 *
 * - `same`: the remote tab gets the affordance through the same code, or
 *   an equivalent one, and it behaves the same;
 * - `different-by-design`: the remote tab deliberately behaves differently, and
 *   `operatorCopy` is the text the operator is shown saying so;
 * - `broken`: it should be the same and is not. `planStem` names the fix;
 * - `unknown`: nobody has established it yet.
 *
 * `basis` says how the verdict is known. `measured` means a live remote run
 * (cited in `evidence`). `code-path` means the verdict was traced through the
 * code on both machines but not yet driven live. A `code-path` row is not a
 * measurement and must not be reported as one. Live UI Bridge runs promote a
 * row to `measured` by editing it here.
 *
 * The rows are pinned against the action rosters the tab chrome renders from
 * (`ZONE_HOVER_ACTIONS`, `REMOTE_TAB_CONTROL_ACTIONS`). A button added to
 * either component without a row here fails `remoteParity.test.ts`. The pin
 * covers those two components only: the same affordance reached from another
 * surface (the command palette, the zone context menu, a keyboard chord) is
 * listed in its row's `localEntry` by hand, and a button rendered through a
 * wrapper component is not seen by the source check. A human-readable copy of
 * this matrix is in qontinui-dev-notes `qontinui-runner/remote-parity-matrix.md`.
 */

import type { RemoteTabControlActionId } from "./RemoteTabControls";
import type { ZoneHoverActionId } from "./ZoneHoverActions";

/** What the operator is told when a restart is refused for a remote tab. */
export const REMOTE_RESTART_REFUSAL =
  "Restart is not available for a remote session: restarting here would detach " +
  "it and start a local shell in its place. For a fresh session on that machine, " +
  "create one from the Fleet picker; if this tab's pane has ended (exit, or an " +
  "expired grant), use its reattach control.";

export type ParityVerdict = "same" | "different-by-design" | "broken" | "unknown";
export type ParityBasis = "measured" | "code-path";

export interface ParityRow {
  /** Stable row id. */
  id: string;
  affordance: string;
  /** The local entry symbol. */
  localEntry: string;
  /** What the same affordance does for a remote tab. */
  remotePath: string;
  verdict: ParityVerdict;
  basis: ParityBasis;
  /** Citations: code locations, PRs, findings, acceptance runs. */
  evidence: string;
  /** Roster ids (`ZONE_HOVER_ACTIONS` / `REMOTE_TAB_CONTROL_ACTIONS`) this row covers. */
  rosterIds: readonly (ZoneHoverActionId | RemoteTabControlActionId)[];
  /**
   * Required for `different-by-design`: the text the operator (or, for an
   * affordance with no tab chrome, the caller) is shown. Quoted from the code;
   * `<...>` marks an interpolated value.
   */
  operatorCopy?: string;
  /**
   * Who sees `operatorCopy`. Defaults to `operator` (rendered in the UI).
   * `api` marks an affordance with no tab chrome, whose refusal text reaches
   * only its programmatic caller: a recorded deviation from Phase C's
   * "operator-visible copy" rule, not an oversight.
   */
  copyAudience?: "operator" | "api";
  /** Required for `broken`: the plan stem that owns the fix. */
  planStem?: string;
}

export const REMOTE_PARITY_MATRIX: readonly ParityRow[] = [
  {
    id: "type-enter",
    affordance: "Type / Enter",
    localEntry: "TerminalSession::write -> LocalPty",
    remotePath:
      "TerminalSession::write -> RemotePaneIo FrameWriter (`remote_terminal_input`, seq-stamped) -> web relay -> target apply_terminal_input",
    verdict: "same",
    basis: "measured",
    evidence:
      "Two-machine acceptance A3(type) PASS merytshost->spaceship 2026-09-11, ONE direction (plan 2026-09-09-two-machine-remote-attach-acceptance); the metric direction (workstation->merytshost) fails at attach, which is Phase B's, not this row's. Input acks end to end since A1 (runner#1810, 794025020 on main).",
    rosterIds: [],
  },
  {
    id: "interrupt",
    affordance: "Interrupt (Ctrl-C) under load",
    localEntry: "TerminalSession::write (0x03)",
    remotePath: "same as type-enter: one `remote_terminal_input` frame carrying 0x03",
    verdict: "same",
    basis: "measured",
    evidence:
      "Acceptance A4 PASS, one direction: aborted at line 15720, UI responsive in 0.28 s (parity plan record).",
    rosterIds: [],
  },
  {
    id: "scrollback",
    affordance: "Scrollback",
    localEntry: "the local scrollback ring",
    remotePath:
      "the attach seeds only the tail of the target's ring; older output on demand via `terminal_remote_history_load` (Earlier output)",
    verdict: "different-by-design",
    basis: "code-path",
    evidence:
      "RemoteTabControls.tsx loadHistory -> terminal_remote_history_load; button shown only while `remote.historyAvailable`. Reattach-without-gap (acceptance A5) has not been run.",
    rosterIds: ["remote.earlier-output"],
    operatorCopy:
      "The attach delivered only the newest part of the remote scrollback. Load what came before it (re-renders this pane).",
  },
  {
    id: "resize",
    affordance: "Resize",
    localEntry: "terminal_resize -> TerminalSession::resize -> LocalPty",
    remotePath:
      "TerminalSession::resize -> RemotePaneIo::resize (`remote_terminal_resize`) -> web relay re-emits `terminal_resize` to the target -> backend_relay handle_terminal_resize",
    verdict: "same",
    basis: "code-path",
    evidence:
      "remote_pane_io.rs resize; qontinui-web remote_terminal_relay.py `remote_terminal_resize` -> `terminal_resize`; backend_relay.rs handle_terminal_resize. Not driven live: `terminal_resize` is not on the UI Bridge safelist, so `stty size` on the target is unmeasured.",
    rosterIds: [],
  },
  {
    id: "paste",
    affordance: "Paste (bracketed)",
    localEntry: "frontend paste -> TerminalSession::write",
    remotePath:
      "same write; the bytes travel base64-encoded in `remote_terminal_input` and are decoded verbatim on the target",
    verdict: "same",
    basis: "code-path",
    evidence:
      "remote_pane_io.rs FrameWriter (STANDARD.encode); remote_terminal.rs apply_terminal_input (STANDARD.decode -> write_input). The input frame is binary-safe, so the `strip_ansi` hazard (plan 2026-08-28-text-framing-escapes-outside-the-pty-choke-point) is not on this path.",
    rosterIds: [],
  },
  {
    id: "submit-prompt",
    affordance: "Slash commands / prompt library",
    localEntry: "TerminalSession::submit_prompt (liveness-gated)",
    remotePath:
      "same TerminalSession::submit_prompt; its writer is the RemotePaneIo FrameWriter, and the prompt row it reads is the source-side grid fed by remote output",
    verdict: "same",
    basis: "code-path",
    evidence:
      "session.rs submit_prompt takes the session writer (RemotePaneIo::writer for a remote tab) behind the same is_alive gate; nothing in it reads a local process.",
    rosterIds: [],
  },
  {
    id: "close",
    affordance: "Close",
    localEntry: "terminal_close -> TerminalSession::close -> LocalPty::kill",
    remotePath:
      "RemotePaneIo::kill sends `remote_terminal_detach`; the remote process keeps running",
    verdict: "different-by-design",
    basis: "code-path",
    evidence:
      "remote_pane_io.rs kill ('A local kill detaches; it never terminates the remote process'); copy from remoteTabs.ts remoteBadgeLabel (the remote badge's title); an unclean detach is called out by RemoteCloseNotice.tsx (runner#1562/#1633).",
    rosterIds: ["zone.close"],
    operatorCopy: "Closing this tab detaches; it does not end the session.",
  },
  {
    id: "graceful-exit",
    affordance: "Graceful exit",
    localEntry: "TerminalManager::graceful_exit",
    remotePath:
      "refused: the probe that proves the pane clear cannot see a remote pane's processes, so the close is refused and nothing is killed",
    verdict: "different-by-design",
    basis: "code-path",
    evidence:
      "graceful_exit.rs probe_claude_under returns Unreadable for a pane with no local pid, and `drive` returns GracefulExitOutcome::ProbeUnavailable at its first probe (graceful_exit.rs:535), before anything is typed; the manager's close re-probe is never reached. Graceful exit has no tab chrome: its callers are the wind-down executor and the HTTP route, which receive this outcome and its detail.",
    rosterIds: [],
    operatorCopy:
      "the pane has no local process id (a remote pane), so its subtree cannot be observed",
    copyAudience: "api",
  },
  {
    id: "resume-closed",
    affordance: "Resume a closed session",
    localEntry: "Previous view / restore",
    remotePath:
      "Reattach on the ended tab (`terminal_attach_remote` for the same deviceId/sessionId), or the sessions-console respawn",
    verdict: "different-by-design",
    basis: "code-path",
    evidence: "RemoteTabControls.tsx reattach; RemoteRestoreBanner.tsx.",
    rosterIds: ["remote.reattach"],
    operatorCopy:
      "This remote pane ended (the session exited, or the grant expired). Mint a fresh grant and reattach to the same session.",
  },
  {
    id: "durability",
    affordance: "Durability across source restart",
    localEntry: "sessionDurability.ts",
    remotePath: "excluded (`!tab.remote`); RemoteRestoreBanner offers re-attach after a restart",
    verdict: "different-by-design",
    basis: "code-path",
    evidence: "sessionDurability.ts `!tab.remote`; RemoteRestoreBanner.tsx.",
    rosterIds: [],
    operatorCopy:
      "Remote tabs are not reopened automatically — a restart voids their grants. Reattach mints a fresh grant for the same session; the tab returns when the target answers.",
  },
  {
    id: "create",
    affordance: "Create",
    localEntry: "New Terminal",
    remotePath: "remote create from the Fleet picker (remoteCreate.ts)",
    verdict: "different-by-design",
    basis: "code-path",
    evidence:
      "remoteCreate.ts; plan 2026-08-31-remote-session-tabs-in-runner-terminal Phases 3-5 (landed per that plan). The created tab carries the remote badge naming its machine.",
    rosterIds: [],
    operatorCopy: "Remote tab — this session runs on device <deviceId> (coord session <sessionId>).",
  },
  {
    id: "maximize",
    affordance: "Maximize / restore zone",
    localEntry: "ZoneHoverActions -> zoneLayout.toggleMaximize",
    remotePath: "same: a layout operation on the zone, independent of the pane's IO",
    verdict: "same",
    basis: "code-path",
    evidence: "ZoneHoverActions.tsx handleMaximize; no remote branch exists or is needed.",
    rosterIds: ["zone.maximize"],
  },
  {
    id: "restart",
    affordance: "Restart session",
    localEntry:
      "ZoneHoverActions, `/restart`, the command palette, the zone context menu, the zone card, Ctrl+Shift+R and auto-restart -> transitionEffects.handleRestartInZone",
    remotePath:
      "refused: a restart spawns a LOCAL terminal and retires the old tab, which for a remote tab only detaches it",
    verdict: "different-by-design",
    basis: "code-path",
    evidence:
      "Was BROKEN until this change: handleRestartInZone created a local shell (cwd = the remote tab's working dir) in the zone and closed the remote tab, which only detached it. With auto-restart armed this fired with no click whenever the remote process exited 0: RemotePaneIo::wait returns the target's exit code and the ended tab stays in its zone, so isRestartable read it as a clean exit. Now refused in handleRestartInZone (reason `remote-session`) and isRestartable; the hover button is greyed with this copy, `/restart` answers it, and the palette, context menu and zone card do not offer restart for a remote tab. Open items, tracked in qontinui-dev-notes qontinui-runner/remote-parity-matrix.md: Ctrl+Shift+R is refused without a message, and the `/restart` remote branch is pinned only by a source check (no realRegistry testkit arm).",
    rosterIds: ["zone.restart"],
    operatorCopy: REMOTE_RESTART_REFUSAL,
  },
  {
    id: "label",
    affordance: "Edit zone label",
    localEntry: "ZoneHoverActions -> labelsAndTags.setZoneLabel",
    remotePath: "same: zone metadata held by this runner",
    verdict: "same",
    basis: "code-path",
    evidence: "ZoneHoverActions.tsx openLabel / setZoneLabel; no remote branch.",
    rosterIds: ["zone.label"],
  },
  {
    id: "export",
    affordance: "Export zone output",
    localEntry: "useZoneActions handleExportZone -> hotStore.getLastOutputLines",
    remotePath:
      "same function over the same hot store; for a remote tab it holds what this runner received (the attach tail, plus Earlier output once loaded)",
    verdict: "same",
    basis: "code-path",
    evidence:
      "useZoneActions.ts handleExportZone reads terminalHotStore lastOutputLines for both kinds of tab; the content difference is the scrollback row's.",
    rosterIds: ["zone.export"],
  },
  {
    id: "send-to-window",
    affordance: "Send terminal to a window",
    localEntry: "useTerminalWindowActions popOutTab / moveTabToWindow",
    remotePath:
      "same: moves the tab's view between webview windows; the receiving window re-reads remote identity via `terminal_remote_identities`",
    verdict: "same",
    basis: "code-path",
    evidence:
      "useTerminalWindowActions.ts; useTerminalManager.ts terminal_remote_identities hydration; the TerminalSession is not touched by a window move.",
    rosterIds: ["zone.send-to-window"],
  },
];
