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
}

interface TerminalInstanceProps {
  terminalId: string;
  visible: boolean;
  onExit?: (exitCode: number | null) => void;
  onSelectionChange?: (hasSelection: boolean) => void;
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
  function TerminalInstanceInner({ terminalId, visible, onExit, onSelectionChange }, ref) {
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

    // Expose selection API to parent components
    useImperativeHandle(ref, () => ({
      getSelection: () => termRef.current?.getSelection() ?? "",
      hasSelection: () => termRef.current?.hasSelection() ?? false,
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

      // Try WebGL renderer, fall back to Canvas, then DOM
      try {
        term.loadAddon(new WebglAddon());
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

      // Forward user input to PTY
      const inputDisposable = term.onData((data) => {
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

      // Listen for PTY output
      let outputUnsub: UnlistenFn | null = null;
      listen<TerminalOutputEvent>("terminal-output", (event) => {
        if (event.payload.terminal_id !== terminalId) return;
        const raw = atob(event.payload.data);
        const bytes = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) {
          bytes[i] = raw.charCodeAt(i);
        }
        term.write(bytes);
        bytesReceivedRef.current += bytes.length;
      }).then((fn) => {
        outputUnsub = fn;
      });

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

      // Cleanup
      return () => {
        clearInterval(ackTimer);
        if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
        observer.disconnect();
        selectionDisposable.dispose();
        inputDisposable.dispose();
        binaryDisposable.dispose();
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
