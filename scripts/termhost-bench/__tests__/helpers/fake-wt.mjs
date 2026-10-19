/**
 * A simulated portable Windows Terminal for the adapter tests: it interprets the
 * `wt.exe` command lines the adapter runs, keeps a process table and the window,
 * and starts a fake session per pane (slot file with WT_SESSION, a session shell
 * and a generator process), so the whole launch -> arrange -> teardown flow runs
 * in Node without Windows.
 *
 * Behaviours copied from the pinned source (microsoft/terminal v1.25.2733.0),
 * not invented:
 *   - `;` as its own argument separates subcommands (`AppCommandlineArgs.cpp`
 *     `_commandDelimiterRegex`); options precede the positional command line;
 *   - pane ids count up per tab in creation order; a tab created from a moved
 *     pane numbers its root 0 (`Tab.cpp`);
 *   - `split-pane` splits the tab's ACTIVE pane, the new pane second and taking
 *     `--size` of it (default 0.5), and becomes active; with neither -V nor -H the
 *     longer side is split;
 *   - `move-pane -t i` resolves tab i BEFORE detaching the active pane; a tab left
 *     empty closes; an index past the end makes a new tab
 *     (`TerminalPage::_MovePane`); moving to the focused tab does nothing;
 *   - an unpackaged instance hands every later `wt.exe` to the process running
 *     from the same path (`WindowEmperor.cpp`); WM_CLOSE closes the window and,
 *     with `compatibility.allowHeadless` off, the process exits;
 *   - each pane's `WT_SESSION` is its `--sessionId` without braces, lowercase.
 *
 * `faults` breaks one behaviour so a read-back can be shown to catch it.
 */

import { writeFileSync } from "node:fs";
import { join } from "node:path";

const SUBCOMMANDS = new Map([
  ["new-tab", "new-tab"],
  ["nt", "new-tab"],
  ["split-pane", "split-pane"],
  ["sp", "split-pane"],
  ["focus-tab", "focus-tab"],
  ["ft", "focus-tab"],
  ["focus-pane", "focus-pane"],
  ["fp", "focus-pane"],
  ["move-pane", "move-pane"],
  ["mp", "move-pane"],
]);
const WITH_VALUE = new Set(["-p", "--profile", "--title", "--sessionId", "-s", "--size", "-t", "--target", "-d", "--startingDirectory"]);

/** Split one wt.exe argv into {window, global flags, subcommands}. */
export function parseWtArgv(argv) {
  const out = { window: null, global: [], commands: [] };
  let i = 0;
  while (i < argv.length && argv[i] !== ";" && !SUBCOMMANDS.has(argv[i])) {
    if (argv[i] === "-w" || argv[i] === "--window") {
      out.window = argv[i + 1];
      i += 2;
    } else out.global.push(argv[i++]);
  }
  let cur = null;
  for (; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === ";") {
      cur = null;
      continue;
    }
    if (!cur) {
      if (!SUBCOMMANDS.has(a)) throw new Error(`fake wt: unknown subcommand ${a}`);
      cur = { name: SUBCOMMANDS.get(a), opts: {}, flags: new Set(), cmd: [] };
      out.commands.push(cur);
      continue;
    }
    if (cur.cmd.length === 0 && WITH_VALUE.has(a)) cur.opts[a] = argv[++i];
    else if (cur.cmd.length === 0 && /^-[A-Z]$|^--(vertical|horizontal)$/.test(a)) cur.flags.add(a);
    else cur.cmd.push(a);
  }
  return out;
}

/**
 * @param {{appDir: string, genDir?: string, genRunId?: string, firstPid?: number,
 *   operator?: boolean, faults?: object}} o
 */
export function createFakeWt({ appDir, genDir = null, genRunId = "r1-u0", firstPid = 7000, operator = true, faults = {} } = {}) {
  let nextPid = firstPid;
  let clock = 1_000_000n;
  const tick = () => String((clock += 10n));
  const procs = new Map();
  const signalled = [];
  const invocations = [];
  const posted = [];
  const sessions = [];
  const addProc = (name, ppid, extra = {}) => {
    const pid = nextPid++;
    procs.set(pid, { pid, ppid, name, argv: null, cmdline: extra.cmdline ?? name, startKey: tick(), exe: extra.exe ?? "" });
    return pid;
  };
  // The operator's own Windows Terminal: same image name, another path, started first.
  const operatorPid = operator ? addProc("WindowsTerminal.exe", 4, { exe: "C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal_1.25\\WindowsTerminal.exe" }) : null;
  const exe = join(appDir, "WindowsTerminal.exe");
  const state = { pid: null, hwnd: 0x5000, windowOpen: false, tabs: [], active: 0 };
  let slot = 0;

  const rect = { x: 0, y: 0, w: 1920, h: 1080 };
  const leaf = (id, sessionId) => ({ id, sessionId });
  const rects = (node, r, out) => {
    if (node.id !== undefined) out.set(node.id, r);
    else if (node.dir === "V") {
      const w1 = r.w * (1 - node.size);
      rects(node.a, { ...r, w: w1 }, out);
      rects(node.b, { ...r, x: r.x + w1, w: r.w - w1 }, out);
    } else {
      const h1 = r.h * (1 - node.size);
      rects(node.a, { ...r, h: h1 }, out);
      rects(node.b, { ...r, y: r.y + h1, h: r.h - h1 }, out);
    }
    return out;
  };
  const replaceLeaf = (node, id, repl) => {
    if (node.id !== undefined) return node.id === id ? repl : node;
    return { ...node, a: replaceLeaf(node.a, id, repl), b: replaceLeaf(node.b, id, repl) };
  };
  const removeLeaf = (node, id) => {
    if (node.id !== undefined) return node.id === id ? null : node;
    const a = removeLeaf(node.a, id);
    const b = removeLeaf(node.b, id);
    if (!a) return b;
    if (!b) return a;
    return { ...node, a, b };
  };
  const leaves = (node, out = []) => {
    if (node.id !== undefined) out.push(node);
    else {
      leaves(node.a, out);
      leaves(node.b, out);
    }
    return out;
  };
  const splitActive = (tab, newLeaf, dir, size) => {
    const r = rects(tab.root, rect, new Map()).get(tab.activeId);
    const d = dir ?? (r.w >= r.h ? "V" : "H");
    tab.root = replaceLeaf(tab.root, tab.activeId, { dir: d, size, a: leaf(tab.activeId, findLeaf(tab.root, tab.activeId).sessionId), b: newLeaf });
    tab.activeId = newLeaf.id;
  };
  const findLeaf = (node, id) => leaves(node).find((l) => l.id === id);

  /** A session: shell (child of the instance) and generator, and the slot file. */
  const startSession = (sessionIdArg, cmd, title) => {
    const sid = sessionIdArg ? sessionIdArg.replace(/[{}]/g, "").toLowerCase() : `auto-${slot}`;
    const shell = addProc("powershell.exe", state.pid, { cmdline: cmd.join(" ") });
    const gen = addProc("node.exe", shell, { cmdline: `node ai-session.mjs --termhost-bench-generator ${genRunId}` });
    const k = slot++;
    if (genDir && !faults.noSlots) {
      writeFileSync(
        join(genDir, `slot-${k}`),
        `${JSON.stringify({ schema: "termhost-bench/slot/v1", slot: k, run_id: genRunId, pid: gen, ppid: shell, host_terminal_id: { WT_SESSION: sid }, claimed_at_ns: String(process.hrtime.bigint()), argv: [] })}\n`,
      );
    }
    sessions.push({ sid, shell, gen, cmd, title, slot: k });
    return sid;
  };

  const apply = (c) => {
    const tab = state.tabs[state.active];
    switch (c.name) {
      case "new-tab": {
        const sid = startSession(c.opts["--sessionId"], c.cmd, c.opts["--title"]);
        state.tabs.push({ root: leaf(0, sid), nextId: 1, activeId: 0, profile: c.opts["-p"] });
        state.active = state.tabs.length - 1;
        break;
      }
      case "split-pane": {
        const sid = startSession(c.opts["--sessionId"], c.cmd, c.opts["--title"]);
        const dir = c.flags.has("-V") ? "V" : c.flags.has("-H") ? "H" : null;
        const size = c.opts["--size"] ? Number(c.opts["--size"]) : 0.5;
        const id = tab.nextId++;
        splitActive(tab, leaf(id, sid), dir, size);
        break;
      }
      case "focus-tab":
        state.active = Number(c.opts["-t"]);
        break;
      case "focus-pane":
        if (faults.ignoreFocusPane) break;
        if (findLeaf(tab.root, Number(c.opts["-t"]))) tab.activeId = Number(c.opts["-t"]);
        break;
      case "move-pane": {
        const idx = Number(c.opts["-t"]);
        if (idx === state.active) break;
        const target = idx < state.tabs.length ? state.tabs[idx] : null;
        const moving = findLeaf(tab.root, tab.activeId);
        const rest = removeLeaf(tab.root, tab.activeId);
        if (rest) {
          tab.root = rest;
          tab.activeId = leaves(rest)[0].id;
        } else state.tabs.splice(state.active, 1);
        if (target) {
          const id = target.nextId++;
          splitActive(target, leaf(id, moving.sessionId), null, 0.5);
          state.active = state.tabs.indexOf(target);
        } else {
          state.tabs.push({ root: leaf(0, moving.sessionId), nextId: 1, activeId: 0 });
          state.active = state.tabs.length - 1;
        }
        break;
      }
      default:
        throw new Error(`fake wt: ${c.name}`);
    }
  };

  /** spawnSync stand-in: wt.exe runs; powershell.exe queries are answered elsewhere. */
  const run = (cmd, args) => {
    if (!String(cmd).endsWith("wt.exe")) return { status: 0, stdout: "", stderr: "" };
    invocations.push(args);
    const p = parseWtArgv(args);
    if (!state.pid) {
      if (faults.handoffToOperator) return { status: 0, stdout: "" };
      state.pid = addProc("WindowsTerminal.exe", nextPid + 1000, { exe });
      addProc("OpenConsole.exe", state.pid);
      state.windowOpen = true;
      state.window = p.window;
      state.maximized = p.global.includes("-M");
    } else if (p.window !== state.window) throw new Error(`fake wt: window ${p.window} is not ${state.window}`);
    for (const c of p.commands) apply(c);
    return { status: 0, stdout: "", stderr: "" };
  };

  const closeWindow = () => {
    state.windowOpen = false;
    const gone = [state.pid, ...[...procs.values()].filter((x) => x.ppid === state.pid).map((x) => x.pid)];
    for (const pid of gone) procs.delete(pid);
  };

  return {
    run,
    state,
    procs,
    sessions,
    invocations,
    posted,
    signalled,
    operatorPid,
    exe,
    list: () => new Map([...procs].map(([k, v]) => [k, { ...v }])),
    kill: (pid, sig) => {
      signalled.push([pid, sig]);
      const p = procs.get(pid);
      if (!p) {
        const e = new Error("ESRCH");
        e.code = "ESRCH";
        throw e;
      }
      if (faults.unkillable?.includes(p.name)) return;
      procs.delete(pid);
      if (pid === state.pid) state.windowOpen = false;
      // A killed session shell closes its pane (the spawned profile's closeOnExit: always).
      for (const t of state.tabs) {
        for (const l of leaves(t.root)) {
          const s = sessions.find((x) => x.sid === l.sessionId);
          if (s && s.shell === pid) {
            const rest = removeLeaf(t.root, l.id);
            if (rest) t.root = rest;
            else state.tabs.splice(state.tabs.indexOf(t), 1);
          }
        }
      }
    },
    queryWt: () => [...procs.values()].filter((p) => p.name === "WindowsTerminal.exe").map((p) => ({ pid: p.pid, ppid: p.ppid, exe: p.exe, startKey: p.startKey })),
    queryWindows: (pid) => (pid === state.pid && state.windowOpen ? [{ hwnd: state.hwnd, visible: true, cls: "CASCADIA_HOSTING_WINDOW_CLASS", title: "s0" }] : pid === operatorPid ? [{ hwnd: 0x9000, visible: true, cls: "CASCADIA_HOSTING_WINDOW_CLASS", title: "operator" }] : []),
    postClose: (pid, hwnd, startKey) => {
      posted.push([pid, hwnd]);
      // The script's own checks: owner, class, and the PID still the proven process.
      if (pid !== state.pid || hwnd !== state.hwnd || !state.windowOpen || procs.get(pid)?.startKey !== startKey) return "mismatch";
      if (!faults.ignoreClose) closeWindow();
      return "posted";
    },
    /** Each tab's panes: [{id, sid, rect}] in id order. */
    layout: () =>
      state.tabs.map((t) => {
        const r = rects(t.root, rect, new Map());
        return leaves(t.root)
          .map((l) => ({ id: l.id, sid: l.sessionId, rect: r.get(l.id) }))
          .sort((a, b) => a.id - b.id);
      }),
    endGenerators: () => {
      for (const s of sessions) procs.delete(s.gen);
    },
  };
}
