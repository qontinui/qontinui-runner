import { init, Terminal, FitAddon } from "ghostty-web";
import type { ILink as GhosttyILink } from "ghostty-web";
import { Osc633Interceptor } from "./osc633-interceptor";
import type {
  IDisposable,
  ITerminalBackend,
  ITerminalLinkProvider,
  TerminalBackendOptions,
} from "./types";

/**
 * Terminal backend using ghostty-web (Ghostty's VT parser compiled to WASM).
 *
 * Key differences from XtermBackend:
 * - Requires async WASM init before terminal creation
 * - Uses Canvas 2D rendering (no WebGL/WebGPU)
 * - No parser hook API — uses Osc633Interceptor for shell integration
 * - No onBinary event — returns a no-op disposable
 * - registerLinkProvider returns void, not IDisposable
 */
export class GhosttyBackend implements ITerminalBackend {
  private readonly term: Terminal;
  private readonly fitAddon: FitAddon;
  private readonly oscInterceptor: Osc633Interceptor;
  private container: HTMLElement | null = null;

  /**
   * Use `GhosttyBackend.create()` instead of calling `new` directly.
   * The WASM module must be initialized before creating a Terminal.
   */
  private constructor(options: TerminalBackendOptions) {
    this.term = new Terminal({
      fontSize: options.fontSize,
      fontFamily: options.fontFamily,
      // ghostty-web does not support lineHeight — skip it
      scrollback: options.scrollback,
      cursorBlink: options.cursorBlink,
      cursorStyle: options.cursorStyle,
      theme: options.theme,
    });

    this.fitAddon = new FitAddon();
    this.term.loadAddon(this.fitAddon);
    this.oscInterceptor = new Osc633Interceptor();
  }

  /**
   * Async factory: initializes the WASM module, then creates the backend.
   */
  static async create(options: TerminalBackendOptions): Promise<GhosttyBackend> {
    await init();
    return new GhosttyBackend(options);
  }

  open(container: HTMLElement): void {
    this.container = container;
    this.term.open(container);
  }

  dispose(): void {
    this.oscInterceptor.dispose();
    this.term.dispose();
  }

  reset(): void {
    this.term.reset();
  }

  focus(): void {
    this.term.focus();
  }

  write(data: Uint8Array | string): void {
    // Scan for OSC 633 sequences BEFORE writing to the terminal.
    // The interceptor is read-only — it never modifies the data.
    this.oscInterceptor.scan(data);
    this.term.write(data);
  }

  onData(cb: (data: string) => void): IDisposable {
    return this.term.onData(cb);
  }

  onBinary(_cb: (data: string) => void): IDisposable {
    // ghostty-web does not emit binary events.
    // Paste is handled by the custom key event handler in TerminalInstance,
    // so this is safe to no-op.
    return { dispose() {} };
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
    // ghostty-web's registerLinkProvider returns void (not IDisposable).
    // ghostty-web's ILink.activate takes (event: MouseEvent) — no text param.
    // We adapt our ITerminalLinkProvider to ghostty-web's ILinkProvider.
    this.term.registerLinkProvider({
      provideLinks(y, callback) {
        provider.provideLinks(y, (links) => {
          if (!links) {
            callback(undefined);
            return;
          }
          const adapted: GhosttyILink[] = links.map((link) => ({
            text: link.text,
            range: link.range,
            activate(event: MouseEvent) {
              // ghostty-web passes only the event; we also pass the text
              // to match our ITerminalLink.activate signature
              link.activate(event, link.text);
            },
          }));
          callback(adapted);
        });
      },
    });
    // Return a no-op disposable since ghostty-web doesn't support unregistering
    return { dispose() {} };
  }

  onOsc633(cb: (data: string) => void): IDisposable {
    return this.oscInterceptor.onPayload(cb);
  }

  getInputElement(): HTMLTextAreaElement | null {
    return this.term.textarea ?? null;
  }

  getViewportElement(): HTMLElement | null {
    // ghostty-web renders to a canvas, not a scrollable viewport div.
    // Return the canvas element's parent for styling purposes.
    return this.container?.querySelector("canvas")?.parentElement ?? null;
  }
}
