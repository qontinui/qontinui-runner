import type { ITerminalLinkProvider, ITerminalLink } from "./backends/types";
import { invoke } from "@tauri-apps/api/core";

/**
 * File path regex: matches absolute paths (Windows or Unix) with optional :line:col suffix.
 *
 * Examples matched:
 *   /home/user/project/src/main.rs:42:10
 *   D:\project\src\main.rs:42
 *   C:/Users/foo/bar.ts
 *   ./relative/path.js:10:5
 *   src/components/App.tsx:123
 *
 * The pattern is intentionally broad — it looks for path-like strings with at
 * least one slash or backslash and a filename component.
 */
const FILE_PATH_RE =
  /(?:(?:[A-Za-z]:)?[/\\]|\.{1,2}[/\\])(?:[^\s:*?"<>|]+[/\\])*[^\s:*?"<>|]+?(?::(\d+)(?::(\d+))?)?/g;

/**
 * Custom terminal link provider that detects file paths in terminal output
 * and opens them in the editor on Ctrl+click (like VS Code's integrated terminal).
 */
export class FilePathLinkProvider implements ITerminalLinkProvider {
  constructor(private _getBufferLine: (line: number) => string | null) {}

  provideLinks(
    bufferLineNumber: number,
    callback: (links: ITerminalLink[] | undefined) => void,
  ): void {
    const lineText = this._getBufferLine(bufferLineNumber);
    if (!lineText) {
      callback(undefined);
      return;
    }

    const links: ITerminalLink[] = [];

    FILE_PATH_RE.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = FILE_PATH_RE.exec(lineText)) !== null) {
      const fullMatch = match[0];
      const startX = match.index + 1; // 1-based
      const endX = match.index + fullMatch.length; // inclusive, 1-based

      // Parse optional :line:col from the match groups
      const lineNumber = match[1] ? parseInt(match[1], 10) : undefined;
      const colNumber = match[2] ? parseInt(match[2], 10) : undefined;

      // Strip the :line:col suffix from the display text to get the pure file path
      let filePath = fullMatch;
      if (lineNumber !== undefined) {
        const colonIdx = filePath.lastIndexOf(`:${match[1]}`);
        if (colonIdx !== -1) {
          filePath = filePath.slice(0, colonIdx);
        }
      }

      links.push({
        range: {
          start: { x: startX, y: bufferLineNumber },
          end: { x: endX, y: bufferLineNumber },
        },
        text: fullMatch,
        decorations: {
          pointerCursor: true,
          underline: true,
        },
        activate: (event: MouseEvent, text: string) => {
          // Only activate on Ctrl+click (or Cmd+click on Mac)
          if (!event.ctrlKey && !event.metaKey) return;

          // Re-parse from the activated text in case it differs
          const parts = text.match(/^(.+?)(?::(\d+)(?::(\d+))?)?$/);
          const path = parts?.[1] ?? text;
          const line = parts?.[2] ? parseInt(parts[2], 10) : undefined;
          const col = parts?.[3] ? parseInt(parts[3], 10) : undefined;

          invoke("open_error_in_editor", {
            filePath: path,
            lineNumber: line ?? null,
            columnNumber: col ?? null,
          }).catch((err) => {
            console.warn(`[FilePathLinkProvider] Failed to open ${path}:`, err);
          });
        },
      });
    }

    callback(links.length > 0 ? links : undefined);
  }
}
