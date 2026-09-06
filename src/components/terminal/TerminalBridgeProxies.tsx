import { memo, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useUIBridgeOptional } from "@qontinui/ui-bridge";
import { attachSubordinateBridgeInput } from "./subordinateBridgeRegistration";
import { throwIfWriteFailed } from "./terminalWriteResult";
import { preparePasteData } from "./preparePaste";
import { toPtySequence } from "./terminalKeySequence";
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
      getElement: () => elRef.current,
      buildDescriptor: () => ({
        type: "textarea",
        // Named so a reader of `getAllElements()` can tell WHICH kind of
        // registration is standing. A proxy means the pane has no mounted
        // view — the automation surface still works, but focus/keyboard go
        // nowhere visible.
        label: `Terminal input (${terminalId.slice(0, 8)}) [no mounted view — ${titleRef.current}]`,
        actions: ["focus", "blur"],
        customActions: {
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
              "TERMINAL_EXITED when the pane's process is gone.",
            handler: async (params?: unknown) => {
              const { text } = (params || {}) as { text?: string };
              if (!text) throw new Error("writeToTerminal: 'text' is required");
              return throwIfWriteFailed(await writePtyById(terminalId, text, exitRef.current));
            },
          },
          pasteText: {
            id: "pasteText",
            description:
              "Paste literal text to the PTY by id (no mounted view). Bracketed-paste mode " +
              "is a property of the live xterm backend and is unknown here, so the text is " +
              "sent unbracketed with the same newline normalization as the Ctrl+V path.",
            handler: async (params?: unknown) => {
              const { text } = (params || {}) as { text?: string };
              if (!text) throw new Error("pasteText: 'text' is required");
              return throwIfWriteFailed(
                await writePtyById(terminalId, preparePasteData(text, false), exitRef.current),
              );
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
