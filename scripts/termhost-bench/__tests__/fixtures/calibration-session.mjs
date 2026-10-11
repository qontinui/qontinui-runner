/**
 * A synthetic `script --log-timing` recording of a Claude-Code-shaped session, for
 * the calibrator tests: the session as timed output events, rendered on demand into
 * a typescript plus a classic and an advanced timing log (written to a temp dir by
 * the tests, so no CRLF-bearing binary file is committed for git to normalize).
 *
 * The timeline is chosen so the correct analysis has round numbers a reader can
 * check by hand (80x24, times in seconds):
 *
 *   0.000          alt screen + clear (a 1-write stream run)
 *   0.050 - 1.500  silence -> idle 1.45 s
 *   1.500 - 3.400  20 spinner frames at 10 fps            -> thinking, ends 3.500
 *   3.500 - 4.950  30 prose chunks at 20 fps              -> stream,   ends 5.000
 *   5.000 - 5.967  30 full repaints at 30 fps (each split into 8 KiB reads, as `script` does)
 *                                                         -> repaint,  ends 6.000
 *   6.000 - 6.475  20 tool-output chunks of 3000 B at 40 fps -> tool_burst, ends 6.500
 *   6.500 - 8.500  silence -> idle 2.0 s
 *   8.500 - 9.400  10 spinner frames at 10 fps            -> thinking, ends 9.500
 *   9.500 - 9.700  5 prose chunks at 20 fps               -> stream,   ends 9.750
 *
 * so per class: stream 36 frames / 1.8 s = 20 fps, thinking 30 / 3.0 s = 10 fps,
 * repaint 30 / 1.0 s = 30 fps, tool_burst 20 / 0.5 s = 40 fps.
 */

import { buildRepaintFrame, buildSpinnerFrame } from "../../../tui-repaint-generator.mjs";

export const GEOMETRY = Object.freeze({ cols: 80, rows: 24 });
/** Recorded content a calibrated profile must never contain. */
export const CANARY = "SECRET-CANARY-7f3a";

const PROSE = [
  "the",
  "test",
  "reads",
  "a",
  "file",
  "and",
  "then",
  CANARY,
  "passes",
  "again",
  "→",
  "✓",
];

function prose(k) {
  const words = [];
  for (let i = 0; i < 6 + (k % 9) * 3; i += 1) words.push(PROSE[(k * 7 + i * 3) % PROSE.length]);
  return `${words.join(" ")}${k % 4 === 3 ? "\r\n" : " "}`;
}

function logChunk(k) {
  let out = "";
  let line = 0;
  while (out.length < 3000) {
    out += `test case_${k}_${line} ... \x1b[32mok\x1b[0m\r\n`;
    line += 1;
  }
  return out.slice(0, 3000);
}

/**
 * The session's output events, in time order.
 *
 * @returns {{t: number, bytes: Buffer, sync: boolean, cls: string}[]}
 */
export function sessionEvents() {
  const ev = [];
  const add = (t, text, sync, cls) =>
    ev.push({ t: Math.round(t * 1e6) / 1e6, bytes: Buffer.from(text, "utf8"), sync, cls });
  add(0, "\x1b[?1049h\x1b[H\x1b[2J", false, "stream");
  for (let k = 0; k < 20; k += 1)
    add(1.5 + k * 0.1, buildSpinnerFrame({ ...GEOMETRY, frame: k }), true, "thinking");
  for (let k = 0; k < 30; k += 1) add(3.5 + k * 0.05, prose(k), false, "stream");
  for (let k = 0; k < 30; k += 1)
    add(5 + k / 30, buildRepaintFrame({ ...GEOMETRY, frame: k }), true, "repaint");
  for (let k = 0; k < 20; k += 1) add(6 + k * 0.025, logChunk(k), false, "tool_burst");
  for (let k = 0; k < 10; k += 1)
    add(8.5 + k * 0.1, buildSpinnerFrame({ ...GEOMETRY, frame: 20 + k }), true, "thinking");
  for (let k = 0; k < 5; k += 1) add(9.5 + k * 0.05, prose(30 + k), false, "stream");
  return ev;
}

/** Split each event into <= 8 KiB reads at its own time, as util-linux `script` records it. */
export function recordedChunks(events = sessionEvents()) {
  const out = [];
  for (const e of events) {
    for (let off = 0; off < e.bytes.length; off += 8192)
      out.push({ t: e.t, bytes: e.bytes.subarray(off, off + 8192) });
  }
  return out;
}

const fmt = (d) => d.toFixed(6);

/** The typescript: util-linux's header line, the output, and its trailer. */
export const SCRIPT_HEADER_LINE = `Script started on 2026-10-11 12:00:00+00:00 [TERM="xterm-256color" TTY="/dev/pts/3" COLUMNS="80" LINES="24"]\n`;
export const SCRIPT_TRAILER_LINE = `\nScript done on 2026-10-11 12:00:10+00:00 [COMMAND_EXIT_CODE="0"]\n`;

/** The keystrokes the advanced timing's two I entries log (1 and 3 bytes); a canary. */
export const INPUT_EVENTS = Object.freeze([
  Object.freeze({ t: 0.7, bytes: "y" }),
  Object.freeze({ t: 7.25, bytes: "K3y" }),
]);

/** The typescript: util-linux's header line, the output, and its trailer (`--log-out`). */
export function renderTypescript(chunks = recordedChunks()) {
  return Buffer.concat([
    Buffer.from(SCRIPT_HEADER_LINE),
    ...chunks.map((c) => c.bytes),
    Buffer.from(SCRIPT_TRAILER_LINE),
  ]);
}

/** A `--log-io` typescript: output and input interleaved in timing order, in one file. */
export function renderCombinedTypescript(chunks = recordedChunks()) {
  const parts = [
    ...chunks.map((c) => ({ t: c.t, bytes: c.bytes })),
    ...INPUT_EVENTS.map((e) => ({ t: e.t, bytes: Buffer.from(e.bytes) })),
  ].sort((a, b) => a.t - b.t);
  return Buffer.concat([
    Buffer.from(SCRIPT_HEADER_LINE),
    ...parts.map((p) => p.bytes),
    Buffer.from(SCRIPT_TRAILER_LINE),
  ]);
}

/** Classic timing: "<delay> <bytes>" per output chunk. */
export function renderClassicTiming(chunks = recordedChunks()) {
  let prev = 0;
  return chunks
    .map((c) => {
      const line = `${fmt(c.t - prev)} ${c.bytes.length}`;
      prev = c.t;
      return line;
    })
    .join("\n")
    .concat("\n");
}

/** Advanced timing: headers, interleaved input entries, and output entries. */
export function renderAdvancedTiming(chunks = recordedChunks(), { resize = false } = {}) {
  const entries = [
    { t: 0, line: "H START_TIME 2026-10-11 12:00:00 +00:00" },
    { t: 0, line: "H TERM xterm-256color" },
    { t: 0, line: `H COLUMNS ${GEOMETRY.cols}` },
    { t: 0, line: `H LINES ${GEOMETRY.rows}` },
    { t: 0, line: "H SHELL /bin/bash" },
    ...INPUT_EVENTS.map((e) => ({ t: e.t, line: `I ${Buffer.byteLength(e.bytes)}` })),
    ...chunks.map((c) => ({ t: c.t, line: `O ${c.bytes.length}` })),
  ];
  if (resize) entries.push({ t: 7.5, line: "S SIGWINCH ROWS=30 COLS=100" });
  entries.sort((a, b) => a.t - b.t);
  let prev = 0;
  return entries
    .map(({ t, line }) => {
      const [type, ...rest] = line.split(" ");
      const out = `${type} ${fmt(t - prev)} ${rest.join(" ")}`;
      prev = t;
      return out;
    })
    .join("\n")
    .concat("\n");
}

/** What a correct analysis of this session finds, derived from the timeline above. */
export const EXPECTED = Object.freeze({
  runs: [
    ["stream", 0, 0.05, 1],
    ["idle", 0.05, 1.5, 0],
    ["thinking", 1.5, 3.5, 20],
    ["stream", 3.5, 5.0, 30],
    ["repaint", 5.0, 6.0, 30],
    ["tool_burst", 6.0, 6.5, 20],
    ["idle", 6.5, 8.5, 0],
    ["thinking", 8.5, 9.5, 10],
    ["stream", 9.5, 9.75, 5],
  ],
  fps: { thinking: 10, stream: 20, repaint: 30, tool_burst: 40 },
  transitions: {
    stream: { idle: 1, repaint: 1 },
    idle: { thinking: 2 },
    thinking: { stream: 2 },
    repaint: { tool_burst: 1 },
    tool_burst: { idle: 1 },
  },
  initial_phase: "stream",
  duration_s: 9.7,
});
