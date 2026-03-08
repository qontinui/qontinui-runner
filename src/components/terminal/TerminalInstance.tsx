import { useEffect, useRef, useCallback, forwardRef, useImperativeHandle } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { CanvasAddon } from "@xterm/addon-canvas";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";

export interface TerminalInstanceHandle {
  getSelection: () => string;
  hasSelection: () => boolean;
  writeToTerminal: (text: string) => void;
  /**
   * Write raw output data directly to the xterm display without sending to the PTY.
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

export const TerminalInstance = forwardRef<TerminalInstanceHandle, TerminalInstanceProps>(
  function TerminalInstanceInner(
    {
      terminalId,
      visible,
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
    const containerRef = useRef<HTMLDivElement>(null);
    const termRef = useRef<Terminal | null>(null);
    const fitAddonRef = useRef<FitAddon | null>(null);
    const bytesReceivedRef = useRef(0);
    const lastAckedRef = useRef(0);
    // Stable ref for onExit to avoid effect re-runs
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
    // Gate for reconnection: queues live events until scrollback is replayed
    const reconnectGateRef = useRef<{
      open: boolean;
      queue: Uint8Array[];
    } | null>(isReconnecting ? { open: false, queue: [] } : null);

    // Expose selection, write, and scrollback API to parent components
    useImperativeHandle(ref, () => ({
      getSelection: () => termRef.current?.getSelection() ?? "",
      hasSelection: () => termRef.current?.hasSelection() ?? false,
      writeToTerminal: (text: string) => {
        const bytes = encoder.encode(text);
        invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(() => {});
      },
      writeToDisplay: (data: string) => {
        termRef.current?.write(data);
      },
      getScrollback: (maxLines = 500) => {
        const term = termRef.current;
        if (!term) return "";
        const buffer = term.buffer.active;
        const totalLines = buffer.length;
        const startLine = Math.max(0, totalLines - maxLines);
        const lines: string[] = [];
        for (let i = startLine; i < totalLines; i++) {
          const line = buffer.getLine(i);
          if (line) {
            lines.push(line.translateToString(true));
          }
        }
        return lines.join("\n");
      },
      scrollToBottom: () => {
        termRef.current?.scrollToBottom();
      },
    }));

    // Debounced fit — coalesce rapid resize events
    const fitTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const fitTerminal = useCallback(() => {
      if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
      fitTimerRef.current = setTimeout(() => {
        const fitAddon = fitAddonRef.current;
        const term = termRef.current;
        if (!fitAddon || !term || !containerRef.current) return;
        try {
          fitAddon.fit();
          invoke("terminal_resize", {
            terminalId,
            cols: term.cols,
            rows: term.rows,
          }).catch(() => {});
        } catch {
          // Container may not be visible yet
        }
      }, 50);
    }, [terminalId]);

    useEffect(() => {
      if (!containerRef.current) return;

      const term = new Terminal({
        cursorBlink: true,
        cursorStyle: "block",
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
        allowProposedApi: true,
      });

      termRef.current = term;

      // Fit addon (must be loaded before open)
      const fitAddon = new FitAddon();
      fitAddonRef.current = fitAddon;
      term.loadAddon(fitAddon);

      // Web links addon — open URLs on click
      term.loadAddon(new WebLinksAddon());

      // Open terminal in container
      term.open(containerRef.current);

      // Style xterm's scrollbar to match the runner's dark theme
      const viewport = containerRef.current.querySelector(".xterm-viewport") as HTMLElement | null;
      if (viewport) {
        viewport.classList.add("scrollbar-dark");
      }

      // Try WebGL renderer, fall back to Canvas, then DOM.
      // Also handle WebGL context loss (GPU crash) by falling back at runtime.
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => {
          console.warn(`[Terminal ${terminalId}] WebGL context lost, falling back to Canvas`);
          try {
            webgl.dispose();
          } catch {
            // ignore dispose errors
          }
          try {
            term.loadAddon(new CanvasAddon());
          } catch {
            // Fall through to DOM renderer
          }
        });
        term.loadAddon(webgl);
      } catch {
        try {
          term.loadAddon(new CanvasAddon());
        } catch {
          // Default DOM renderer
        }
      }

      // Initial fit after layout settles
      requestAnimationFrame(() => fitTerminal());

      // Track selection changes for parent components
      const selectionDisposable = term.onSelectionChange(() => {
        onSelectionChangeRef.current?.(term.hasSelection());
      });

      // Handle Ctrl+C copy (when text is selected) and Ctrl+V paste.
      // Tauri's webview doesn't fire the browser clipboard events that xterm.js
      // relies on, so we intercept the keys and use the clipboard API manually.
      term.attachCustomKeyEventHandler((event) => {
        // Ctrl+C: copy selected text, or pass through as SIGINT when nothing selected
        if (event.type === "keydown" && event.key === "c" && event.ctrlKey && !event.shiftKey) {
          if (term.hasSelection()) {
            navigator.clipboard.writeText(term.getSelection()).catch(() => {});
            return false; // prevent xterm from sending SIGINT
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
                // Write directly to PTY instead of term.paste() to avoid double
                // paste when WebView2 also fires a native paste event.
                const bytes = encoder.encode(text);
                invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(
                  () => {},
                );
              }
            })
            .catch(() => {});
          return false; // prevent xterm default handling
        }
        return true;
      });

      // Forward user input to PTY + track first input line for auto-naming
      const inputDisposable = term.onData((data) => {
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
      });

      // Forward binary data (e.g. paste with special chars)
      const binaryDisposable = term.onBinary((data) => {
        const bytes = new Uint8Array(data.length);
        for (let i = 0; i < data.length; i++) {
          bytes[i] = data.charCodeAt(i);
        }
        invoke("terminal_write", {
          terminalId,
          data: uint8ToBase64(bytes),
        }).catch(() => {});
      });

      // Listen for PTY output — gate during reconnection
      const gate = reconnectGateRef.current;
      let outputUnsub: UnlistenFn | null = null;
      listen<TerminalOutputEvent>("terminal-output", (event) => {
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
          term.write(bytes);
        } catch (e) {
          console.error(`[Terminal ${terminalId}] term.write error:`, e);
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
      }).then((fn) => {
        outputUnsub = fn;
      });

      // If reconnecting: fetch scrollback buffer, write it, then open the gate
      if (gate) {
        (async () => {
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
                try {
                  term.write(bytes);
                } catch (e) {
                  console.error(`[Terminal ${terminalId}] scrollback write error:`, e);
                }
                bytesReceivedRef.current += bytes.length;
              }
            }
          } catch (err) {
            console.warn(`[Terminal ${terminalId}] Failed to fetch scrollback:`, err);
          }

          // Open the gate and flush queued live events
          gate.open = true;
          for (const queued of gate.queue) {
            try {
              term.write(queued);
            } catch (e) {
              console.error(`[Terminal ${terminalId}] queued write error:`, e);
            }
            bytesReceivedRef.current += queued.length;
          }
          gate.queue.length = 0;

          onReconnectedRef.current?.();
        })();
      }

      // Listen for process exit
      let exitUnsub: UnlistenFn | null = null;
      listen<TerminalExitEvent>("terminal-exit", (event) => {
        if (event.payload.terminal_id !== terminalId) return;
        term.write(
          `\r\n\x1b[90m[Process exited with code ${event.payload.exit_code ?? "unknown"}]\x1b[0m\r\n`,
        );
        onExitRef.current?.(event.payload.exit_code);
      }).then((fn) => {
        exitUnsub = fn;
      });

      // Resize observer
      const observer = new ResizeObserver(() => fitTerminal());
      observer.observe(containerRef.current);

      // Flow control: periodically ack received bytes
      const ackTimer = setInterval(() => {
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
      const oscDisposable = term.parser.registerOscHandler(633, (data) => {
        const cb = onShellIntegrationRef.current;
        if (!cb) return false;
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
        // Return false: don't consume — let xterm render normally (sequences are invisible)
        return false;
      });

      // Cleanup
      return () => {
        clearInterval(ackTimer);
        if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
        observer.disconnect();
        selectionDisposable.dispose();
        inputDisposable.dispose();
        binaryDisposable.dispose();
        oscDisposable.dispose();
        outputUnsub?.();
        exitUnsub?.();
        term.dispose();
        termRef.current = null;
        fitAddonRef.current = null;
      };
    }, [terminalId, fitTerminal]);

    // Re-fit and focus when visibility changes
    useEffect(() => {
      if (visible) {
        // Delay slightly so the container is actually laid out
        const id = setTimeout(() => {
          fitTerminal();
          termRef.current?.focus();
        }, 16);
        return () => clearTimeout(id);
      }
    }, [visible, fitTerminal]);

    return (
      <div
        ref={containerRef}
        className={`h-full w-full ${visible ? "" : "hidden"}`}
        style={{ padding: "4px 0 0 4px" }}
      />
    );
  },
);
