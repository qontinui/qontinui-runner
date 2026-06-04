/**
 * Terminal backend abstraction layer.
 *
 * Decouples the runner's terminal UI from any specific terminal emulator library
 * (xterm.js, ghostty-web, restty, etc.) so backends can be swapped via a setting.
 */

// ---------------------------------------------------------------------------
// Disposable
// ---------------------------------------------------------------------------

export interface IDisposable {
  dispose(): void;
}

// ---------------------------------------------------------------------------
// Link provider (backend-agnostic version of xterm's ILinkProvider / ILink)
// ---------------------------------------------------------------------------

export interface ITerminalLink {
  range: {
    start: { x: number; y: number };
    end: { x: number; y: number };
  };
  text: string;
  decorations?: { pointerCursor?: boolean; underline?: boolean };
  activate: (event: MouseEvent, text: string) => void;
}

export interface ITerminalLinkProvider {
  provideLinks(lineNumber: number, callback: (links: ITerminalLink[] | undefined) => void): void;
}

// ---------------------------------------------------------------------------
// Terminal theme
// ---------------------------------------------------------------------------

export interface ITerminalTheme {
  background?: string;
  foreground?: string;
  cursor?: string;
  selectionBackground?: string;
  selectionForeground?: string;
  black?: string;
  red?: string;
  green?: string;
  yellow?: string;
  blue?: string;
  magenta?: string;
  cyan?: string;
  white?: string;
  brightBlack?: string;
  brightRed?: string;
  brightGreen?: string;
  brightYellow?: string;
  brightBlue?: string;
  brightMagenta?: string;
  brightCyan?: string;
  brightWhite?: string;
}

// ---------------------------------------------------------------------------
// Backend options (passed to factory)
// ---------------------------------------------------------------------------

export interface TerminalBackendOptions {
  fontSize?: number;
  fontFamily?: string;
  lineHeight?: number;
  scrollback?: number;
  cursorBlink?: boolean;
  cursorStyle?: "block" | "underline" | "bar";
  theme?: ITerminalTheme;
}

// ---------------------------------------------------------------------------
// Terminal backend interface
// ---------------------------------------------------------------------------

export interface ITerminalBackend {
  // -- Lifecycle ------------------------------------------------------------
  /** Mount the terminal into the given DOM container. */
  open(container: HTMLElement): void;
  /** Tear down the terminal and release all resources. */
  dispose(): void;
  /**
   * Reset terminal state (clear screen + scrollback + cursor home + reset SGR).
   * Used only on hard reconnect — Layer 2 of the bootstrap does NOT call
   * reset(), see paintGrid.
   */
  reset(): void;
  /** Focus the terminal input. */
  focus(): void;

  // -- I/O ------------------------------------------------------------------
  /** Write data to the terminal display (does NOT send to PTY). */
  write(data: Uint8Array | string): void;
  /** Subscribe to user keyboard input. */
  onData(cb: (data: string) => void): IDisposable;
  /** Subscribe to binary input (e.g. paste with special chars). Optional — may be a no-op. */
  onBinary(cb: (data: string) => void): IDisposable;

  // -- Selection ------------------------------------------------------------
  getSelection(): string;
  hasSelection(): boolean;
  onSelectionChange(cb: () => void): IDisposable;

  // -- Buffer access --------------------------------------------------------
  /** Get the text content of a single buffer line (0-based index). */
  getBufferLine(line: number): string | null;
  /** Total number of lines in the active buffer (scrollback + viewport). */
  getBufferLength(): number;

  // -- Layout ---------------------------------------------------------------
  /** Fit the terminal to its container. */
  fit(): void;
  /** Current column count. */
  readonly cols: number;
  /** Current row count. */
  readonly rows: number;

  // -- Scrolling ------------------------------------------------------------
  scrollToBottom(): void;
  /**
   * Scroll the viewport by `amount` lines through the scrollback. Positive
   * scrolls DOWN (toward newest output), negative scrolls UP (into history).
   * Used by the Shift+wheel local-scroll override in `TerminalInstance` to
   * scroll even when the foreground app has captured the wheel via a mouse
   * tracking mode. No-op when there is no scrollback to move through.
   */
  scrollLines(amount: number): void;

  // -- Key handling ---------------------------------------------------------
  /** Intercept key events before the terminal processes them. Return false to prevent default. */
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void;

  // -- Links ----------------------------------------------------------------
  /** Register a custom link provider for detecting clickable links in terminal output. */
  registerLinkProvider(provider: ITerminalLinkProvider): IDisposable;

  // -- OSC 633 shell integration --------------------------------------------
  /**
   * Subscribe to OSC 633 shell integration payloads.
   * The callback receives the raw payload string (e.g. "A", "D;0", "P;Cwd=/home").
   * Each backend implements detection differently:
   * - XtermBackend: uses parser.registerOscHandler(633, ...)
   * - GhosttyBackend: scans output stream before write()
   */
  onOsc633(cb: (data: string) => void): IDisposable;

  // -- DOM access (for UI Bridge and paste workarounds) ---------------------
  /** The hidden textarea used for terminal input, if available. */
  getInputElement(): HTMLTextAreaElement | null;
  /** The scrollable viewport element, if available. */
  getViewportElement(): HTMLElement | null;
}

export type BackendType = "xterm" | "ghostty";
