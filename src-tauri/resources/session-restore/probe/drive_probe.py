#!/usr/bin/env python3
"""Phase 1 probe driver — runs throwaway interactive `claude` sessions in a PTY.

Plan: `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
Phase 1. Hand-run only. It never starts, stops or talks to a qontinui runner;
it spawns `claude` directly, in a PTY it owns, and kills only that child.

Requires `pyte` (a VT100 screen emulator) to turn the PTY byte stream into the
rendered screen text the footer-hint questions are about. A throwaway venv is
enough: `python3 -m venv <scratch>/venv && <scratch>/venv/bin/pip install pyte`,
then run this file with that interpreter.

Usage:

    drive_probe.py <scenario> --claude <path> --out <scratch-dir> [--cwd <dir>]

Scenarios (each is ONE short model interaction at most, `--model haiku`):

* `footer:<variant>` — variant `none` (hooks, no statusLine), `sl_empty`
  (statusLine printing nothing), `sl_text` (statusLine printing a marker after
  a 1.5 s sleep, to provoke the CLI's cancel-in-flight path). Idle screen, a
  Bash tool call that needs permission (answered "yes"), the mid-turn screen
  while `sleep 8` runs, the post-turn idle screen, then `/exit`.
* `bypass:<variant>` — `--resume` the session the footer run created, under
  `--dangerously-skip-permissions`; idle screen, then a tool call.
* `askq` — a prompt asking for exactly one `AskUserQuestion`, answered with
  Enter, then `/clear`, then `/exit`.
* `plan` — `--permission-mode plan`, a prompt asking for `ExitPlanMode`, the
  dialog is declined with Esc, then `/exit`.
* `compact:<variant>` — shrink the auto-compact window so one trivial turn
  lands in the context-low band, then read the footer for
  `until auto-compact:`.
* `cancel` — no model call: boot with a slow statusLine and time how long a
  run the CLI cancels survives its SIGTERM.
* `precedence` — a project-level `.claude/settings.json` statusLine in `--cwd`
  and a `--settings` statusLine; whichever renders/runs wins.

Every screen snapshot is appended to `<out>/screens.jsonl` with the four footer
hints' presence precomputed. Snapshots may contain prompt text: they stay in
the scratch dir and are NEVER committed; only the recorder's type skeletons
become fixtures.
"""

import argparse
import json
import os
import re
import select
import signal
import sys
import threading
import time
import uuid

import pyte

HINTS = {
    "esc to interrupt": re.compile(r"esc to interrupt", re.I),
    "? for shortcuts": re.compile(r"\? for shortcuts"),
    "bypass permissions": re.compile(r"bypass permissions", re.I),
    "until auto-compact:": re.compile(r"until auto-compact:", re.I),
}
PROBE_DIR = os.path.dirname(os.path.abspath(__file__))
COLS, ROWS = 140, 45


class PtySession:
    def __init__(self, argv, env, cwd, out_dir, label):
        self.label = label
        self.out_dir = out_dir
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.lock = threading.Lock()
        self.alive = True
        # The CLI's native binary is rewritten in place by its auto-updater
        # (observed twice during the 2.1.285 probe): exec then fails with
        # ETXTBSY. Retry the spawn instead of recording an empty session.
        for attempt in range(6):
            pid, fd = os.forkpty()
            if pid == 0:  # child
                os.chdir(cwd)
                os.environ.clear()
                os.environ.update(env)
                try:
                    os.execv(argv[0], argv)
                finally:
                    os._exit(127)
            time.sleep(1.5)
            done, status = os.waitpid(pid, os.WNOHANG)
            if done and os.waitstatus_to_exitcode(status) == 127:
                os.close(fd)
                print("[%s] exec failed (attempt %d), retrying" % (label, attempt + 1), flush=True)
                time.sleep(5)
                continue
            break
        self.pid, self.fd = pid, fd
        import fcntl
        import struct
        import termios

        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        while self.alive:
            try:
                ready, _, _ = select.select([self.fd], [], [], 0.2)
                if not ready:
                    continue
                data = os.read(self.fd, 65536)
            except OSError:
                break
            if not data:
                break
            with self.lock:
                self.stream.feed(data)
            with open(os.path.join(self.out_dir, "raw-%s.bin" % self.label.replace(":", "_")), "ab") as raw:
                raw.write(data)
        self.alive = False

    def text(self):
        with self.lock:
            return "\n".join(line.rstrip() for line in self.screen.display)

    def hints(self, text=None):
        text = self.text() if text is None else text
        return {name: bool(rx.search(text)) for name, rx in HINTS.items()}

    def snap(self, tag):
        text = self.text()
        record = {"label": self.label, "tag": tag, "ts": time.time(), "hints": self.hints(text), "screen": text.split("\n")}
        with open(os.path.join(self.out_dir, "screens.jsonl"), "a", encoding="utf-8") as handle:
            handle.write(json.dumps(record) + "\n")
        print("[%s] %-22s %s" % (self.label, tag, " ".join("%s=%d" % (k, v) for k, v in record["hints"].items())), flush=True)
        return record

    def send(self, data):
        os.write(self.fd, data.encode() if isinstance(data, str) else data)

    def type_line(self, line):
        self.send(line)
        time.sleep(0.6)
        self.send("\r")

    def wait_for(self, pattern, timeout, poll=0.25):
        rx = re.compile(pattern, re.I | re.S)
        deadline = time.time() + timeout
        while time.time() < deadline:
            if rx.search(self.text()):
                return True
            if not self.alive:
                return False
            time.sleep(poll)
        return False

    def watch(self, seconds, tag, poll=0.5):
        """Sample the screen for `seconds`; record the UNION and INTERSECTION of hints."""
        seen_any = {k: False for k in HINTS}
        samples = 0
        deadline = time.time() + seconds
        while time.time() < deadline and self.alive:
            for key, present in self.hints().items():
                seen_any[key] = seen_any[key] or present
            samples += 1
            time.sleep(poll)
        record = {"label": self.label, "tag": tag, "ts": time.time(), "samples": samples, "hints_any": seen_any}
        with open(os.path.join(self.out_dir, "screens.jsonl"), "a", encoding="utf-8") as handle:
            handle.write(json.dumps(record) + "\n")
        print("[%s] %-22s any: %s" % (self.label, tag, seen_any), flush=True)
        return seen_any

    def close(self, grace=15):
        deadline = time.time() + grace
        while time.time() < deadline:
            pid, _ = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.alive = False
                return "exited"
            time.sleep(0.25)
        os.kill(self.pid, signal.SIGTERM)  # only the child this driver spawned
        time.sleep(2)
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(self.pid, 0)
        self.alive = False
        return "killed"


def render_settings(out_dir, variant, http_port):
    with open(os.path.join(PROBE_DIR, "probe_settings.json"), encoding="utf-8") as handle:
        text = handle.read()
    text = text.replace("@@RECORDER@@", os.path.join(PROBE_DIR, "recorder.py")).replace("@@HTTP_PORT@@", str(http_port))
    settings = json.loads(text)
    if variant == "none":
        settings.pop("statusLine", None)
    path = os.path.join(out_dir, "settings-%s.json" % variant)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(settings, handle, indent=2)
    return path


def base_env(out_dir, variant):
    # A probe launched from inside another Claude Code session inherits that
    # session's child markers (CLAUDECODE, CLAUDE_CODE_CHILD_SESSION, ...), which
    # switch transcript saving off and so break `--resume`. Strip every one of
    # them; CLAUDE_CONFIG_DIR (the operator's existing account) is kept as-is.
    env = {k: v for k, v in os.environ.items() if not (k == "CLAUDECODE" or k.startswith("CLAUDE_CODE_") or k in ("CLAUDE_PID", "CLAUDE_EFFORT"))}
    env["PROBE_OUT"] = os.path.join(out_dir, "events.jsonl")
    env["TERM"] = "xterm-256color"
    env.pop("PROBE_SL_TEXT", None)
    env.pop("PROBE_SL_SLEEP", None)
    if variant == "sl_text":
        env["PROBE_SL_TEXT"] = "PROBE-SL-MARKER"
        env["PROBE_SL_SLEEP"] = "1.5"
    return env


def mark(out_dir, label, what):
    with open(os.path.join(out_dir, "events.jsonl"), "a", encoding="utf-8") as handle:
        handle.write(json.dumps({"mode": "driver", "label": label, "what": what, "ts": time.time()}) + "\n")


def boot(session, out_dir):
    """Get past a first-run trust dialog, then wait for the idle input box."""
    session.wait_for(r"Yes, I trust this folder|for shortcuts|bypass permissions|for agents|mode on", 25)
    time.sleep(1)
    if re.search(r"Yes, I trust this folder", session.text(), re.I):
        # The default selection is "No, exit": move to "Yes" before confirming.
        session.snap("trust-dialog")
        session.send("\x1b[B")
        time.sleep(0.5)
        session.send("\r")
        time.sleep(3)
    session.wait_for(r"for shortcuts|bypass permissions|for agents|mode on", 30)
    time.sleep(2)
    return session.snap("idle-boot")


def turn_ended_since(out_dir, since):
    """True once the recorder logged a Stop or StopFailure after `since`.

    The hook event, not the screen, decides that a turn is over — the whole
    point of the plan being probed.
    """
    try:
        with open(os.path.join(out_dir, "events.jsonl"), encoding="utf-8") as handle:
            for line in handle:
                record = json.loads(line)
                if record.get("mode") == "hook" and record.get("event") in ("Stop", "StopFailure") and record.get("ts", 0) > since:
                    return True
    except (OSError, ValueError):
        pass
    return False


def run_turn(s, out_dir, prompt, dialog_rx=None, dialog_keys=("\r",), max_s=150, dialog_wait=1.0):
    """Send `prompt`, sample the screen until the turn's Stop/StopFailure hook.

    Records the first snapshot in which each footer hint appears, a snapshot
    every ~2 s while the turn runs, and the union of hints seen mid-turn. A
    dialog matching `dialog_rx` is snapshotted and answered with `dialog_keys`
    (at most three times).
    """
    label = s.label
    mark(out_dir, label, "prompt")
    started = time.time()
    s.type_line(prompt)
    union = {k: False for k in HINTS}
    dialogs = 0
    next_snap = time.time() + 1.0
    seq = 0
    while time.time() - started < max_s and s.alive:
        text = s.text()
        hints = s.hints(text)
        for key, present in hints.items():
            if present and not union[key]:
                s.snap("turn-first:" + key)
            union[key] = union[key] or present
        if dialog_rx and dialogs < 3 and re.search(dialog_rx, text, re.I | re.S):
            # --dialog-wait: leave the dialog up long enough for a delayed
            # `Notification` to fire (the Notification-vs-PermissionRequest
            # duplication question).
            time.sleep(dialog_wait)
            s.snap("dialog-%d" % dialogs)
            mark(out_dir, label, "dialog-%d" % dialogs)
            for key in dialog_keys:
                s.send(key)
                time.sleep(0.8)
            dialogs += 1
            time.sleep(1.5)
            continue
        if time.time() >= next_snap and seq < 8:
            s.snap("turn-%d" % seq)
            seq += 1
            next_snap = time.time() + 2.0
        if turn_ended_since(out_dir, started):
            break
        time.sleep(0.2)
    mark(out_dir, label, "turn-ended")
    record = {"label": label, "tag": "turn-union", "ts": time.time(), "hints_any": union, "dialogs": dialogs, "wall_s": time.time() - started}
    with open(os.path.join(out_dir, "screens.jsonl"), "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record) + "\n")
    print("[%s] %-22s any: %s dialogs=%d" % (label, "turn-union", union, dialogs), flush=True)
    time.sleep(3)
    s.snap("idle-after-turn")
    s.watch(4, "idle-after-watch")
    return union


def finish(s, out_dir):
    mark(out_dir, s.label, "exit")
    s.type_line("/exit")
    print("[%s] close: %s" % (s.label, s.close()), flush=True)


def claude_argv(args, settings, extra):
    # --strict-mcp-config with no --mcp-config: no MCP server from any .mcp.json
    # up the tree, so no "new MCP server" dialog swallows the first keystroke.
    return [args.claude, "--model", "haiku", "--strict-mcp-config", "--settings", settings] + extra


PERMISSION_DIALOG = r"Do you want to (proceed|make this edit|create)"


def scenario_footer(args, variant):
    label = "footer:" + variant
    settings = render_settings(args.out, variant, args.http_port)
    sid = str(uuid.uuid4())
    with open(os.path.join(args.out, "session-%s.id" % variant), "w", encoding="utf-8") as handle:
        handle.write(sid)
    mark(args.out, label, "spawn")
    s = PtySession(claude_argv(args, settings, ["--session-id", sid]), base_env(args.out, variant), args.cwd, args.out, label)
    boot(s, args.out)
    s.watch(4, "idle-watch")
    # `touch` is a write, so the default mode asks; `sleep 8` keeps the turn busy.
    run_turn(s, args.out, "Use the Bash tool to run exactly this one command: touch probe-marker.txt && sleep 8 ; then reply with the word done.", PERMISSION_DIALOG, dialog_wait=args.dialog_wait)
    finish(s, args.out)


def scenario_bypass(args, variant):
    label = "bypass:" + variant
    settings = render_settings(args.out, variant, args.http_port)
    with open(os.path.join(args.out, "session-%s.id" % variant), encoding="utf-8") as handle:
        sid = handle.read().strip()
    mark(args.out, label, "spawn-resume")
    env = base_env(args.out, variant)
    # Q5: a 6 s async sleeper on UserPromptSubmit/Stop. If the turn's Stop hook
    # lands before the UserPromptSubmit sleeper's `end`, `async: true` held.
    env["PROBE_ASYNC_SLEEP"] = "6"
    s = PtySession(claude_argv(args, settings, ["--resume", sid, "--dangerously-skip-permissions"]), env, args.cwd, args.out, label)
    boot(s, args.out)
    s.watch(4, "idle-watch")
    if args.prompt:
        run_turn(s, args.out, "Use the Bash tool to run exactly this one command: touch probe-marker-2.txt && sleep 5 ; then reply ok.", PERMISSION_DIALOG)
    finish(s, args.out)


def scenario_askq(args):
    label = "askq"
    settings = render_settings(args.out, "sl_empty", args.http_port)
    mark(args.out, label, "spawn")
    s = PtySession(claude_argv(args, settings, []), base_env(args.out, "sl_empty"), args.cwd, args.out, label)
    boot(s, args.out)
    run_turn(s, args.out, "Call the AskUserQuestion tool exactly once, asking me to pick option A or option B. Do nothing else.", r"Enter to select", ("\r",), dialog_wait=args.dialog_wait)
    mark(args.out, label, "clear")
    s.type_line("/clear")
    time.sleep(5)
    s.snap("after-clear")
    finish(s, args.out)


def scenario_plan(args):
    label = "plan"
    settings = render_settings(args.out, "sl_empty", args.http_port)
    mark(args.out, label, "spawn")
    s = PtySession(claude_argv(args, settings, ["--permission-mode", "plan"]), base_env(args.out, "sl_empty"), args.cwd, args.out, label)
    boot(s, args.out)
    run_turn(s, args.out, "Do not read any files. Immediately call the ExitPlanMode tool with the plan text: noop.", r"Exit plan mode\?|Would you like to proceed|ready to code", ("\x1b",), dialog_wait=args.dialog_wait)
    finish(s, args.out)


def scenario_precedence(args):
    label = "precedence"
    settings = render_settings(args.out, "sl_text", args.http_port)
    project = os.path.join(args.cwd, ".claude")
    os.makedirs(project, exist_ok=True)
    with open(os.path.join(project, "settings.json"), "w", encoding="utf-8") as handle:
        json.dump({"statusLine": {"type": "command", "command": "printf PROJECT-LEVEL-SL"}}, handle)
    mark(args.out, label, "spawn")
    env = base_env(args.out, "sl_text")
    env["PROBE_SL_SLEEP"] = "1.5"
    env["PROBE_SL_TERM_GRACE"] = "3"
    env["PROBE_SL_TEXT"] = "FLAG-LEVEL-SL"
    s = PtySession(claude_argv(args, settings, []), env, args.cwd, args.out, label)
    boot(s, args.out)
    run_turn(s, args.out, "Reply with the single word: ok")
    text = s.text()
    print("[precedence] flag=%s project=%s" % ("FLAG-LEVEL-SL" in text, "PROJECT-LEVEL-SL" in text), flush=True)
    finish(s, args.out)


def scenario_compact(args, variant):
    """Q1's fourth hint. `until auto-compact:` renders only near the compaction
    threshold; instead of filling ~150k tokens of real context, shrink the
    window the CLI compacts against (`--compact-env`, default
    `CLAUDE_CODE_MAX_CONTEXT_TOKENS`) so one trivial turn lands inside the
    warning band."""
    label = "compact:" + variant
    settings = render_settings(args.out, variant, args.http_port)
    env = base_env(args.out, variant)
    env[args.compact_env] = str(args.compact_window)
    mark(args.out, label, "spawn")
    s = PtySession(claude_argv(args, settings, []), env, args.cwd, args.out, label)
    boot(s, args.out)
    run_turn(s, args.out, "Reply with the single word: ok")
    finish(s, args.out)


def scenario_cancel(args):
    """No model call: boot with a slow statusLine, which the CLI re-runs at
    startup and so cancels at least once; the recorder's `survived` ticks time
    the gap between the CLI's SIGTERM and whatever ends the process."""
    label = "cancel"
    settings = render_settings(args.out, "sl_text", args.http_port)
    env = base_env(args.out, "sl_text")
    env["PROBE_SL_SLEEP"] = "1.5"
    env["PROBE_SL_TERM_GRACE"] = "10"
    mark(args.out, label, "spawn")
    s = PtySession(claude_argv(args, settings, []), env, args.cwd, args.out, label)
    boot(s, args.out)
    time.sleep(12)
    finish(s, args.out)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("scenario")
    parser.add_argument("--claude", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--cwd", required=True)
    parser.add_argument("--http-port", type=int, default=18777)
    parser.add_argument("--prompt", action="store_true", help="bypass: also run one tool call")
    parser.add_argument("--compact-window", type=int, default=40000, help="compact: the token value given to --compact-env")
    parser.add_argument("--compact-env", default="CLAUDE_CODE_MAX_CONTEXT_TOKENS", help="compact: which env knob shrinks the window (CLAUDE_CODE_AUTO_COMPACT_WINDOW had no visible effect on 2.1.285)")
    parser.add_argument("--dialog-wait", type=float, default=1.0, help="seconds a dialog is left up before it is answered")
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)
    name, _, variant = args.scenario.partition(":")
    {
        "footer": lambda: scenario_footer(args, variant),
        "bypass": lambda: scenario_bypass(args, variant),
        "askq": lambda: scenario_askq(args),
        "plan": lambda: scenario_plan(args),
        "precedence": lambda: scenario_precedence(args),
        "cancel": lambda: scenario_cancel(args),
        "compact": lambda: scenario_compact(args, variant),
    }[name]()


if __name__ == "__main__":
    sys.exit(main())
