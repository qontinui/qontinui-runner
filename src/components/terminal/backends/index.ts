export type { BackendType, ITerminalBackend, TerminalBackendOptions } from "./types";
export type { ITerminalLinkProvider, ITerminalLink, IDisposable } from "./types";
export type { TerminalSearchOptions, TerminalSearchResults } from "./types";
export type { ScrollbackRingWindow } from "./types";
// `readLocalScrollbackRing` is deliberately NOT re-exported here: this index
// statically pulls in XtermBackend (and with it `@xterm/addon-canvas`, which
// touches `self` at import), so a backend-less consumer that only needs the
// local ring reader imports `./backends/localScrollbackRing` directly and
// stays loadable under the node test environment.

export { XtermBackend } from "./XtermBackend";

/**
 * Create a terminal backend instance.
 *
 * Returns a Promise because some backends (e.g. ghostty-web) require async
 * WASM initialization before a terminal can be created.
 */
export async function createTerminalBackend(
  type: "xterm" | "ghostty",
  options: import("./types").TerminalBackendOptions,
): Promise<import("./types").ITerminalBackend> {
  switch (type) {
    case "ghostty": {
      const { GhosttyBackend } = await import("./GhosttyBackend");
      return GhosttyBackend.create(options);
    }
    case "xterm":
    default: {
      const { XtermBackend } = await import("./XtermBackend");
      return new XtermBackend(options);
    }
  }
}
