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

The prompt is replayed at the pane's width, which only the pane's /terminals
row gives: on Windows, ConPTY writes a full-width row with no line break, so
an idle prompt replayed without the width reads as a draft.

Exit codes: 0 /rename queued, 2 not in a runner tab ($QONTINUI_TERMINAL_ID is
unset), the name is not usable, or the arguments are malformed, 3 UNKNOWN: the
runner could not be read, the pane could not be proven this session's or is
not in /terminals, or the send was not confirmed, 4 the prompt holds text, so
nothing was sent.
"""
import http.client
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
# A runner read or reply that fails: cfs.READ_ERRORS, and a torn body (http.client's
# IncompleteRead), which is not among them. Each is exit 3, never a traceback.
RUNNER_ERRORS = cfs.READ_ERRORS + (http.client.HTTPException,)


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
    send_bad, sends = send_selftest()
    for line in send_bad:
        print("FAIL " + line)
    failures = len(bad) + len(send_bad)
    print(f"selftest: {len(cases)} name cases, {sends} send cases, {failures} failure(s)")
    return 1 if failures else 0


def send_selftest():
    """main() against stubbed runner doors: the doors are replaced, urlopen only
    records, and cfs.API points at a closed port as a backstop, so nothing
    reaches a runner. The prompt is read at this pane's width (plan
    2026-11-07-triage-sessions-every-page-run-defects-aftercare-veto-reader-
    revival-and-concurrent-passes, Phase 7, D3), so the SYNTHETIC idle Windows
    pane cfs.IDLE_CONPTY_PROMPT_SCREEN is queued with wrap_cols held on its
    Windows arm, whatever the host. A draft is held; a pane /terminals does not
    list, a runner read that fails or comes back torn, sends nothing; a torn
    reply to the submit says it may have been queued. Returns (failure lines,
    case count)."""
    import contextlib
    import io
    term = {"id": "t0000001-own-pane", "cols": cfs.CONPTY_FIXTURE_COLS}
    sid, idle, real_wrap, posted = "11111111-2222-3333-4444-555555555555", cfs.IDLE_CONPTY_PROMPT_SCREEN, \
        cfs.wrap_cols, []
    ok = b'{"success": true, "data": {"submitted": true, "sanitized": false}}'
    torn, refused = http.client.IncompleteRead(b"{"), urllib.error.URLError("selftest: refused")

    def door(answer):
        """A stubbed runner read (or a submit reply's read): `answer`, raised when it is an exception."""
        def read(*_a, **_k):
            if isinstance(answer, Exception):
                raise answer
            return answer
        return read

    class Reply:
        def __init__(self, body):
            self.read = door(body)
    rows, mark = {term["id"]: {"claudeSessionId": sid}}, cfs.PROMPT_MARK
    cases = [  # (registry, /terminals, the pane's screen, the submit's reply, exit, the line's first word, a POST)
        (rows, [term], idle, ok, 0, "QUEUED:", True),
        (rows, [term], idle.replace(mark + "\xa0", mark + "\xa0draft", 1), ok, 4, "HELD:", False),
        (rows, [{"id": "t0000002-other"}], idle, ok, 3, "UNKNOWN:", False),
        (rows, refused, idle, ok, 3, "UNKNOWN:", False),
        # A torn body (http.client's IncompleteRead) is not among cfs.READ_ERRORS; it is exit 3 all the same.
        (torn, [term], idle, ok, 3, "UNKNOWN:", False),
        (rows, torn, idle, ok, 3, "UNKNOWN:", False),
        (rows, [term], torn, ok, 3, "UNKNOWN:", False),
        (rows, [term], idle, torn, 3, "UNKNOWN:", True),  # the submit was sent; its reply is torn
    ]
    saved = {k: getattr(cfs, k) for k in ("API", "open_rows", "terminals", "raw_tail", "wrap_cols")}
    saved_env = {k: os.environ.get(k) for k in ("QONTINUI_TERMINAL_ID", "CLAUDE_CODE_SESSION_ID")}
    saved_argv, saved_urlopen = sys.argv, urllib.request.urlopen
    bad = []
    try:
        cfs.API = "http://127.0.0.1:9"
        cfs.wrap_cols = lambda t, windows=None: real_wrap(t, windows=True)
        os.environ.update(QONTINUI_TERMINAL_ID=term["id"], CLAUDE_CODE_SESSION_ID=sid)
        sys.argv = ["name-session.py", "2283", "session", "triage"]
        for registry, terminals, screen, reply, want_rc, want_word, want_post in cases:
            posted.clear()
            cfs.open_rows, cfs.terminals, cfs.raw_tail = door(registry), door(terminals), door(screen)
            urllib.request.urlopen = lambda req, timeout=None, reply=reply: \
                posted.append(json.loads(req.data)) or Reply(reply)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                try:
                    rc = main()
                except Exception as e:  # a width passed by position raises TypeError: a failure, never a crash
                    rc = f"raised {type(e).__name__}: {e}"
            line = out.getvalue().split("\n", 1)[0]
            if rc != want_rc or not line.startswith(want_word) or \
                    posted != ([{"message": "/rename 2283 session triage"}] if want_post else []):
                bad.append(f"main() = exit {rc}, {line[:90]!r}, posted {posted}; want exit {want_rc}, "
                           f"{want_word} and {'one POST' if want_post else 'nothing sent'}")
    finally:
        for k, v in saved.items():
            setattr(cfs, k, v)
        for k, v in saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        sys.argv, urllib.request.urlopen = saved_argv, saved_urlopen
    return bad, len(cases)


def main():
    for stream in (sys.stdout, sys.stderr):  # a piped Windows console is cp1252
        try:
            stream.reconfigure(encoding="utf-8", errors="backslashreplace")
        except (AttributeError, ValueError, OSError):
            pass
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
    except RUNNER_ERRORS as e:
        pane_sid, own_sid = "", own_sid or f"(registry unreadable: {e})"
    if not own_sid or pane_sid != own_sid:
        print(f"UNKNOWN: pane {tid[:8]} could not be proven this session's own (registry session "
              f"{pane_sid or '?'}, this session {own_sid or '?'}; after a /clear the registry still names the "
              f"old session); nothing sent. Run it yourself:\n/rename {name}")
        return 3
    # The prompt is replayed at the pane's width, as the close screen's is, or a
    # Windows pane's idle prompt reads as a draft. Only /terminals gives it.
    try:
        term = next((t for t in cfs.terminals() if t.get("id") == tid), None)
    except RUNNER_ERRORS as e:
        print(f"UNKNOWN: could not read the runner's /terminals from {cfs.API} ({e}), so this pane's width is "
              f"unknown; nothing sent. Run it yourself:\n/rename {name}")
        return 3
    if term is None:
        print(f"UNKNOWN: pane {tid[:8]} is not in the runner's /terminals, so its width is unknown; nothing "
              f"sent. Run it yourself:\n/rename {name}")
        return 3
    try:
        raw = cfs.raw_tail(tid)  # None on cfs.READ_ERRORS; a torn body raises
    except RUNNER_ERRORS:
        raw = None
    if raw is None:
        print(f"UNKNOWN: could not read this session's screen from {cfs.API}; nothing sent. "
              f"Run it yourself:\n/rename {name}")
        return 3
    state, typed = cfs.prompt_state(raw, cols=cfs.wrap_cols(term))
    if state != "empty":
        print(f"HELD: the input prompt is not empty ({state}: {str(typed)[:60]!r}), so nothing was sent. "
              f"Run it yourself once the prompt is clear:\n/rename {name}")
        return 4
    try:
        req = urllib.request.Request(f"{cfs.API}/terminals/{tid}/submit-prompt", method="POST",
                                     data=json.dumps({"message": f"/rename {name}"}).encode(),
                                     headers={"Content-Type": "application/json"})
        body = json.loads(urllib.request.urlopen(req, timeout=60).read() or b"{}")
    except (urllib.error.URLError, OSError, ValueError, http.client.HTTPException) as e:
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
