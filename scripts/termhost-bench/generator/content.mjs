/**
 * Content builders for the termhost-bench AI-session generator: a seeded PRNG,
 * seed mixing, and the streaming-text and tool-output chunk builders. Repaint and
 * spinner frames are NOT built here — they come from runner#1856's
 * `scripts/tui-repaint-generator.mjs`, the one implementation of a
 * Claude-Code-shaped synchronized repaint (plan D3: not a second one).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1.
 *
 * Every builder is a pure function of its arguments, so a chunk can be rebuilt
 * byte-identically from its descriptor (the driver drops queued bytes when it falls
 * behind and rebuilds them at write time). Chunks are EXACTLY `size` UTF-8 bytes:
 * whole tokens while they fit, then ASCII spaces, so no multi-byte character or
 * escape sequence is ever split and a chunk's size is known without building it.
 */

import { buildRepaintFrame } from "../../tui-repaint-generator.mjs";

/**
 * mulberry32 — a small, fast 32-bit PRNG; integer-only, so identical on every platform.
 *
 * @param {number} seed
 * @returns {{nextU32: () => number, next: () => number}}
 */
export function mulberry32(seed) {
  let a = seed >>> 0;
  const nextU32 = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return (t ^ (t >>> 14)) >>> 0;
  };
  return { nextU32, next: () => nextU32() / 4294967296 };
}

/** murmur3's 32-bit finalizer. */
function fmix32(h) {
  let x = h >>> 0;
  x ^= x >>> 16;
  x = Math.imul(x, 0x85ebca6b);
  x ^= x >>> 13;
  x = Math.imul(x, 0xc2b2ae35);
  x ^= x >>> 16;
  return x >>> 0;
}

/**
 * Mix two 32-bit values into an independent 32-bit seed.
 *
 * @param {number} a
 * @param {number} b
 * @returns {number}
 */
export function mixSeed(a, b) {
  return fmix32(fmix32(a >>> 0) ^ Math.imul(((b >>> 0) + 0x9e3779b9) | 0, 0x27d4eb2d));
}

const PROSE = [
  "the",
  "session",
  "reads",
  "the",
  "file",
  "and",
  "updates",
  "a",
  "function",
  "that",
  "returns",
  "its",
  "result",
  "to",
  "the",
  "caller",
  "so",
  "we",
  "need",
  "to",
  "check",
  "whether",
  "each",
  "test",
  "still",
  "passes",
  "after",
  "this",
  "change",
  "because",
  "terminal",
  "output",
  "should",
  "render",
  "every",
  "frame",
  "with",
  "its",
  "marker",
  "while",
  "other",
  "panes",
  "stream",
  "under",
  "load",
  "then",
  "I'll",
  "run",
  "it",
  "again",
  "—",
  "…",
  "→",
  "✓",
  "first,",
  "next,",
  "finally,",
  "which",
  "means",
  "one",
];
const IDENTS = [
  "emit_terminal_output",
  "SyncFrameCoalescer",
  "buildRepaintFrame",
  "session.rs",
  "useZoneLayout",
  "pickLayout",
  "MAX_WEBGL_PANES",
  "fs.writeSync",
  "process.hrtime",
  "admit_to_webview",
  "resource_guard",
  "create-with-command",
];
/** Foreground indices for inline code: deliberately NOT binary-channel colours. */
const CODE_FG = [180, 117, 214, 151];

const CRATES = [
  "qontinui-runner",
  "tokio",
  "serde_json",
  "portable-pty",
  "tauri",
  "sysinfo",
  "xcap",
  "windows",
];
const MODULES = [
  "terminal::session",
  "mcp::terminals",
  "ci_node::manifest",
  "pair::subject_isolation",
  "terminal::visibility",
];
const CASES = [
  "admits_spawn_below_floor",
  "coalesces_sync_frames",
  "reads_bound_port",
  "refuses_primary_port",
  "drains_cleanly",
];
const FILES = ["terminal/session", "mcp/terminals", "settings", "terminal/grid", "main"];

/**
 * Fill exactly `size` bytes: tokens from `nextToken` while they fit, then spaces.
 *
 * @param {number} size
 * @param {() => string} nextToken
 * @returns {string}
 */
function exactFill(size, nextToken) {
  if (!Number.isInteger(size) || size < 1)
    throw new RangeError(`chunk size must be a positive integer: ${size}`);
  let out = "";
  let used = 0;
  for (let guard = 0; guard < 1_000_000; guard += 1) {
    const token = nextToken();
    const bytes = Buffer.byteLength(token, "utf8");
    if (used + bytes > size) break;
    out += token;
    used += bytes;
  }
  return out + " ".repeat(size - used);
}

/**
 * Assistant prose streamed in small chunks: wrapped words, inline code and bold.
 *
 * @param {number} size exact UTF-8 byte length
 * @param {number} seed per-chunk seed
 * @param {number} cols terminal width (lines wrap a little short of it)
 * @returns {string}
 */
export function buildStreamChunk(size, seed, cols) {
  const rng = mulberry32(seed);
  const wrapAt = Math.max(20, Math.min(cols - 2, 100));
  let col = rng.nextU32() % wrapAt;
  return exactFill(size, () => {
    const pick = rng.nextU32();
    let word;
    let width;
    if (pick % 11 === 0) {
      const id = IDENTS[(pick >>> 4) % IDENTS.length];
      word = `\x1b[38;5;${CODE_FG[(pick >>> 8) % CODE_FG.length]}m${id}\x1b[39m`;
      width = id.length;
    } else if (pick % 13 === 0) {
      const w = PROSE[(pick >>> 4) % PROSE.length];
      word = `\x1b[1m${w}\x1b[22m`;
      width = [...w].length;
    } else {
      word = PROSE[(pick >>> 4) % PROSE.length];
      width = [...word].length;
    }
    if (pick % 37 === 0) {
      col = 0;
      return `\r\n\r\n${word}`;
    }
    if (col + 1 + width > wrapAt) {
      col = width;
      return `\r\n${word}`;
    }
    const sep = col === 0 ? "" : " ";
    col += sep.length + width;
    return sep + word;
  });
}

/**
 * Tool output dumped in large chunks: compiler, test-runner and log lines.
 *
 * @param {number} size exact UTF-8 byte length
 * @param {number} seed per-chunk seed
 * @returns {string}
 */
export function buildBurstChunk(size, seed) {
  const rng = mulberry32(seed);
  const pick = (list) => list[rng.nextU32() % list.length];
  let line = 1 + (rng.nextU32() % 900);
  return exactFill(size, () => {
    line += 1;
    switch (rng.nextU32() % 6) {
      case 0:
        return `\x1b[1m\x1b[32m   Compiling\x1b[0m ${pick(CRATES)} v0.${rng.nextU32() % 40}.${rng.nextU32() % 20}\r\n`;
      case 1:
        return `test ${pick(MODULES)}::tests::${pick(CASES)} ... \x1b[32mok\x1b[0m\r\n`;
      case 2:
        return `\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: unused variable: \`${pick(PROSE)}\`\x1b[0m\r\n`;
      case 3:
        return `\x1b[1m\x1b[34m  -->\x1b[0m src/${pick(FILES)}.rs:${line}:${1 + (rng.nextU32() % 80)}\r\n`;
      case 4:
        return `\x1b[1m\x1b[34m${String(line).padStart(5)} |\x1b[0m     let ${pick(PROSE).replace(/\W/g, "x")} = ${pick(IDENTS).replace(/\W/g, "_")}(&mut ctx)?;\r\n`;
      default:
        return `2026-10-11T12:${String(rng.nextU32() % 60).padStart(2, "0")}:${String(rng.nextU32() % 60).padStart(2, "0")}Z  INFO ${pick(MODULES)}: ${pick(PROSE)} ${pick(PROSE)} ${pick(PROSE)}\r\n`;
    }
  });
}

const rowBytesCache = new Map();

/**
 * Average bytes per painted row of a runner#1856 repaint at `cols` columns, measured
 * from real frames (so fitting a frame-size distribution never re-models the builder).
 *
 * @param {number} cols
 * @returns {number}
 */
export function repaintRowBytes(cols) {
  let v = rowBytesCache.get(cols);
  if (v === undefined) {
    const rows = 40;
    let total = 0;
    for (let frame = 0; frame < 4; frame += 1) {
      total += Buffer.byteLength(buildRepaintFrame({ cols, rows, frame }), "utf8");
    }
    v = total / (4 * rows);
    rowBytesCache.set(cols, v);
  }
  return v;
}

/**
 * Rows to repaint so a frame approximates `targetBytes`, clamped to [1, geometry.rows].
 *
 * @param {number} targetBytes
 * @param {{cols: number, rows: number}} geometry
 * @returns {number}
 */
export function repaintRowsFor(targetBytes, geometry) {
  const rows = Math.round(targetBytes / repaintRowBytes(geometry.cols));
  return Math.min(geometry.rows, Math.max(1, rows));
}
