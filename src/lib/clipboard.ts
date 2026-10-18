import { invoke } from "@tauri-apps/api/core";

/**
 * Write text to the OS clipboard reliably from within the Tauri webview.
 *
 * Why not `navigator.clipboard.writeText`? The browser Async Clipboard API is
 * flaky inside WebView2: it requires a secure context AND document focus /
 * transient user activation, and it silently rejects otherwise. That is the
 * root cause of terminal copy "often not working" — the write rejects and the
 * failure gets swallowed, so the text never lands and the user has to retype
 * it. We instead route through the native `clipboard_write_text` command
 * (backed by the `arboard` crate), which writes to the OS clipboard directly
 * from Rust with no focus requirement.
 *
 * The browser API is kept only as a fallback for non-Tauri contexts
 * (Storybook, unit tests) where `invoke` is unavailable.
 *
 * @returns `true` if the text reached the clipboard, `false` otherwise. Never
 *   throws — callers can rely on the boolean to decide whether to show
 *   success/failure feedback.
 */
/**
 * The browser's own Async Clipboard functions, captured by
 * {@link installNativeClipboardShim} BEFORE it patches `navigator.clipboard`,
 * so the fallbacks below never call back into the shim (which would recurse).
 */
let browserWriteText: ((text: string) => Promise<void>) | null = null;
let browserReadText: (() => Promise<string>) | null = null;

export async function writeClipboard(text: string): Promise<boolean> {
  try {
    await invoke("clipboard_write_text", { text });
    return true;
  } catch (nativeErr) {
    // Fall back to the browser API (non-Tauri contexts, or if the native
    // command is somehow unavailable). This is the flaky path, hence the
    // native attempt first.
    try {
      await (browserWriteText ?? ((t: string) => navigator.clipboard.writeText(t)))(text);
      return true;
    } catch (browserErr) {
      console.error("writeClipboard failed (native + browser):", nativeErr, browserErr);
      return false;
    }
  }
}

/**
 * Read text from the OS clipboard reliably from within the Tauri webview.
 *
 * Paste counterpart of {@link writeClipboard}: `navigator.clipboard.readText`
 * is rejected outright by WebKitGTK (Linux) and needs focus/permission on
 * WebView2, so the native `clipboard_read_text` command (`arboard`) is tried
 * first and the browser API is only the non-Tauri fallback.
 *
 * @returns the clipboard text, `""` when it is empty or not text.
 * @throws when neither the native nor the browser read works, so callers can
 *   tell a failed read from an empty clipboard.
 */
export async function readClipboard(): Promise<string> {
  try {
    const text = await invoke<unknown>("clipboard_read_text");
    if (typeof text !== "string") throw new Error("clipboard_read_text returned a non-string");
    return text;
  } catch (nativeErr) {
    try {
      return await (browserReadText ?? (() => navigator.clipboard.readText()))();
    } catch (browserErr) {
      throw new Error(
        `readClipboard failed (native: ${String(nativeErr)}; browser: ${String(browserErr)})`,
        { cause: browserErr },
      );
    }
  }
}

/**
 * Route `navigator.clipboard.writeText` / `readText` through the native
 * commands for the whole app. The webview's Async Clipboard API is rejected on
 * WebKitGTK and flaky on WebView2, and dozens of copy buttons call it
 * directly; patching it once here fixes them all. A no-op outside Tauri.
 */
export function installNativeClipboardShim(): void {
  if (typeof window === "undefined" || typeof navigator === "undefined") return;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const w = window as any;
  if (!w.__TAURI_INTERNALS__ && !w.__TAURI__) return;
  if (navigator.clipboard) {
    browserWriteText = navigator.clipboard.writeText?.bind(navigator.clipboard) ?? null;
    browserReadText = navigator.clipboard.readText?.bind(navigator.clipboard) ?? null;
  }
  const shim = {
    writeText: async (text: string) => {
      if (!(await writeClipboard(text))) throw new Error("clipboard write failed");
    },
    readText: () => readClipboard(),
  };
  try {
    if (navigator.clipboard) {
      Object.assign(navigator.clipboard, shim);
    } else {
      Object.defineProperty(navigator, "clipboard", { value: shim, configurable: true });
    }
  } catch {
    // Read-only clipboard object: callers keep the browser API.
  }
}
