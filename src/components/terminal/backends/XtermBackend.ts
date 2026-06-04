import { Terminal, type IMarker } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { CanvasAddon } from "@xterm/addon-canvas";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { SearchAddon } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";

import type {
  IDisposable,
  ITerminalBackend,
  ITerminalLinkProvider,
  TerminalBackendOptions,
  TerminalSearchOptions,
  TerminalSearchResults,
} from "./types";

/**
 * Match-highlight colors for find-in-terminal, aligned with the Tokyo Night
 * theme in `TerminalInstance.TERMINAL_OPTIONS`. The two `*OverviewRuler`
 * fields are required by the addon's types even though the runner doesn't
 * render an overview ruler.
 */
const SEARCH_DECORATIONS = {
  matchBackground: "#3d59a1",
  matchOverviewRuler: "#3d59a1",
  activeMatchBackground: "#ff9e64",
  activeMatchColorOverviewRuler: "#ff9e64",
};

export class XtermBackend implements ITerminalBackend {
  private readonly term: Terminal;
  private readonly fitAddon: FitAddon;
  private readonly searchAddon: SearchAddon;
  private container: HTMLElement | null = null;
  /** OSC 633;A prompt-start markers, in registration (= buffer) order. */
  private promptMarkers: IMarker[] = [];

  constructor(options: TerminalBackendOptions = {}) {
    this.term = new Terminal({
      fontSize: options.fontSize,
      fontFamily: options.fontFamily,
      lineHeight: options.lineHeight,
      scrollback: options.scrollback,
      cursorBlink: options.cursorBlink,
      cursorStyle: options.cursorStyle,
      theme: options.theme,
      altClickMovesCursor: options.altClickMovesCursor,
      wordSeparator: options.wordSeparator,
      allowProposedApi: true,
    });

    this.fitAddon = new FitAddon();
    this.term.loadAddon(this.fitAddon);
    this.searchAddon = new SearchAddon();
    this.term.loadAddon(this.searchAddon);

    // Track shell-prompt positions for Ctrl+Up/Ctrl+Down command navigation
    // (VS Code "Scroll to Previous/Next Command"). OSC 633;A fires at every
    // prompt start when shell integration is installed; registerMarker pins
    // the cursor's buffer line and keeps it stable as scrollback trims (the
    // marker disposes itself when its line scrolls out of the buffer).
    // Registered as a SECOND OSC 633 handler — returning false lets the
    // `onOsc633` subscriber (TerminalInstance shell-integration wiring) run
    // for the same payload.
    this.term.parser.registerOscHandler(633, (data: string) => {
      if (data === "A") {
        this.promptMarkers.push(this.term.registerMarker(0));
      }
      return false;
    });
  }

  /** Drop disposed markers (line === -1) and return the live, sorted list. */
  private livePromptMarkers(): IMarker[] {
    this.promptMarkers = this.promptMarkers.filter((m) => !m.isDisposed && m.line >= 0);
    return this.promptMarkers;
  }

  open(container: HTMLElement): void {
    this.container = container;
    this.term.open(container);

    // Load rendering addons BEFORE WebLinksAddon so the renderer's
    // onShowLinkUnderline / onHideLinkUnderline are available when
    // the link decorator initialises.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        console.warn("[XtermBackend] WebGL context lost, falling back to Canvas");
        webgl.dispose();
        // Fall back to canvas renderer on context loss
        try {
          this.term.loadAddon(new CanvasAddon());
        } catch {
          // DOM renderer is the final fallback — already active
        }
      });
      this.term.loadAddon(webgl);
    } catch {
      // WebGL not available, try canvas
      try {
        this.term.loadAddon(new CanvasAddon());
      } catch {
        // DOM renderer is the final fallback — already active
      }
    }

    this.term.loadAddon(new WebLinksAddon());
  }

  dispose(): void {
    this.term.dispose();
  }

  reset(): void {
    this.term.reset();
  }

  focus(): void {
    this.term.focus();
  }

  write(data: Uint8Array | string): void {
    this.term.write(data);
  }

  onData(cb: (data: string) => void): IDisposable {
    return this.term.onData(cb);
  }

  onBinary(cb: (data: string) => void): IDisposable {
    return this.term.onBinary(cb);
  }

  getSelection(): string {
    return this.term.getSelection();
  }

  hasSelection(): boolean {
    return this.term.hasSelection();
  }

  onSelectionChange(cb: () => void): IDisposable {
    return this.term.onSelectionChange(cb);
  }

  getBufferLine(line: number): string | null {
    return this.term.buffer.active.getLine(line)?.translateToString(true) ?? null;
  }

  getBufferLength(): number {
    return this.term.buffer.active.length;
  }

  fit(): void {
    this.fitAddon.fit();
  }

  get cols(): number {
    return this.term.cols;
  }

  get rows(): number {
    return this.term.rows;
  }

  scrollToBottom(): void {
    this.term.scrollToBottom();
  }

  scrollLines(amount: number): void {
    this.term.scrollLines(amount);
  }

  scrollPages(pageCount: number): void {
    this.term.scrollPages(pageCount);
  }

  scrollToTop(): void {
    this.term.scrollToTop();
  }

  scrollToPreviousCommand(): void {
    const viewportTop = this.term.buffer.active.viewportY;
    const markers = this.livePromptMarkers();
    // Last prompt strictly above the current viewport top. When the earliest
    // prompt is already at/above the top, fall through to the very top so
    // repeated presses still make progress (matches VS Code).
    for (let i = markers.length - 1; i >= 0; i--) {
      if (markers[i].line < viewportTop) {
        this.term.scrollToLine(markers[i].line);
        return;
      }
    }
    if (markers.length > 0) this.term.scrollToTop();
    // No markers at all (no shell integration): deliberate no-op — a blind
    // jump to the top of 10k lines of scrollback would be a surprise.
  }

  scrollToNextCommand(): void {
    const viewportTop = this.term.buffer.active.viewportY;
    const markers = this.livePromptMarkers();
    for (const m of markers) {
      if (m.line > viewportTop) {
        this.term.scrollToLine(m.line);
        return;
      }
    }
    if (markers.length > 0) this.term.scrollToBottom();
  }

  findNext(term: string, options?: TerminalSearchOptions): boolean {
    return this.searchAddon.findNext(term, { ...options, decorations: SEARCH_DECORATIONS });
  }

  findPrevious(term: string, options?: TerminalSearchOptions): boolean {
    return this.searchAddon.findPrevious(term, { ...options, decorations: SEARCH_DECORATIONS });
  }

  clearSearch(): void {
    this.searchAddon.clearDecorations();
    this.term.clearSelection();
  }

  onSearchResults(cb: (results: TerminalSearchResults) => void): IDisposable {
    return this.searchAddon.onDidChangeResults(cb);
  }

  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void {
    this.term.attachCustomKeyEventHandler(handler);
  }

  registerLinkProvider(provider: ITerminalLinkProvider): IDisposable {
    return this.term.registerLinkProvider({
      provideLinks(bufferLineNumber, callback) {
        provider.provideLinks(bufferLineNumber, (links) => {
          // Adapt ITerminalLink[] to xterm ILink[] — the only difference is
          // xterm requires decorations fields to be non-optional booleans.
          callback(
            links?.map((link) => ({
              ...link,
              decorations: link.decorations
                ? {
                    pointerCursor: link.decorations.pointerCursor ?? false,
                    underline: link.decorations.underline ?? false,
                  }
                : undefined,
            })),
          );
        });
      },
    });
  }

  onOsc633(cb: (data: string) => void): IDisposable {
    return this.term.parser.registerOscHandler(633, (data: string) => {
      cb(data);
      // Return false: don't consume the sequence — let xterm process it normally
      return false;
    });
  }

  getInputElement(): HTMLTextAreaElement | null {
    return (this.container?.querySelector(".xterm-helper-textarea") as HTMLTextAreaElement) ?? null;
  }

  getViewportElement(): HTMLElement | null {
    return (this.container?.querySelector(".xterm-viewport") as HTMLElement) ?? null;
  }
}
