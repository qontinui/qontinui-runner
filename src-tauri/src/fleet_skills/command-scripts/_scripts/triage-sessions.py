#!/usr/bin/env python3
"""Triage every runner terminal session in one pass: close the finished ones,
hand the usage-limited ones to another account, name the unnamed ones, trim
each page's grid, nudge the stopped ones.

Usage: triage-sessions.py [--act] [--skip STEP[,STEP..]] [--max-name N]
                          [--max-nudge N] [--name-timeout S] [--idle-min N]
                          [--cooldown-hours H] [--include-closeout]
                          [--only ID[,ID..]] [--tab NAME ...] [--port N]
       triage-sessions.py --selftest

STEP is close, limit, name, layout or nudge. Without --act every step reports what it
would do and changes nothing.

--tab NAME (repeatable) limits every step to the sessions in the Terminal-page
tab of that name, matched whole and case-insensitively. Every tab carrying the
name is included, so two tabs both named "infra" are both triaged. The tab
names are read from the runner webview's page list; a name no tab carries is
refused (exit 2) rather than skipped, as is a blank name, and an unreadable
page list for the runner on --port is UNKNOWN (exit 3). With --only as well, a terminal must satisfy both. With --act the steps run in this order:

  close   close-finished-sessions.py --close: closes FINISHED and
          EXITED-FINISHED tabs (re-screened and read back by that script).
  limit   for each LIMITED session (stopped by an account usage limit): type
          /next-move. It reaches the exhausted account, Claude prints its limit
          message, and the runner resumes the session in a new pane under
          another account and closes this one (HANDED).
  name    for each STOPPED session with no /rename title: type /name, which
          names the session itself (it queues /rename into its own pane), and
          read the title back from the transcript. When /name could not queue
          it, type the `/rename <name>` line its reply printed. Only STOPPED rows this run will also nudge
          are named (none under --skip nudge, at most --max-nudge), because a
          prompt changes the verdict the screens read: a finished session would
          read REVIVED, a /next-move cooldown hold would be lost, and an
          un-nudged session's "work remains" message would be replaced.
  layout  runner-layout.py fit --trim-only over every page (under --only or
          --tab, the pages holding those terminals): shrinks each grid to fit its
          remaining tabs, since the runner grows a grid by itself but never
          shrinks one.
  nudge   nudge-stopped-sessions.py --send: types /next-move into each STOPPED
          row. A session named in this run just finished a turn, so it reads
          BUSY to the idle floor; it is nudged in a second pass limited to the
          named rows with the idle floor off (every other check still runs).

After typing into a pane (/name, /next-move) the script watches it. A prompt
that reaches an exhausted account makes the runner move the session to another
account: HANDED once a new pane holds the same session. Nothing on Claude's
usage-limit menu is ever chosen, since what a number selects depends on the
account (2 can be "Switch to usage credits"); a menu the runner leaves open is
dismissed with Esc (FAILED). A session that answers "Unknown command:
/next-move" is asked to run the command from qontinui-claude-config
origin/main (REDIRECT).

Reuses nudge-stopped-sessions.py's screen and guarded send, and calls the three
helpers beside this file for their steps, so each keeps its own re-screen and
read-back. Stdlib only. The command /triage-sessions documents it.

Exit codes: 0 every step done (or nothing to do), 1 some row in some step
failed, was skipped at its re-screen, or did not verify, or a step's helper
answered UNKNOWN (a layout step on a runner build without page-targeted layout
included), 2 an unknown --skip step or a --tab name no tab carries, 3 UNKNOWN:
the runner (or, under --tab, its tab names) could not be read, so nothing was
triaged.
"""
import argparse
import base64
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.dont_write_bytecode = True
_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("nss", os.path.join(_here, "nudge-stopped-sessions.py"))
nss = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nss)
cfs = nss.cfs

STEPS = ("close", "limit", "name", "layout", "nudge")
NAME_RAN = re.compile(r"<command-name>/name</command-name>")
# The line /name ends its reply with (its step 4): "/rename <prs> <words>".
RENAME_LINE = re.compile(r"^[ \t>`]*/rename[ \t]+(.+?)[ \t`]*$", re.M)
NAME_MAX = 80


# ---------------------------------------------------------------- typing
def ready_to_type(tid, idle_min, cooldown_h, own, want="STOPPED"):
    """Re-screen one row just before typing into it: nudge_one's pre-send
    checks in nudge-stopped-sessions.py, for a prompt other than /next-move.
    Returns (ok, msg, tx): ok only when the row still reads `want`, its output
    is still, and its input prompt is empty on the final screen."""
    t = next((x for x in cfs.terminals() if x["id"] == tid), None)
    if t is None:
        return False, "terminal no longer listed", None
    ready, err = cfs.readiness()
    if err:
        return False, f"readiness unreadable at re-screen ({err[:80]})", None
    row = cfs.open_rows().get(tid)
    verdict, why = cfs.classify(t, row, cfs.proc_table(), idle_min, own, ready)
    tx = cfs.transcript_facts(row) if row else None
    verdict, why = nss.judge(verdict, why, nss.row_facts(row, ready), tx, time.time() * 1000, cooldown_h)
    if verdict != want:
        return False, f"re-screen now says {verdict}: {'; '.join(why)}", tx
    moving = cfs.output_moving(tid)
    if moving is not False:
        return False, "terminal output still moving" if moving else "terminal output could not be sampled", tx
    # submit-prompt pastes onto whatever the input box holds, so a draft typed
    # during the re-screen must not ride along.
    raw = cfs.raw_tail(tid)
    state, typed = cfs.prompt_state(raw) if raw is not None else ("unknown", "screen unreadable")
    if state != "empty":
        return False, f"input prompt not empty just before the send ({state}: {str(typed)[:50]!r})", tx
    return True, "", tx


def submit(tid, message):
    """POST one prompt into a pane through the runner's submit-prompt route.
    Returns (sent, error): `sent` is False only when the runner refused it;
    otherwise the caller's read-back decides."""
    try:
        req = urllib.request.Request(f"{cfs.API}/terminals/{tid}/submit-prompt", method="POST",
                                     data=json.dumps({"message": message}).encode(),
                                     headers={"Content-Type": "application/json"})
        body = json.loads(urllib.request.urlopen(req, timeout=60).read() or b"{}")
    except urllib.error.HTTPError as e:
        return False, f"submit-prompt answered {e.code}: {e.read()[:200]!r}"
    except Exception:  # the runner may have typed it anyway: the read-back decides
        return True, ""
    # The body is advisory: the caller's transcript read-back is the verdict.
    data = body.get("data") if isinstance(body, dict) else None  # envelope-ok: advisory; the read-back decides
    data = data if isinstance(data, dict) else {}
    if data.get("sanitized"):
        return True, "submit-prompt reports the message was altered on the way to the PTY; not verified"
    return True, ""


# ---------------------------------------------------------------- aftercare
# Claude Code's usage-limit menu, painted when a prompt reaches an exhausted
# account. Triage never chooses an option on it: what a number selects depends
# on the account (with usage credits enabled, 2 is "Switch to usage credits",
# which is paid usage and opens a login dialog), and a narrow pane draws only
# the first option. The handoff is the runner's: it reads the limit message
# Claude prints, resumes the session in a new pane under another account, and
# closes this one.
# The pane's rendering can drop characters ("What do you wan to do?"), so the
# match is the first option's text, which a narrow pane always draws.
LIMIT_MENU = re.compile(r"Stop and wait for limit to reset")
# A menu still open ends the screen with its key hint; one left in scrollback
# does not.
MENU_OPEN = re.compile(r"Enter to confirm \u00b7 Esc to cancel\s*$")


def limit_menu_open(text):
    return bool(text) and bool(LIMIT_MENU.search(text)) and bool(MENU_OPEN.search(text[-400:]))
UNKNOWN_NEXT_MOVE = "Unknown command: /next-move"
SURVEY = "How is Claude doing this session?"
# Typed into a session whose own command set has no /next-move.
NEXT_MOVE_FROM_MAIN = ("This session has no /next-move command. Run it from qontinui-claude-config origin/main: "
                       "read .claude/commands/next-move.md at origin/main of the qontinui-claude-config repo "
                       "and follow it as if /next-move had been invoked.")


def screen_text(tid, chars=3000):
    """The ANSI-stripped end of a pane's screen, or None when unreadable."""
    raw = cfs.raw_tail(tid)
    return None if raw is None else cfs.ANSI_RE.sub("", raw)[-chars:]


def write_keys(tid, keys):
    """Write raw keystrokes into a pane."""
    req = urllib.request.Request(f"{cfs.API}/terminals/{tid}/write", method="POST",
                                 data=json.dumps({"data": base64.b64encode(keys).decode()}).encode(),
                                 headers={"Content-Type": "application/json"})
    urllib.request.urlopen(req, timeout=20).read()


def session_id(tid):
    """The Claude session id the runner registry records for a pane."""
    return (cfs.open_rows().get(tid) or {}).get("claudeSessionId") or ""


def successor(sid, gone, wait_s=30):
    """The pane, other than `gone`, that now holds Claude session `sid`, or ""."""
    deadline = time.time() + wait_s
    while sid:
        found = [i for i, r in cfs.open_rows().items() if i != gone and (r or {}).get("claudeSessionId") == sid]
        if found or time.time() >= deadline:
            return found[0] if found else ""
        time.sleep(2)
    return ""


def await_handoff(tid, sid, wait_s=30):
    """Wait for the runner to move a session that hit a usage limit. Returns
    (handed, message). The read-back is another pane holding the same Claude
    session. A pane the runner did not move gets its limit menu dismissed with
    Esc, and nothing on the menu is chosen."""
    deadline = time.time() + wait_s
    while time.time() < deadline:
        time.sleep(2)
        if not any(t["id"] == tid for t in cfs.terminals()):
            new = successor(sid, tid)
            return (True, f"the runner resumed the session in pane {new[:8]} under another account") if new else \
                (False, "the pane closed, but no other pane holds its session")
    if limit_menu_open(screen_text(tid) or ""):
        write_keys(tid, b"\x1b")
        return False, "the runner did not move the session; its usage-limit menu was dismissed with Esc, no option chosen"
    return False, "the runner did not move the session"


def hand_off_limited(targets, a, own):
    """Type /next-move into each LIMITED session. The prompt reaches the
    exhausted account, Claude prints its limit message, and the runner moves
    the session to another account; aftercare reads that back, and redirects a
    session with no /next-move, whose redirect is then the prompt that reaches
    the account. Returns the count that did not complete."""
    failures, sent = 0, []
    for r in targets:
        tid = r["terminalId"]
        if limit_menu_open(screen_text(tid) or ""):  # a menu left open holds the prompt
            write_keys(tid, b"\x1b")
            time.sleep(1)
        ok, msg, _ = ready_to_type(tid, a.idle_min, a.cooldown_hours, own, want="LIMITED")
        if ok:
            ok, msg = submit(tid, "/next-move")
        if not ok:
            failures += 1
            print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - {msg}")
            continue
        print(f"  SENT    {tid[:8]}  {r['name'][:50]} - /next-move")
        sent.append(tid)
    return failures + aftercare(sent, a, own, wait_s=90, report_running=True)


def aftercare(tids, a, own, wait_s=60, report_running=False):
    """Watch panes this run just typed into. A pane that closes with its session
    in a new pane was moved by the runner to another account (HANDED); one
    whose limit menu stays open was not, and the menu is dismissed with Esc.
    An "Unknown command: /next-move" reply gets the command from origin/main.
    A redirected pane stays watched, since the redirect is a prompt too and can
    open the limit menu. A pane drops out once its turn is visibly running, it
    closes, or it was handled. Returns the count of panes whose handling did
    not complete."""
    pending, redirected, failures, deadline = set(tids), set(), 0, time.time() + wait_s
    sids = {tid: session_id(tid) for tid in tids}
    while pending and time.time() < deadline:
        time.sleep(5)
        listed = {t["id"] for t in cfs.terminals()}
        for tid in sorted(pending):
            if tid not in listed:
                new = successor(sids[tid], tid)
                failures += not new
                print(f"  HANDED  {tid[:8]}  - the runner resumed the session in pane {new[:8]} under another account"
                      if new else f"  GONE    {tid[:8]}  - the pane closed, and no other pane holds its session")
                pending.discard(tid)
                continue
            text = screen_text(tid)
            if text is None:
                continue
            if limit_menu_open(text):
                handed, msg = await_handoff(tid, sids[tid])
                failures += not handed
                print(f"  {'HANDED ' if handed else 'FAILED '} {tid[:8]}  - {msg}")
                pending.discard(tid)
            elif UNKNOWN_NEXT_MOVE in text[-600:] and tid not in redirected:
                ok, msg = ready_for_redirect(tid)
                if ok:
                    _, err = submit(tid, NEXT_MOVE_FROM_MAIN)
                    ok, msg = not err, err
                failures += not ok
                print(f"  {'REDIRECT' if ok else 'FAILED  '} {tid[:8]}  - "
                      + ("no /next-move here; asked it to run the one on qontinui-claude-config origin/main"
                         if ok else f"no /next-move here, and the redirect was not sent: {msg}"))
                if ok:
                    redirected.add(tid)
                    deadline = max(deadline, time.time() + 30)
                else:
                    pending.discard(tid)
            elif cfs.running_turn(text):
                if report_running:
                    print(f"  WORKING {tid[:8]}  - the prompt started a turn: this account has capacity again")
                pending.discard(tid)
    return failures


def ready_for_redirect(tid):
    """(ok, why) for typing the redirect. The full re-screen cannot be used:
    "Unknown command" ends no turn, so the spinner line above it still reads as
    a running turn. Instead the reply must be the last thing on screen, the
    output still, and the prompt empty. Claude's optional session survey can
    print after the reply; it is dismissed with 0 first."""
    text = screen_text(tid) or ""
    if SURVEY in text[text.rfind(UNKNOWN_NEXT_MOVE):]:
        write_keys(tid, b"0")
        time.sleep(2)
        text = screen_text(tid) or ""
        if SURVEY in text[text.rfind(UNKNOWN_NEXT_MOVE):]:
            return False, "the session survey after the Unknown command reply did not dismiss"
    if "\u23bf" in text[text.rfind(UNKNOWN_NEXT_MOVE):] or "\u25cf" in text[text.rfind(UNKNOWN_NEXT_MOVE) + 1:]:
        return False, "the pane printed more after the Unknown command reply"
    moving = cfs.output_moving(tid)
    if moving is not False:
        return False, "terminal output still moving" if moving else "terminal output could not be sampled"
    raw = cfs.raw_tail(tid)
    state, typed = cfs.prompt_state(raw) if raw is not None else ("unknown", "screen unreadable")
    if state != "empty":
        return False, f"input prompt not empty ({state}: {str(typed)[:50]!r})"
    return True, ""


def tids_in(out, prefix_labels):
    """Full terminal ids of the rows a helper's output reports under labels."""
    ids = {t["id"] for t in cfs.terminals()}
    found = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] in prefix_labels:
            found += [i for i in ids if i.startswith(parts[1])]
    return found


# ---------------------------------------------------------------- names
# Claude Code's permission-mode indicators, as its status line draws them.
MODE_LINE = re.compile(r"(bypass|accept\s*edits|plan\s*mode|auto\s*mode)\s*(?:permissions\s*)?on", re.I)


def bypass_mode(tid):
    """True when the LAST permission-mode indicator in the pane's tail is
    bypass. A session switched out of bypass keeps the old status text in its
    scrollback, so the last indicator decides, not any one. False when none is
    found or the screen cannot be read. The status line is redrawn
    differentially, so a short tail can miss it: a 30000-character tail held it
    on 40 of 44 STOPPED panes measured on 2026-10-18."""
    raw = cfs.raw_tail(tid, 30000)
    modes = MODE_LINE.findall(cfs.ANSI_RE.sub("", raw)) if raw else []
    return bool(modes) and modes[-1].lower() == "bypass"


def custom_title(path):
    """The last /rename title recorded in a transcript, or None. /rename writes
    a `custom-title` record; the runner's resumeName falls back to Claude's own
    ai-title, so it cannot say whether a session was ever named."""
    title = None
    try:
        with open(path, errors="replace") as f:
            for line in f:
                if '"custom-title"' not in line:
                    continue
                try:
                    e = json.loads(line)
                except ValueError:
                    continue
                if e.get("type") == "custom-title" and e.get("customTitle"):
                    title = e["customTitle"]
    except OSError:
        return None
    return title


def suggested_name(text):
    """The name in the last `/rename <name>` line of a /name reply, cleaned, or
    None when there is none or it is not safe to type into a prompt."""
    hits = RENAME_LINE.findall(text or "")
    if not hits:
        return None
    name = re.sub(r"\s+", " ", hits[-1].replace("`", "")).strip()
    if not name or len(name) > NAME_MAX or name[0] in "/<" or "<" in name or name.startswith("<"):
        return None
    return name


def turn_done(tid, path, sent_ms):
    """(done, final_text): the reply to a prompt sent at `sent_ms` is written
    and the pane is back at an empty, idle prompt."""
    row = cfs.open_rows().get(tid)
    tx = cfs.transcript_facts(row) if row else None
    if not tx or not tx.get("final_text_at") or tx["final_text_at"] < sent_ms or tx.get("pending_tool"):
        return False, ""
    raw = cfs.raw_tail(tid)
    if raw is None or cfs.running_turn(cfs.ANSI_RE.sub("", raw)) or cfs.prompt_state(raw)[0] != "empty":
        return False, ""
    return True, tx.get("final_text") or ""


def name_sessions(targets, a, own):
    """Run /name in each target, then type the /rename line it suggests.
    Returns (named terminal ids, failures)."""
    sent = {}  # tid -> (sent_ms, transcript path, row)
    failures = 0
    for r in targets:
        tid = r["terminalId"]
        # /name runs git and gh. Outside bypass mode a session can stop at a
        # permission dialog for one of them, and triage would leave that dialog
        # open in a session it does not own, so such a session is not named.
        # Checked first, so it adds nothing to the gap between the empty-prompt
        # check and the send.
        try:
            in_bypass = bypass_mode(tid)
        except cfs.READ_ERRORS:
            in_bypass = False
        if not in_bypass:
            print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - not in bypass-permissions mode, or the mode "
                  "could not be read; /name could stop at a permission dialog")
            continue
        try:
            ok, msg, tx = ready_to_type(tid, a.idle_min, a.cooldown_hours, own)
        except cfs.READ_ERRORS as e:
            ok, msg, tx = False, f"UNKNOWN at re-screen, not sent: {e}", None
        if not ok or not (tx or {}).get("path"):
            failures += 1
            print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - {msg or 'no transcript to read back from'}")
            continue
        at = time.time() * 1000
        posted, err = submit(tid, "/name")
        if err:
            failures += 1
            print(f"  {'SENT?  ' if posted else 'SKIPPED'} {tid[:8]}  {r['name'][:50]} - /name: {err}")
            continue
        sent[tid] = (at, tx["path"], r)
    named, ran, deadline = [], set(), time.time() + a.name_timeout
    while sent and time.time() < deadline:
        time.sleep(5)
        for tid, (at, path, r) in list(sent.items()):
            try:
                result = poll_one(tid, at, path, r, ran, a, own)
            except cfs.READ_ERRORS as e:
                result = ("SENT?  ", r["name"], f"/name was typed; a runner read failed before the title was read back: {e}")
            if result is None:
                continue
            del sent[tid]
            label, shown, msg = result
            failures += label in ("FAILED ", "SENT?  ")
            named += [tid] if label == "NAMED  " else []
            print(f"  {label} {tid[:8]}  {shown[:50]} - {msg}")
    for tid, (_, _, r) in sent.items():
        failures += 1
        print(f"  SENT?   {tid[:8]}  {r['name'][:50]} - /name still running after {a.name_timeout}s, or waiting "
              "at a dialog; not renamed")
    return named, failures


def poll_one(tid, at, path, r, ran, a, own):
    """One look at a session /name was typed into. None while it is still
    working; otherwise (label, name shown, message), after typing the /rename
    when the reply suggested one. A usage-limit menu means the runner should
    move the session to another account; that is waited for, never chosen."""
    if not any(t["id"] == tid for t in cfs.terminals()):
        return "GONE   ", r["name"], "the pane closed while /name ran"
    text = screen_text(tid)
    if limit_menu_open(text):
        handed, msg = await_handoff(tid, session_id(tid))
        return ("HANDED " if handed else "FAILED "), r["name"], msg
    if tid not in ran:
        row = cfs.open_rows().get(tid)
        tx = cfs.transcript_facts(row) if row else None
        p_at, text = (tx or {}).get("last_prompt"), (tx or {}).get("last_prompt_text") or ""
        if p_at and p_at >= at - 1000 and NAME_RAN.search(text):
            ran.add(tid)
        elif time.time() * 1000 - at > 60000:
            return "SENT?  ", r["name"], "/name did not run within 60s"
        return None
    done, reply = turn_done(tid, path, at)
    if not done:
        return None
    # /name queues `/rename <name>` into its own pane, which Claude Code runs
    # as the turn ends. Targets had no title, so any title now is that one.
    for _ in range(10):
        title = custom_title(path)
        if title:
            return "NAMED  ", title, "read back: the session's own /name set the title"
        time.sleep(2)
    # It could not queue it (no runner pane, a held prompt): type the line its
    # reply printed instead.
    name = suggested_name(reply)
    if name is None:
        return "FAILED ", r["name"], "no title after /name, and its reply has no usable /rename line"
    ok, msg = rename_one(tid, path, name, a, own)
    return ("NAMED  " if ok else "FAILED "), name, msg


def rename_one(tid, path, name, a, own):
    """Type `/rename <name>` and read the title back. The idle floor is off for
    the re-screen: the only recent activity is the /name turn this run started,
    and every other check (shells, running turn, moving output, empty prompt)
    still applies."""
    ok, msg, _ = ready_to_type(tid, 0, a.cooldown_hours, own)
    if not ok:
        return False, f"not renamed: {msg}"
    _, err = submit(tid, f"/rename {name}")
    if err:
        return False, f"/rename: {err}"
    for _ in range(10):
        time.sleep(2)
        if custom_title(path) == name:
            return True, "read back: transcript title set"
    return False, "/rename sent; read back: title not in the transcript after 20s"


def tab_pages(pages, names):
    """({pageId, ..} of every tab whose name matches one of `names`, [names no
    tab carries]). A name matches whole, ignoring case and surrounding space, so
    every tab sharing a name is selected. Pure, so the selftest can pin it."""
    def key(n):
        return n.strip().casefold()
    hit = {pid for pid, n in pages.items() if key(n) in {key(x) for x in names}}
    found = {key(pages[pid]) for pid in hit}
    return hit, [x for x in names if key(x) not in found]


# ---------------------------------------------------------------- steps
def run_helper(script, args):
    """Run a helper beside this file, echo its output, return (exit, stdout)."""
    p = subprocess.run([sys.executable, os.path.join(_here, script), *args],
                       capture_output=True, text=True)
    for line in (p.stdout + p.stderr).splitlines():
        print("  " + line)
    return p.returncode, p.stdout


def common(a):
    return (["--idle-min", str(a.idle_min)] + (["--only", a.only] if a.only else [])
            + (["--port", a.port] if a.port else []))


# Everything this script calls on its two imported siblings. The selftest
# checks they still exist, so a rename there fails CI here rather than at the
# first --act run.
USES_NSS = ("judge", "row_facts", "screen")
USES_CFS = ("API", "ANSI_RE", "READ_ERRORS", "classify", "open_rows", "output_moving", "page_names", "proc_table",
            "prompt_state", "raw_tail", "readiness", "running_turn", "terminals", "transcript_facts")


def selftest():
    failures = 0
    for mod, names in ((nss, USES_NSS), (cfs, USES_CFS)):
        for missing in [n for n in names if not hasattr(mod, n)]:
            print(f"FAIL {mod.__name__}.{missing} no longer exists")
            failures += 1
    modes = [("x bypass permissions on y", True), ("bypass permissions on ... accept edits on", False),
             ("accept edits on ... bypasspermissionson", True), ("no indicator", False),
             ("plan mode on", False)]
    for text, want in modes:
        got = bool(MODE_LINE.findall(text)) and MODE_LINE.findall(text)[-1].lower() == "bypass"
        if got != want:
            print(f"FAIL mode({text!r}) = {got}, want {want}")
            failures += 1
    cases = [
        ("```\n/rename 614,615 headless agent provisioning\n```\nDerived from #614.",
         "614,615 headless agent provisioning"),
        ("/rename `fleet auto-response`", "fleet auto-response"),
        ("no suggestion here", None),
        ("/rename /etc/passwd", None),
        ("/rename <script>", None),
        ("/rename " + "x" * 81, None),
        ("/rename first\n...\n/rename 1917 nudge stopped sessions", "1917 nudge stopped sessions"),
        ("Run `/rename x` yourself", None),
    ]
    menu = ("You've hit your weekly limit\nWhat do you want to do?\n  \u276f1. Stop and wait for limit to reset\n"
            "   2. Resume when the limit resets\n Enter to confirm \u00b7 Esc to cancel")
    for text, want in ((menu, True), ("What do you want to do?\n 1. Yes\n 2. No", False)):
        if limit_menu_open(text) != want:
            print(f"FAIL limit menu({text[:30]!r}) != {want}")
            failures += 1
    for text, want in cases:
        got = suggested_name(text)
        if got != want:
            print(f"FAIL suggested_name({text[:40]!r}) = {got!r}, want {want!r}")
            failures += 1
    pages = {"p1": "infra", "p2": "Infra ", "p3": "web", "p4": "infra-2"}
    tabs = [(["infra"], ({"p1", "p2"}, [])), (["WEB", "infra"], ({"p1", "p2", "p3"}, [])),
            (["inf"], (set(), ["inf"])), (["web", "nope"], ({"p3"}, ["nope"])), ([" web "], ({"p3"}, []))]
    for names, want in tabs:
        got = tab_pages(pages, names)
        if got != want:
            print(f"FAIL tab_pages({names!r}) = {got!r}, want {want!r}")
            failures += 1
    print(f"selftest: {len(cases)} name cases, {len(modes)} mode cases, {len(tabs)} tab cases, "
          f"{failures} failure(s)")
    return 1 if failures else 0


def main():
    if "--selftest" in sys.argv[1:]:
        return selftest()
    ap = argparse.ArgumentParser()
    ap.add_argument("--act", action="store_true", help="run the steps; without it, report only")
    ap.add_argument("--skip", default="", help=f"comma-separated steps to leave out: {', '.join(STEPS)}")
    ap.add_argument("--max-name", type=int, default=0, help="sessions to name at most (default: no cap)")
    ap.add_argument("--max-nudge", type=int, default=0, help="nudges to send at most (default: no cap)")
    ap.add_argument("--name-timeout", type=int, default=600, help="seconds to wait for /name replies")
    ap.add_argument("--idle-min", type=int, default=15)
    ap.add_argument("--cooldown-hours", type=float, default=24.0)
    ap.add_argument("--include-closeout", action="store_true", help="also close CLOSEOUT rows")
    ap.add_argument("--only", default="", help="terminal id prefixes to limit every step to (layout: the pages holding them)")
    ap.add_argument("--tab", action="append", default=[], metavar="NAME",
                    help="limit every step to the sessions in the Terminal-page tab(s) of this name; repeatable")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    a = ap.parse_args()
    skip = {s for s in a.skip.split(",") if s}
    if skip - set(STEPS):
        print(f"unknown step(s): {', '.join(sorted(skip - set(STEPS)))}", file=sys.stderr)
        return 2
    if a.port:
        cfs.API = f"http://127.0.0.1:{a.port}"
    own = os.environ.get("QONTINUI_TERMINAL_ID", "")
    tabs = a.tab
    if any(not n.strip() for n in tabs):
        # Never drop a blank value: an empty selection would widen to every session.
        print("--tab needs a tab name; a blank one names no tab", file=sys.stderr)
        return 2
    if tabs:
        # Resolve the tab names to their terminals' full ids, which then scope
        # every step (and every helper) exactly as --only does.
        pages = cfs.page_names()
        if not pages:
            print(f"UNKNOWN: --tab given but the tab names of the runner at {cfs.API} could not be read "
                  "(its page list is only read from WebKitGTK storage)", file=sys.stderr)
            return 3
        hit, missing = tab_pages(pages, tabs)
        if missing:
            print(f"no tab named {', '.join(repr(m) for m in missing)}; tabs: "
                  + ", ".join(sorted({repr(n) for n in pages.values()})), file=sys.stderr)
            return 2
        try:
            # The runner UI shows a terminal with no pageId on the "default" page.
            ids = [t["id"] for t in cfs.terminals() if (t.get("pageId") or "default") in hit
                   and (not a.only or any(t["id"].startswith(o) for o in a.only.split(",") if o))]
        except cfs.READ_ERRORS as e:
            print(f"UNKNOWN: could not read the runner at {cfs.API}: {e}", file=sys.stderr)
            return 3
        print(f"tab {', '.join(repr(n) for n in tabs)}: {len(hit)} tab(s), {len(ids)} terminal(s)")
        if not ids:
            print(f"no terminal on this runner is in those tabs (page id(s) {', '.join(sorted(hit))}); "
                  "nothing to triage")
            return 0
        a.only = ",".join(ids)
    try:
        terms = [t for t in cfs.terminals() if any(t["id"].startswith(o) for o in a.only.split(",") if o)]
        res, warnings, ready_err = nss.screen(a.idle_min, [o for o in a.only.split(",") if o], a.cooldown_hours,
                                              False)
        rows = cfs.open_rows()
    except cfs.READ_ERRORS as e:
        print(f"UNKNOWN: could not read the runner at {cfs.API}: {e}", file=sys.stderr)
        return 3
    for w in warnings:
        print("WARNING: " + w)
    counts = {}
    for r in res:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1
    print("verdicts: " + ", ".join(f"{v}={n}" for v, n in sorted(counts.items())))
    if a.act and ready_err:
        print("UNKNOWN: --act refused - GET /restart-readiness could not be read", file=sys.stderr)
        return 3

    failed = 0
    closable = {"FINISHED", "EXITED-FINISHED"} | ({"CLOSEOUT"} if a.include_closeout else set())
    if "close" not in skip:
        print(f"\n== close ({sum(r['verdict'] in closable for r in res)} closable)")
        if a.act:
            code, _ = run_helper("close-finished-sessions.py",
                                 ["--close"] + (["--include-closeout"] if a.include_closeout else []) + common(a))
            failed += code != 0
        else:
            for r in res:
                if r["verdict"] in closable:
                    print(f"  would close {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}")

    if "limit" not in skip:
        limited = [r for r in res if r["verdict"] == "LIMITED"]
        print(f"\n== limit ({len(limited)} LIMITED)")
        if a.act:
            failed += hand_off_limited(limited, a, own) > 0
        else:
            for r in limited:
                print(f"  would hand off {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}")

    named = []
    if "name" not in skip:
        unnamed = []
        for r in res:
            row = rows.get(r["terminalId"])
            tx = cfs.transcript_facts(row) if row and r["verdict"] == "STOPPED" else None
            if tx and not custom_title(tx["path"]):
                unnamed.append(r)
        # A /name turn replaces the final message the nudge screen reads, so
        # only sessions this run will also nudge are named: none when the nudge
        # step is skipped, and no more than the nudge budget.
        caps = [c for c in (a.max_name, a.max_nudge) if c]
        cap = 0 if "nudge" in skip else min(caps) if caps else len(unnamed)
        targets = unnamed[:cap]
        why = " (none: the nudge step is skipped)" if "nudge" in skip else ""
        print(f"\n== name ({len(unnamed)} STOPPED with no title, naming {len(targets)}){why}")
        if a.act:
            named, n_failed = name_sessions(targets, a, own)
            failed += n_failed > 0
        else:
            for r in targets:
                print(f"  would name {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}")

    if "layout" not in skip:
        print("\n== layout (trim each page to its tabs)")
        # Under --only (and --tab, which sets it), trim only the pages holding those terminals.
        pages = sorted({t.get("pageId") or "default" for t in terms})
        if a.only and not pages:
            print("  no page holds a terminal --only names; nothing to trim")
        else:
            where = ["--page", ",".join(pages)] if a.only else ["--all-pages"]
            code, _ = run_helper("runner-layout.py", ["fit", *where, "--trim-only"]
                                 + ([] if a.act else ["--dry-run"]) + (["--port", a.port] if a.port else []))
            failed += code != 0

    if "nudge" not in skip:
        print(f"\n== nudge ({counts.get('STOPPED', 0)} STOPPED)")
        if a.act:
            cap = ["--cooldown-hours", str(a.cooldown_hours)]
            port = ["--port", a.port] if a.port else []
            sends = 0
            # The named sessions go first and take their share of --max-nudge
            # before anything else, so a session this run named is never left
            # un-nudged. Their /name turn just ended, so the idle floor is off
            # for this pass only; the general pass then sees them held by the
            # /next-move cooldown and skips them.
            if named:
                print("  -- the sessions named above, with the idle floor off")
                code, out = run_helper("nudge-stopped-sessions.py",
                                       ["--send", "--only", ",".join(named), "--idle-min", "0"] + cap + port)
                failed += code != 0
                sends = sum(line.startswith(("  NUDGED", "  SENT?")) for line in out.splitlines())
                failed += aftercare(tids_in(out, {"NUDGED", "SENT?"}), a, own) > 0
            left = a.max_nudge - sends if a.max_nudge else 0
            if not a.max_nudge or left > 0:
                if named:
                    print("  -- every other STOPPED session")
                code, out = run_helper("nudge-stopped-sessions.py",
                                       ["--send"] + cap + common(a) + (["--max", str(left)] if a.max_nudge else []))
                failed += code != 0
                failed += aftercare(tids_in(out, {"NUDGED", "SENT?"}), a, own) > 0
        else:
            for r in res:
                if r["verdict"] == "STOPPED":
                    print(f"  would nudge {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
