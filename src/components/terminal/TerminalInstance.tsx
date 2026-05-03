import { useEffect, useRef, useCallback, forwardRef, useImperativeHandle } from "react";
import { FilePathLinkProvider } from "./FilePathLinkProvider";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useUIBridgeOptional } from "@qontinui/ui-bridge";
import { createTerminalBackend } from "./backends";
import type { BackendType, ITerminalBackend } from "./backends";
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

interface ScrollbackBufferResponse {
  data: string; // base64
  start_offset: number;
  total_bytes_produced: number;
}

interface TerminalOutputEvent {
  terminal_id: string;
  data: string; // base64
}

interface TerminalExitEvent {
  terminal_id: string;
  exit_code: number | null;
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
    // Mount/reconnect gate: queues live `terminal-output` events until the
    // PTY scrollback has been replayed into xterm. Always created — on first
    // mount this catches the init burst (alt-screen + welcome banner) that
    // the PTY emits before the frontend listener is wired up; on reconnect
    // it replays the entire backlog. `onReconnected` still only fires when
    // the prop indicated a real reconnect (see usage below).
    const reconnectGateRef = useRef<{
      open: boolean;
      queue: Uint8Array[];
    } | null>({ open: false, queue: [] });

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

        // Listen for PTY output — gate during reconnection
        const gate = reconnectGateRef.current;
        const unsub = await listen<TerminalOutputEvent>("terminal-output", (event) => {
          if (event.payload.terminal_id !== terminalId) return;
          const raw = atob(event.payload.data);
          const bytes = new Uint8Array(raw.length);
          for (let i = 0; i < raw.length; i++) {
            bytes[i] = raw.charCodeAt(i);
          }

          // If we're reconnecting and the gate is still closed, queue the event
          if (gate && !gate.open) {
            gate.queue.push(bytes);
            return;
          }

          try {
            backend.write(bytes);
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
        });
        outputUnsub = unsub;
        if (disposed) return;

        // Bootstrap: replay PTY scrollback into the freshly-mounted xterm.
        // Always runs (initial mount + reconnect) so the init burst that
        // arrived before `listen()` was wired up is replayed.
        //
        // Two correctness rules learned the hard way:
        //   1. Wait for the canvas to actually composite before writing.
        //      `backend.open(container)` is synchronous but the WebGL/canvas
        //      renderer doesn't paint until the next paint frame. Writing
        //      alt-screen sequences (`\x1b[?1049h` …) into an un-painted
        //      canvas drops them — the bytes process internally but the
        //      rendered output is the pre-alt-screen state. Two rAFs is the
        //      tiniest reliable "after first paint" signal in browsers.
        //   2. Chunk the replay across paint frames. xterm-canvas processes
        //      alt-screen state-changes incrementally between frames; a
        //      single big synchronous `write()` of the full backlog can
        //      collapse them to a final state with the visible canvas
        //      stuck on the start state.
        if (gate) {
          // Wait two animation frames so backend.open()'s canvas is composited.
          await new Promise<void>((resolve) => {
            requestAnimationFrame(() => {
              requestAnimationFrame(() => resolve());
            });
          });
          if (disposed) return;

          try {
            const result = await invoke<{
              success: boolean;
              data: ScrollbackBufferResponse | null;
            }>("terminal_get_buffer", { terminalId });
            if (result.success && result.data) {
              const bufData = result.data as unknown as ScrollbackBufferResponse;
              const raw = atob(bufData.data);
              const bytes = new Uint8Array(raw.length);
              for (let i = 0; i < raw.length; i++) {
                bytes[i] = raw.charCodeAt(i);
              }
              if (bytes.length > 0) {
                // Chunk the replay across rAFs so alt-screen state-changes
                // get a paint cycle each. 8 KB is roughly one Claude Code
                // banner draw — small enough that any single chunk is one
                // logical screen update; large enough to not drown rAFs.
                const CHUNK = 8 * 1024;
                for (let off = 0; off < bytes.length; off += CHUNK) {
                  if (disposed) return;
                  const slice = bytes.subarray(off, Math.min(off + CHUNK, bytes.length));
                  try {
                    backend.write(slice);
                  } catch (e) {
                    console.error(`[Terminal ${terminalId}] scrollback write error:`, e);
                  }
                  bytesReceivedRef.current += slice.length;
                  if (off + CHUNK < bytes.length) {
                    await new Promise<void>((resolve) =>
                      requestAnimationFrame(() => resolve()),
                    );
                  }
                }
              }
            }
          } catch (err) {
            console.warn(`[Terminal ${terminalId}] Failed to fetch scrollback:`, err);
          }
          if (disposed) return;

          // Open the gate and flush queued live events
          gate.open = true;
          for (const queued of gate.queue) {
            try {
              backend.write(queued);
            } catch (e) {
              console.error(`[Terminal ${terminalId}] queued write error:`, e);
            }
            bytesReceivedRef.current += queued.length;
          }
          gate.queue.length = 0;

          // Reconnect callback only fires on actual reconnects, not on the
          // first-mount scrollback bootstrap that uses the same gate path.
          if (isReconnecting) {
            onReconnectedRef.current?.();
          }
        }
        if (disposed) return;

        // Listen for process exit
        exitUnsub = await listen<TerminalExitEvent>("terminal-exit", (event) => {
          if (event.payload.terminal_id !== terminalId) return;
          backend.write(
            `\r\n\x1b[90m[Process exited with code ${event.payload.exit_code ?? "unknown"}]\x1b[0m\r\n`,
          );
          onExitRef.current?.(event.payload.exit_code);
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
