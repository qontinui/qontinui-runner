import { useEffect, useRef, useState, useCallback, forwardRef, useImperativeHandle } from "react";
import { FilePathLinkProvider } from "./FilePathLinkProvider";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useUIBridgeOptional } from "@qontinui/ui-bridge";
import type { TerminalOutputEvent, TerminalExitEvent } from "@qontinui/shared-types/tauri-events";
import { createTerminalBackend } from "./backends";
import type { BackendType, ITerminalBackend, TerminalSearchResults } from "./backends";
import { TerminalFindBar } from "./TerminalFindBar";
import { paintGrid, type GridSnapshot } from "./paintGrid";
import { instanceStorage } from "@/lib/instance-storage";
import { consumeInputChunk } from "./consumeInputChunk";
import { wheelToLineDelta, DEFAULT_CELL_HEIGHT_PX } from "./wheelScroll";
import { matchScrollShortcut } from "./scrollKeys";

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
  /**
   * Called for EVERY non-empty newline-terminated input line typed into
   * the terminal. Unlike `onFirstInput`, this is ungated and fires on
   * every subsequent line — the consumer (e.g. `useMidSessionProbe`)
   * applies its own debouncing / rate-limiting / gating.
   */
  onUserInputLine?: (input: string) => void;
  /** Called when the shell emits an OSC 633 shell integration event. */
  onShellIntegration?: (event: ShellIntegrationEvent) => void;
  /** Called with decoded text whenever PTY output is received. */
  onOutput?: (text: string) => void;
  /**
   * Called with the latest OSC 0 / OSC 2 title observed by the Rust grid.
   * Fires after each `paintGrid` whenever the snapshot's `title` field
   * differs from the previous value. Layer 4 polish wiring per
   * `plans/terminal-grid-bootstrap-redesign.md`.
   */
  onTitleChange?: (title: string) => void;
}

// `TerminalOutputEvent` and `TerminalExitEvent` are imported from
// `@qontinui/shared-types/tauri-events` — generated from the canonical Rust
// structs in `qontinui-schemas/rust/src/terminal.rs`. Future serde renames
// will break this file at compile time instead of silently dropping events.

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
      onUserInputLine,
      onShellIntegration,
      onOutput,
      onTitleChange,
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
    const onUserInputLineRef = useRef(onUserInputLine);
    onUserInputLineRef.current = onUserInputLine;
    const onReconnectedRef = useRef(onReconnected);
    onReconnectedRef.current = onReconnected;
    const onShellIntegrationRef = useRef(onShellIntegration);
    onShellIntegrationRef.current = onShellIntegration;
    const onOutputRef = useRef(onOutput);
    onOutputRef.current = onOutput;
    const onTitleChangeRef = useRef(onTitleChange);
    onTitleChangeRef.current = onTitleChange;
    /**
     * Most-recently reported title — guards `onTitleChange` against firing on
     * every paintGrid (200ms idle cadence) when the title is unchanged.
     */
    const lastTitleRef = useRef<string | null>(null);
    const outputDecoderRef = useRef(new TextDecoder());
    const firstInputReportedRef = useRef(false);
    const inputAccumulatorRef = useRef("");
    // Fractional carry for the Shift+wheel local-scroll override — sub-line
    // pixel deltas (trackpads) accumulate here so slow scrolling advances
    // smoothly instead of snapping a whole line per tick. See `wheelScroll.ts`.
    const wheelScrollAccumRef = useRef(0);

    // ── Find-in-terminal (VS Code Ctrl+F parity) ─────────────────────────
    // State drives the TerminalFindBar overlay; the refs mirror it so the
    // backend key handler (created once in the init effect) and runFind can
    // read fresh values without re-running the effect.
    const [findOpen, setFindOpen] = useState(false);
    // Bumped on every Ctrl+F so a re-press while already open remounts the
    // bar (key prop), refocusing + selecting the query like VS Code.
    const [findFocusSeq, setFindFocusSeq] = useState(0);
    const [findQuery, setFindQuery] = useState("");
    const [findOpts, setFindOpts] = useState({
      caseSensitive: false,
      wholeWord: false,
      regex: false,
    });
    const [findResults, setFindResults] = useState<TerminalSearchResults>({
      resultIndex: -1,
      resultCount: 0,
    });
    const findQueryRef = useRef(findQuery);
    findQueryRef.current = findQuery;
    const findOptsRef = useRef(findOpts);
    findOptsRef.current = findOpts;

    /**
     * Run a search pass against the backend. `incremental` keeps the active
     * match anchored while the operator types (VS Code behavior); plain
     * next/prev navigation passes false to advance. Invalid regexes are
     * swallowed — the addon throws on bad patterns mid-typing (e.g. "[").
     */
    const runFind = useCallback((direction: "next" | "prev", incremental = false) => {
      const b = backendRef.current;
      if (!b) return;
      const q = findQueryRef.current;
      if (!q) {
        b.clearSearch();
        setFindResults({ resultIndex: -1, resultCount: 0 });
        return;
      }
      const opts = { ...findOptsRef.current, incremental };
      try {
        if (direction === "next") b.findNext(q, opts);
        else b.findPrevious(q, opts);
      } catch {
        // Invalid regex while typing — leave previous results in place.
      }
    }, []);

    const handleFindQueryChange = useCallback(
      (q: string) => {
        setFindQuery(q);
        findQueryRef.current = q; // searchable immediately, before re-render
        runFind("next", true);
      },
      [runFind],
    );

    const toggleFindOpt = useCallback(
      (key: "caseSensitive" | "wholeWord" | "regex") => {
        const next = { ...findOptsRef.current, [key]: !findOptsRef.current[key] };
        findOptsRef.current = next;
        setFindOpts(next);
        runFind("next", true);
      },
      [runFind],
    );

    const closeFind = useCallback(() => {
      setFindOpen(false);
      setFindResults({ resultIndex: -1, resultCount: 0 });
      backendRef.current?.clearSearch();
      backendRef.current?.focus();
    }, []);

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
      let wheelScrollOverride: ((e: WheelEvent) => void) | null = null;
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

      /**
       * Emit `onTitleChange` if the snapshot's OSC 0/2 title is non-empty AND
       * differs from the last reported value. Called after every paintGrid
       * (initial bootstrap + every idle repaint) so the latest title from the
       * Rust grid flows up to the tab title without spamming the callback.
       */
      const reportTitleFromSnapshot = (title: string | undefined | null) => {
        if (!title) return;
        if (title === lastTitleRef.current) return;
        lastTitleRef.current = title;
        try {
          onTitleChangeRef.current?.(title);
        } catch (e) {
          console.warn(`[Terminal ${terminalId}] onTitleChange handler error:`, e);
        }
      };

      const buildOutputListener = () =>
        listen<TerminalOutputEvent>("terminal-output", (event) => {
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

      // Cold-mount path: attach the live `terminal-output` listener IMMEDIATELY
      // — before any awaits — so we don't lose bytes during the backend's
      // async init. Bytes arriving before backendReady are buffered into
      // pendingBytes and drained once the backend is ready.
      //
      // Reconnect path (Layer 4 polish): the PTY survived the page reload and
      // the Rust grid already holds the full state. Live bytes during the
      // catch-up window would race with paintGrid, so we DEFER the listener
      // attach until after the bootstrap paintGrid resolves. See
      // `plans/terminal-grid-bootstrap-redesign.md` Layer 4.
      if (!isReconnecting) {
        (async () => {
          const unsub = await buildOutputListener();
          if (disposed) {
            unsub();
            return;
          }
          outputUnsub = unsub;
        })();
      }

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

        // Shift+wheel → always scroll the local scrollback, even when the
        // foreground program has captured the wheel via a mouse-tracking mode
        // (DECSET 1000/1002/1003). xterm.js sets `handleMouseWheel: false` in
        // that state and forwards the wheel to the app, so a plain wheel can't
        // scroll the buffer; its `attachCustomWheelEventHandler` is also
        // bypassed once mouse mode is on. We intercept on the container in the
        // CAPTURE phase (before xterm's own bubble-phase listeners on the
        // viewport/screen) and, only when Shift is held, take the event over.
        // Un-modified wheel events are left untouched so the app keeps its
        // mouse interaction — matching the Konsole / GNOME Terminal / Windows
        // Terminal "Shift bypasses application mouse reporting" convention.
        wheelScrollOverride = (e: WheelEvent) => {
          if (!e.shiftKey) return;
          const b = backendRef.current;
          if (!b) return;
          // Take over: stop xterm (and the app) from also seeing this wheel.
          e.preventDefault();
          e.stopImmediatePropagation();
          wheelScrollAccumRef.current += wheelToLineDelta(e, b.rows, DEFAULT_CELL_HEIGHT_PX);
          const whole = Math.trunc(wheelScrollAccumRef.current);
          if (whole !== 0) {
            wheelScrollAccumRef.current -= whole;
            b.scrollLines(whole);
          }
        };
        container.addEventListener("wheel", wheelScrollOverride, {
          capture: true,
          passive: false,
        });

        // Initial fit after layout settles
        requestAnimationFrame(() => fitTerminal());

        // Track selection changes for parent components
        disposables.push(
          backend.onSelectionChange(() => {
            onSelectionChangeRef.current?.(backend.hasSelection());
          }),
        );

        // Find-in-terminal match counts → TerminalFindBar ("k of n").
        disposables.push(
          backend.onSearchResults((results) => {
            setFindResults(results);
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

          // VS Code-parity find: Ctrl+F opens the find bar; F3 / Shift+F3
          // jump to the next/previous match (opening the bar if needed).
          // Swallowed so the keys never reach the PTY — Ctrl+F would
          // otherwise be forward-char in readline/PSReadLine.
          if (
            event.type === "keydown" &&
            (event.ctrlKey || event.metaKey) &&
            !event.altKey &&
            !event.shiftKey &&
            event.key.toLowerCase() === "f"
          ) {
            event.preventDefault();
            setFindOpen(true);
            setFindFocusSeq((s) => s + 1);
            return false;
          }
          if (
            event.type === "keydown" &&
            event.key === "F3" &&
            !event.ctrlKey &&
            !event.altKey &&
            !event.metaKey
          ) {
            event.preventDefault();
            setFindOpen(true);
            if (findQueryRef.current) {
              runFind(event.shiftKey ? "prev" : "next");
            } else {
              setFindFocusSeq((s) => s + 1);
            }
            return false;
          }

          // VS Code-parity scrollback navigation: Shift+PageUp/PageDown,
          // Ctrl+Alt+PageUp/PageDown, Ctrl+Home/End, Ctrl+Up/Down (prev/next
          // command via OSC 633;A marks). Scroll the focused terminal locally
          // and swallow the key so it isn't forwarded to the PTY (xterm
          // doesn't preventDefault on a `false` return, so we do it here to
          // also stop any browser-default scroll/caret motion).
          if (event.type === "keydown") {
            const action = matchScrollShortcut(event);
            if (action) {
              event.preventDefault();
              switch (action.kind) {
                case "lines":
                  backend.scrollLines(action.amount);
                  break;
                case "pages":
                  backend.scrollPages(action.amount);
                  break;
                case "top":
                  backend.scrollToTop();
                  break;
                case "bottom":
                  backend.scrollToBottom();
                  break;
                case "prevCommand":
                  backend.scrollToPreviousCommand();
                  break;
                case "nextCommand":
                  backend.scrollToNextCommand();
                  break;
              }
              return false;
            }
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
        // + emit every non-empty line to `onUserInputLine` for mid-session
        // probes. The accumulator logic lives in the pure `consumeInputChunk`
        // helper in `./consumeInputChunk.ts` (a leaf module — kept separate
        // so vitest tests don't pull in xterm.js's canvas addon).
        disposables.push(
          backend.onData((data) => {
            const result = consumeInputChunk(
              data,
              inputAccumulatorRef.current,
              firstInputReportedRef.current,
            );
            inputAccumulatorRef.current = result.accum;
            // First-input gate: fires at most once per session.
            if (result.firstInputLineIfAny !== undefined) {
              firstInputReportedRef.current = true;
              onFirstInputRef.current?.(result.firstInputLineIfAny);
            }
            // Per-line callback: fires for EVERY non-empty newline-terminated
            // line, ungated. Consumer (e.g. useMidSessionProbe) applies its
            // own gates.
            const perLine = onUserInputLineRef.current;
            if (perLine) {
              for (const line of result.lines) {
                perLine(line);
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
                reportTitleFromSnapshot(r.data.title);
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
            reportTitleFromSnapshot(result.data.title);
          }
        } catch (err) {
          console.warn(`[Terminal ${terminalId}] grid bootstrap failed:`, err);
        }
        if (disposed) return;

        // Layer 4 polish: on the reconnect path the live `terminal-output`
        // listener was deferred so it wouldn't race with the bootstrap
        // paintGrid above. Attach it NOW that the grid has landed. The
        // pendingBytes drain below is a no-op since nothing buffered while
        // the listener wasn't attached, but the structure stays uniform.
        if (isReconnecting && !outputUnsub) {
          try {
            const unsub = await buildOutputListener();
            if (disposed) {
              unsub();
            } else {
              outputUnsub = unsub;
            }
          } catch (e) {
            console.warn(`[Terminal ${terminalId}] reconnect listener attach failed:`, e);
          }
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
          onExitRef.current?.(event.payload.exitCode ?? null);
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
        if (wheelScrollOverride) {
          container.removeEventListener("wheel", wheelScrollOverride, { capture: true });
        }
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
      // Wrapper exists so the find bar can overlay as a React-managed
      // sibling: xterm imperatively appends its own DOM into the inner
      // container div, and mixing React children with foreign nodes in the
      // same parent invites reconciliation surprises.
      <div className={`relative h-full w-full ${visible ? "" : "hidden"}`}>
        <div ref={containerRef} className="h-full w-full" style={{ padding: "4px 0 0 4px" }} />
        {findOpen && (
          <TerminalFindBar
            key={findFocusSeq}
            query={findQuery}
            onQueryChange={handleFindQueryChange}
            resultIndex={findResults.resultIndex}
            resultCount={findResults.resultCount}
            caseSensitive={findOpts.caseSensitive}
            wholeWord={findOpts.wholeWord}
            regex={findOpts.regex}
            onToggleCase={() => toggleFindOpt("caseSensitive")}
            onToggleWholeWord={() => toggleFindOpt("wholeWord")}
            onToggleRegex={() => toggleFindOpt("regex")}
            onNext={() => runFind("next")}
            onPrev={() => runFind("prev")}
            onClose={closeFind}
          />
        )}
      </div>
    );
  },
);
