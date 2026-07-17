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
export async function writeClipboard(text: string): Promise<boolean> {
  try {
    await invoke("clipboard_write_text", { text });
    return true;
  } catch (nativeErr) {
    // Fall back to the browser API (non-Tauri contexts, or if the native
    // command is somehow unavailable). This is the flaky path, hence the
    // native attempt first.
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (browserErr) {
      console.error("writeClipboard failed (native + browser):", nativeErr, browserErr);
      return false;
    }
  }
}
