#!/usr/bin/env node
/**
 * TUI repaint load generator — the terminal load a `claude` session actually
 * produces, which the perf harness's default 20 Hz timestamp loop is not.
 *
 * Plan `2026-09-20-terminal-output-transport-is-unmeasured-encoded-broadcast`,
 * Phase 1. Each frame is a synchronized-output block (DEC mode 2026):
 *
 *     ESC[?2026h  ESC[H  <full-screen SGR-heavy repaint>  ESC[?2026l
 *
 * `--spinner` repaints only one row per frame (the spinner line of an idle
 * TUI) inside the same sync block.
 *
 * Cross-platform: plain node, no dependencies, writes to stdout. The frame
 * builders are exported pure functions — the unit test
 * (`scripts/__tests__/tui-repaint-generator.test.mjs`) and the TypeScript
 * transport-cost corpus (`src/components/terminal/transportCorpus.ts`) share
 * them, so the bench, the harness load and any committed fixture are built by
 * ONE implementation. `--corpus-out DIR` writes that corpus as raw `.bin`
 * files (one per frame set, frames concatenated) plus a `manifest.json` of
 * frame lengths, for consumers that cannot import JS.
 *
 * The Rust transport-cost bench (`src-tauri/src/terminal/transport_cost.rs`)
 * does NOT use this corpus: it builds its own (xorshift PRNG, 24 frames per
 * set, 256-colour SGR) where this one is LCG, 8 frames per set, truecolour
 * SGR. Cross-language timings are therefore over different bytes — compare
 * them per byte (ns/B), not per frame.
 *
 * Usage:
 *   node scripts/tui-repaint-generator.mjs [--fps 30] [--cols 120] [--rows 40]
 *        [--spinner] [--alt-screen] [--duration-ms N]
 *   node scripts/tui-repaint-generator.mjs --corpus-out DIR
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const SYNC_BEGIN = "\x1b[?2026h";
export const SYNC_END = "\x1b[?2026l";
export const CURSOR_HOME = "\x1b[H";
export const SGR_RESET = "\x1b[0m";

/** Glyphs a `claude` TUI actually paints: box drawing, bullets, spinner. */
const WORDS = [
  "Reading",
  "src/terminal/session.rs",
  "⏺",
  "Update(",
  "│",
  "✻ Thinking…",
  "─────",
  "tokens",
  "esc to interrupt",
  "╭─",
  "─╮",
  "fn",
  "emit_terminal_output",
  "✓",
  "base64",
  "⎿",
];
const SPINNER = ["✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳"];

/**
 * Deterministic 32-bit LCG — the same frame index always yields the same bytes.
 *
 * @param {number} seed
 * @returns {() => number} uniform in [0, 2^32)
 */
function lcg(seed) {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s;
  };
}

/**
 * Fill exactly `width` columns with words (every glyph above is one column
 * wide), truncating or padding with spaces.
 *
 * @param {() => number} rng
 * @param {number} width
 * @returns {string}
 */
function textRun(rng, width) {
  let out = "";
  let cells = 0;
  while (cells < width) {
    const word = WORDS[rng() % WORDS.length];
    const chars = [...word];
    for (const ch of chars) {
      if (cells >= width) break;
      out += ch;
      cells += 1;
    }
    if (cells < width) {
      out += " ";
      cells += 1;
    }
  }
  return out;
}

/** 24-bit SGR: foreground + background, optionally bold/italic. */
function styleFor(rng) {
  const fg = `38;2;${rng() & 255};${rng() & 255};${rng() & 255}`;
  const bg = `48;2;${rng() & 255};${rng() & 255};${rng() & 255}`;
  const attr = rng() % 4;
  const prefix = attr === 0 ? "1;" : attr === 1 ? "3;" : "";
  return `\x1b[${prefix}${fg};${bg}m`;
}

/**
 * One row of an SGR-heavy repaint: a cursor move to the row, then runs of 4-12
 * columns each under its own truecolor style, filling exactly `cols` columns.
 *
 * @param {() => number} rng
 * @param {number} row 1-based
 * @param {number} cols
 * @returns {string}
 */
function repaintRow(rng, row, cols) {
  let out = `\x1b[${row};1H`;
  let col = 0;
  while (col < cols) {
    const run = Math.min(cols - col, 4 + (rng() % 9));
    out += styleFor(rng) + textRun(rng, run);
    col += run;
  }
  return out + SGR_RESET;
}

/**
 * A full-screen synchronized repaint: every row repainted under dense SGR.
 *
 * @param {{cols:number, rows:number, frame?:number}} opts
 * @returns {string}
 */
export function buildRepaintFrame({ cols, rows, frame = 0 }) {
  assertGeometry(cols, rows);
  const rng = lcg(0x2026 ^ Math.imul(frame + 1, 2654435761) ^ (cols << 8) ^ rows);
  let out = SYNC_BEGIN + CURSOR_HOME;
  for (let r = 1; r <= rows; r += 1) out += repaintRow(rng, r, cols);
  return out + SYNC_END;
}

/**
 * A spinner-only frame: one row (the last) repainted inside a sync block —
 * what an idle `claude` TUI emits between real repaints.
 *
 * @param {{cols:number, rows:number, frame?:number}} opts
 * @returns {string}
 */
export function buildSpinnerFrame({ cols, rows, frame = 0 }) {
  assertGeometry(cols, rows);
  const glyph = SPINNER[frame % SPINNER.length];
  const label = ` Thinking… (${frame}s · esc to interrupt)`;
  const text = [...(glyph + label)].slice(0, cols).join("");
  const pad = " ".repeat(cols - [...text].length);
  return (
    SYNC_BEGIN +
    `\x1b[${rows};1H` +
    `\x1b[1;38;2;215;119;87m${glyph}\x1b[22;38;2;153;153;153m${text.slice(glyph.length)}${pad}` +
    SGR_RESET +
    SYNC_END
  );
}

/**
 * A small-write typing trace: one echoed character per chunk, a newline and
 * prompt every 40 characters — the shape of an operator typing at a shell.
 *
 * @param {{chunks?:number}} [opts]
 * @returns {string[]}
 */
export function buildTypingTrace({ chunks = 400 } = {}) {
  const rng = lcg(0x7e57);
  const alphabet = "abcdefghijklmnopqrstuvwxyz -./_";
  const out = [];
  for (let i = 0; i < chunks; i += 1) {
    if (i > 0 && i % 40 === 0) out.push("\r\n\x1b[32m$\x1b[0m ");
    else out.push(alphabet[rng() % alphabet.length]);
  }
  return out;
}

/** The geometries the transport-cost corpus covers. */
export const CORPUS_GEOMETRIES = [
  { cols: 120, rows: 40 },
  { cols: 200, rows: 60 },
  { cols: 250, rows: 80 },
];

/**
 * The deterministic transport-cost corpus: `framesPerSet` full repaints at each
 * geometry, plus the typing trace.
 *
 * @param {{framesPerSet?:number}} [opts]
 * @returns {{name:string, frames:string[]}[]}
 */
export function buildCorpus({ framesPerSet = 8 } = {}) {
  const sets = CORPUS_GEOMETRIES.map(({ cols, rows }) => ({
    name: `repaint-${cols}x${rows}`,
    frames: Array.from({ length: framesPerSet }, (_, frame) =>
      buildRepaintFrame({ cols, rows, frame }),
    ),
  }));
  sets.push({ name: "typing", frames: buildTypingTrace() });
  return sets;
}

function assertGeometry(cols, rows) {
  if (!Number.isInteger(cols) || cols < 1 || !Number.isInteger(rows) || rows < 1) {
    throw new RangeError(`bad geometry ${cols}x${rows}`);
  }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/**
 * @param {string[]} argv
 * @returns {{fps:number, cols:number, rows:number, spinner:boolean, altScreen:boolean, durationMs:number|null, corpusOut:string|null, help:boolean}}
 */
export function parseGeneratorArgs(argv) {
  const opts = {
    fps: 30,
    cols: 120,
    rows: 40,
    spinner: false,
    altScreen: false,
    durationMs: null,
    corpusOut: null,
    help: false,
  };
  const num = (name, v) => {
    const n = Number(v);
    if (!Number.isFinite(n) || n <= 0) throw new Error(`--${name} needs a positive number`);
    return n;
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const next = () => {
      const v = argv[i + 1];
      if (v === undefined || v.startsWith("--")) throw new Error(`${a} needs a value`);
      i += 1;
      return v;
    };
    if (a === "--help" || a === "-h") opts.help = true;
    else if (a === "--spinner") opts.spinner = true;
    else if (a === "--alt-screen") opts.altScreen = true;
    else if (a === "--fps") opts.fps = num("fps", next());
    else if (a === "--cols") opts.cols = Math.floor(num("cols", next()));
    else if (a === "--rows") opts.rows = Math.floor(num("rows", next()));
    else if (a === "--duration-ms") opts.durationMs = num("duration-ms", next());
    else if (a === "--corpus-out") opts.corpusOut = next();
    else throw new Error(`unknown argument: ${a}`);
  }
  return opts;
}

/**
 * Write the corpus as `<name>.bin` (frames concatenated, UTF-8) plus a
 * `manifest.json` listing each set's frame byte lengths.
 *
 * @param {string} dir
 */
function writeCorpus(dir) {
  mkdirSync(dir, { recursive: true });
  const manifest = {};
  for (const set of buildCorpus()) {
    const bufs = set.frames.map((f) => Buffer.from(f, "utf8"));
    writeFileSync(join(dir, `${set.name}.bin`), Buffer.concat(bufs));
    manifest[set.name] = bufs.map((b) => b.length);
  }
  writeFileSync(join(dir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
}

function run(opts) {
  const build = opts.spinner ? buildSpinnerFrame : buildRepaintFrame;
  const out = process.stdout;
  let frame = 0;
  let stopped = false;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    clearInterval(timer);
    if (opts.altScreen) out.write("\x1b[?1049l");
    out.write(`${SGR_RESET}\r\n`);
  };
  out.on("error", () => {
    // EPIPE: the reader went away (terminal closed). Nothing left to do.
    stopped = true;
    clearInterval(timer);
  });
  if (opts.altScreen) out.write("\x1b[?1049h");
  if (opts.spinner) {
    // Paint the screen once so the spinner row sits under a real frame.
    out.write(buildRepaintFrame({ cols: opts.cols, rows: opts.rows, frame: 0 }));
  }
  const timer = setInterval(() => {
    if (stopped) return;
    out.write(build({ cols: opts.cols, rows: opts.rows, frame }));
    frame += 1;
  }, 1000 / opts.fps);
  if (opts.durationMs !== null) setTimeout(stop, opts.durationMs);
  process.on("SIGINT", () => {
    stop();
    process.exit(0);
  });
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  let opts;
  try {
    opts = parseGeneratorArgs(process.argv.slice(2));
  } catch (e) {
    process.stderr.write(`tui-repaint-generator: ${e.message}\n`);
    process.exit(2);
  }
  if (opts.help) {
    process.stdout.write(
      "usage: node scripts/tui-repaint-generator.mjs [--fps 30] [--cols 120] [--rows 40] " +
        "[--spinner] [--alt-screen] [--duration-ms N] | --corpus-out DIR\n",
    );
  } else if (opts.corpusOut) {
    writeCorpus(resolve(opts.corpusOut));
  } else {
    run(opts);
  }
}
