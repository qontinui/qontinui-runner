"use strict";
/**
 * termhost-bench VS Code harness: the controller and the control endpoint that
 * run INSIDE the isolated VS Code (plan
 * `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 4).
 * `extension.js` hands it the real `vscode` module; the tests hand it a
 * simulated workbench. Stable API only — no proposed API, which would need
 * Insiders — plus built-in commands confirmed present in the pinned build.
 *
 * What it does
 *   - spawn     `window.createTerminal({name, shellPath, shellArgs, env, location})`:
 *               the platform's interactive shell with the generator as its child,
 *               on D13's one command line, `TERMHOST_BENCH_TERMINAL_ID=<id>` in its
 *               environment so the generator's slot file names its terminal.
 *   - arrange   focus: every terminal a panel tab, the panel maximized, one shown.
 *               tiled: `vscode.setEditorLayout` builds the grid `planGrid` chose,
 *               and each visible terminal is moved into its group — focus group g
 *               (`workbench.action.focus<Nth>EditorGroup`), make the terminal the
 *               active one (`Terminal.show(true)`), then
 *               `workbench.action.terminal.moveToEditor`, which opens it in the
 *               ACTIVE editor group. Every move is read back through
 *               `window.tabGroups` before the next one; an arrangement that did
 *               not happen is reported, never assumed.
 *   - show / focus / close / list / quit.
 *
 * Commands that change UI state complete asynchronously in VS Code (the
 * terminal-move commands return before the editor opens), so every step waits
 * for its read-back with a bound rather than sleeping a guessed time.
 *
 * Requests are executed one at a time, in arrival order: two arrangements must
 * never interleave their focus and move steps.
 */

const fs = require("node:fs");
const http = require("node:http");
const path = require("node:path");

const protocol = require("./protocol.js");

/** Built-in commands, each confirmed in the pinned build's workbench bundle. */
const CMD = Object.freeze({
  focusGroup: Object.freeze([
    "workbench.action.focusFirstEditorGroup",
    "workbench.action.focusSecondEditorGroup",
    "workbench.action.focusThirdEditorGroup",
    "workbench.action.focusFourthEditorGroup",
    "workbench.action.focusFifthEditorGroup",
    "workbench.action.focusSixthEditorGroup",
    "workbench.action.focusSeventhEditorGroup",
    "workbench.action.focusEighthEditorGroup",
  ]),
  focusLastGroup: "workbench.action.focusLastEditorGroup",
  moveToEditor: "workbench.action.terminal.moveToEditor",
  moveToPanel: "workbench.action.terminal.moveToTerminalPanel",
  setLayout: "vscode.setEditorLayout",
  closePanel: "workbench.action.closePanel",
  toggleMaxPanel: "workbench.action.toggleMaximizedPanel",
  closeSidebar: "workbench.action.closeSidebar",
  closeAuxiliaryBar: "workbench.action.closeAuxiliaryBar",
  closeAllEditors: "workbench.action.closeAllEditors",
  quit: "workbench.action.quit",
});

/** The single-group layout. */
const SINGLE_GROUP = Object.freeze({ orientation: 0, groups: [{}] });

/** A refusal with an HTTP status and a stable code. */
class HarnessError extends Error {
  constructor(status, code, message) {
    super(message);
    this.name = "HarnessError";
    this.status = status;
    this.code = code;
  }
}

/**
 * Poll `predicate` every `intervalMs` until it holds or `timeoutMs` passes.
 *
 * @returns {Promise<boolean>}
 */
async function waitFor(predicate, timeoutMs, { intervalMs = 25, sleep = (ms) => new Promise((r) => setTimeout(r, ms)), now = Date.now } = {}) {
  const deadline = now() + timeoutMs;
  for (;;) {
    try {
      if (predicate()) return true;
    } catch {
      // a read that throws mid-transition is "not yet"
    }
    if (now() >= deadline) return false;
    await sleep(intervalMs);
  }
}

/** Resolve `thenable` or `fallback` after `ms`. */
function withTimeout(thenable, ms, fallback = undefined) {
  return Promise.race([Promise.resolve(thenable), new Promise((r) => setTimeout(() => r(fallback), ms))]);
}

class HarnessController {
  /**
   * @param {{vscode: object, config: ReturnType<typeof protocol.parseConfig>, log?: (s: string) => void,
   *   timing?: {stepMs?: number, processIdMs?: number, closeMs?: number}}} o
   */
  constructor({ vscode, config, log = () => {}, timing = {} }) {
    this.vscode = vscode;
    this.config = config;
    this.log = log;
    this.stepMs = timing.stepMs ?? 5000;
    this.processIdMs = timing.processIdMs ?? 20000;
    this.closeMs = timing.closeMs ?? 5000;
    this.entries = new Map();
    this.byTerminal = new Map();
    this.counter = 0;
    this.commandLine = null;
    this.closedWaiters = new Map();
    // A close is confirmed by VS Code's own event, never assumed from dispose().
    this.closeSubscription = vscode.window.onDidCloseTerminal((t) => this.onClosed(t));
  }

  dispose() {
    this.closeSubscription?.dispose?.();
  }

  /** Dispatch one validated command. */
  async handle(cmd, args) {
    switch (cmd) {
      case "hello":
        return this.hello();
      case "prepare":
        return this.prepare();
      case "spawn":
        return this.spawn(args);
      case "arrange":
        return this.arrange(args);
      case "show":
        return this.show(args);
      case "focus":
        return this.focus(args);
      case "close":
        return this.close(args);
      case "list":
        return this.list();
      case "quit":
        return this.quit();
      default:
        throw new HarnessError(400, "unknown_command", `unknown command ${cmd}`);
    }
  }

  /** Run a built-in command; a rejection is returned, not thrown. */
  async exec(id, ...args) {
    try {
      await this.vscode.commands.executeCommand(id, ...args);
      return { id, ok: true };
    } catch (e) {
      this.log(`command ${id} failed: ${e?.message ?? e}`);
      return { id, ok: false, error: String(e?.message ?? e) };
    }
  }

  wait(predicate, ms = this.stepMs) {
    return waitFor(predicate, ms);
  }

  require(id) {
    const e = this.entries.get(id);
    if (!e) throw new HarnessError(404, "unknown_terminal", `no terminal ${id} in this launch`);
    if (e.exited) throw new HarnessError(409, "terminal_exited", `terminal ${id} has exited`);
    return e;
  }

  /** Effective terminal settings, and their defaults, as VS Code resolved them. */
  readSettings() {
    const ti = this.vscode.workspace.getConfiguration("terminal.integrated");
    const read = (key) => {
      const i = ti.inspect(key);
      return { value: ti.get(key) ?? null, default: i?.defaultValue ?? null, user: i?.globalValue ?? null };
    };
    return {
      gpuAcceleration: read("gpuAcceleration"),
      windowsUseConptyDll: read("windowsUseConptyDll"),
      scrollback: read("scrollback"),
      shellIntegrationEnabled: read("shellIntegration.enabled"),
      enablePersistentSessions: read("enablePersistentSessions"),
    };
  }

  hello() {
    const v = this.vscode;
    return {
      protocol: protocol.PROTOCOL,
      vscode: { version: v.version, appName: v.env?.appName ?? null, appHost: v.env?.appHost ?? null },
      process: {
        pid: process.pid,
        ppid: process.ppid,
        platform: process.platform,
        arch: process.arch,
        electron: process.versions.electron ?? null,
        node: process.versions.node,
      },
      settings: this.readSettings(),
      terminals: this.entries.size,
    };
  }

  /** Close what would compete with the panes: both side bars, any editor, the panel. */
  async closeChrome() {
    return [await this.exec(CMD.closeSidebar), await this.exec(CMD.closeAuxiliaryBar)];
  }

  async prepare() {
    const steps = [...(await this.closeChrome())];
    if (this.terminalTabs().length === 0) steps.push(await this.exec(CMD.closeAllEditors));
    steps.push(await this.exec(CMD.setLayout, SINGLE_GROUP));
    steps.push(await this.exec(CMD.closePanel));
    return { done: steps.filter((s) => s.ok).map((s) => s.id), failed: steps.filter((s) => !s.ok) };
  }

  /** Every terminal-editor tab, with its group's view column. */
  terminalTabs() {
    const out = [];
    for (const g of this.vscode.window.tabGroups.all) {
      for (const tab of g.tabs) {
        if (tab.input instanceof this.vscode.TabInputTerminal) out.push({ viewColumn: g.viewColumn, label: tab.label, active: tab === g.activeTab });
      }
    }
    return out;
  }

  /** The view column holding `entry` as a terminal editor, by tab label; null when it is not one. */
  editorColumn(entry) {
    return this.terminalTabs().find((t) => t.label === entry.name)?.viewColumn ?? null;
  }

  async spawn({ count, location, shellPath, shellArgs, show }) {
    const check = protocol.checkCommandLine(this.commandLine, { shellPath, shellArgs }, {
      allowedShellPath: this.config.shellPath,
      caseInsensitivePath: this.config.platform === "win32",
    });
    if (!check.ok) throw new HarnessError(check.code === "shell_not_allowed" ? 403 : 409, check.code, check.message);
    this.commandLine = check.recorded;
    const v = this.vscode;
    const created = [];
    for (let i = 0; i < count; i += 1) {
      this.counter += 1;
      const id = `${this.config.terminalPrefix}-${this.counter}`;
      const terminal = v.window.createTerminal({
        name: id,
        shellPath,
        shellArgs,
        // `null` deletes a variable: the shells never see the harness's config path.
        env: { [protocol.TERMINAL_ID_ENV]: id, [protocol.CONFIG_ENV]: null },
        location: location === "panel" ? v.TerminalLocation.Panel : { viewColumn: v.ViewColumn.Active, preserveFocus: true },
      });
      const entry = { id, name: id, terminal, pid: null, location: location === "panel" ? "panel" : "editor", exited: false };
      this.entries.set(id, entry);
      this.byTerminal.set(terminal, entry);
      if (show && location === "panel") terminal.show(true);
      created.push(entry);
    }
    await Promise.all(
      created.map(async (e) => {
        const pid = await withTimeout(e.terminal.processId, this.processIdMs, undefined);
        e.pid = Number.isInteger(pid) ? pid : null;
      }),
    );
    this.log(`spawned ${created.map((e) => `${e.id}(pid ${e.pid})`).join(", ")} in ${location}`);
    return { terminals: created.map((e) => ({ id: e.id, name: e.name, pid: e.pid, location: e.location })) };
  }

  /** Make `entry` the active terminal (showing it), waiting for VS Code to agree. */
  async activate(entry, preserveFocus) {
    entry.terminal.show(preserveFocus);
    return this.wait(() => this.vscode.window.activeTerminal === entry.terminal, 3000);
  }

  /** Move every terminal editor back into the panel; one-group layout afterwards. */
  async toPanel(failures) {
    for (const entry of this.entries.values()) {
      if (entry.exited) continue;
      const inEditor = entry.location === "editor" || this.editorColumn(entry) !== null;
      if (!inEditor) continue;
      const before = this.terminalTabs().length;
      if (!(await this.activate(entry, false))) {
        failures.push(`${entry.id}: did not become the active terminal before moving to the panel`);
        continue;
      }
      const r = await this.exec(CMD.moveToPanel);
      const moved = r.ok && (await this.wait(() => this.terminalTabs().length === before - 1 && this.editorColumn(entry) === null));
      if (moved) entry.location = "panel";
      else failures.push(`${entry.id}: ${r.ok ? "still an editor tab after moveToTerminalPanel" : r.error}`);
    }
    await this.exec(CMD.setLayout, SINGLE_GROUP);
  }

  /** Focus editor group `g` (1-based, grid order) of `total`, read back. */
  async focusGroup(g, total) {
    const id = g <= CMD.focusGroup.length ? CMD.focusGroup[g - 1] : g === total ? CMD.focusLastGroup : null;
    if (!id) return { ok: false, error: `no command focuses group ${g} of ${total}` };
    const r = await this.exec(id);
    if (!r.ok) return r;
    const ok = await this.wait(() => this.vscode.window.tabGroups.activeTabGroup?.viewColumn === g, 2000);
    return ok ? { ok: true } : { ok: false, error: `group ${g} did not become active after ${id}` };
  }

  async arrange({ arm, order, maxTiled }) {
    for (const id of order) this.require(id);
    const plan = protocol.planArrangement({ arm, order, maxTiled });
    return arm === "focus" ? this.arrangeFocus(plan) : this.arrangeTiled(plan);
  }

  async arrangeFocus(plan) {
    const failures = [];
    await this.toPanel(failures);
    await this.exec(CMD.closePanel); // hidden and, if it was maximized, restored first
    await this.closeChrome();
    const focus = this.require(plan.focus);
    if (!(await this.activate(focus, false))) failures.push(`${focus.id} did not become the active terminal`);
    const max = await this.exec(CMD.toggleMaxPanel);
    if (!max.ok) failures.push(`maximizing the panel failed: ${max.error}`);
    this.log(`arranged focus on ${focus.id}${failures.length ? ` with failures: ${failures.join("; ")}` : ""}`);
    return { arm: plan.arm, ok: failures.length === 0, visible: plan.visible, hidden: plan.hidden, focus: plan.focus, groups: [], failures, notes: [] };
  }

  async arrangeTiled(plan) {
    const failures = [];
    const notes = [];
    await this.toPanel(failures);
    await this.exec(CMD.closePanel);
    await this.closeChrome();
    if (this.terminalTabs().length === 0) await this.exec(CMD.closeAllEditors);
    const v = plan.visible.length;
    const layout = await this.exec(CMD.setLayout, plan.grid.layout);
    if (!layout.ok || !(await this.wait(() => this.vscode.window.tabGroups.all.length === v, 3000))) {
      failures.push(`the ${plan.grid.rows.join("+")} grid was not built: ${layout.error ?? `${this.vscode.window.tabGroups.all.length} group(s)`}`);
    }
    const groups = [];
    for (let g = 1; g <= v && failures.length === 0; g += 1) {
      const entry = this.require(plan.visible[g - 1]);
      const fg = await this.focusGroup(g, v);
      if (!fg.ok) {
        failures.push(fg.error);
        break;
      }
      const countIn = () => this.terminalTabs().filter((t) => t.viewColumn === g).length;
      const before = countIn();
      if (!(await this.activate(entry, true))) {
        failures.push(`${entry.id} did not become the active terminal before its move to group ${g}`);
        break;
      }
      const r = await this.exec(CMD.moveToEditor);
      const landed = r.ok && (await this.wait(() => countIn() === before + 1));
      if (!landed) {
        failures.push(`${entry.id}: ${r.ok ? `no terminal tab arrived in group ${g}` : r.error}`);
        break;
      }
      entry.location = "editor";
      const byLabel = this.editorColumn(entry);
      const activeGroup = this.vscode.window.tabGroups.activeTabGroup;
      if (byLabel === g) {
        groups.push({ viewColumn: g, id: entry.id, matched: "tab_label" });
      } else if (
        byLabel === null &&
        this.vscode.window.activeTerminal === entry.terminal &&
        activeGroup?.viewColumn === g &&
        activeGroup.activeTab?.input instanceof this.vscode.TabInputTerminal
      ) {
        groups.push({ viewColumn: g, id: entry.id, matched: "active_terminal" });
        notes.push(`${entry.id}: its tab label is not its name; matched as the active terminal of group ${g}`);
      } else {
        failures.push(`${entry.id} landed in group ${byLabel ?? "?"}, not ${g}`);
        break;
      }
    }
    await this.exec(CMD.closePanel);
    const focus = this.require(plan.focus);
    if (failures.length === 0 && !(await this.activate(focus, false))) failures.push(`${focus.id} did not take focus`);
    this.log(`arranged tiled ${plan.grid.rows.join("+")} over ${v} pane(s)${failures.length ? ` with failures: ${failures.join("; ")}` : ""}`);
    return { arm: plan.arm, ok: failures.length === 0, visible: plan.visible, hidden: plan.hidden, focus: plan.focus, rows: plan.grid.rows, groups, failures, notes };
  }

  show({ id, preserveFocus }) {
    const entry = this.require(id);
    entry.terminal.show(preserveFocus);
    return { id, active: this.vscode.window.activeTerminal === entry.terminal };
  }

  async focus({ id }) {
    const entry = this.require(id);
    const active = await this.activate(entry, false);
    return { id, active };
  }

  /** Called on `window.onDidCloseTerminal`. */
  onClosed(terminal) {
    const entry = this.byTerminal.get(terminal);
    if (!entry) return;
    entry.exited = true;
    const waiter = this.closedWaiters.get(entry.id);
    if (waiter) waiter();
  }

  async close(args) {
    const targets = args.all ? [...this.entries.values()].filter((e) => !e.exited) : [this.require(args.id)];
    const closed = [];
    const timedOut = [];
    await Promise.all(
      targets.map(async (entry) => {
        const gone = new Promise((r) => this.closedWaiters.set(entry.id, r));
        entry.terminal.dispose();
        const ok = await withTimeout(gone.then(() => true), this.closeMs, false);
        this.closedWaiters.delete(entry.id);
        if (ok) closed.push(entry.id);
        else timedOut.push(entry.id);
        entry.exited = true;
      }),
    );
    return { closed: closed.sort(), timedOut: timedOut.sort() };
  }

  list() {
    return {
      terminals: [...this.entries.values()].map((e) => ({ id: e.id, pid: e.pid, location: e.location, exited: e.exited })),
      active: this.byTerminal.get(this.vscode.window.activeTerminal)?.id ?? null,
    };
  }

  quit() {
    setTimeout(() => {
      this.exec(CMD.quit);
    }, 100);
    return { quitting: true };
  }
}

/** Write `record` to `file` atomically (temp file + rename), readable by the user only. */
function writeAtomic(file, text) {
  const tmp = `${file}.tmp-${process.pid}`;
  fs.writeFileSync(tmp, text, { mode: 0o600 });
  fs.renameSync(tmp, file);
}

/**
 * The control endpoint: 127.0.0.1, an ephemeral port, `POST /rpc` only, every
 * request authenticated by `protocol.parseRpc`, executed one at a time.
 *
 * @returns {Promise<{server: http.Server, port: number}>}
 */
function listen({ controller, nonce, log = () => {} }) {
  let port = null;
  let queue = Promise.resolve();
  const send = (res, { status, body }) => {
    const text = JSON.stringify(body);
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", "content-length": Buffer.byteLength(text) });
    res.end(text);
  };
  const server = http.createServer((req, res) => {
    const chunks = [];
    let size = 0;
    req.on("data", (c) => {
      size += c.length;
      if (size <= protocol.MAX_BODY_BYTES) chunks.push(c);
    });
    req.on("end", () => {
      if (size > protocol.MAX_BODY_BYTES) {
        send(res, protocol.errorResponse(413, "too_large", `body over ${protocol.MAX_BODY_BYTES} bytes`));
        return;
      }
      const parsed = protocol.parseRpc({ method: req.method, url: req.url, headers: req.headers, body: Buffer.concat(chunks).toString("utf8") }, { port, nonce });
      if (!parsed.ok) {
        log(`refused ${req.method} ${req.url}: ${parsed.code}`);
        send(res, protocol.errorResponse(parsed.status, parsed.code, parsed.message));
        return;
      }
      queue = queue
        .then(() => controller.handle(parsed.cmd, parsed.args))
        .then(
          (result) => send(res, protocol.okResponse(result)),
          (err) => {
            log(`${parsed.cmd} failed: ${err?.message ?? err}`);
            send(res, protocol.errorResponse(err?.status ?? 500, err?.code ?? "internal", err?.message ?? String(err)));
          },
        );
    });
    req.on("error", () => {});
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      port = server.address().port;
      resolve({ server, port });
    });
  });
}

/**
 * Start the harness when the environment names a config (the adapter's launch);
 * otherwise do nothing — the extension never opens an endpoint on its own.
 *
 * @param {{vscode: object, env?: object, timing?: object}} o
 * @returns {Promise<{port: number, server: http.Server, controller: HarnessController, dispose: () => Promise<void>} | null>}
 */
async function startHarness({ vscode, env = process.env, timing }) {
  const configPath = env[protocol.CONFIG_ENV];
  if (!configPath) return null;
  const config = protocol.parseConfig(fs.readFileSync(configPath, "utf8"));
  const log = (line) => {
    if (!config.logFile) return;
    try {
      fs.appendFileSync(config.logFile, `${new Date().toISOString()} ${line}\n`);
    } catch {
      // logging never fails the harness
    }
  };
  const controller = new HarnessController({ vscode, config, log, timing });
  const { server, port } = await listen({ controller, nonce: config.nonce, log });
  writeAtomic(
    config.controlFile,
    `${JSON.stringify(protocol.controlRecord({ port, pid: process.pid, ppid: process.ppid, vscodeVersion: vscode.version, startedAt: new Date().toISOString() }))}\n`,
  );
  log(`listening on 127.0.0.1:${port}; control file ${path.basename(config.controlFile)}`);
  return {
    port,
    server,
    controller,
    dispose: () =>
      new Promise((resolve) => {
        controller.dispose();
        server.close(() => resolve());
      }),
  };
}

module.exports = { CMD, SINGLE_GROUP, HarnessError, HarnessController, waitFor, listen, startHarness };
