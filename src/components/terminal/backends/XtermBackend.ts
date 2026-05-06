import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { CanvasAddon } from "@xterm/addon-canvas";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";

import type {
  IDisposable,
  ITerminalBackend,
  ITerminalLinkProvider,
  TerminalBackendOptions,
} from "./types";

export class XtermBackend implements ITerminalBackend {
  private readonly term: Terminal;
  private readonly fitAddon: FitAddon;
  private container: HTMLElement | null = null;

  constructor(options: TerminalBackendOptions = {}) {
    this.term = new Terminal({
      fontSize: options.fontSize,
      fontFamily: options.fontFamily,
      lineHeight: options.lineHeight,
      scrollback: options.scrollback,
      cursorBlink: options.cursorBlink,
      cursorStyle: options.cursorStyle,
      theme: options.theme,
      allowProposedApi: true,
    });

    this.fitAddon = new FitAddon();
    this.term.loadAddon(this.fitAddon);
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
