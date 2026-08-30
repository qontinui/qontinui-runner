/**
 * Deliver an approval keystroke to a set of panes, and report what actually
 * reached a PTY.
 *
 * ## The defect this replaces
 *
 * `/approve-all` used to be this, inline in the handler:
 *
 * ```ts
 * const waiting = tabs.filter((t) => sessionStates[t.id] === "needs-input");
 * for (const tab of waiting) {
 *   terminalRefs.current.get(tab.id)?.current?.writeToTerminal("y\r");
 * }
 * return ok({ approved: waiting.length });
 * ```
 *
 * Two optional chains, neither observed. With no `TerminalInstance` mounted
 * for a waiting tab — which is the NORMAL state for a flow-grid zone scrolled
 * offscreen, and for every pane during the restore window — the chain
 * short-circuits and nothing is written at all. The count came from `waiting`,
 * i.e. from INTENT, so the operator read `/approve-all ✓` for the action the
 * code's own comment calls "the most irreversible action on this page" having
 * delivered zero keystrokes.
 *
 * ## What this does instead
 *
 * Per pane, in tab order:
 *  - a mounted `TerminalInstance` handle is preferred (identical byte path,
 *    local echo) and its `TerminalWriteResult` is AWAITED and read;
 *  - with no mounted handle it falls through to {@link writePtyById}, which
 *    addresses the PTY by id over `terminal_write` and returns the same
 *    envelope. This is not a consolation path — it is the path that makes an
 *    offscreen pane approvable at all.
 *
 * `delivered` counts envelopes that said `success: true`. Nothing else.
 *
 * ## Why an unreported write is a FAILURE and is never retried
 *
 * A handle that hands back `undefined` (a stale build, a proxy that predates
 * the envelope) has told us nothing. Counting it would reintroduce exactly the
 * assertion this module deletes, so it is reported as
 * {@link TERMINAL_WRITE_UNREPORTED} — an honest "we do not know".
 *
 * It is emphatically NOT retried down the by-id path. The write may well have
 * landed; `y\r` delivered twice can answer a prompt the operator never saw,
 * and this command exists precisely because answering yes on an agent's behalf
 * is unrecoverable. An undercount is a verdict the operator can act on; a
 * double keystroke is not.
 */

import { writePtyById } from "./writePtyById";
import type { TerminalWriteResult } from "./terminalWriteResult";

/** Machine-readable failure code: the write path gave back no envelope. */
export const TERMINAL_WRITE_UNREPORTED = "TERMINAL_WRITE_UNREPORTED";

/**
 * The MINIMUM a mounted pane has to offer to be written to.
 *
 * Deliberately structural rather than `TerminalInstanceHandle`. Three callers
 * reach this module and they hold three different views of the same map:
 * `useKeyboardShortcuts` declares its own `{writeToTerminal: (data: string) =>
 * void}`, `TerminalOverlays` and `TerminalPage` hold the full handle. Naming
 * the concrete handle here would force all three to converge on the type that
 * transitively pulls `@xterm/addon-canvas` — which touches `self` at module
 * init and crashes under the runner's `environment: "node"` vitest config.
 * That is the same leaf-module constraint `terminalWriteResult.ts` documents,
 * and it is why this file is testable at all.
 *
 * The return is `unknown` on purpose: an older handle answers `void`, and
 * {@link readEnvelope} is the one place that decides what a non-envelope means.
 */
export interface ApprovalWriteTarget {
  writeToTerminal: (text: string) => unknown;
}

/** The mounted-pane map, in the narrowest shape this module needs. */
export type ApprovalRefs = Map<string, { readonly current: ApprovalWriteTarget | null }>;

/** The route a delivery took, so a report can say WHERE it failed. */
export type DeliveryRoute = "mounted" | "by-id";

/** One pane's outcome. */
export interface ApprovalDelivery {
  tabId: string;
  route: DeliveryRoute;
  delivered: boolean;
  /** Failure code from the write envelope; absent on success. */
  code?: string;
  /** Human-readable failure detail; absent on success. */
  error?: string;
}

/** What {@link deliverApprovals} observed. */
export interface ApprovalReport {
  /** Panes we tried to reach. */
  targeted: number;
  /** Panes whose write envelope said it reached the process. */
  delivered: number;
  /** Per-pane outcomes, in the order attempted. */
  deliveries: ApprovalDelivery[];
}

/** Injection seam for tests — the same pattern `writePtyById` uses. */
export type WriteById = (
  terminalId: string,
  text: string,
  exit: { exitCode: number | null } | null,
) => Promise<TerminalWriteResult>;

/**
 * Read an unknown-shaped write return as an envelope. A handle predating the
 * envelope returns `undefined`; anything without a boolean `success` is
 * treated the same way — unreported, never assumed good.
 */
function readEnvelope(value: unknown): TerminalWriteResult | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as { success?: unknown };
  return typeof v.success === "boolean" ? (value as TerminalWriteResult) : null;
}

function failureOf(envelope: TerminalWriteResult | null): Pick<ApprovalDelivery, "code" | "error"> {
  if (!envelope) {
    return {
      code: TERMINAL_WRITE_UNREPORTED,
      error: "the write path returned no result envelope",
    };
  }
  if (envelope.success) return {};
  return { code: envelope.code, error: envelope.error };
}

/**
 * Write `text` into every named pane and report deliveries.
 *
 * Sequential rather than parallel: the wire order then equals the tab order,
 * which is what an operator watching the grid expects and what an automation
 * client asserting on `terminal_write` can rely on.
 *
 * @param exitOf  the pane's known liveness, so a write to a pane already known
 *                dead is refused BEFORE the IPC rather than reported as an IPC
 *                failure. Defaults to "presumed live", in which case the Rust
 *                write funnel's own `TERMINAL_EXITED` refusal classifies it.
 */
export async function deliverApprovals(
  tabIds: readonly string[],
  terminalRefs: ApprovalRefs,
  text: string,
  opts: {
    writeById?: WriteById;
    exitOf?: (tabId: string) => { exitCode: number | null } | null;
  } = {},
): Promise<ApprovalReport> {
  const writeById = opts.writeById ?? writePtyById;
  const deliveries: ApprovalDelivery[] = [];

  for (const tabId of tabIds) {
    const mounted = terminalRefs.get(tabId)?.current;
    const route: DeliveryRoute = mounted ? "mounted" : "by-id";
    let envelope: TerminalWriteResult | null;
    try {
      envelope = readEnvelope(
        mounted
          ? await mounted.writeToTerminal(text)
          : await writeById(tabId, text, opts.exitOf?.(tabId) ?? null),
      );
    } catch (err) {
      // A throw is a failed delivery like any other; it must not abort the
      // remaining panes, or one dead pane would silently cap the approval at
      // however many happened to come before it.
      deliveries.push({
        tabId,
        route,
        delivered: false,
        code: "TERMINAL_WRITE_THREW",
        error: err instanceof Error ? err.message : String(err),
      });
      continue;
    }
    deliveries.push({
      tabId,
      route,
      delivered: envelope?.success === true,
      ...failureOf(envelope),
    });
  }

  return {
    targeted: tabIds.length,
    delivered: deliveries.filter((d) => d.delivered).length,
    deliveries,
  };
}

// ── The one-off writes ───────────────────────────────────────────────

/**
 * Write into ONE pane by ref, and say what happened.
 *
 * The four one-off writers on this page — `useFindingsActions.ts`'s
 * finding-respond and resume paths, and `useShellIntegration.ts`'s two resume
 * timers — spelled this as
 * `terminalRefs.current.get(id)?.current?.writeToTerminal(text)`. Two silent
 * failures hide in that one line: the optional chain swallows an UNMOUNTED
 * pane, and the discarded return swallows a write the funnel REFUSED. Both
 * end the same way for the operator — a pane that was supposed to have a
 * command typed into it, sitting idle, with nothing anywhere saying why.
 *
 * None of the four feeds a verdict surface (three are inside `setTimeout`
 * callbacks, long after the status line has moved on), so there is nowhere to
 * render a report. What there is to do is stop it being SILENT: the console
 * carries the reason, which is what the runner's dev log picks up and what an
 * autonomous debugging loop reads. `deliverApprovals` above is the same
 * doctrine for the batch path.
 *
 * @returns `true` when the write envelope said the bytes reached a process.
 */
export async function writeToPaneOrReport(
  refs: ApprovalRefs,
  tabId: string,
  text: string,
  what: string,
): Promise<boolean> {
  const handle = refs.get(tabId)?.current;
  if (!handle) {
    console.warn(`[terminal] ${what}: pane ${tabId} has no mounted handle; nothing was written`);
    return false;
  }
  const envelope = readEnvelope(await handle.writeToTerminal(text));
  if (envelope?.success) return true;
  const { code, error } = failureOf(envelope);
  console.warn(`[terminal] ${what}: write to ${tabId} did not land (${code}): ${error}`);
  return false;
}
