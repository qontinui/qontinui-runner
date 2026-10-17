"use strict";
/**
 * termhost-bench VS Code harness: the control protocol and the arrangement
 * planner. Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 4.
 *
 * PURE: no `vscode`, no network, no files. Both ends load this one module, so a
 * request is validated by one definition: the extension (`harness.js`, the
 * server inside the isolated VS Code) and the orchestrator
 * (`adapters/vscode.mjs`, the client, via `createRequire`). Node built-ins only,
 * no dependencies, no build step.
 *
 * The control endpoint
 * --------------------
 * The extension listens on 127.0.0.1, on an ephemeral port that it writes into
 * the unit's directory (the "control file"). One route: `POST /rpc` with a JSON
 * body `{cmd, args}`. Every request must carry:
 *   - `Host: 127.0.0.1:<port>`, exactly. A DNS-rebinding page names another host.
 *   - `x-termhost-nonce: <nonce>`, the 64-hex secret the adapter generated for
 *     this one launch and handed over in the harness config file (mode 0600).
 *     A custom header also means no browser can send the request without a CORS
 *     preflight, which this server never answers.
 *   - `content-type: application/json`.
 * The answer is `{ok: true, result}` or `{ok: false, error: {code, message}}`.
 *
 * Commands (see COMMANDS): hello, prepare, spawn, arrange, show, focus, close,
 * list, quit. `spawn` cannot run an arbitrary program: the shell is fixed by
 * the launch config, and every spawn in one launch must carry the command line
 * the first one did. That is D13's "one identical command line", enforced in the
 * host rather than trusted (`checkCommandLine`).
 *
 * The arrangement (plan D4)
 * -------------------------
 *   focus  V = 1: every session is a terminal tab in the panel, the panel is
 *          maximized, and one tab is shown.
 *   tiled  V = min(N, 9): the first nine sessions, in slot order, each in its
 *          own editor group of a grid as square as the count allows (3 x 3 at
 *          nine), as terminal editors (`TerminalLocation.Editor`); sessions
 *          beyond nine stay as panel tabs with the panel closed.
 * `planGrid` and `planArrangement` decide the shape; `harness.js` carries it
 * out with stable API calls and reads it back.
 */

const crypto = require("node:crypto");

/** Wire protocol id (`hello` echoes it; the adapter refuses another). */
const PROTOCOL = "termhost-bench/vscode-harness/v1";
/** The harness config file the adapter writes for one launch. */
const CONFIG_SCHEMA = "termhost-bench/vscode-harness-config/v1";
/** The control file the extension writes once it listens. */
const CONTROL_SCHEMA = "termhost-bench/vscode-harness-control/v1";
/** The environment variable that names the harness config file. */
const CONFIG_ENV = "TERMHOST_BENCH_HARNESS_CONFIG";
/** The environment variable every session exports, naming its terminal (D13 slot mapping). */
const TERMINAL_ID_ENV = "TERMHOST_BENCH_TERMINAL_ID";
/** The request header carrying the launch nonce. */
const NONCE_HEADER = "x-termhost-nonce";
/** The one route. */
const RPC_PATH = "/rpc";
/** Largest request body accepted. */
const MAX_BODY_BYTES = 64 * 1024;
/** Most visible panes in the tiled arm (D4: V ~ min(N, 9)); also VS Code's ViewColumn.Nine. */
const MAX_TILED = 9;
/** Most terminals one `spawn` creates. */
const MAX_SPAWN = 64;
/** Most terminals one `arrange` orders (the marker codec's session limit is 343). */
const MAX_ORDER = 343;
/** Where `spawn` puts terminals: the panel, or the active editor group as a new tab. */
const LOCATIONS = Object.freeze(["panel", "editor-active"]);
/** Arms (D4). */
const ARMS = Object.freeze(["focus", "tiled"]);
/** Terminal ids: what TERMHOST_BENCH_TERMINAL_ID may carry. */
const ID_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;
/** The nonce: 32 random bytes as hex. */
const NONCE_PATTERN = /^[0-9a-f]{64}$/;
const MAX_SHELL_ARGS_BYTES = 48 * 1024;

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const fail = (message) => ({ ok: false, error: message });

/**
 * Refuse keys outside `allowed`: a typo is a refusal, not a silent default.
 *
 * @param {object} args
 * @param {string[]} allowed
 */
function unknownKeys(args, allowed) {
  return Object.keys(args).filter((k) => !allowed.includes(k));
}

function noArgs(args) {
  const extra = unknownKeys(args, []);
  return extra.length > 0 ? fail(`takes no arguments (got ${extra.join(", ")})`) : { ok: true, args: {} };
}

function idArg(args, key = "id") {
  if (typeof args[key] !== "string" || !ID_PATTERN.test(args[key])) return fail(`${key} must match ${ID_PATTERN}`);
  return null;
}

/**
 * Normalise shell arguments: an array of strings (any OS) or one string (Windows
 * command-line remainder, as VS Code accepts it).
 */
function shellArgsValid(v) {
  if (typeof v === "string") return Buffer.byteLength(v) <= MAX_SHELL_ARGS_BYTES;
  return Array.isArray(v) && v.every((a) => typeof a === "string") && Buffer.byteLength(v.join("\0")) <= MAX_SHELL_ARGS_BYTES;
}

/** Per-command argument validators: raw args -> {ok, args} | {ok: false, error}. */
const COMMANDS = Object.freeze({
  hello: noArgs,
  prepare: noArgs,
  list: noArgs,
  quit: noArgs,
  spawn(args) {
    const extra = unknownKeys(args, ["count", "location", "shellPath", "shellArgs", "show"]);
    if (extra.length > 0) return fail(`unknown argument(s) ${extra.join(", ")}`);
    if (!Number.isInteger(args.count) || args.count < 1 || args.count > MAX_SPAWN) return fail(`count must be an integer in 1..${MAX_SPAWN}`);
    if (!LOCATIONS.includes(args.location)) return fail(`location must be one of ${LOCATIONS.join(", ")}`);
    if (typeof args.shellPath !== "string" || args.shellPath === "" || args.shellPath.length > 1024) return fail("shellPath must be a non-empty string");
    if (!shellArgsValid(args.shellArgs)) return fail("shellArgs must be an array of strings (or one string) under 48 KiB");
    if (args.show !== undefined && typeof args.show !== "boolean") return fail("show must be a boolean");
    return {
      ok: true,
      args: { count: args.count, location: args.location, shellPath: args.shellPath, shellArgs: args.shellArgs, show: args.show === true },
    };
  },
  arrange(args) {
    const extra = unknownKeys(args, ["arm", "order", "maxTiled"]);
    if (extra.length > 0) return fail(`unknown argument(s) ${extra.join(", ")}`);
    if (!ARMS.includes(args.arm)) return fail(`arm must be one of ${ARMS.join(", ")}`);
    if (!Array.isArray(args.order) || args.order.length < 1 || args.order.length > MAX_ORDER) return fail(`order must list 1..${MAX_ORDER} terminal ids`);
    if (!args.order.every((id) => typeof id === "string" && ID_PATTERN.test(id))) return fail(`every order entry must match ${ID_PATTERN}`);
    if (new Set(args.order).size !== args.order.length) return fail("order lists a terminal twice");
    const maxTiled = args.maxTiled ?? MAX_TILED;
    if (!Number.isInteger(maxTiled) || maxTiled < 1 || maxTiled > MAX_TILED) return fail(`maxTiled must be an integer in 1..${MAX_TILED}`);
    return { ok: true, args: { arm: args.arm, order: [...args.order], maxTiled } };
  },
  show(args) {
    const extra = unknownKeys(args, ["id", "preserveFocus"]);
    if (extra.length > 0) return fail(`unknown argument(s) ${extra.join(", ")}`);
    const bad = idArg(args);
    if (bad) return bad;
    if (args.preserveFocus !== undefined && typeof args.preserveFocus !== "boolean") return fail("preserveFocus must be a boolean");
    return { ok: true, args: { id: args.id, preserveFocus: args.preserveFocus === true } };
  },
  focus(args) {
    const extra = unknownKeys(args, ["id"]);
    if (extra.length > 0) return fail(`unknown argument(s) ${extra.join(", ")}`);
    const bad = idArg(args);
    return bad ?? { ok: true, args: { id: args.id } };
  },
  close(args) {
    const extra = unknownKeys(args, ["id", "all"]);
    if (extra.length > 0) return fail(`unknown argument(s) ${extra.join(", ")}`);
    if (args.all === true && args.id === undefined) return { ok: true, args: { all: true } };
    if (args.all !== undefined) return fail("close takes either {id} or {all: true}");
    const bad = idArg(args);
    return bad ?? { ok: true, args: { id: args.id } };
  },
});

/**
 * Validate one command's arguments.
 *
 * @param {string} cmd
 * @param {unknown} args
 * @returns {{ok: true, args: object} | {ok: false, error: string}}
 */
function validateArgs(cmd, args) {
  if (!Object.prototype.hasOwnProperty.call(COMMANDS, cmd)) return fail(`unknown command ${JSON.stringify(cmd)}`);
  if (!isObject(args)) return fail("args must be an object");
  return COMMANDS[cmd](args);
}

/** Constant-time comparison of two strings (false on any length difference). */
function safeEqual(a, b) {
  if (typeof a !== "string" || typeof b !== "string") return false;
  const x = Buffer.from(a);
  const y = Buffer.from(b);
  return x.length === y.length && crypto.timingSafeEqual(x, y);
}

/** The one Host value the server accepts. */
function expectedHost(port) {
  return `127.0.0.1:${port}`;
}

const refuse = (status, code, message) => ({ ok: false, status, code, message });

/**
 * Parse and authenticate one request. Every refusal names its HTTP status and a
 * stable code; nothing is executed for a refused request.
 *
 * @param {{method: string, url: string, headers: Record<string, string | string[] | undefined>,
 *   body: string}} req
 * @param {{port: number, nonce: string}} auth
 * @returns {{ok: true, cmd: string, args: object} | {ok: false, status: number, code: string, message: string}}
 */
function parseRpc(req, auth) {
  const path = String(req.url ?? "").split("?")[0];
  if (path !== RPC_PATH) return refuse(404, "not_found", `no route ${path}`);
  if (req.method !== "POST") return refuse(405, "method_not_allowed", `${req.method} ${RPC_PATH}`);
  const header = (name) => {
    const v = req.headers?.[name];
    return Array.isArray(v) ? v.join(",") : v;
  };
  if (header("host") !== expectedHost(auth.port)) return refuse(403, "bad_host", `Host must be ${expectedHost(auth.port)}`);
  if (!NONCE_PATTERN.test(auth.nonce ?? "") || !safeEqual(header(NONCE_HEADER), auth.nonce)) {
    return refuse(403, "bad_nonce", `missing or wrong ${NONCE_HEADER}`);
  }
  if (!/^application\/json(\s*;|$)/i.test(String(header("content-type") ?? ""))) {
    return refuse(415, "unsupported_media_type", "content-type must be application/json");
  }
  const body = String(req.body ?? "");
  if (Buffer.byteLength(body) > MAX_BODY_BYTES) return refuse(413, "too_large", `body over ${MAX_BODY_BYTES} bytes`);
  let parsed;
  try {
    parsed = JSON.parse(body);
  } catch (e) {
    return refuse(400, "bad_json", e.message);
  }
  if (!isObject(parsed) || typeof parsed.cmd !== "string") return refuse(400, "bad_request", "body must be {cmd, args}");
  const extra = unknownKeys(parsed, ["cmd", "args"]);
  if (extra.length > 0) return refuse(400, "bad_request", `unknown field(s) ${extra.join(", ")}`);
  if (!Object.prototype.hasOwnProperty.call(COMMANDS, parsed.cmd)) return refuse(400, "unknown_command", `unknown command ${parsed.cmd}`);
  const v = validateArgs(parsed.cmd, parsed.args ?? {});
  if (!v.ok) return refuse(400, "bad_args", `${parsed.cmd}: ${v.error}`);
  return { ok: true, cmd: parsed.cmd, args: v.args };
}

/**
 * The request a client sends (validated with the server's own validator, so a
 * malformed call fails in the caller rather than on the wire).
 *
 * @returns {{path: string, headers: Record<string, string>, body: string}}
 */
function buildRpc(cmd, args, nonce) {
  const v = validateArgs(cmd, args ?? {});
  if (!v.ok) throw new TypeError(`${cmd}: ${v.error}`);
  if (!NONCE_PATTERN.test(nonce ?? "")) throw new TypeError("the nonce must be 64 lowercase hex characters");
  return {
    path: RPC_PATH,
    headers: { "content-type": "application/json", [NONCE_HEADER]: nonce },
    body: JSON.stringify({ cmd, args: v.args }),
  };
}

/** A success answer. */
function okResponse(result) {
  return { status: 200, body: { ok: true, result } };
}

/** A failure answer. */
function errorResponse(status, code, message) {
  return { status, body: { ok: false, error: { code, message: String(message ?? "") } } };
}

/**
 * The editor grid for V visible panes, as square as the count allows: c =
 * ceil(sqrt(V)) columns, rows of c filled in order, the last row holding the
 * rest (a single trailing pane spans the row, as tmux's `tiled` does). Group
 * g (1-based) is row-major, which is VS Code's GRID_APPEARANCE order for a
 * vertical root whose rows split horizontally.
 *
 * @param {number} v 1..9
 * @returns {{v: number, rows: number[], layout: {orientation: number, groups: object[]}}}
 *   `rows` = panes per row; `layout` is `vscode.setEditorLayout`'s argument.
 */
function planGrid(v) {
  if (!Number.isInteger(v) || v < 1 || v > MAX_TILED) throw new RangeError(`a grid holds 1..${MAX_TILED} panes, not ${v}`);
  const cols = Math.ceil(Math.sqrt(v));
  const rowCount = Math.ceil(v / cols);
  const rows = [];
  for (let left = v; left > 0; left -= cols) rows.push(Math.min(cols, left));
  if (rows.length !== rowCount) throw new Error(`grid planning broke for ${v}`);
  if (v === 1) return { v, rows, layout: { orientation: 0, groups: [{}] } };
  const size = 1 / rows.length;
  const layout = {
    orientation: 1,
    groups: rows.map((c) => (c === 1 ? { size } : { size, groups: Array.from({ length: c }, () => ({ size: 1 / c })) })),
  };
  return { v, rows, layout };
}

/**
 * What one arm puts where.
 *
 * @param {{arm: "focus" | "tiled", order: string[], maxTiled?: number}} o
 *   `order`: terminal ids in slot order (the session index each generator claimed)
 * @returns {{arm: string, visible: string[], hidden: string[], focus: string, grid: object | null}}
 */
function planArrangement({ arm, order, maxTiled = MAX_TILED }) {
  const v = validateArgs("arrange", { arm, order, maxTiled });
  if (!v.ok) throw new RangeError(v.error);
  if (arm === "focus") return { arm, visible: [order[0]], hidden: order.slice(1), focus: order[0], grid: null };
  const n = Math.min(order.length, v.args.maxTiled);
  return { arm, visible: order.slice(0, n), hidden: order.slice(n), focus: order[0], grid: planGrid(n) };
}

/**
 * D13: the first spawn of a launch fixes the session command line, and every
 * later one must repeat it exactly; the shell itself was fixed by the launch
 * config. Compared on the exact strings — a host that let two command lines
 * coexist would compare two workloads, not two hosts.
 *
 * @param {{shellPath: string, shellArgs: string[] | string} | null} recorded
 * @param {{shellPath: string, shellArgs: string[] | string}} requested
 * @param {{allowedShellPath: string, caseInsensitivePath?: boolean}} o
 * @returns {{ok: true, recorded: object} | {ok: false, code: string, message: string}}
 */
function checkCommandLine(recorded, requested, { allowedShellPath, caseInsensitivePath = false }) {
  const norm = (p) => (caseInsensitivePath ? String(p).toLowerCase() : String(p));
  if (norm(requested.shellPath) !== norm(allowedShellPath)) {
    return { ok: false, code: "shell_not_allowed", message: `the launch fixed the session shell to ${allowedShellPath}, not ${requested.shellPath}` };
  }
  const key = (x) => JSON.stringify([norm(x.shellPath), x.shellArgs]);
  if (recorded && key(recorded) !== key(requested)) {
    return { ok: false, code: "command_line_mismatch", message: "every session of a launch runs one identical command line (D13); this spawn's differs from the first" };
  }
  return { ok: true, recorded: recorded ?? { shellPath: requested.shellPath, shellArgs: requested.shellArgs } };
}

/**
 * Parse the harness config file the adapter wrote.
 *
 * @param {string} text
 * @returns {{schema: string, nonce: string, controlFile: string, shellPath: string, platform: string,
 *   logFile: string | null, terminalPrefix: string}}
 */
function parseConfig(text) {
  const c = JSON.parse(text);
  if (!isObject(c) || c.schema !== CONFIG_SCHEMA) throw new Error(`not a ${CONFIG_SCHEMA} file`);
  if (!NONCE_PATTERN.test(c.nonce ?? "")) throw new Error("config nonce must be 64 lowercase hex characters");
  for (const k of ["controlFile", "shellPath", "platform"]) {
    if (typeof c[k] !== "string" || c[k] === "") throw new Error(`config ${k} must be a non-empty string`);
  }
  const terminalPrefix = c.terminalPrefix ?? "vsc";
  if (!/^[A-Za-z0-9-]{1,24}$/.test(terminalPrefix)) throw new Error("config terminalPrefix must be 1..24 letters, digits or dashes");
  return {
    schema: c.schema,
    nonce: c.nonce,
    controlFile: c.controlFile,
    shellPath: c.shellPath,
    platform: c.platform,
    logFile: typeof c.logFile === "string" && c.logFile !== "" ? c.logFile : null,
    terminalPrefix,
  };
}

/**
 * The control file's record.
 *
 * @param {{port: number, pid: number, ppid: number, vscodeVersion: string, startedAt: string}} o
 */
function controlRecord({ port, pid, ppid, vscodeVersion, startedAt }) {
  return { schema: CONTROL_SCHEMA, protocol: PROTOCOL, port, pid, ppid, vscode_version: vscodeVersion, started_at: startedAt };
}

/**
 * Parse a control file (client side).
 *
 * @param {string} text
 * @returns {{port: number, pid: number, ppid: number, vscode_version: string, protocol: string}}
 */
function parseControl(text) {
  const c = JSON.parse(text);
  if (!isObject(c) || c.schema !== CONTROL_SCHEMA) throw new Error(`not a ${CONTROL_SCHEMA} file`);
  if (c.protocol !== PROTOCOL) throw new Error(`the extension speaks ${c.protocol}, this harness ${PROTOCOL}`);
  if (!Number.isInteger(c.port) || c.port < 1 || c.port > 65535) throw new Error("control port is not a TCP port");
  if (!Number.isInteger(c.pid) || c.pid < 1) throw new Error("control pid is not a pid");
  return c;
}

module.exports = {
  PROTOCOL,
  CONFIG_SCHEMA,
  CONTROL_SCHEMA,
  CONFIG_ENV,
  TERMINAL_ID_ENV,
  NONCE_HEADER,
  RPC_PATH,
  MAX_BODY_BYTES,
  MAX_TILED,
  MAX_SPAWN,
  MAX_ORDER,
  LOCATIONS,
  ARMS,
  ID_PATTERN,
  NONCE_PATTERN,
  COMMANDS,
  validateArgs,
  safeEqual,
  expectedHost,
  parseRpc,
  buildRpc,
  okResponse,
  errorResponse,
  planGrid,
  planArrangement,
  checkCommandLine,
  parseConfig,
  controlRecord,
  parseControl,
};
