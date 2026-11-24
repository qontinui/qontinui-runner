/**
 * Types for `tui-repaint-generator.mjs`, so `src/components/terminal/transportCorpus.ts`
 * can import the one frame-builder implementation without `allowJs`.
 */
export declare const SYNC_BEGIN: string;
export declare const SYNC_END: string;
export declare const CURSOR_HOME: string;
export declare const SGR_RESET: string;
export declare const CORPUS_GEOMETRIES: ReadonlyArray<{ cols: number; rows: number }>;
export declare function buildRepaintFrame(opts: {
  cols: number;
  rows: number;
  frame?: number;
}): string;
export declare function buildSpinnerFrame(opts: {
  cols: number;
  rows: number;
  frame?: number;
}): string;
export declare function buildTypingTrace(opts?: { chunks?: number }): string[];
export declare function buildCorpus(opts?: {
  framesPerSet?: number;
}): { name: string; frames: string[] }[];
export declare function parseGeneratorArgs(argv: string[]): {
  fps: number;
  cols: number;
  rows: number;
  spinner: boolean;
  altScreen: boolean;
  durationMs: number | null;
  corpusOut: string | null;
  help: boolean;
};
