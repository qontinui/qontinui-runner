import { memo, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIBridgeOptional } from "@qontinui/ui-bridge";
import { attachSubordinateBridgeInput } from "./subordinateBridgeRegistration";
import { throwIfWriteFailed } from "./terminalWriteResult";
import { preparePasteData } from "./preparePaste";
import { readBracketedPasteMode } from "./bracketedPasteById";
import { hasMountedTerminalView } from "./mountedTerminalViews";
import { toPtySequence } from "./terminalKeySequence";
import { PASTE_TEXT_INVALID, WRITE_TEXT_INVALID, requireTextPayload } from "./terminalTextPayload";
import { writePtyById } from "./writePtyById";
import { stripAnsi } from "./outputLineTracking";
import { readLocalScrollbackRing } from "./backends/localScrollbackRing";

/**
 * Mount-independent UI Bridge elements for every terminal tab on a page.
 *
 * Rendered by `PageSessionScope`, which is ALWAYS mounted — one per terminal
 * page, outside `TerminalPage`'s `initialized` spinner, outside `ZoneGrid`, and
 * unaffected by which page is active. That placement is the point: it is the
 * only place in the tree that knows every tab and is never gated on a pane
 * being visible, mounted, restored or scrolled to.
 *
 * See `./subordinateBridgeRegistration.ts` for the full defect write-up
 * (manual-test-loop iter 18, item 1). In short: `terminal-input-<id>` used to
 * exist only while a `TerminalInstance` was mounted, and flow-grid
 * virtualization mounts nothing for an offscreen zone — so a restored pane
 * could sit `isAlive: true` in `GET /terminals` with no bridge element and no
 * warning anywhere, exactly as iteration 17 measured after a runner restart.
 *
 * Each proxy holds `terminal-input-<id>` only while no mounted instance does,
 * and serves the pane's custom actions through the ID-ADDRESSED runner routes
 * (`terminal_write`, and the local scrollback ring via
 * `readLocalScrollbackRing`) — the same mount-independent route
 * `writeToTerminalById` already uses for compact-card quick actions. There is
 * no `ITerminalBackend` to go through here BY CONSTRUCTION: the proxy exists
 * exactly while no backend is mounted for the pane, which is why it reads the
 * local ring module directly rather than `backend.readScrollbackRing`.
 * A mounted instance always wins the id back, and its
 * DOM-attached xterm textarea is strictly better (real focus, real rect, local
 * echo), so nothing regresses for a visible pane.
 *
 * ## The proxy must behave IDENTICALLY, or refuse (iter 24)
 *
 * "A mounted instance always wins the id back" turned out to be an aspiration
 * rather than an invariant: after a soft-nav remount (`/terminal` → `/settings`
 * → `/terminal`) the proxy kept the id indefinitely, so a visible, painted pane
 * with a live xterm was served from here (item 1). That made every divergence
 * between the two descriptors observable on an ordinary pane, and there were
 * four:
 *
 *  - `sendKeys` translated on the mounted path and not here (iteration 23,
 *    fixed on #1228 and preserved below);
 *  - `writeToTerminal` `String()`-coerced a non-string `text` into a live PTY
 *    and answered 200 (item 2);
 *  - `focus`/`blur` were advertised, and moved REAL keyboard focus onto the
 *    hidden 1×1 textarea while reporting success (item 4);
 *  - `paste` existed only on the mounted path, so a capability appeared and
 *    disappeared with virtualization (item 5);
 *  - `pasteText` hardcoded `bracketedPasteMode: false`, so one call produced
 *    different bytes depending on whether the pane happened to be on screen
 *    (item 6).
 *
 * The rule this component is now written to: for every action, either behave
 * exactly as the mounted path does, or refuse with a TYPED error. Never
 * silently do something different, and never silently succeed at something a
 * proxy cannot actually do.
 */
export interface TerminalBridgeProxyTab {
  id: string;
  title?: string;
  isAlive?: boolean;
  exitCode?: number | null;
  type?: "terminal" | "plan";
}

/**
 * Report a registration failure to the RUNNER log.
 *
 * The `console.error` beside every call site never reaches it: a webview console
 * line leaves WebView2 only through the SDK's optional browser-capture pipeline
 * into the DB-backed error monitor. This command is the dependency-free path —
 * one `tracing::warn!` in the runner log, readable whatever else is degraded.
 */
function reportRegistrationFailure(
  terminalId: string,
  elementId: string,
  reason: string,
  elapsedMs: number,
  detail: unknown,
): void {
  invoke("terminal_report_bridge_registration_failure", {
    terminalId,
    elementId,
    reason,
    elapsedMs: Math.round(elapsedMs),
    detail: detail instanceof Error ? detail.message : detail === undefined ? null : String(detail),
  }).catch(() => {
    // Best-effort observability: a failed report must not become a second
    // silent failure, but there is nothing useful to do about it here.
  });
}

/** Machine-readable refusal: this action needs a mounted view and there is none. */
export const TERMINAL_NO_MOUNTED_VIEW = "TERMINAL_NO_MOUNTED_VIEW";

/**
 * Build the typed refusal a proxy answers for a view-only action (iter 24,
 * item 4). Same `message` + `.code` shape as `toPtySequence`'s
 * `SEND_KEYS_INVALID`, which is what the SDK hoists onto the response.
 */
function noMountedView(action: string, terminalId: string): Error {
  const err = new Error(
    `${TERMINAL_NO_MOUNTED_VIEW}: '${action}' needs a mounted terminal view and ` +
      `terminal ${terminalId} has none (this pane is virtualized). Nothing was ` +
      `focused, blurred or written. Bring the pane on screen so its ` +
      `TerminalInstance mounts, then retry.`,
  ) as Error & { code?: string };
  err.code = TERMINAL_NO_MOUNTED_VIEW;
  return err;
}

/**
 * The PTY's bracketed-paste state, or a throw (iter 24, item 6).
 *
 * Skipped for a pane already known dead so the more specific `TERMINAL_EXITED`
 * from the write path stays the diagnosis; `writePtyById` refuses before the
 * IPC in that case anyway, so `false` here never reaches a process.
 */
async function bracketedPasteFor(
  terminalId: string,
  exit: { exitCode: number | null } | null,
): Promise<boolean> {
  if (exit) return false;
  return readBracketedPasteMode(terminalId);
}

const HIDDEN_HOST_STYLE: React.CSSProperties = {
  position: "absolute",
  left: -10000,
  top: 0,
  // 1x1 rather than 0x0 and NOT `display:none`: the bridge registry skips any
  // element whose `isConnected` is false and reports a detached one as
  // `UB-STALE-ELEMENT`, so the proxy has to be a real, connected node. It stays
  // out of the layout, out of the tab order and out of the accessibility tree.
  width: 1,
  height: 1,
  overflow: "hidden",
  opacity: 0,
  pointerEvents: "none",
};

interface ProxyProps {
  terminalId: string;
  title: string;
  isAlive: boolean;
  exitCode: number | null;
}

const TerminalBridgeProxy = memo(function TerminalBridgeProxy({
  terminalId,
  title,
  isAlive,
  exitCode,
}: ProxyProps) {
  const uiBridge = useUIBridgeOptional();
  const uiBridgeRef = useRef(uiBridge);
  useEffect(() => {
    uiBridgeRef.current = uiBridge;
  }, [uiBridge]);

  const elRef = useRef<HTMLTextAreaElement | null>(null);

  // Liveness read through a ref so the attachment is not torn down and
  // re-registered every time a pane's `isAlive` flips — re-registration would
  // hand the id back and forth with a mounted instance for no reason.
  const exitRef = useRef<{ exitCode: number | null } | null>(null);
  exitRef.current = isAlive ? null : { exitCode };
  const titleRef = useRef(title);
  titleRef.current = title;

  useEffect(() => {
    const elementId = `terminal-input-${terminalId}`;
    return attachSubordinateBridgeInput({
      elementId,
      getRegistry: () => uiBridgeRef.current?.registry,
      // Item 1. Entry ownership is not the same question as "is there a live
      // view?": a pane whose registration the proxy happens to hold reads as
      // unowned and gets re-claimed, which is exactly the state a soft-nav
      // remount left behind. Consulting the mounted-view record makes the
      // subordination positive — a proxy registration cannot stand while a
      // live xterm exists, whatever order the two attachments ran in.
      shouldYield: () => hasMountedTerminalView(terminalId),
      getElement: () => elRef.current,
      buildDescriptor: () => ({
        type: "textarea",
        // Named so a reader of `getAllElements()` can tell WHICH kind of
        // registration is standing. A proxy means the pane has no mounted
        // view — the automation surface still works, but focus/keyboard go
        // nowhere visible.
        label: `Terminal input (${terminalId.slice(0, 8)}) [no mounted view — ${titleRef.current}]`,
        // NO standard actions (iter 24, item 4). `["focus", "blur"]` used to be
        // here, and the runner's own gates take an advertised action at its
        // word: a `focus` request therefore reached the SDK, which called
        // `element.focus()` UNCONDITIONALLY on the hidden 1×1 textarea. Real
        // keyboard focus left whatever the operator was typing into and landed
        // on an offscreen node — reported `success: true`, with nothing on
        // screen to show for it. A proxy has no focusable view; the honest
        // answer is a refusal, which the `focus`/`blur` entries below give.
        actions: [],
        customActions: {
          // ── item 4: refuse, loudly ────────────────────────────────────────
          // Registered rather than merely omitted because a customAction SHADOWS
          // the same-named built-in (ui-bridge#165), which is what guarantees
          // `element.focus()` is never reached. Omitting them alone would rely
          // on the two advertise-gates staying strict, and one of the two
          // (`isElementActionAllowed`) treats an EMPTY action list as
          // permissive. Refusing here does not depend on either.
          focus: {
            id: "focus",
            description:
              "REFUSED on a pane with no mounted view: there is nothing focusable. " +
              "Scroll or select the pane so its TerminalInstance mounts, then focus it.",
            handler: async () => {
              throw noMountedView("focus", terminalId);
            },
          },
          blur: {
            id: "blur",
            description: "REFUSED on a pane with no mounted view: nothing here can hold focus.",
            handler: async () => {
              throw noMountedView("blur", terminalId);
            },
          },
          sendKeys: {
            id: "sendKeys",
            description:
              "Send key sequences to the terminal by id (no mounted view). Accepts `keys` " +
              'as a raw string (written verbatim), an array of key names (["Enter"]), or ' +
              'the SDK\'s descriptor array ([{ key: "c", modifiers: { ctrl: true } }]). ' +
              "Fails with TERMINAL_EXITED when the pane's process is gone.",
            handler: async (params?: unknown) => {
              // MUST translate, exactly as the mounted path does
              // (`TerminalInstance.tsx`'s `sendKeys` → `toPtySequence`).
              //
              // THE DEFECT this closes (manual-test-loop iter 23, item 1): this
              // proxy handler was added after iteration 21 fixed the mounted
              // path, and handed the raw `keys` value straight to
              // `writePtyById`, whose `TextEncoder.encode` coerces anything
              // non-string via `String()`. On a virtualized pane — the ONLY
              // panes this proxy owns — `{keys:["Enter"]}` therefore typed the
              // literal text `Enter`, `{keys:[{key:"Enter"}]}` typed
              // `[object Object]`, and the untranslatable `"Enterr"` typed
              // itself instead of failing SEND_KEYS_INVALID. All three answered
              // `success: true` with a byte count, because the write genuinely
              // reached the PTY. Those panes are live Claude/PowerShell
              // sessions, so that was silent corruption of real work reported
              // green — the exact failure `terminalKeySequence.ts` was written
              // to prevent, reintroduced by a second code path.
              //
              // `toPtySequence` also owns the missing/empty `keys` rejection,
              // so there is no separate guard here: an untranslatable key must
              // THROW, never type its own name.
              const { keys } = (params || {}) as { keys?: unknown };
              return throwIfWriteFailed(
                await writePtyById(terminalId, toPtySequence(keys), exitRef.current),
              );
            },
          },
          writeToTerminal: {
            id: "writeToTerminal",
            description:
              "Write text directly to the PTY by id (no mounted view). Fails with " +
              "WRITE_TEXT_INVALID when `text` is not a string, and with TERMINAL_EXITED " +
              "when the pane's process is gone.",
            handler: async (params?: unknown) => {
              // Item 2, and the same shape as `sendKeys` above: the old
              // `if (!text)` was a `string` ASSERTION rather than a check, so a
              // non-string `text` reached `TextEncoder.encode` and was coerced
              // via `String()` — `{text: 42}` wrote `42` into a live shell and
              // answered HTTP 200 with a byte count. It also rejected the
              // perfectly valid falsy string `"0"`.
              const { text } = (params || {}) as { text?: unknown };
              const value = requireTextPayload(text, WRITE_TEXT_INVALID, "writeToTerminal");
              return throwIfWriteFailed(await writePtyById(terminalId, value, exitRef.current));
            },
          },
          paste: {
            id: "paste",
            description:
              "Read clipboard and write to the PTY by id (same as Ctrl+V, no mounted view).",
            handler: async () => {
              // Item 5. This action existed ONLY on the mounted path, so the
              // pane's advertised capability set changed as it scrolled in and
              // out of a virtualized flow grid — `paste` present on one commit
              // and `Unknown action` on the next, for the same live terminal.
              // A capability that flickers with the viewport is worse than one
              // that is absent: automation written against it fails
              // intermittently and looks like a flake.
              const text = await navigator.clipboard.readText().catch(() => "");
              if (!text) return { success: true, bytes: 0 };
              const prepared = preparePasteData(
                text,
                await bracketedPasteFor(terminalId, exitRef.current),
              );
              return throwIfWriteFailed(await writePtyById(terminalId, prepared, exitRef.current));
            },
          },
          pasteText: {
            id: "pasteText",
            description:
              "Paste literal text to the PTY by id (no mounted view), through the same " +
              "bracketed-paste-aware path as the mounted pane: the PTY's DEC 2004 state is " +
              "read from the runner's own VT parser by id. Fails with PASTE_TEXT_INVALID " +
              "when `text` is not a string.",
            handler: async (params?: unknown) => {
              const { text } = (params || {}) as { text?: unknown };
              const value = requireTextPayload(text, PASTE_TEXT_INVALID, "pasteText");
              // Item 6. `false` used to be hardcoded here with the note that
              // bracketed-paste mode "is a property of the live xterm backend
              // and is unknown here". It is not unknown — it was merely unasked
              // for. The runner's server-side VT parser sees every output byte
              // of every session, mounted or not, so the same DEC 2004 state
              // the mounted path reads off xterm is available by id.
              const prepared = preparePasteData(
                value,
                await bracketedPasteFor(terminalId, exitRef.current),
              );
              return throwIfWriteFailed(await writePtyById(terminalId, prepared, exitRef.current));
            },
          },
          getScrollback: {
            id: "getScrollback",
            description:
              "Read the terminal's scrollback as plain text. With no mounted xterm this " +
              "comes from the Rust PTY ring rather than the rendered buffer, with escape " +
              "sequences stripped.",
            handler: async (params?: unknown) => {
              const { maxLines = 500 } = (params || {}) as { maxLines?: number };
              const ring = await readLocalScrollbackRing(terminalId);
              if (!ring) return "";
              const decoded = new TextDecoder().decode(ring.bytes);
              const lines = stripAnsi(decoded).split("\n");
              return lines.slice(Math.max(0, lines.length - maxLines)).join("\n");
            },
          },
        },
      }),
      onUnowned: (elapsedMs, lastError) => {
        // The give-up warning made genuinely reachable: this reporter does NOT
        // need a mounted component, so it covers the never-mounted case that
        // made iteration 17's failure silent — and it lands in the runner log,
        // not only in a webview console the runner does not read.
        console.error(
          `[Terminal ${terminalId}] ${elementId} has been unregistered for ${elapsedMs}ms — ` +
            `custom actions on this pane will answer ELEMENT_NOT_FOUND`,
          lastError ?? "(no error thrown; the bridge registry never appeared)",
        );
        reportRegistrationFailure(terminalId, elementId, "no-owner", elapsedMs, lastError);
      },
    });
  }, [terminalId]);

  return (
    <textarea
      ref={elRef}
      readOnly
      tabIndex={-1}
      aria-hidden="true"
      data-terminal-bridge-proxy={terminalId}
      // `opacity: 0` ON THE TEXTAREA ITSELF, not merely on the host, and this
      // is load-bearing. The SDK's auto-register DOM walker matches every
      // `textarea` and skips only elements whose OWN computed style is
      // `display:none` / `visibility:hidden` / `opacity:0` / zero-sized
      // (`useAutoRegister` → `isElementVisible`). `opacity` does not inherit as
      // a computed value, so a textarea inside an `opacity:0` host still reads
      // `1` and gets auto-registered a SECOND time under a generated id
      // (`textarea-main-1`).
      //
      // That second registration breaks custom actions outright: the SDK
      // dispatches an unknown action via `registry.findByDOMElement(element)`,
      // which returns the FIRST registration matching that DOM node — and if
      // that is the walker's, it has no `customActions`, so
      // `POST /control/element/terminal-input-<id>/action` with
      // `writeToTerminal` answers `Unknown action` even though the element is
      // present and advertises it. Measured on this build before the fix.
      //
      // xterm's own helper textarea escapes the walker the same way (it is
      // `opacity: 0`), which is why the mounted path never hit this.
      style={{ width: 1, height: 1, opacity: 0, resize: "none", border: 0, padding: 0 }}
    />
  );
});

/**
 * Host for one page's proxies. Renders a single hidden container so N tabs cost
 * one extra DOM subtree, not N scattered nodes.
 */
export const TerminalBridgeProxies = memo(function TerminalBridgeProxies({
  tabs,
}: {
  tabs: readonly TerminalBridgeProxyTab[];
}) {
  // Plan tabs are a markdown viewer with no PTY — they have no `/terminals`
  // row and must not claim a `terminal-input-*` id.
  const terminals = useMemo(() => tabs.filter((t) => t.type !== "plan"), [tabs]);
  return (
    <div style={HIDDEN_HOST_STYLE} aria-hidden="true" data-terminal-bridge-proxies="">
      {terminals.map((t) => (
        <TerminalBridgeProxy
          key={t.id}
          terminalId={t.id}
          title={t.title ?? t.id}
          isAlive={t.isAlive !== false}
          exitCode={t.exitCode ?? null}
        />
      ))}
    </div>
  );
});
