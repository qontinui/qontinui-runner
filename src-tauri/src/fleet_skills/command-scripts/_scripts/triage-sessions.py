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
          read REVIVED, a /next-move hold would be lost, and an
          un-nudged session's "work remains" message would be replaced.
  layout  runner-layout.py fit --trim-only over every page (under --only or
          --tab, the pages holding those terminals): shrinks each grid to fit its
          remaining tabs, since the runner grows a grid by itself but never
          shrinks one.
  nudge   nudge-stopped-sessions.py --send: types /next-move into each STOPPED
          row. A session named in this run just finished a turn, so it reads
          BUSY to the idle floor; it is nudged in a second pass limited to the
          named rows with the idle floor off (every other check still runs).
          STUCK and CONTRADICTED rows are listed and never nudged.

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

Every run that screened the runner records one pass in coord's overlord pass
ledger: the run (port, runner build, scope, mode, skipped steps, step counts,
the verdict histogram, the blob shas of these scripts) and one row per
screened terminal (its screen and final verdicts, reasons, and the action a
step took with its outcome). Under --act each helper writes its acted rows to
a --rows-out file this script merges; nothing is parsed from their output. The
write goes through the session's own coord door (nudge-stopped-sessions.py
coord_call); when that door did not carry it (the runner proxy refused the
tool or the nonce, nothing listened, or this session has none provisioned),
it goes to coord's /mcp door with a device JWT ($COORD_DEVICE_JWT, then
~/.qontinui/coord-device-jwt, an expired one skipped; no redirect followed). The pass is read back by
its pass_id and its row count compared, and the run ends with
"ledger: recorded pass <id> (<n> rows, read back)" or "ledger UNKNOWN: <why>".
The ledger never changes the exit code.

--act takes an exclusive lock on ~/.qontinui/triage-sessions.<port>.lock for
the whole run (fcntl.flock; msvcrt.locking on Windows), so one acting pass at
a time types into a runner. The kernel releases it when the holder exits, so
there is no stale lock to take over. A report pass takes no lock.

Exit codes: 0 every step done (or nothing to do), 1 some row in some step
failed, was skipped at its re-screen, or did not verify, or a step's helper
answered UNKNOWN (a layout step on a runner build without page-targeted layout
included), 2 an unknown --skip step, a --tab name no tab carries, or a --port
that is not a port number, 3 UNKNOWN: the runner (or, under --tab, its tab names)
could not be read, so nothing was triaged, or the --act lock file could not be
opened, 4 BUSY: another --act pass holds this runner's lock (its pid, start
and pass id are printed; the holder writes them just after taking the lock,
so for a moment they can name the previous holder), so nothing was triaged.
"""
import argparse
import base64
import datetime as dt
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

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
    # The same coord / PR signals the screen read, so the two never disagree on a /next-move hold.
    ext = nss.external_signals(tx, row) if verdict in nss.STOPPED_FROM else None
    verdict, why = nss.judge(verdict, why, nss.row_facts(row, ready), tx, time.time() * 1000, cooldown_h,
                             False, ext)
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


# {terminal id: "sent" | "refused"} for every prompt submit() POSTed since the
# name step cleared it, the last one per pane: the pass ledger's record of which
# rows that step typed into.
SUBMITTED = {}


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
        SUBMITTED[tid] = "refused"
        return False, f"submit-prompt answered {e.code}: {e.read()[:200]!r}"
    except Exception:  # the runner may have typed it anyway: the read-back decides
        SUBMITTED[tid] = "sent"
        return True, ""
    SUBMITTED[tid] = "sent"
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


def hand_off_limited(targets, a, own, outcomes=None):
    """Type /next-move into each LIMITED session. The prompt reaches the
    exhausted account, Claude prints its limit message, and the runner moves
    the session to another account; aftercare reads that back, and redirects a
    session with no /next-move, whose redirect is then the prompt that reaches
    the account. Returns the count that did not complete. `outcomes`, when
    given, receives each row's pass-ledger outcome by terminal id."""
    failures, sent = 0, []
    outcomes = {} if outcomes is None else outcomes
    for r in targets:
        tid = r["terminalId"]
        if limit_menu_open(screen_text(tid) or ""):  # a menu left open holds the prompt
            write_keys(tid, b"\x1b")
            time.sleep(1)
        ok, msg, _ = ready_to_type(tid, a.idle_min, a.cooldown_hours, own, want="LIMITED")
        reached = ok
        if ok:
            ok, msg = submit(tid, "/next-move")
        if not ok:
            failures += 1
            outcomes[tid] = "failed" if reached else "skipped"  # reached: the runner refused the POST
            print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - {msg}")
            continue
        outcomes[tid] = "sent_unconfirmed"  # until aftercare reads the handoff back
        print(f"  SENT    {tid[:8]}  {r['name'][:50]} - /next-move")
        sent.append(tid)
    return failures + aftercare(sent, a, own, wait_s=90, report_running=True, outcomes=outcomes)


def aftercare(tids, a, own, wait_s=60, report_running=False, outcomes=None):
    """Watch panes this run just typed into. A pane that closes with its session
    in a new pane was moved by the runner to another account (HANDED); one
    whose limit menu stays open was not, and the menu is dismissed with Esc.
    An "Unknown command: /next-move" reply gets the command from origin/main.
    A redirected pane stays watched, since the redirect is a prompt too and can
    open the limit menu. A pane drops out once its turn is visibly running, it
    closes, or it was handled. Returns the count of panes whose handling did
    not complete. `outcomes`, when given, receives the pass-ledger outcome of
    each pane it settles (a pane still pending at the deadline is left as it was)."""
    outcomes = {} if outcomes is None else outcomes
    pending, redirected, failures, deadline = set(tids), set(), 0, time.time() + wait_s
    sids = {tid: session_id(tid) for tid in tids}
    while pending and time.time() < deadline:
        time.sleep(5)
        listed = {t["id"] for t in cfs.terminals()}
        for tid in sorted(pending):
            if tid not in listed:
                new = successor(sids[tid], tid)
                failures += not new
                outcomes[tid] = "done" if new else "failed"
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
                outcomes[tid] = "done" if handed else "failed"
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
                    outcomes[tid] = "failed"
                    pending.discard(tid)
            elif cfs.running_turn(text):
                outcomes[tid] = "done"  # the prompt reached a session that is now working
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
USES_NSS = ("judge", "row_facts", "screen", "external_signals", "STOPPED_FROM")
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
    ap.add_argument("--cooldown-hours", type=float, default=None,
                    help="the nudge step's backstop hours (default: nudge-stopped-sessions.py's own)")
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
    refused = PASS.begin(a, skip)
    if refused:
        return refused
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
    PASS.screened(res)
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
        PASS.step("close", candidates=sum(r["verdict"] in closable for r in res))
        if a.act:
            code, _ = run_helper("close-finished-sessions.py",
                                 ["--close"] + (["--include-closeout"] if a.include_closeout else []) + common(a)
                                 + PASS.rows_out("close"))
            failed += code != 0
            PASS.step("close", exit=code)
        else:
            for r in res:
                if r["verdict"] in closable:
                    print(f"  would close {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}")

    if "limit" not in skip:
        limited = [r for r in res if r["verdict"] == "LIMITED"]
        print(f"\n== limit ({len(limited)} LIMITED)")
        PASS.step("limit", candidates=len(limited))
        if a.act:
            limit_outcomes = {}
            failed += hand_off_limited(limited, a, own, limit_outcomes) > 0
            PASS.handed_off(limited, limit_outcomes)
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
        PASS.step("name", candidates=len(unnamed), targets=len(targets))
        if a.act:
            SUBMITTED.clear()  # what the name step types, alone
            named, n_failed = name_sessions(targets, a, own)
            failed += n_failed > 0
            PASS.named(targets, named, dict(SUBMITTED))
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
            PASS.step("layout", pages=len(pages) if a.only else "all", exit=code)

    if "nudge" not in skip:
        print(f"\n== nudge ({counts.get('STOPPED', 0)} STOPPED)")
        PASS.step("nudge", candidates=counts.get("STOPPED", 0))
        # Never nudged: for the operator. STUCK is a session /next-move could not
        # move three times running; CONTRADICTED is a finished session whose
        # final message says work remains.
        for r in res:
            if r["verdict"] in ("STUCK", "CONTRADICTED"):
                print(f"  {r['verdict']:<12} {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]} - "
                      f"{'; '.join(r['reasons'])[:160]}")
        if a.act:
            cap = ["--cooldown-hours", str(a.cooldown_hours)] if a.cooldown_hours is not None else []
            port = ["--port", a.port] if a.port else []
            sends = 0
            # The named sessions go first and take their share of --max-nudge
            # before anything else, so a session this run named is never left
            # un-nudged. Their /name turn just ended, so the idle floor is off
            # for this pass only; the general pass then reads them BUSY by the
            # idle floor (their /next-move turn has just started) and skips them.
            if named:
                print("  -- the sessions named above, with the idle floor off")
                code, out = run_helper("nudge-stopped-sessions.py",
                                       ["--send", "--only", ",".join(named), "--idle-min", "0"] + cap + port
                                       + PASS.rows_out("nudge"))
                failed += code != 0
                sends = sum(line.startswith(("  NUDGED", "  SENT?")) for line in out.splitlines())
                failed += aftercare(tids_in(out, {"NUDGED", "SENT?"}), a, own) > 0
            left = a.max_nudge - sends if a.max_nudge else 0
            if not a.max_nudge or left > 0:
                if named:
                    print("  -- every other STOPPED session")
                code, out = run_helper("nudge-stopped-sessions.py",
                                       ["--send"] + cap + common(a) + (["--max", str(left)] if a.max_nudge else [])
                                       + PASS.rows_out("nudge"))
                failed += code != 0
                failed += aftercare(tids_in(out, {"NUDGED", "SENT?"}), a, own) > 0
        else:
            for r in res:
                if r["verdict"] == "STOPPED":
                    print(f"  would nudge {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}")
    return 1 if failed else 0



# ---------------------------------------------------------------- pass ledger
# One record per run in coord.overlord_passes and one row per screened terminal
# in coord.overlord_pass_rows, so the overlord's flow (time to close, what a
# nudge does, what never moves) is measured rather than remembered. Plan
# 2026-10-23-the-overlord-keeps-no-record-of-session-flow-so-its-development-runs-on-anecdote.
WRITE_TOOL = "coord_overlord_pass_record"
READ_TOOL = "coord_query_overlord_passes"
# The action a step's row records (the coord.overlord_pass_rows `action` set).
STEP_ACTIONS = {"close": "close", "limit": "limit_handoff", "name": "name", "nudge": "nudge"}
ACTIONS = set(STEP_ACTIONS.values()) | {"layout"}
OUTCOMES = ("done", "sent_unconfirmed", "skipped", "failed", "unknown")
VERDICT_RE = re.compile(r"^[A-Z][A-Z-]*$")
# Verdicts whose runner finished marker is the time the row has been in that state since.
SINCE_FINISHED = {"FINISHED", "EXITED-FINISHED"}
TEXT_MAX = 256
REASONS_MAX, REASON_CHARS = 3, 160
# coord-revive.sh `call` takes the arguments as ONE argv entry (at most 128 KiB
# on Linux) or, on Windows, one environment variable (32767 characters). A pass
# larger than that cannot go through the session's door at all, so it goes to
# coord's /mcp door directly: nothing would have been sent.
PROXY_ARG_MAX = 32000 if sys.platform == "win32" else 120000
COORD_URL_DEFAULT = "https://coord.qontinui.io"
LOOPBACK = {"127.0.0.1", "::1"}
# coord-revive.sh `call` verdicts that say the session's door did NOT carry the
# call to coord, so the pass may go to coord's own /mcp door instead: the runner
# proxy refused the tool (its HTTP 403 / JSON-RPC -32601
# COORD_MCP_PROXY_METHOD_NOT_ALLOWED), nothing listened on its port (curl exit
# 7), the forwarder refused the nonce or refused out of its own dead credential
# (401), it withheld the call while its credential refreshed (503), or the
# session has no provisioned nonce at all. A re-send is safe besides: coord
# answers a replayed pass_id `replayed` and writes nothing. Any other failure (a
# timeout, an RPC_ERROR, a 5xx from coord) may have reached coord and is
# reported UNKNOWN. Each verdict is matched in the WHOLE line coord-revive.sh
# printed (SignalUnknown.door_verdict), never the 80-character excerpt, and the
# text beside each token is what coord-revive.sh's source prints it with: the
# selftest reads the script and fails when a reword would silently disable this.
NOT_CARRIED_VERDICTS = {
    "METHOD_NOT_ALLOWED": 'echo "METHOD_NOT_ALLOWED (',
    "CONNECT_REFUSED": 'echo "CONNECT_REFUSED (',
    "CREDENTIAL_REFRESHING": 'echo "CREDENTIAL_REFRESHING (',
    "COORD_MCP_PROXY_UNAUTHORIZED": '-> COORD_MCP_PROXY_UNAUTHORIZED (',
    "RUNNER_CREDENTIAL_": '-> RUNNER_CREDENTIAL_$(',
    "NO_PROXY_CONFIG": '-> NO_PROXY_CONFIG (',
}
DOOR_VERDICT_LINE = 'echo "coord-revive: $VERB -> $VV ('
_NOT_CARRIED_ALTS = "|".join(re.escape(k) + ("[A-Z_]+" if k.endswith("_") else "") for k in NOT_CARRIED_VERDICTS)
NOT_CARRIED_RE = re.compile(rf"^coord-revive: call -> (?:{_NOT_CARRIED_ALTS}) \(")
# nss.coord_call's own message when there is no coord-revive.sh to run at all.
NO_DOOR = "no coord-revive.sh beside this script"


def door_never_carried(err):
    """True when a coord_call failure proves the call never reached coord."""
    return str(err).startswith(NO_DOOR) or bool(NOT_CARRIED_RE.match(getattr(err, "door_verdict", None) or ""))
LOCK_BUSY = 4
# Windows locks a byte range, and a locked range cannot be read by anyone else:
# the lock sits far past the holder line at offset 0, which stays readable.
LOCK_OFFSET = 1 << 30


def rfc3339(t):
    return t.astimezone(dt.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _text(value):
    return value[:TEXT_MAX] if isinstance(value, str) and value else None


def pass_row(r, observed_at):
    """The coord.overlord_pass_rows row for one screened terminal `r` (a row of
    nss.screen()), before any step acted on it. Pure. Reason codes are not
    emitted yet: the prose reasons ride in detail.reasons."""
    sid, fin, verdict = r.get("claudeSessionId"), r.get("finishedAt"), r["verdict"]
    since = None
    if verdict in SINCE_FINISHED and isinstance(fin, (int, float)) and not isinstance(fin, bool) and fin > 0:
        since = int(fin)
    screen = r.get("screen")
    return {
        "terminal_id": r["terminalId"],
        "observed_at": observed_at,
        "claude_session_id": sid if isinstance(sid, str) and nss.UUID_RE.match(sid) else None,
        "session_name": _text(r.get("name")),
        "tab": _text(r.get("tab")),
        "account": _text(r.get("account")) if r.get("account") != "?" else None,
        "screen_verdict": screen if isinstance(screen, str) and VERDICT_RE.match(screen) else None,
        "verdict": verdict,
        "reason_codes": [],
        "state_since_ms": since,
        "progress_since_nudge": None,
        "action": "none",
        "action_outcome": None,
        "detail": {"reasons": [str(x)[:REASON_CHARS] for x in (r.get("reasons") or [])[:REASONS_MAX]]},
    }


def apply_events(rows, events):
    """Set each row's action and outcome from the steps' acted rows, taken in
    the order the steps ran, so the LAST step that acted on a terminal names
    the row's action; each step's own detail is kept under its action. Pure.
    Returns the count of events dropped (a terminal the screen did not list, or
    an action or outcome outside the ledger's vocabulary)."""
    by_id = {r["terminal_id"]: r for r in rows}
    dropped = 0
    for ev in events:
        row = by_id.get(ev.get("terminal_id"))
        if row is None or ev.get("action") not in ACTIONS or ev.get("action_outcome") not in OUTCOMES:
            dropped += 1
            continue
        row["action"], row["action_outcome"] = ev["action"], ev["action_outcome"]
        if isinstance(ev.get("detail"), dict):
            row["detail"] = dict(row["detail"], **{ev["action"]: ev["detail"]})
    return dropped


def step_tallies(step_counts, events):
    """step_counts with each step's acted rows counted by outcome. Pure."""
    out = {k: dict(v) for k, v in step_counts.items()}
    by_action = {v: k for k, v in STEP_ACTIONS.items()}
    for ev in events:
        step = by_action.get(ev.get("action"))
        if step and ev.get("action_outcome") in OUTCOMES:
            tally = out.setdefault(step, {})
            tally[ev["action_outcome"]] = tally.get(ev["action_outcome"], 0) + 1
    return out


def read_rows_file(path):
    """The acted rows a helper wrote to its --rows-out file, and the count of
    lines that were not a JSON object. An absent file is no rows: the helper
    acted on nothing, or exited before its acting loop."""
    rows, bad = [], 0
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
    except OSError:
        return rows, bad
    for line in lines:
        try:
            ev = json.loads(line)
        except ValueError:
            ev = None
        if isinstance(ev, dict):
            rows.append(ev)
        else:
            bad += 1
    return rows, bad


def blob_shas(paths):
    """{path: git blob sha} via one `git hash-object`; None for each when git
    cannot answer. hash-object needs no repository, so the provisioned skill
    copy hashes as well as a checkout."""
    try:
        p = subprocess.run(["git", "hash-object", "--", *paths], capture_output=True, text=True, timeout=30,
                           env=nss.git_scope_env(), check=False)
        shas = p.stdout.split() if p.returncode == 0 else []
    except (OSError, subprocess.TimeoutExpired):
        shas = []
    return dict(zip(paths, shas)) if len(shas) == len(paths) else {x: None for x in paths}


def next_move_path():
    """.claude/commands/next-move.md beside this script: the repository's
    (scripts/ -> .claude/commands/) or the provisioned skills tree's
    (.claude/skills/command-scripts/_scripts/ -> .claude/commands/). None when neither exists."""
    for rel in ((os.pardir, ".claude", "commands"), (os.pardir, os.pardir, os.pardir, "commands")):
        c = os.path.normpath(os.path.join(_here, *rel, "next-move.md"))
        if os.path.isfile(c):
            return c
    return None


def overlord_versions(runner_sha):
    """The blob sha of each overlord script as run, and the runner's git sha."""
    files = {n: os.path.join(_here, n) for n in ("triage-sessions.py", "nudge-stopped-sessions.py",
                                                 "close-finished-sessions.py", "runner-layout.py")}
    nm = next_move_path()
    shas = blob_shas([p for p in files.values()] + ([nm] if nm else []))
    out = {n: shas[p] for n, p in files.items()}
    out["next-move.md"] = shas[nm] if nm else None
    out["runner_git_sha"] = runner_sha
    return out


def runner_build():
    """The runner's gitSha from /health, or None when it cannot be read."""
    try:
        sha = cfs.read("/health", "data.gitSha", timeout=20)
    except cfs.READ_ERRORS:
        return None
    return sha if isinstance(sha, str) and sha else None


def runner_port():
    """The port of the runner this run reads (cfs.API), or None when it is not a valid port."""
    try:
        port = urllib.parse.urlsplit(cfs.API).port
    except ValueError:
        return None
    return port if port and 1 <= port <= 65535 else None


# ---------------------------------------------------------------- pass lock
def lock_path(port, lock_dir=None):
    return os.path.join(lock_dir or os.path.join(os.path.expanduser("~"), ".qontinui"),
                        f"triage-sessions.{port}.lock")


def read_holder(fd):
    """The holder line at the head of a lock file, as text for the BUSY message."""
    try:
        os.lseek(fd, 0, os.SEEK_SET)
        line = os.read(fd, 4096).decode("utf-8", errors="replace").split("\n", 1)[0]
        h = json.loads(line)
    except (OSError, ValueError):
        return "holder unreadable"
    if not isinstance(h, dict):
        return "holder unreadable"
    return f"pid {h.get('pid')}, started {h.get('started_at')}, pass {h.get('pass_id')}"


def take_lock(path, holder):
    """(fd, None) holding the exclusive lock on `path`, with `holder` written at
    its head; or (None, holder text) when another process holds it. Non-blocking.
    OSError when the file cannot be opened or locked for any other reason."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        if os.name == "nt":
            import msvcrt
            os.lseek(fd, LOCK_OFFSET, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except (BlockingIOError, PermissionError):  # held: flock's EWOULDBLOCK, msvcrt's EACCES
        held = read_holder(fd)
        os.close(fd)
        return None, held
    except OSError:
        os.close(fd)
        raise
    data = (json.dumps(holder) + "\n").encode()
    os.lseek(fd, 0, os.SEEK_SET)
    os.write(fd, data)
    if os.name != "nt":  # a Windows reader takes the first line only, so a longer stale tail is harmless
        os.ftruncate(fd, len(data))
    return fd, None


def release_lock(fd):
    try:
        if os.name == "nt":
            import msvcrt
            os.lseek(fd, LOCK_OFFSET, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
    except OSError:
        pass
    finally:
        os.close(fd)


# ---------------------------------------------------------------- pass write
def jwt_exp(token):
    """The `exp` claim of a JWT (epoch seconds), or None when unreadable. The
    payload is decoded, never verified: coord verifies; this only skips a token
    that is plainly expired."""
    parts = token.split(".")
    if len(parts) != 3:
        return None
    try:
        claims = json.loads(base64.urlsafe_b64decode(parts[1] + "=" * (-len(parts[1]) % 4)))
    except ValueError:
        return None
    exp = claims.get("exp") if isinstance(claims, dict) else None
    return exp if isinstance(exp, (int, float)) and not isinstance(exp, bool) else None


def device_jwt(now_s=None, home=None):
    """(token, source) of the first device JWT that has not expired, from
    $COORD_DEVICE_JWT then ~/.qontinui/coord-device-jwt; or (None, what each
    source was). Never mints, and never prints a token.

    jwt-cascade-selection: each static source is gated on its own decoded
    `exp` (jwt_exp, with 30 s of margin) and an expired one is skipped, so a
    stale $COORD_DEVICE_JWT never shadows a live file token behind it."""
    now_s = time.time() if now_s is None else now_s
    path = os.path.join(home or os.path.expanduser("~"), ".qontinui", "coord-device-jwt")
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            from_file = f.read()
    except OSError:
        from_file = None
    tried = []
    for src, raw in (("$COORD_DEVICE_JWT", os.environ.get("COORD_DEVICE_JWT")), (path, from_file)):
        tok = "".join((raw or "").split())
        exp = jwt_exp(tok) if tok else None
        if not tok:
            tried.append(f"{src} absent")
        elif exp is None:
            tried.append(f"{src} has no readable exp")
        elif exp <= now_s + 30:
            tried.append(f"{src} expired")
        else:
            return tok, src
    return None, "; ".join(tried)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Follow no redirect: urllib's default re-issues the request, Authorization
    header and all, to whatever Location a 3xx names, http or another host
    included. Declining makes the 3xx an HTTPError, which direct_call reports."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def door_opener(proxies=None):
    """The opener direct_call sends through: no redirects followed. `proxies`
    None uses the environment's proxy settings; {} uses none (the selftest's
    loopback stubs)."""
    handlers = [_NoRedirect()] + ([urllib.request.ProxyHandler(proxies)] if proxies is not None else [])
    return urllib.request.build_opener(*handlers)


def direct_call(tool, args, opener=None):
    """One coord MCP tool over coord's own remote door, POST $COORD_HTTP_URL/mcp,
    with a device JWT. The tool's body, or nss.SignalUnknown naming why not.
    The token travels in-process only (never on argv), only over https (or
    plain http to a loopback stub), and never to a redirect's Location."""
    tok, src = device_jwt()
    if tok is None:
        raise nss.SignalUnknown(f"{tool}: no unexpired device JWT ({src})")
    url = (os.environ.get("COORD_HTTP_URL") or COORD_URL_DEFAULT).strip().rstrip("/") + "/mcp"
    parts = urllib.parse.urlsplit(url)
    if parts.scheme != "https" and not (parts.scheme == "http" and parts.hostname in LOOPBACK):
        raise nss.SignalUnknown(f"{tool}: refusing to send a device JWT to {parts.scheme}://{parts.hostname}")
    frame = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}
    req = urllib.request.Request(url, method="POST", data=json.dumps(frame).encode(),
                                 headers={"Content-Type": "application/json", "Accept": "application/json",
                                          "Authorization": "Bearer " + tok})
    try:
        raw = (opener or door_opener()).open(req, timeout=nss.DOOR_TIMEOUT_S).read()
    except urllib.error.HTTPError as e:
        if 300 <= e.code < 400:
            raise nss.SignalUnknown(f"{tool}: coord /mcp answered a redirect (HTTP {e.code} to "
                                    f"{str(e.headers.get('Location'))[:80]}); not followed, so nothing was "
                                    "sent there") from None
        raise nss.SignalUnknown(f"{tool}: coord /mcp answered HTTP {e.code} ({e.read()[:120]!r})") from None
    except (OSError, ValueError) as e:
        raise nss.SignalUnknown(f"{tool}: coord /mcp {type(e).__name__}: {str(e)[:80]}") from None
    try:
        body = nss.unwrap_mcp_text(json.loads(raw), door=tool)
    except ValueError:
        raise nss.SignalUnknown(f"{tool}: coord /mcp answered a body that is not JSON") from None
    except nss.EnvelopeUnknown as e:
        # The detail carries the tool's own error text; the full str() spends its length on provenance.
        raise nss.SignalUnknown(f"{tool}: coord /mcp: {(e.detail or f'key {e.path} {e.state}')[:240]}") from None
    if not isinstance(body, dict):
        raise nss.SignalUnknown(f"{tool}: body is not an object")
    return body


def record_pass(doc, proxy=None, direct=None):
    """Write the pass, then read it back by pass_id over the door that wrote it.
    Returns the one line to print. `proxy` is the session's own door
    (nss.coord_call), `direct` coord's /mcp door; both are injectable for the
    selftest. Raises only what a door raises that is not nss.SignalUnknown."""
    proxy, direct = proxy or nss.coord_call, direct or direct_call
    pid, n = doc["pass_id"], len(doc["rows"])
    size = len(json.dumps(doc, separators=(",", ":")))
    call, via = proxy, ""
    if size > PROXY_ARG_MAX:
        call, via = direct, f" over coord's /mcp door (the pass is {size} bytes, more than the session door carries)"
    try:
        try:
            answer = call(WRITE_TOOL, doc)
        except nss.SignalUnknown as e:
            if call is direct or not door_never_carried(e):
                return f"ledger UNKNOWN: {e}" + ("" if call is direct else " (not re-sent: the door may have carried it)")
            why = getattr(e, "door_verdict", None) or str(e)
            call, via = direct, f" over coord's /mcp door (the session door did not carry it: {why[:120]})"
            answer = call(WRITE_TOOL, doc)
        if answer.get("ok") is not True or answer.get("pass_id") != pid:
            return f"ledger UNKNOWN: {WRITE_TOOL} answered {json.dumps(answer)[:160]}{via}"
        back = call(READ_TOOL, {"pass_id": pid})
    except nss.SignalUnknown as e:
        return f"ledger UNKNOWN: {e}{via}"
    stored = back.get("pass") if isinstance(back.get("pass"), dict) else {}
    if stored.get("row_count") != n:
        return (f"ledger UNKNOWN: pass {pid} was written, but its read-back holds row_count="
                f"{stored.get('row_count')!r}, not {n}{via}")
    return f"ledger: recorded pass {pid} ({n} rows, read back){via}"


class PassRecorder:
    """What one run records: begun at argument parse, fed by each step, written
    once main() has its exit code. main() calls it; run() finishes it."""

    def __init__(self):
        self.doc = None
        self.res = None
        self.events = []  # ("rows", step, [acted rows]) or ("file", step, path), in the order the steps ran
        self.steps = {}
        self.lock = None
        self.tmpdir = None

    def begin(self, a, skip, lock_dir=None):
        """Mint the pass and, under --act, take the runner's pass lock. Returns
        an exit code when the run must stop (2 bad --port, 3 lock unopenable,
        4 BUSY), else None."""
        port = runner_port()
        if port is None:
            given = f"--port {a.port!r}" if a.port else \
                f"$QONTINUI_RUNNER_API_PORT={os.environ.get('QONTINUI_RUNNER_API_PORT')!r}"
            print(f"the runner port must be a number from 1 to 65535; {given} is not", file=sys.stderr)
            return 2
        started = dt.datetime.now(dt.timezone.utc)
        only = [o for o in a.only.split(",") if o]
        self.doc = {"pass_id": str(uuid.uuid4()), "runner_port": port, "scope": "filtered" if only or a.tab else "full",
                    "filter": {"only": only, "tab": list(a.tab)}, "mode": "act" if a.act else "report",
                    "skipped_steps": sorted(skip), "started_at": rfc3339(started)}
        if not a.act:
            return None
        path = lock_path(port, lock_dir)
        try:
            self.lock, held = take_lock(path, {"pid": os.getpid(), "started_at": self.doc["started_at"],
                                               "pass_id": self.doc["pass_id"]})
        except OSError as e:
            print(f"UNKNOWN: --act refused - the pass lock {path} could not be taken ({e})", file=sys.stderr)
            return 3
        if self.lock is None:
            print(f"BUSY: another --act pass holds {path} (holder: {held}; the holder writes that line just "
                  "after it takes the lock, so for a moment it can still name the previous holder); "
                  "nothing was triaged", file=sys.stderr)
            return LOCK_BUSY
        return None

    def screened(self, res):
        self.res = res

    def step(self, name, **facts):
        self.steps.setdefault(name, {}).update(facts)

    def rows_out(self, step):
        """--rows-out PATH for a helper this step runs; the file is merged at the end."""
        if self.tmpdir is None:
            self.tmpdir = tempfile.mkdtemp(prefix="triage-pass-")
        path = os.path.join(self.tmpdir, f"{len(self.events)}-{step}.jsonl")
        self.events.append(("file", step, path))
        return ["--rows-out", path]

    def handed_off(self, targets, outcomes):
        """The limit step's rows, from the outcome hand_off_limited and its
        aftercare recorded per row; a row with none recorded is unknown."""
        self.events.append(("rows", "limit", [
            {"terminal_id": r["terminalId"], "action": "limit_handoff",
             "action_outcome": outcomes.get(r["terminalId"], "unknown")} for r in targets]))

    def named(self, targets, named, typed):
        """The name step's rows. `typed` is what submit() recorded during the
        step ({terminal id: "sent" | "refused"}): done for a title read back,
        skipped for a row never typed into (no bypass mode, or held at its
        re-screen), failed for a POST the runner refused, sent_unconfirmed for
        a prompt typed whose title did not read back."""
        rows = []
        for r in targets:
            tid = r["terminalId"]
            outcome = ("done" if tid in named else "skipped" if tid not in typed
                       else "failed" if typed[tid] == "refused" else "sent_unconfirmed")
            rows.append({"terminal_id": tid, "action": "name", "action_outcome": outcome})
        self.events.append(("rows", "name", rows))

    def build(self, code, now=None, build=None, versions=None):
        """The pass document for coord. `now`, `build` and `versions` are
        injectable so the selftest builds without the clock, runner or git."""
        events, bad = [], 0
        for kind, _step, item in self.events:
            if kind == "file":
                got, n_bad = read_rows_file(item)
                events += got
                bad += n_bad
            else:
                events += item
        observed = self.doc["started_at"]
        rows = [pass_row(r, observed) for r in self.res]
        dropped = apply_events(rows, events) + bad
        histogram = {}
        for r in self.res:
            histogram[r["verdict"]] = histogram.get(r["verdict"], 0) + 1
        counts = step_tallies(self.steps, events)
        if dropped:
            counts["ledger"] = {"rows_dropped": dropped}
        build = runner_build() if build is None else build
        return dict(self.doc, runner_build=build,
                    ended_at=rfc3339(now or dt.datetime.now(dt.timezone.utc)), exit_code=code,
                    step_counts=counts, verdict_histogram=histogram,
                    overlord_versions=overlord_versions(build) if versions is None else versions, rows=rows)

    def finish(self, code, writer=None):
        """Write the pass and print the ledger line. Never raises (an interrupt
        included), never changes the exit code, and releases the lock."""
        try:
            if self.doc is None:
                return
            if self.res is None:
                _say(f"\nledger: no pass recorded - nothing was screened (exit {code})")
                return
            _say("\n" + (writer or record_pass)(self.build(code)))
        except KeyboardInterrupt:
            _say("\nledger UNKNOWN: interrupted")
        except Exception as e:  # the ledger must never take the run down with it
            _say(f"\nledger UNKNOWN: {type(e).__name__}: {str(e)[:160]}")
        finally:
            self.release()

    def release(self):
        if self.lock is not None:
            release_lock(self.lock)
            self.lock = None
        if self.tmpdir is not None:
            shutil.rmtree(self.tmpdir, ignore_errors=True)
            self.tmpdir = None


def _say(line):
    """print() for the ledger: a closed stdout (BrokenPipeError) or an
    interrupt mid-print must not end the run either."""
    try:
        print(line, flush=True)
    except (OSError, ValueError, KeyboardInterrupt):
        pass


PASS = PassRecorder()


def run(main_fn=main, recorder=None, writer=None, argv=None):
    """main(), then the pass ledger: under --selftest the ledger's own tests
    run after main()'s; otherwise the recorder writes the pass. The exit code
    is main()'s, whatever the ledger did. When main() itself raises, the lock
    and the rows-out files are released before the error propagates."""
    argv = sys.argv[1:] if argv is None else argv
    recorder = PASS if recorder is None else recorder
    try:
        code = main_fn()
    except BaseException:
        recorder.release()
        raise
    if "--selftest" in argv:
        return 1 if ledger_selftest() or code else 0
    recorder.finish(code, writer)
    return code


# ---------------------------------------------------------------- ledger selftest
LOCK_CHILD = (
    "import importlib.util, os, sys\n"
    "spec = importlib.util.spec_from_file_location('ts', sys.argv[1])\n"
    "m = importlib.util.module_from_spec(spec)\n"
    "spec.loader.exec_module(m)\n"
    "fd, held = m.take_lock(sys.argv[2], {'pid': os.getpid(), 'started_at': 'selftest', 'pass_id': 'child'})\n"
    "print('held' if fd is not None else 'busy: ' + held, flush=True)\n"
    "sys.stdin.read()\n")


def _fake_jwt(exp):
    def part(obj):
        return base64.urlsafe_b64encode(json.dumps(obj).encode()).decode().rstrip("=")
    return f"{part({'alg': 'none'})}.{part({'exp': exp})}.sig"


def ledger_selftest():
    """The row builder, the pass lock, the failed-write path and the door
    fallback. Reads nothing live and types nothing."""
    import contextlib
    import io
    bad = []
    sid = "0f2a1b3c-4d5e-4f60-8a7b-9c0d1e2f3a4b"
    res = [
        {"terminalId": "t-stop", "verdict": "STOPPED", "screen": "ACTIVE", "reasons": ["a", "b", "c", "d"],
         "name": "n" * 300, "tab": "infra", "account": "acct1", "claudeSessionId": sid, "finishedAt": None},
        {"terminalId": "t-fin", "verdict": "FINISHED", "screen": "FINISHED", "reasons": ["marker"],
         "name": "done one", "tab": "web", "account": "?", "claudeSessionId": "not-a-uuid",
         "finishedAt": 1761270000000},
        {"terminalId": "t-held", "verdict": "HELD", "screen": "REVIVED", "reasons": [], "name": "",
         "tab": "web", "account": "acct2", "claudeSessionId": None, "finishedAt": 1761270000000},
    ]
    row = pass_row(res[0], "2026-10-24T00:00:00.000Z")
    want = {"terminal_id": "t-stop", "claude_session_id": sid, "session_name": "n" * TEXT_MAX, "tab": "infra",
            "account": "acct1", "screen_verdict": "ACTIVE", "verdict": "STOPPED", "reason_codes": [],
            "state_since_ms": None, "progress_since_nudge": None, "action": "none", "action_outcome": None,
            "detail": {"reasons": ["a", "b", "c"]}, "observed_at": "2026-10-24T00:00:00.000Z"}
    if row != want:
        bad.append(f"pass_row(STOPPED) = {row}")
    fin, held = pass_row(res[1], "x"), pass_row(res[2], "x")
    if (fin["claude_session_id"], fin["account"], fin["state_since_ms"]) != (None, None, 1761270000000):
        bad.append(f"pass_row(FINISHED) = {fin}")
    if (held["state_since_ms"], held["session_name"]) != (None, None):
        bad.append(f"pass_row(HELD) keeps finishedAt or an empty name: {held}")

    # The recorder over those screens: the last acting step names a row's action.
    rec = PassRecorder()
    ns = argparse.Namespace(act=False, only="t-st,t-fi", tab=[], port=None)
    if rec.begin(ns, {"layout"}) is not None:
        bad.append("begin() in report mode refused")
    rec.screened(res)
    try:
        rec.step("close", candidates=1)
        args = rec.rows_out("close")
        # The helpers' own writer, so the format they emit is the one this reads.
        cfs.append_rows_out(args[1], {"terminal_id": "t-fin", "action": "close", "action_outcome": "done",
                                      "detail": {"message": "DELETE sent"}})
        with open(args[1], "a", encoding="utf-8") as f:
            f.write("not json\n")
        rec.named([res[0]], ["t-stop"], {"t-stop": "sent"})
        rec.handed_off([res[2]], {})
        args = rec.rows_out("nudge")
        with open(args[1], "w", encoding="utf-8") as f:
            f.write(json.dumps({"terminal_id": "t-stop", "action": "nudge", "action_outcome": "sent_unconfirmed",
                                "detail": {"sent": True}}) + "\n")
            f.write(json.dumps({"terminal_id": "t-gone", "action": "nudge", "action_outcome": "done"}) + "\n")
            f.write(json.dumps({"terminal_id": "t-stop", "action": "poke", "action_outcome": "done"}) + "\n")
        when = dt.datetime(2026, 10, 24, 1, 2, 3, tzinfo=dt.timezone.utc)
        doc = rec.build(1, now=when, build="abc123", versions={"triage-sessions.py": "f00"})
        by = {r["terminal_id"]: r for r in doc["rows"]}
        checks = [
            (doc["scope"], "filtered"), (doc["filter"], {"only": ["t-st", "t-fi"], "tab": []}),
            (doc["mode"], "report"), (doc["skipped_steps"], ["layout"]), (doc["exit_code"], 1),
            (doc["ended_at"], "2026-10-24T01:02:03.000Z"), (doc["runner_build"], "abc123"),
            (doc["verdict_histogram"], {"STOPPED": 1, "FINISHED": 1, "HELD": 1}),
            ((by["t-stop"]["action"], by["t-stop"]["action_outcome"]), ("nudge", "sent_unconfirmed")),
            (sorted(by["t-stop"]["detail"]), ["nudge", "reasons"]),
            ((by["t-fin"]["action"], by["t-fin"]["action_outcome"]), ("close", "done")),
            ((by["t-held"]["action"], by["t-held"]["action_outcome"]), ("limit_handoff", "unknown")),
            (doc["step_counts"]["close"], {"candidates": 1, "done": 1}),
            # t-gone counts: the helper acted on it, though no screened row carries it.
            (doc["step_counts"]["nudge"], {"sent_unconfirmed": 1, "done": 1}),
            (doc["step_counts"]["name"], {"done": 1}),
            (doc["step_counts"]["ledger"], {"rows_dropped": 3}),  # the bad line, t-gone, the unknown action
            (len(doc["rows"]), 3),
        ]
        for got, exp in checks:
            if got != exp:
                bad.append(f"build(): {got!r}, want {exp!r}")
        if not all((r["action"] == "none") == (r["action_outcome"] is None) for r in doc["rows"]):
            bad.append("build(): a row breaks (action = 'none') = (action_outcome IS NULL)")
    finally:
        rec.release()

    # The --act lock: refused with 4 while a live child holds it, free once the child exits.
    lock_dir = tempfile.mkdtemp(prefix="triage-lock-")
    try:
        act = argparse.Namespace(act=True, only="", tab=[], port=None)
        port = runner_port()
        # -B: the child imports this file as a module, which must not leave a __pycache__ in the skill tree.
        child = subprocess.Popen([sys.executable, "-B", "-c", LOCK_CHILD, os.path.abspath(__file__),
                                  lock_path(port, lock_dir)],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            first = child.stdout.readline().strip()
            if first != "held":
                bad.append(f"lock child did not take the lock: {first!r}")
            err = io.StringIO()
            busy = PassRecorder()
            with contextlib.redirect_stderr(err):
                got = busy.begin(act, set(), lock_dir=lock_dir)
            if got != LOCK_BUSY or f"pid {child.pid}" not in err.getvalue():
                bad.append(f"begin() under a held lock = {got!r}, printed {err.getvalue()!r}")
            busy.release()
        finally:
            child.stdin.close()
            child.wait(timeout=30)
        free = PassRecorder()
        got = free.begin(act, set(), lock_dir=lock_dir)
        if got is not None or free.lock is None:
            bad.append(f"begin() after the holder exited = {got!r}")
        free.release()
    finally:
        shutil.rmtree(lock_dir, ignore_errors=True)

    # A failed write prints ledger UNKNOWN and leaves the exit code alone; so
    # does an interrupt, and a stdout that is gone.
    def boom(_doc):
        raise RuntimeError("selftest writer failed")

    def interrupt(_doc):
        raise KeyboardInterrupt
    writers = ((boom, 1, "ledger UNKNOWN: RuntimeError"), (lambda _doc: "ledger UNKNOWN: door down", 0, "door down"),
               (interrupt, 1, "ledger UNKNOWN: interrupted"))
    for writer, code, want_text in writers:
        r2 = PassRecorder()
        r2.begin(argparse.Namespace(act=False, only="", tab=[], port=None), set())
        r2.screened(res)
        r2.build = lambda c, _r=r2: {"pass_id": "p", "rows": []}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            got = run(main_fn=lambda c=code: c, recorder=r2, writer=writer, argv=[])
        if got != code or want_text not in out.getvalue():
            bad.append(f"run() with a failing writer = {got!r} (want {code}), printed {out.getvalue()!r}")

    class GoneStdout(io.StringIO):
        def write(self, _s):
            raise BrokenPipeError
    with contextlib.redirect_stdout(GoneStdout()):
        try:
            _say("ledger: x")
        except BaseException as e:
            bad.append(f"_say() on a closed stdout raised {e!r}")

    # A main() that raises still releases the rows-out files and the lock.
    r3 = PassRecorder()
    r3.begin(argparse.Namespace(act=False, only="", tab=[], port=None), set())
    out_dir = os.path.dirname(r3.rows_out("close")[1])

    def crash():
        raise RuntimeError("selftest main crashed")
    try:
        run(main_fn=crash, recorder=r3, argv=[])
        bad.append("run() swallowed main()'s exception")
    except RuntimeError:
        pass
    if os.path.isdir(out_dir) or r3.tmpdir is not None:
        bad.append(f"run() left {out_dir} behind after main() raised")

    # A report pass takes no lock: nothing is created in the lock directory.
    lock_dir = tempfile.mkdtemp(prefix="triage-lock-")
    try:
        r4 = PassRecorder()
        if r4.begin(argparse.Namespace(act=False, only="", tab=[], port=None), set(), lock_dir=lock_dir) is not None \
                or os.listdir(lock_dir) or r4.lock is not None:
            bad.append(f"a report pass touched the lock directory: {os.listdir(lock_dir)}")
        r4.release()
    finally:
        shutil.rmtree(lock_dir, ignore_errors=True)

    bad += outcome_selftest()

    # The door fallback: only a verdict that proves coord never saw the call goes to /mcp.
    doc = {"pass_id": "p-1", "rows": [{}, {}]}

    def door(name, fail_write=None, rows=2, log=None):
        def call(tool, args):
            log.append((name, tool))
            if tool == WRITE_TOOL and fail_write:
                raise nss.SignalUnknown(*fail_write)
            if tool == WRITE_TOOL:
                return {"ok": True, "pass_id": args["pass_id"], "row_count": len(args["rows"]), "replayed": False}
            return {"ok": True, "pass": {"pass_id": args["pass_id"], "row_count": rows}}
        return call

    def verdict(line):  # what nss.coord_call raises: the cut excerpt, and the door's whole verdict line
        return f"{WRITE_TOOL}: door exit 1 ({line[:80]})", line
    direct_log = [("proxy", WRITE_TOOL), ("direct", WRITE_TOOL), ("direct", READ_TOOL)]
    sent_on = "ledger: recorded pass p-1 (2 rows, read back) over coord's /mcp door"
    carried = [  # the door's whole verdict line: re-sent over /mcp
        ("coord-revive: call -> METHOD_NOT_ALLOWED (cause=drift remedy=if coord lists coord_overlord_pass_record) "
         "(door http://127.0.0.1:9876/coord-mcp from /w/.mcp.json; the call was NOT carried)"),
        "coord-revive: call -> CONNECT_REFUSED (dead port - no listener) (door http://127.0.0.1:9876/coord-mcp)",
        "coord-revive: call -> CREDENTIAL_REFRESHING (retry-safe - HTTP 503) (door http://127.0.0.1:9876/x)",
        ("coord-revive: call -> COORD_MCP_PROXY_UNAUTHORIZED (the forwarder at http://127.0.0.1:9876/coord-mcp "
         "rejected the nonce in /w/.mcp.json; HTTP 401)."),
        "coord-revive: call -> RUNNER_CREDENTIAL_EXPIRED (the forwarder at http://127.0.0.1:9876/x answered 401)",
        "coord-revive: call -> NO_PROXY_CONFIG (no proxy-shaped coord-mcp .mcp.json with a key ...)",
    ]
    fallback = [(None, 2, "ledger: recorded pass p-1 (2 rows, read back)", [("proxy", WRITE_TOOL), ("proxy", READ_TOOL)])]
    fallback += [(verdict(v), 2, sent_on, direct_log) for v in carried]
    fallback += [
        ((NO_DOOR + ", or no Git Bash to run it",), 2, "ledger: recorded pass p-1", direct_log),
        # Coord answered, or may have: never re-sent.
        (verdict("coord-revive: call -> RPC_ERROR (the tool answered with a JSON-RPC error - printed above)"), 2,
         "ledger UNKNOWN: ", [("proxy", WRITE_TOOL)]),
        (verdict("coord-revive: call -> TIMEOUT (75s - the request was abandoned client-side)"), 2,
         "ledger UNKNOWN: ", [("proxy", WRITE_TOOL)]),
        ((f"{WRITE_TOOL}: TimeoutExpired",), 2, "ledger UNKNOWN: ", [("proxy", WRITE_TOOL)]),
        # The verdict word in the cut excerpt alone, with no whole verdict line, is not enough.
        ((f"{WRITE_TOOL}: door exit 1 (coord-revive: call -> METHOD_NOT_ALLOWED (cause=drift)",), 2,
         "ledger UNKNOWN: ", [("proxy", WRITE_TOOL)]),
        (None, 1, "ledger UNKNOWN: pass p-1 was written, but its read-back holds row_count=1, not 2",
         [("proxy", WRITE_TOOL), ("proxy", READ_TOOL)]),
    ]
    for fail, rows, want_line, want_log in fallback:
        log = []
        line = record_pass(doc, proxy=door("proxy", fail, rows, log), direct=door("direct", None, rows, log))
        if not line.startswith(want_line) or log != want_log:
            bad.append(f"record_pass(proxy fails {str(fail)[:60]!r}) = {line!r} via {log}")
    log = []
    big = {"pass_id": "p-2", "rows": [{"x": "y" * PROXY_ARG_MAX}]}
    line = record_pass(big, proxy=door("proxy", None, 1, log), direct=door("direct", None, 1, log))
    if log != [("direct", WRITE_TOOL), ("direct", READ_TOOL)] or "over coord's /mcp door" not in line:
        bad.append(f"record_pass(oversize) = {line!r} via {log}")
    bad += door_wording_selftest()

    # The device JWT: an expired source is skipped, never sent.
    home = tempfile.mkdtemp(prefix="triage-jwt-")
    saved = os.environ.get("COORD_DEVICE_JWT")
    try:
        os.makedirs(os.path.join(home, ".qontinui"))
        with open(os.path.join(home, ".qontinui", "coord-device-jwt"), "w") as f:
            f.write(_fake_jwt(2000) + "\n")
        os.environ["COORD_DEVICE_JWT"] = _fake_jwt(500)
        tok, src = device_jwt(now_s=1000, home=home)
        if tok != _fake_jwt(2000) or not src.endswith("coord-device-jwt"):
            bad.append(f"device_jwt(env expired, file live) chose {src!r}")
        tok, why = device_jwt(now_s=5000, home=home)
        if tok is not None or "expired" not in why:
            bad.append(f"device_jwt(both expired) = {tok!r}, {why!r}")
        if jwt_exp("a.b") is not None or jwt_exp("a.%%%.c") is not None:
            bad.append("jwt_exp read a malformed token")
    finally:
        shutil.rmtree(home, ignore_errors=True)
        if saved is None:
            os.environ.pop("COORD_DEVICE_JWT", None)
        else:
            os.environ["COORD_DEVICE_JWT"] = saved
    bad += direct_door_selftest()
    for b in bad:
        print("FAIL: " + b)
    print(f"ledger selftest: 3 row cases, {len(checks)} build checks, 3 lock cases, {len(writers) + 2} never-raises "
          f"cases, outcome mappings, {len(fallback) + 1} door cases, coord-revive wording pins, 3 device-JWT cases, "
          f"7 /mcp door cases, {len(bad)} failure(s)")
    return 1 if bad else 0


def outcome_selftest():
    """The per-row outcome mappings: the close and nudge helpers' (pinned
    against the message prefixes their act functions return), the limit step's
    (hand_off_limited over stubbed doors) and the name step's. Returns failures."""
    import contextlib
    import inspect
    import io
    bad = []
    close_cases = [(True, "DELETE sent; read back: tab gone", "done"),
                   (False, "DELETE answered 409: b'busy'", "failed"),
                   (False, "DELETE sent; read back: listed=True, claude pids still alive=[1]", "failed"),
                   (False, "DELETE errored (URLError: x); read back: listed=None", "failed"),
                   (False, "re-screen now says BUSY: moving", "skipped"),
                   (False, "terminal no longer listed (already closed?)", "skipped")]
    for ok, msg, want in close_cases:
        if cfs.close_outcome(ok, msg) != want:
            bad.append(f"close_outcome({ok}, {msg[:30]!r}) = {cfs.close_outcome(ok, msg)!r}, want {want!r}")
    nudge_cases = [(True, True, "submitted; read back", "done"),
                   (False, True, "submitted; read back: no /next-move command", "sent_unconfirmed"),
                   (False, False, "submit-prompt answered 500: b'x'", "failed"),
                   (False, False, "re-screen now says HELD: x", "skipped")]
    for ok, sent, msg, want in nudge_cases:
        if nss.nudge_outcome(ok, sent, msg) != want:
            bad.append(f"nudge_outcome({ok}, {sent}, {msg[:30]!r}) = {nss.nudge_outcome(ok, sent, msg)!r}")
    # The prefixes the mappings key on are the ones the act functions return.
    close_src, nudge_src = inspect.getsource(cfs.close_one), inspect.getsource(nss.nudge_one)
    for src, needle in ((close_src, 'return False, f"' + cfs.DELETE_SENT + 'answered '),
                        (close_src, 'return True, f"' + cfs.DELETE_SENT + '{sent}; read back'),
                        (close_src, 'return False, f"' + cfs.DELETE_SENT + '{sent}; read back'),
                        (nudge_src, 'return False, False, f"' + nss.SUBMIT_REFUSED)):
        if needle not in src:
            bad.append(f"the act function no longer returns {needle!r}: its outcome mapping would misread it")

    # The limit step: held at the re-screen -> skipped; POST refused -> failed;
    # sent and read back by aftercare -> done; sent with no read-back -> sent_unconfirmed.
    g = globals()
    saved = g["ready_to_type"], g["submit"], g["aftercare"], g["screen_text"]
    try:
        g["screen_text"] = lambda tid, chars=3000: ""
        g["ready_to_type"] = lambda tid, *_a, **_k: (tid != "l-held", "held at re-screen", None)
        g["submit"] = lambda tid, msg: (tid != "l-refused", "submit-prompt answered 500" if tid == "l-refused" else "")

        def after(tids, a, own, wait_s=60, report_running=False, outcomes=None):
            outcomes["l-handed"] = "done"
            return 0
        g["aftercare"] = after
        outcomes = {}
        targets = [{"terminalId": t, "name": t} for t in ("l-held", "l-refused", "l-handed", "l-quiet")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            hand_off_limited(targets, argparse.Namespace(idle_min=15, cooldown_hours=None), "", outcomes)
        want = {"l-held": "skipped", "l-refused": "failed", "l-handed": "done", "l-quiet": "sent_unconfirmed"}
        if outcomes != want:
            bad.append(f"hand_off_limited outcomes = {outcomes}, want {want}")
    finally:
        g["ready_to_type"], g["submit"], g["aftercare"], g["screen_text"] = saved

    # The name step: from the titles read back and what submit() recorded.
    rec = PassRecorder()
    rec.named([{"terminalId": t} for t in ("n-named", "n-manual", "n-refused", "n-typed")], ["n-named"],
              {"n-named": "sent", "n-refused": "refused", "n-typed": "sent"})
    got = {r["terminal_id"]: r["action_outcome"] for r in rec.events[0][2]}
    want = {"n-named": "done", "n-manual": "skipped", "n-refused": "failed", "n-typed": "sent_unconfirmed"}
    if got != want:
        bad.append(f"named() outcomes = {got}, want {want}")
    return bad


def coord_revive_source():
    """coord-revive.sh beside this script (the repository's or the provisioned
    skills tree's), or None."""
    for rel in ((os.pardir, os.pardir, "coord-revive"), (os.pardir, ".claude", "skills", "coord-revive")):
        c = os.path.normpath(os.path.join(_here, *rel, "coord-revive.sh"))
        if os.path.isfile(c):
            return c
    return None


def door_wording_selftest():
    """Pin the not-carried verdicts to coord-revive.sh's own text, and check that
    nss.coord_call hands the door's whole verdict line up uncut. Returns failures."""
    bad = []
    src_path = coord_revive_source()
    if src_path is None:
        return ["coord-revive.sh not found beside this script: the fallback's verdict words cannot be pinned"]
    with open(src_path, encoding="utf-8", errors="replace") as f:
        src = f.read()
    for token, printed in list(NOT_CARRIED_VERDICTS.items()) + [("verdict line", DOOR_VERDICT_LINE)]:
        if printed not in src:
            bad.append(f"coord-revive.sh no longer prints {printed!r} ({token}): the /mcp fallback would never fire")
    # No door at all: coord_call's own message.
    saved_door = nss.coord_door
    try:
        nss.coord_door = lambda: None
        try:
            nss.coord_call(WRITE_TOOL, {})
            bad.append("coord_call with no door did not raise")
        except nss.SignalUnknown as e:
            if not door_never_carried(e):
                bad.append(f"coord_call's no-door message is not read as not carried: {e}")
    finally:
        nss.coord_door = saved_door
    if os.name == "nt":
        return bad  # the fake door below is a POSIX script
    tmp = tempfile.mkdtemp(prefix="triage-door-")
    saved = os.environ.get("NSS_COORD_DOOR")
    try:
        fake = os.path.join(tmp, "door")
        line = ("coord-revive: call -> COORD_MCP_PROXY_UNAUTHORIZED (the forwarder at http://127.0.0.1:9876/coord-mcp "
                "rejected the nonce in /w/.mcp.json; HTTP 401). The BINDING was superseded or never registered")
        with open(fake, "w") as f:
            f.write(f"#!/bin/sh\necho '{line}' >&2\necho '  Recovery for THIS caller: start a new session' >&2\nexit 1\n")
        os.chmod(fake, 0o755)
        os.environ["NSS_COORD_DOOR"] = fake
        try:
            nss.coord_call(WRITE_TOOL, {})
            bad.append("the fake door's exit 1 did not raise")
        except nss.SignalUnknown as e:
            if e.door_verdict != line or not door_never_carried(e) or "Recovery" not in str(e):
                bad.append(f"coord_call's door_verdict = {e.door_verdict!r} (message {e})")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
        if saved is None:
            os.environ.pop("NSS_COORD_DOOR", None)
        else:
            os.environ["NSS_COORD_DOOR"] = saved
    return bad


def direct_door_selftest():
    """direct_call() against loopback stubs of coord's /mcp door: the body, the
    bearer it sends, the tool's own error text, an HTTP refusal, a redirect it
    must not follow (the second stub must never see the bearer), and the
    refusal to send a token over plain http to a remote host. The stubs are
    reached with no proxy, so an http_proxy in the environment cannot divert
    them. Returns failure lines."""
    import http.server
    import threading
    bad, seen, elsewhere = [], [], []
    token = _fake_jwt(time.time() + 3600)
    no_proxy = door_opener({})

    class Elsewhere(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            elsewhere.append(self.headers.get("Authorization"))
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"{}")
        do_GET = do_POST  # urllib's default redirect re-issues a POST as a GET

        def log_message(self, *_a):
            pass
    other = http.server.HTTPServer(("127.0.0.1", 0), Elsewhere)
    threading.Thread(target=other.serve_forever, daemon=True).start()

    class Stub(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            frame = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            seen.append((self.path, self.headers.get("Authorization"), frame["params"]["name"]))
            if frame["params"]["arguments"]["case"] == "redirect":
                self.send_response(302)
                self.send_header("Location", f"http://127.0.0.1:{other.server_port}/mcp")
                self.end_headers()
                return
            reply = {"body": {"ok": True, "pass_id": "p"}, "error": None}.get(frame["params"]["arguments"]["case"])
            if frame["params"]["arguments"]["case"] == "http":
                self.send_response(401)
                self.end_headers()
                self.wfile.write(b'{"error":"unauthorized"}')
                return
            text = json.dumps(reply) if reply is not None else "unknown tool: x"
            result = {"content": [{"type": "text", "text": text}]}
            if reply is None:
                result["isError"] = True
            self.send_response(200)
            self.end_headers()
            self.wfile.write(json.dumps({"jsonrpc": "2.0", "id": 1, "result": result}).encode())

        def log_message(self, *_a):
            pass
    server = http.server.HTTPServer(("127.0.0.1", 0), Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    saved = {k: os.environ.get(k) for k in ("COORD_HTTP_URL", "COORD_DEVICE_JWT")}
    try:
        os.environ["COORD_HTTP_URL"] = f"http://127.0.0.1:{server.server_port}/"
        os.environ["COORD_DEVICE_JWT"] = token
        cases = (("body", None), ("error", "the tool answered an error, not a body: 'unknown tool: x'"),
                 ("http", "answered HTTP 401"))
        cases += (("redirect", "answered a redirect (HTTP 302"),)
        for case, want_err in cases:
            try:
                got, err = direct_call(WRITE_TOOL, {"case": case}, opener=no_proxy), ""
            except nss.SignalUnknown as e:
                got, err = None, str(e)
            if (want_err is None and got != {"ok": True, "pass_id": "p"}) or (want_err and want_err not in err):
                bad.append(f"direct_call({case}) = {got!r}, {err!r}")
        if seen and seen[0] != ("/mcp", "Bearer " + token, WRITE_TOOL):
            bad.append(f"direct_call sent {seen[0][0]!r} for {seen[0][2]!r}, or not the device JWT as the bearer")
        if elsewhere:
            bad.append(f"direct_call followed a redirect and sent {len(elsewhere)} request(s) to its Location")
        if not any(isinstance(h, _NoRedirect) for h in door_opener().handlers):
            bad.append("the production opener follows redirects")
        sent = len(seen)
        os.environ["COORD_HTTP_URL"] = "http://coord.example.invalid"
        try:
            direct_call(WRITE_TOOL, {"case": "body"}, opener=no_proxy)
            bad.append("direct_call sent a device JWT over plain http to a remote host")
        except nss.SignalUnknown as e:
            if "refusing" not in str(e) or len(seen) != sent:
                bad.append(f"direct_call over remote http: {e}")
    finally:
        for srv in (server, other):
            srv.shutdown()
            srv.server_close()
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
    return bad


if __name__ == "__main__":
    sys.exit(run())
