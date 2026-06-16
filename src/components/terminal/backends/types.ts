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
  /**
   * Alt+click repositions the shell cursor by synthesizing arrow-key
   * presses (VS Code `terminal.integrated.altClickMovesCursor`, default ON
   * there). Only meaningful on backends that support it (xterm).
   */
  altClickMovesCursor?: boolean;
  /** Word-boundary characters for double-click selection (VS Code parity). */
  wordSeparator?: string;
  /**
   * GPU acceleration policy for the renderer (xterm only):
   * - `"auto"` (default): load the WebGL renderer, falling back to Canvas then
   *   DOM (the existing behavior).
   * - `"dom"`: skip the WebGL/Canvas addons entirely → pure DOM renderer. The
   *   DOM renderer has no glyph texture atlas, so it can never ghost. This is
   *   both a user escape hatch and the force-DOM diagnostic lever for the
   *   ghosting investigation. No-op on ghostty (Canvas-only, no WebGL).
   */
  gpuAcceleration?: "auto" | "dom";
}

// ---------------------------------------------------------------------------
// Find-in-terminal (backend-agnostic subset of xterm's ISearchOptions)
// ---------------------------------------------------------------------------

export interface TerminalSearchOptions {
  /** Treat the term as a regular expression. */
  regex?: boolean;
  /** Match whole words only. */
  wholeWord?: boolean;
  /** Case-sensitive matching. */
  caseSensitive?: boolean;
  /**
   * Incremental search: expand the current selection if it still matches —
   * used while the operator types so the active match doesn't jump ahead.
   * Only affects findNext.
   */
  incremental?: boolean;
}

/** Fired when highlighted search results change (see `onSearchResults`). */
export interface TerminalSearchResults {
  /** Index of the active match, or -1 when none / over the highlight limit. */
  resultIndex: number;
  /** Total number of matches. */
  resultCount: number;
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
  /**
   * Write data to the terminal display (does NOT send to PTY).
   *
   * `onRendered`, when supplied, is invoked once the chunk has been parsed
   * and rendered by the backend. This is the render-completion signal that
   * drives the render-based flow-control ack (see `TerminalInstance`): we
   * ack bytes the renderer has actually drained, not bytes merely received
   * off the wire, so a burst can't outrun xterm's input buffer (~50MB cap,
   * silently drops overflow) before backpressure engages. Mirrors xterm's
   * `write(data, callback)`. Backends with no async completion signal call
   * it synchronously right after their own write.
   */
  write(data: Uint8Array | string, onRendered?: () => void): void;
  /** Subscribe to user keyboard input. */
  onData(cb: (data: string) => void): IDisposable;
  /** Subscribe to binary input (e.g. paste with special chars). Optional — may be a no-op. */
  onBinary(cb: (data: string) => void): IDisposable;
  /**
   * Whether the foreground app has enabled bracketed paste mode (DEC mode
   * 2004). The runner intercepts Ctrl+V and writes to the PTY itself, so it
   * must replicate the bracketing/normalization xterm's own paste would do —
   * this exposes the mode state needed to decide. False on backends that don't
   * track it. See `preparePaste.ts`.
   */
  readonly bracketedPasteMode: boolean;

  // -- Selection ------------------------------------------------------------
  getSelection(): string;
  hasSelection(): boolean;
  /** Clear the active text selection (e.g. after a right-click copy). */
  clearSelection(): void;
  onSelectionChange(cb: () => void): IDisposable;

  // -- Buffer access --------------------------------------------------------
  /** Get the text content of a single buffer line (0-based index). */
  getBufferLine(line: number): string | null;
  /** Total number of lines in the active buffer (scrollback + viewport). */
  getBufferLength(): number;
  /**
   * Set the maximum scrollback line count, trimming the existing buffer down
   * to the new cap immediately when it is lowered. Used to release memory held
   * by a terminal whose process has exited — a dead pane never produces more
   * output, so only its tail is worth keeping resident. Backends without
   * runtime scrollback control may no-op.
   */
  setScrollback(lines: number): void;
  /**
   * Change the rendered font size (px). On GPU-accelerated renderers this also
   * clears the glyph texture atlas — a stale atlas keyed to the old cell
   * metrics is a ghosting source. Backends without runtime font control may
   * no-op.
   */
  setFontSize(px: number): void;
  /**
   * Swap the color theme. On GPU-accelerated renderers this also clears the
   * glyph texture atlas (cached glyphs are colored, so a theme change must
   * invalidate them). Backends without runtime theme control may no-op.
   */
  setTheme(theme: ITerminalTheme): void;

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
  /**
   * Scroll the viewport by `pageCount` pages (one page ≈ one screen height).
   * Positive scrolls DOWN, negative UP. Backs the Shift+PageUp/PageDown
   * keyboard shortcuts (VS Code "Scroll Up/Down (Page)").
   */
  scrollPages(pageCount: number): void;
  /** Scroll the viewport to the very top of the scrollback (VS Code Ctrl+Home). */
  scrollToTop(): void;
  /**
   * Jump the viewport to the previous shell prompt (VS Code Ctrl+Up "Scroll
   * to Previous Command"). Prompt positions come from OSC 633;A shell
   * integration marks tracked by the backend; no-op when no marks exist
   * (shell integration not installed, or a backend without marker support).
   */
  scrollToPreviousCommand(): void;
  /** Jump to the next shell prompt (VS Code Ctrl+Down); bottom when none ahead. */
  scrollToNextCommand(): void;

  // -- Find-in-terminal (VS Code Ctrl+F parity) ------------------------------
  /**
   * Find the next match for `term` (wraps at the end). Returns false when no
   * match exists. Highlights all matches when the backend supports
   * decorations. Backends without search support return false.
   */
  findNext(term: string, options?: TerminalSearchOptions): boolean;
  /** Find the previous match for `term` (wraps at the start). */
  findPrevious(term: string, options?: TerminalSearchOptions): boolean;
  /** Clear search highlights and the active-match selection. */
  clearSearch(): void;
  /** Subscribe to match-count changes while a search is active. */
  onSearchResults(cb: (results: TerminalSearchResults) => void): IDisposable;

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
