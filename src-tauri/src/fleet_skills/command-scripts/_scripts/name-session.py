#!/usr/bin/env python3
"""Set THIS Claude session's display name: type `/rename <name>` into its own
runner pane.

Usage: name-session.py <name words...> [--port N]
       name-session.py --selftest

Claude cannot run the built-in /rename itself, and `claude --name` only names a
session at launch, which is how the runner names the sessions it spawns. A
session running in a runner tab can still name itself: the runner's
POST /terminals/{id}/submit-prompt types into the session's own pane, and
Claude Code queues a slash command typed during a turn and runs it when the
turn ends. So the name is applied right after the calling turn finishes, never
during it, and this script cannot read the result back.

The route pastes onto whatever the input box holds, so the prompt is checked
immediately before the send, and the script refuses when it holds text: an
operator's half-typed message is not sent with the command glued to it. The
pane must also be this session's own: $QONTINUI_TERMINAL_ID is inherited by
child processes, so the runner registry's session id for that pane has to
equal $CLAUDE_CODE_SESSION_ID, or nothing is sent. That refuses a nested
`claude`, but not a subagent: a subagent inherits both variables from its
parent and would rename the parent. Do not run it from a subagent.

Exit codes: 0 /rename queued, 2 not in a runner tab ($QONTINUI_TERMINAL_ID is
unset), the name is not usable, or the arguments are malformed, 3 UNKNOWN: the
runner could not be read, the pane could not be proven this session's, or the
send was not confirmed, 4 the prompt holds text, so nothing was sent.
"""
import importlib.util
import json
import os
import re
import sys
import urllib.error
import urllib.request

sys.dont_write_bytecode = True
_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("cfs", os.path.join(_here, "close-finished-sessions.py"))
cfs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cfs)

NAME_MAX = 80


def clean_name(raw):
    """The name as /rename will receive it, or None when it is unusable: one
    line, whitespace collapsed, no leading `-` or `/`, no `<`, at most NAME_MAX."""
    name = re.sub(r"\s+", " ", raw.replace("`", "")).strip().lstrip("-/ ").strip()
    if not name or len(name) > NAME_MAX or "<" in name or any(ord(c) < 32 or ord(c) == 127 for c in name):
        return None
    return name


def selftest():
    cases = [("614,615 headless agent provisioning", "614,615 headless agent provisioning"),
             ("  2283  session\n triage  ", "2283 session triage"), ("`quoted`", "quoted"),
             ("/rename x", "rename x"), ("--flag", "flag"), ("", None), ("<script>", None), ("x" * 81, None),
             ("esc\x1b[2J", None), ("del\x7f", None)]
    bad = [(raw, got, want) for raw, want in cases if (got := clean_name(raw)) != want]
    for raw, got, want in bad:
        print(f"FAIL clean_name({raw!r}) = {got!r}, want {want!r}")
    print(f"selftest: {len(cases)} name cases, {len(bad)} failure(s)")
    return 1 if bad else 0


def main():
    args = sys.argv[1:]
    if args == ["--selftest"]:
        return selftest()
    if "--port" in args:
        i = args.index("--port")
        if i + 1 >= len(args):
            print("NOT-SET: --port needs a value")
            return 2
        cfs.API = f"http://127.0.0.1:{args[i + 1]}"
        del args[i:i + 2]
    name = clean_name(" ".join(args))
    if name is None:
        print(f"NOT-SET: {' '.join(args)!r} is not a usable name (one line, no `<`, at most {NAME_MAX} chars)")
        return 2
    tid = os.environ.get("QONTINUI_TERMINAL_ID", "")
    if not tid:
        print(f"NOT-IN-RUNNER: $QONTINUI_TERMINAL_ID is unset, so there is no pane to type into. "
              f"Run it yourself:\n/rename {name}")
        return 2
    own_sid = os.environ.get("CLAUDE_CODE_SESSION_ID") or os.environ.get("CLAUDE_SESSION_ID") or ""
    try:
        pane_sid = (cfs.open_rows().get(tid) or {}).get("claudeSessionId") or ""
    except cfs.READ_ERRORS as e:
        pane_sid, own_sid = "", own_sid or f"(registry unreadable: {e})"
    if not own_sid or pane_sid != own_sid:
        print(f"UNKNOWN: pane {tid[:8]} could not be proven this session's own (registry session "
              f"{pane_sid or '?'}, this session {own_sid or '?'}; after a /clear the registry still names the "
              f"old session); nothing sent. Run it yourself:\n/rename {name}")
        return 3
    raw = cfs.raw_tail(tid)
    if raw is None:
        print(f"UNKNOWN: could not read this session's screen from {cfs.API}; nothing sent. "
              f"Run it yourself:\n/rename {name}")
        return 3
    state, typed = cfs.prompt_state(raw)
    if state != "empty":
        print(f"HELD: the input prompt is not empty ({state}: {str(typed)[:60]!r}), so nothing was sent. "
              f"Run it yourself once the prompt is clear:\n/rename {name}")
        return 4
    try:
        req = urllib.request.Request(f"{cfs.API}/terminals/{tid}/submit-prompt", method="POST",
                                     data=json.dumps({"message": f"/rename {name}"}).encode(),
                                     headers={"Content-Type": "application/json"})
        body = json.loads(urllib.request.urlopen(req, timeout=60).read() or b"{}")
    except (urllib.error.URLError, OSError, ValueError) as e:
        print(f"UNKNOWN: submit-prompt failed ({e}); the name may or may not be queued. "
              f"Run it yourself if the title does not change:\n/rename {name}")
        return 3
    data = body.get("data") if isinstance(body, dict) else None  # envelope-ok: advisory; the title is the verdict
    if not isinstance(data, dict) or not data.get("submitted") or data.get("sanitized"):
        print(f"UNKNOWN: the runner did not confirm a clean submit ({str(body)[:160]}). "
              f"Run it yourself if the title does not change:\n/rename {name}")
        return 3
    print(f"QUEUED: /rename {name}  (runs when this turn ends)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
