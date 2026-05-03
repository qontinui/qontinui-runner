import { useEffect, useRef, useCallback, forwardRef, useImperativeHandle } from "react";
import { FilePathLinkProvider } from "./FilePathLinkProvider";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useUIBridgeOptional } from "@qontinui/ui-bridge";
import { createTerminalBackend } from "./backends";
import type { BackendType, ITerminalBackend } from "./backends";
import { paintGrid, type GridSnapshot } from "./paintGrid";
import { instanceStorage } from "@/lib/instance-storage";

export interface TerminalInstanceHandle {
  getSelection: () => string;
  hasSelection: () => boolean;
  writeToTerminal: (text: string) => void;
  /**
   * Write raw output data directly to the terminal display without sending to the PTY.
   * Used for replaying saved scrollback buffers on session restore.
   */
  writeToDisplay: (data: string) => void;
  /** Read up to `maxLines` lines from the terminal scrollback buffer. */
  getScrollback: (maxLines?: number) => string;
  /** Scroll the terminal viewport to the very bottom. */
  scrollToBottom: () => void;
}

export type ShellIntegrationEvent =
  | { type: "prompt_start" }
  | { type: "command_ready" }
  | { type: "command_execute" }
  | { type: "command_done"; exitCode: number }
  | { type: "command_line"; command: string }
  | { type: "cwd"; path: string };

interface TerminalInstanceProps {
  terminalId: string;
  visible: boolean;
  /** Which terminal backend to use. Defaults to "xterm". */
  backendType?: BackendType;
  /** True when this instance is reconnecting to an existing Rust PTY session. */
  isReconnecting?: boolean;
  /** Called after scrollback buffer has been replayed and live events are flowing. */
  onReconnected?: () => void;
  onExit?: (exitCode: number | null) => void;
  onSelectionChange?: (hasSelection: boolean) => void;
  onFirstInput?: (input: string) => void;
  /** Called when the shell emits an OSC 633 shell integration event. */
  onShellIntegration?: (event: ShellIntegrationEvent) => void;
  /** Called with decoded text whenever PTY output is received. */
  onOutput?: (text: string) => void;
}

interface TerminalOutputEvent {
  terminalId: string;
  data: string; // base64
}

interface TerminalExitEvent {
  terminalId: string;
  exitCode: number | null;
}

/** Encode a Uint8Array to base64 without stack overflow on large buffers. */
function uint8ToBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 8192;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const slice = bytes.subarray(i, Math.min(i + CHUNK, bytes.length));
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

const encoder = new TextEncoder();

/** Default terminal options shared by all backends. */
const TERMINAL_OPTIONS = {
  cursorBlink: true,
  cursorStyle: "block" as const,
  fontSize: 14,
  fontFamily:
    "'Cascadia Code', 'Fira Code', 'JetBrains Mono', Menlo, Monaco, 'Courier New', monospace",
  lineHeight: 1.2,
  scrollback: 10000,
  theme: {
    background: "#1a1b26",
    foreground: "#c0caf5",
    cursor: "#c0caf5",
    selectionBackground: "#33467c",
    selectionForeground: "#c0caf5",
    black: "#15161e",
    red: "#f7768e",
    green: "#9ece6a",
    yellow: "#e0af68",
    blue: "#7aa2f7",
    magenta: "#bb9af7",
    cyan: "#7dcfff",
    white: "#a9b1d6",
    brightBlack: "#414868",
    brightRed: "#f7768e",
    brightGreen: "#9ece6a",
    brightYellow: "#e0af68",
    brightBlue: "#7aa2f7",
    brightMagenta: "#bb9af7",
    brightCyan: "#7dcfff",
    brightWhite: "#c0caf5",
  },
};

export const TerminalInstance = forwardRef<TerminalInstanceHandle, TerminalInstanceProps>(
  function TerminalInstanceInner(
    {
      terminalId,
      visible,
      backendType: backendTypeProp,
      isReconnecting,
      onReconnected,
      onExit,
      onSelectionChange,
      onFirstInput,
      onShellIntegration,
      onOutput,
    },
    ref,
  ) {
    const uiBridge = useUIBridgeOptional();
    // Resolve backend type: explicit prop > instanceStorage > "xterm"
    const backendType: BackendType =
      backendTypeProp ??
      ((instanceStorage.getItem("terminal-backend") as BackendType | null) || "xterm");
    const containerRef = useRef<HTMLDivElement>(null);
    const backendRef = useRef<ITerminalBackend | null>(null);
    const bytesReceivedRef = useRef(0);
    const lastAckedRef = useRef(0);
    // Stable refs for callbacks to avoid effect re-runs
    const onExitRef = useRef(onExit);
    onExitRef.current = onExit;
    const onSelectionChangeRef = useRef(onSelectionChange);
    onSelectionChangeRef.current = onSelectionChange;
    const onFirstInputRef = useRef(onFirstInput);
    onFirstInputRef.current = onFirstInput;
    const onReconnectedRef = useRef(onReconnected);
    onReconnectedRef.current = onReconnected;
    const onShellIntegrationRef = useRef(onShellIntegration);
    onShellIntegrationRef.current = onShellIntegration;
    const onOutputRef = useRef(onOutput);
    onOutputRef.current = onOutput;
    const outputDecoderRef = useRef(new TextDecoder());
    const firstInputReportedRef = useRef(false);
    const inputAccumulatorRef = useRef("");

    // Expose selection, write, and scrollback API to parent components
    useImperativeHandle(ref, () => ({
      getSelection: () => backendRef.current?.getSelection() ?? "",
      hasSelection: () => backendRef.current?.hasSelection() ?? false,
      writeToTerminal: (text: string) => {
        const bytes = encoder.encode(text);
        invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(() => {});
      },
      writeToDisplay: (data: string) => {
        backendRef.current?.write(data);
      },
      getScrollback: (maxLines = 500) => {
        const backend = backendRef.current;
        if (!backend) return "";
        const totalLines = backend.getBufferLength();
        const startLine = Math.max(0, totalLines - maxLines);
        const lines: string[] = [];
        for (let i = startLine; i < totalLines; i++) {
          const line = backend.getBufferLine(i);
          if (line) {
            lines.push(line);
          }
        }
        return lines.join("\n");
      },
      scrollToBottom: () => {
        backendRef.current?.scrollToBottom();
      },
    }));

    // Debounced fit — coalesce rapid resize events
    const fitTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const fitTerminal = useCallback(() => {
      if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
      fitTimerRef.current = setTimeout(() => {
        const backend = backendRef.current;
        if (!backend || !containerRef.current) return;
        // Skip resize when container is hidden (e.g., compact mode) to avoid
        // resizing the PTY to zero columns which garbles output wrapping.
        const { clientWidth, clientHeight } = containerRef.current;
        if (clientWidth === 0 || clientHeight === 0) return;
        try {
          backend.fit();
          invoke("terminal_resize", {
            terminalId,
            cols: backend.cols,
            rows: backend.rows,
          }).catch(() => {});
        } catch {
          // Container may not be visible yet
        }
      }, 50);
    }, [terminalId]);

    useEffect(() => {
      if (!containerRef.current) return;
      const container = containerRef.current;

      let disposed = false;
      const disposables: Array<{ dispose(): void }> = [];
      let outputUnsub: UnlistenFn | null = null;
      let exitUnsub: UnlistenFn | null = null;
      let ackTimer: ReturnType<typeof setInterval> | null = null;
      let observer: ResizeObserver | null = null;
      let blockNativePaste: ((e: Event) => void) | null = null;
      // Layer 2 bootstrap: idle-debounced grid re-paint. The live listener
      // resets this timer on every received byte; when the stream goes
      // quiet for IDLE_REPAINT_MS we re-fetch the Rust grid and overlay
      // it via paintGrid so xterm and the grid stay in sync.
      let idleTimer: ReturnType<typeof setTimeout> | null = null;
      // Bytes received before the backend finishes async init. Tauri does
      // NOT queue events before listen() resolves, and createTerminalBackend
      // (WASM load + open + key handlers + UI Bridge registration) takes
      // ~200ms; without this buffer we'd silently drop everything Claude
      // emits in that window and xterm would freeze on whatever the grid
      // contained at bootstrap snapshot time.
      let pendingBytes: Uint8Array[] = [];
      let backendReady = false;
      // Assigned once the backend exists (see Bug A note above). Until then
      // the listener buffers bytes; we can't schedule a repaint that needs
      // the backend.
      let scheduleIdleRepaint: (() => void) | null = null;

      // Attach the live `terminal-output` listener IMMEDIATELY — before any
      // awaits — so we don't lose bytes during the backend's async init.
      // Bytes arriving before backendReady are buffered into pendingBytes
      // and drained once the backend is ready.
      (async () => {
        const unsub = await listen<TerminalOutputEvent>("terminal-output", (event) => {
          if (event.payload.terminalId !== terminalId) return;
          const raw = atob(event.payload.data);
          const bytes = new Uint8Array(raw.length);
          for (let i = 0; i < raw.length; i++) {
            bytes[i] = raw.charCodeAt(i);
          }

          if (!backendReady) {
            // Backend not yet created — buffer the bytes; they'll be drained
            // (and the idle timer primed) once the backend init completes.
            pendingBytes.push(bytes);
            return;
          }

          const b = backendRef.current;
          if (!b) return;
          try {
            b.write(bytes);
          } catch (e) {
            console.error(`[Terminal ${terminalId}] write error:`, e);
          }
          if (onOutputRef.current) {
            try {
              const text = outputDecoderRef.current.decode(bytes, { stream: true });
              onOutputRef.current(text);
            } catch {
              /* ignore decode errors */
            }
          }
          bytesReceivedRef.current += bytes.length;
          scheduleIdleRepaint?.();
        });
        if (disposed) {
          unsub();
          return;
        }
        outputUnsub = unsub;
      })();

      // Async init because backend creation may require WASM loading
      (async () => {
        const backend = await createTerminalBackend(backendType, TERMINAL_OPTIONS);
        if (disposed) {
          backend.dispose();
          return;
        }

        backendRef.current = backend;

        // Register file path link provider (backend-agnostic)
        disposables.push(
          backend.registerLinkProvider(
            new FilePathLinkProvider((line) => backend.getBufferLine(line)),
          ),
        );

        // Open terminal in container
        backend.open(container);

        // Style viewport scrollbar to match the runner's dark theme
        const viewport = backend.getViewportElement();
        if (viewport) {
          viewport.classList.add("scrollbar-dark");
        }

        // Initial fit after layout settles
        requestAnimationFrame(() => fitTerminal());

        // Track selection changes for parent components
        disposables.push(
          backend.onSelectionChange(() => {
            onSelectionChangeRef.current?.(backend.hasSelection());
          }),
        );

        // Handle Ctrl+C copy (when text is selected) and Ctrl+V paste.
        // Tauri's webview doesn't fire the browser clipboard events that xterm.js
        // relies on, so we intercept the keys and use the clipboard API manually.
        backend.attachCustomKeyEventHandler((event) => {
          // Ctrl+C: copy selected text, or pass through as SIGINT when nothing selected
          if (event.type === "keydown" && event.key === "c" && event.ctrlKey && !event.shiftKey) {
            if (backend.hasSelection()) {
              navigator.clipboard.writeText(backend.getSelection()).catch(() => {});
              return false; // prevent terminal from sending SIGINT
            }
            // No selection → let Ctrl+C pass through as SIGINT
          }

          if (
            event.type === "keydown" &&
            event.key === "v" &&
            (event.ctrlKey || event.metaKey) &&
            !event.shiftKey
          ) {
            navigator.clipboard
              .readText()
              .then((text) => {
                if (text) {
                  // Write directly to PTY instead of paste to avoid double
                  // paste when WebView2 also fires a native paste event.
                  const bytes = encoder.encode(text);
                  invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(
                    () => {},
                  );
                }
              })
              .catch(() => {});
            return false; // prevent terminal default handling
          }
          return true;
        });

        // Block native paste events from reaching the terminal — we handle paste
        // manually in the custom key handler above.  Without this, WebView2
        // fires a native "paste" event that the terminal processes via onData(),
        // causing the pasted text to be written to the PTY a second time.
        const inputEl = backend.getInputElement();
        blockNativePaste = (e: Event) => {
          e.preventDefault();
          e.stopPropagation();
        };
        inputEl?.addEventListener("paste", blockNativePaste, true);

        // Register terminal input with UI Bridge for external automation.
        const bridgeRegistry = uiBridge?.registry;
        if (bridgeRegistry && inputEl) {
          const termId = terminalId; // capture for closures
          bridgeRegistry.registerElement(`terminal-input-${termId}`, inputEl, {
            type: "textarea",
            label: `Terminal input (${termId.slice(0, 8)})`,
            actions: ["focus", "blur"],
            customActions: {
              sendKeys: {
                id: "sendKeys",
                description: "Send key sequences to the terminal",
                handler: (params?: unknown) => {
                  const { keys } = (params || {}) as { keys?: string };
                  if (!keys) return;
                  const bytes = encoder.encode(keys);
                  invoke("terminal_write", {
                    terminalId: termId,
                    data: uint8ToBase64(bytes),
                  }).catch(() => {});
                },
              },
              writeToTerminal: {
                id: "writeToTerminal",
                description: "Write text directly to the PTY (no keyboard events)",
                handler: (params?: unknown) => {
                  const { text } = (params || {}) as { text?: string };
                  if (!text) return;
                  const bytes = encoder.encode(text);
                  invoke("terminal_write", {
                    terminalId: termId,
                    data: uint8ToBase64(bytes),
                  }).catch(() => {});
                },
              },
              paste: {
                id: "paste",
                description: "Read clipboard and write to PTY (same as Ctrl+V)",
                handler: async () => {
                  const text = await navigator.clipboard.readText().catch(() => "");
                  if (text) {
                    const bytes = encoder.encode(text);
                    invoke("terminal_write", {
                      terminalId: termId,
                      data: uint8ToBase64(bytes),
                    }).catch(() => {});
                  }
                },
              },
              getScrollback: {
                id: "getScrollback",
                description: "Read the terminal scrollback buffer as plain text",
                handler: (params?: unknown) => {
                  const { maxLines = 500 } = (params || {}) as { maxLines?: number };
                  const b = backendRef.current;
                  if (!b) return "";
                  const totalLines = b.getBufferLength();
                  const startLine = Math.max(0, totalLines - maxLines);
                  const lines: string[] = [];
                  for (let i = startLine; i < totalLines; i++) {
                    const line = b.getBufferLine(i);
                    if (line) lines.push(line);
                  }
                  return lines.join("\n");
                },
              },
            },
          });
        }

        // Forward user input to PTY + track first input line for auto-naming
        disposables.push(
          backend.onData((data) => {
            // Track first input line for auto-naming
            if (!firstInputReportedRef.current) {
              for (const ch of data) {
                if (ch === "\r" || ch === "\n") {
                  const line = inputAccumulatorRef.current.trim();
                  if (line.length > 0) {
                    firstInputReportedRef.current = true;
                    onFirstInputRef.current?.(line);
                  }
                  inputAccumulatorRef.current = "";
                  break;
                } else if (ch.charCodeAt(0) >= 32) {
                  // Only accumulate printable characters
                  inputAccumulatorRef.current += ch;
                }
              }
            }

            const bytes = encoder.encode(data);
            invoke("terminal_write", {
              terminalId,
              data: uint8ToBase64(bytes),
            }).catch(() => {});
          }),
        );

        // Forward binary data (e.g. paste with special chars)
        disposables.push(
          backend.onBinary((data) => {
            const bytes = new Uint8Array(data.length);
            for (let i = 0; i < data.length; i++) {
              bytes[i] = data.charCodeAt(i);
            }
            invoke("terminal_write", {
              terminalId,
              data: uint8ToBase64(bytes),
            }).catch(() => {});
          }),
        );

        // Layer 2 bootstrap (live-first):
        //   1. The live `terminal-output` listener was attached at the very
        //      top of this effect, BEFORE createTerminalBackend. Bytes that
        //      arrived during async init were buffered into pendingBytes —
        //      drain them here now that the backend exists.
        //   2. Every received byte resets a 200ms idle timer (defined
        //      below). On idle we re-fetch the grid and paint — Claude's
        //      TUI repaints always end with [?2026l + silence, so paint-
        //      on-idle reconciles divergence accumulated during the burst.
        //   3. After waiting for the container to have non-zero dims, fit
        //      + terminal_resize + brief 50ms wait for SIGWINCH, then
        //      fetch the GridSnapshot and paintGrid. paintGrid uses
        //      absolute CUP per row inside a DECAWM-off bracket — safe to
        //      write concurrently with live bytes.
        for (const buffered of pendingBytes) {
          try {
            backend.write(buffered);
          } catch (e) {
            console.error(`[Terminal ${terminalId}] drain write error:`, e);
          }
          bytesReceivedRef.current += buffered.length;
        }
        pendingBytes = [];
        backendReady = true;

        // Define the idle repaint scheduler now that the backend exists.
        // The early-attached listener has been writing into pendingBytes
        // and reading scheduleIdleRepaint via closure — we wire the
        // implementation here.
        const IDLE_REPAINT_MS = 200;
        scheduleIdleRepaint = () => {
          if (idleTimer) clearTimeout(idleTimer);
          idleTimer = setTimeout(async () => {
            if (disposed) return;
            try {
              const r = await invoke<{ success: boolean; data: GridSnapshot | null }>(
                "terminal_get_grid",
                { terminalId },
              );
              const b = backendRef.current;
              if (r.success && r.data && b && !disposed) {
                paintGrid(b, r.data);
              }
            } catch {
              /* idle repaint best-effort; ignore failures */
            }
          }, IDLE_REPAINT_MS);
        };

        // Wait for the container to have non-zero dimensions before
        // fitting. On fast initial mount the layout is already done; on
        // slow mount (off-screen tab, second pane in a not-yet-laid-out
        // grid) we need to wait. Without this guard, fitAddon computes
        // ~10x5 from a 0x0 box and the subsequent terminal_resize
        // SIGWINCHes Claude into a 10x5 redraw — the Rust grid is then
        // destructively resized and stays at 10x5 forever.
        const waitForLayout = async (): Promise<boolean> => {
          const c = containerRef.current;
          if (!c) return false;
          if (c.clientWidth > 0 && c.clientHeight > 0) return true;
          return new Promise<boolean>((resolve) => {
            const ro = new ResizeObserver(() => {
              if (disposed) {
                ro.disconnect();
                resolve(false);
                return;
              }
              const cc = containerRef.current;
              if (cc && cc.clientWidth > 0 && cc.clientHeight > 0) {
                ro.disconnect();
                resolve(true);
              }
            });
            ro.observe(c);
            // Safety timeout — give up after 1s and try anyway.
            setTimeout(() => {
              ro.disconnect();
              resolve(true);
            }, 1000);
          });
        };

        // Wait one paint frame so backend.cols/rows are valid after open().
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        if (disposed) return;
        const laidOut = await waitForLayout();
        if (disposed) return;

        try {
          if (laidOut) {
            backend.fit();
            // Sanity check: refuse to ship a tiny resize that would wipe
            // the grid. Claude's TUI requires a non-trivial viewport;
            // anything below this is almost certainly a measurement error.
            if (backend.cols >= 20 && backend.rows >= 5) {
              await invoke("terminal_resize", {
                terminalId,
                cols: backend.cols,
                rows: backend.rows,
              });
              // Brief wait for SIGWINCH to propagate to the child process.
              await new Promise<void>((resolve) => setTimeout(resolve, 50));
            } else {
              console.warn(
                `[Terminal ${terminalId}] skipping bootstrap resize: too small (${backend.cols}x${backend.rows})`,
              );
            }
          }
          if (disposed) return;
          const result = await invoke<{ success: boolean; data: GridSnapshot | null }>(
            "terminal_get_grid",
            { terminalId },
          );
          if (result.success && result.data && !disposed) {
            paintGrid(backend, result.data);
          }
        } catch (err) {
          console.warn(`[Terminal ${terminalId}] grid bootstrap failed:`, err);
        }
        if (disposed) return;

        // Prime the idle re-paint heartbeat once after bootstrap, so the
        // timer ticks even when zero live events arrive (e.g. Claude
        // already finished its initial render before the listener
        // attached and no further bytes are coming for a while).
        scheduleIdleRepaint?.();

        // Reconnect callback fires only on actual reconnects (not first
        // mount), preserving the prior semantic.
        if (isReconnecting) {
          onReconnectedRef.current?.();
        }

        // Listen for process exit
        exitUnsub = await listen<TerminalExitEvent>("terminal-exit", (event) => {
          if (event.payload.terminalId !== terminalId) return;
          backend.write(
            `\r\n\x1b[90m[Process exited with code ${event.payload.exitCode ?? "unknown"}]\x1b[0m\r\n`,
          );
          onExitRef.current?.(event.payload.exitCode);
        });
        if (disposed) return;

        // Resize observer
        observer = new ResizeObserver(() => fitTerminal());
        observer.observe(container);

        // Flow control: periodically ack received bytes
        ackTimer = setInterval(() => {
          const received = bytesReceivedRef.current;
          const delta = received - lastAckedRef.current;
          if (delta > 0) {
            lastAckedRef.current = received;
            invoke("terminal_ack", {
              terminalId,
              bytesAcked: delta,
            }).catch(() => {});
          }
        }, 250);

        // OSC 633 shell integration handler
        disposables.push(
          backend.onOsc633((data) => {
            const cb = onShellIntegrationRef.current;
            if (!cb) return;
            if (data === "A") cb({ type: "prompt_start" });
            else if (data === "B") cb({ type: "command_ready" });
            else if (data === "C") cb({ type: "command_execute" });
            else if (data.startsWith("D")) {
              const code = parseInt(data.slice(2), 10);
              cb({ type: "command_done", exitCode: isNaN(code) ? 0 : code });
            } else if (data.startsWith("E;")) {
              cb({ type: "command_line", command: data.slice(2).replace(/\\x3b/g, ";") });
            } else if (data.startsWith("P;Cwd=")) {
              cb({ type: "cwd", path: data.slice(6) });
            }
          }),
        );
      })();

      // Cleanup
      return () => {
        disposed = true;
        if (ackTimer) clearInterval(ackTimer);
        if (idleTimer) clearTimeout(idleTimer);
        if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
        observer?.disconnect();
        for (const d of disposables) d.dispose();
        const inputEl = backendRef.current?.getInputElement();
        if (inputEl && blockNativePaste) {
          inputEl.removeEventListener("paste", blockNativePaste, true);
        }
        uiBridge?.registry?.unregisterElement(`terminal-input-${terminalId}`);
        outputUnsub?.();
        exitUnsub?.();
        try {
          backendRef.current?.dispose();
        } catch {
          // xterm.js WebGL renderer may throw during disposal when its internal
          // options are already cleaned up (known issue with onShowLinkUnderline).
        }
        backendRef.current = null;
      };
      // eslint-disable-next-line react-hooks/exhaustive-deps -- uiBridge is stable context
    }, [terminalId, backendType, fitTerminal]);

    // Re-fit and focus when visibility changes.
    // Track previous visibility with a ref and trigger fit/focus on transition,
    // avoiding a useEffect that merely simulates an event handler.
    const prevVisibleRef = useRef(visible);
    if (visible && !prevVisibleRef.current) {
      // Schedule after layout so the container has actual dimensions
      setTimeout(() => {
        fitTerminal();
        backendRef.current?.focus();
      }, 16);
    }
    prevVisibleRef.current = visible;

    return (
      <div
        ref={containerRef}
        className={`h-full w-full ${visible ? "" : "hidden"}`}
        style={{ padding: "4px 0 0 4px" }}
      />
    );
  },
);
