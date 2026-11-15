#!/usr/bin/env python3
"""Triage every runner terminal session in one pass: close the finished ones,
hand the usage-limited ones to another account, name the unnamed ones, trim
each page's grid, nudge the stopped ones.

Usage: triage-sessions.py [--act] [--skip STEP[,STEP..]] [--max-name N]
                          [--max-nudge N] [--name-timeout S] [--idle-min N]
                          [--cooldown-hours H]
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

  close   close-finished-sessions.py --close: closes that script's CLOSABLE
          tabs, FINISHED (a session coord reads finished) and EXITED-FINISHED,
          each re-screened and read back by that script. The report predicts
          from the same set, read off the same screen. Then, in every mode,
          the finished rows a screen rule holds, with their finish_stranded
          code (cfs.print_stranded).
  limit   for each LIMITED session (stopped by an account usage limit): type
          /next-move. It reaches the exhausted account, Claude prints its limit
          message, and the runner resumes the session in a new pane under
          another account and closes this one (HANDED). Every session parked on
          Claude's open usage-limit menu gets Esc first, whether or not its
          transcript carries a limit message (a background subagent's limit
          carries none; a row whose reset passed carries one), which can let
          Claude print that message. When the runner then moves
          it, the row reads HANDED; when its pane closes with the session
          nowhere, GONE; nothing is typed either way (hand_off_limited).
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
          remaining tabs, since a runner older than auto-trim grows a grid by
          itself but never shrinks one. A page in tmux work mode is skipped, not re-gridded, and
          so is a page whose runner reports it chose the layout itself (it
          trims those on close) or that the operator chose (layoutOrigin).
          Without --act the preview passes the close step's would-close
          terminals as --assume-closed (layout_argv), so it names the trims an
          act pass makes after its closes; none when the close step is skipped.
  nudge   nudge-stopped-sessions.py --send: types /next-move into each STOPPED
          row. A session named in this run just finished a turn, so it reads
          BUSY to the idle floor; it is nudged in a second pass limited to the
          named rows with the idle floor off (every other check still runs).
          STUCK rows, and HELD rows carrying a finish_stranded code, are
          listed and never nudged (operator_rows). A finish the session's own
          record contradicts (CONTRADICTED) is listed and nudged, so /next-move
          reconciles it.

While an operator restart prepare is `preparing` or `ready`, limit, name and
nudge are held (typing_steps_held) and an unreadable restart_prep refuses --act.

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
import http.client
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
RESCREEN_NOW_SAYS = "re-screen now says "
RESCREEN_STOPPED = RESCREEN_NOW_SAYS + "STOPPED"


def ready_to_type(tid, idle_min, cooldown_h, own, want="STOPPED"):
    """Re-screen one row just before typing into it: nudge_one's pre-send
    checks in nudge-stopped-sessions.py, for a prompt other than /next-move.
    Returns (ok, msg, tx): ok only when the row still reads `want`, its output
    is still, and on the final screen no usage-limit menu is open and its input
    prompt is empty."""
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
        return False, f"{RESCREEN_NOW_SAYS}{verdict}: {'; '.join(why)}", tx
    moving = cfs.output_moving(tid)
    if moving is not False:
        return False, "terminal output still moving" if moving else "terminal output could not be sampled", tx
    # A prepare can start after the pass's own read. This read can take seconds,
    # so it comes before the prompt look below, which must stay the last thing here.
    prep, prep_text = cfs.prepare_status()
    if prep != "clear":
        return False, f"re-screen: {prep_text}", tx
    # submit-prompt pastes onto whatever the input box holds, so a draft typed
    # during the re-screen must not ride along. The replay wraps at the pane's
    # width, as the screen's does, or a Windows idle prompt reads as a draft.
    raw = cfs.raw_tail(tid)
    # An open usage-limit menu takes the next Enter as a choice, and prompt_state
    # does not count text drawn dim (a suggestion is dim), so a menu whose option
    # label is dim reads as an empty prompt there: an open menu refuses the send
    # by itself, read from this same final look.
    if raw is not None and limit_menu_open(cfs.ANSI_RE.sub("", raw)):
        return False, "Claude's usage-limit menu is open just before the send, so nothing is typed into it", tx
    state, typed = cfs.prompt_state(raw, cols=cfs.wrap_cols(t)) if raw is not None else ("unknown", "screen unreadable")
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
        try:  # a refused POST's body can be cut short: that must not end the pass
            body_text = e.read()[:200]
        except Exception:  # noqa: BLE001 - IncompleteRead, timeout, reset: the refusal itself is the fact
            body_text = b""
        return False, f"submit-prompt answered {e.code}: {body_text!r}"
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
# account, or when a background subagent hits the limit. Triage never chooses
# an option on it: what a number selects depends on the account (with usage
# credits enabled, 2 is "Switch to usage credits", which is paid usage and
# opens a login dialog), and a narrow pane draws only the first option. The
# handoff is the runner's: it reads the limit message Claude prints, resumes
# the session in a new pane under another account, and closes this one.
# The menu is matched by the screen verdict's own helpers, so the classifier
# that reads such a pane LIMITED and the steps that dismiss its menu read one
# screen alike (plan
# 2026-11-12-a-limit-hit-by-a-background-subagent-shows-only-the-rate-limit-menu-so-the-pane-is-never-migrated, D2).
LIMIT_MENU, MENU_OPEN, limit_menu_open = cfs.LIMIT_MENU, cfs.MENU_OPEN, cfs.limit_menu_open
UNKNOWN_NEXT_MOVE = "Unknown command: /next-move"
SURVEY = "How is Claude doing this session?"
# Typed into a session whose own command set has no /next-move.
NEXT_MOVE_FROM_MAIN = ("This session has no /next-move command. Run it from qontinui-claude-config origin/main: "
                       "read .claude/commands/next-move.md at origin/main of the qontinui-claude-config repo "
                       "and follow it as if /next-move had been invoked.")


def screen_text(tid, chars=3000):
    """The ANSI-stripped end of a pane's screen: its last `chars` characters, or
    all that raw_tail reads when `chars` is None (what limit_menu_open takes,
    since it windows the text itself). None when unreadable."""
    raw = cfs.raw_tail(tid)
    if raw is None:
        return None
    text = cfs.ANSI_RE.sub("", raw)
    return text if chars is None else text[-chars:]


# {terminal id: keystroke writes attempted}, counted BEFORE each POST, so a write
# that raised still counts: it may have reached the pane. aftercare ends a pane
# whose look raised after it typed (Esc on a limit menu, 0 on a survey), rather
# than look again and type a second time into a session this run does not own.
KEYS_WRITTEN = {}


def write_keys(tid, keys):
    """Write raw keystrokes into a pane, counted in KEYS_WRITTEN first."""
    KEYS_WRITTEN[tid] = KEYS_WRITTEN.get(tid, 0) + 1
    post_keys(tid, keys)


def post_keys(tid, keys):
    """The runner's POST /terminals/{id}/write, the keys base64-encoded."""
    req = urllib.request.Request(f"{cfs.API}/terminals/{tid}/write", method="POST",
                                 data=json.dumps({"data": base64.b64encode(keys).decode()}).encode(),
                                 headers={"Content-Type": "application/json"})
    urllib.request.urlopen(req, timeout=20).read()


def session_id(rows, tid):
    """The Claude session id a registry read (cfs.open_rows()) records for a
    pane, or "". The caller reads the registry once for all its panes: a
    registry read is slow on a loaded runner."""
    return (rows.get(tid) or {}).get("claudeSessionId") or ""


def successor(sid, gone, wait_s=30):
    """The pane, other than `gone`, that now holds Claude session `sid`, or ""."""
    deadline = time.time() + wait_s
    while sid:
        found = [i for i, r in cfs.open_rows().items() if i != gone and (r or {}).get("claudeSessionId") == sid]
        if found or time.time() >= deadline:
            return found[0] if found else ""
        time.sleep(2)
    return ""


def wait_for_handoffs(panes, wait_s=30, found=None):
    """Wait, against ONE deadline, for the runner to move the sessions of every
    pane in `panes` ({terminal id: Claude session id or None}). Fills and returns
    `found`, {terminal id: (state, message)} for each pane that closed: "handed"
    (another pane holds the same Claude session) or "gone" (no other pane does, or
    the session id is unknown, so none could be looked for). A pane absent from it
    is still listed after the wait. `found` is passed in so a raise partway (the
    /terminals read, successor's own read) leaves the panes already settled."""
    found = {} if found is None else found
    left = dict(panes)
    deadline = time.time() + wait_s
    while left and time.time() < deadline:
        time.sleep(2)
        listed = {t["id"] for t in cfs.terminals()}
        for tid, sid in list(left.items()):
            if tid in listed:
                continue
            del left[tid]
            new = successor(sid, tid)
            if new:
                found[tid] = "handed", f"the runner resumed the session in pane {new[:8]} under another account"
            else:
                found[tid] = "gone", ("the pane closed, but no other pane holds its session" if sid else
                                      "the pane closed; its session id is unknown, so no other pane could be checked for it")
    return found


def dismiss_open_menu(tid, where=None):
    """The step after a wait the runner did not answer: if the pane's usage-limit
    menu is still open, dismiss it with Esc (nothing on it is chosen). Returns
    ("listed", message). The same rule as hand_off_limited and ready_to_type: the
    prepare read comes first, so the menu look is the last read before the Esc,
    and under a prepare the pane is left alone. `where`, when given, is a dict
    whose "step" is set to name the Esc just before it is sent, so a caller that
    catches a raise names the call that raised."""
    prep = cfs.prepare_status()[0]
    if limit_menu_open(screen_text(tid, chars=None) or ""):
        if prep != "clear":
            return "listed", "the runner did not move the session; its usage-limit menu was left open because a restart prepare is in force"
        if where is not None:
            where["step"] = "the second Esc on its usage-limit menu"
        write_keys(tid, b"\x1b")
        return "listed", "the runner did not move the session; its usage-limit menu was dismissed with Esc, no option chosen"
    return "listed", "the runner did not move the session"


def await_handoff(tid, sid, wait_s=30, where=None):
    """Wait for the runner to move a session that hit a usage limit. Returns
    (state, message). "handed": another pane holds the same Claude session.
    "gone": the pane closed and no other pane holds its session, or its session
    id is unknown, so none could be looked for. "listed": the pane is still
    listed after the wait; if its limit menu is still open, the menu is
    dismissed with Esc, and nothing on it is chosen (dismiss_open_menu). A pane
    that closes near the end of the wait also waits on successor's own read, so
    the whole wait can reach about twice `wait_s`. `where`: see
    dismiss_open_menu. The wait is wait_for_handoffs's, which hand_off_limited
    runs once for every pane it sent an Esc to."""
    got = wait_for_handoffs({tid: sid}, wait_s)
    return got[tid] if tid in got else dismiss_open_menu(tid, where)


def hand_off_limited(targets, a, own, outcomes=None):
    """Type /next-move into each LIMITED session. The prompt reaches the
    exhausted account, Claude prints its limit message, and the runner moves
    the session to another account; aftercare reads that back, and redirects a
    session with no /next-move, whose redirect is then the prompt that reaches
    the account. A pane whose usage-limit menu is open gets Esc first, which
    can deliver the limit by itself. Every such Esc is sent before any wait, and
    the runner's move is then waited for on all of those panes against one
    shared deadline (wait_for_handoffs). A moved pane reads HANDED, and one that
    closed with its session nowhere reads GONE, both with nothing typed. A
    pane the runner did not move gets the second Esc if its menu is still open
    (dismiss_open_menu), then the re-screen: when that reads the row STOPPED
    after the Esc (its reset passed and the Esc unstuck it), the row is skipped,
    not failed, and nothing is typed (the nudge step works from the pre-act
    screen, where it read LIMITED, so a later pass picks it up); any
    other re-screen refusal after an Esc is a failure. A runner call that
    raises, from the look for the menu to the /next-move POST, ends that row
    and the step goes on to the next: FAILED once anything went out to the pane
    (naming the call that raised), else SKIPPED. Returns the count that did not
    complete. `outcomes`, when given, receives each row's pass-ledger outcome
    by terminal id. After an Esc a row is never skipped (a row never typed
    into): one that ends in this step is done when HANDED, skipped when unstuck, unknown
    when GONE with no session id, and failed otherwise; one sent /next-move is
    settled by aftercare."""
    failures, sent = 0, []
    outcomes = {} if outcomes is None else outcomes
    state_of = {}  # terminal id -> (touched, where): what went out to the pane, and the call in flight
    esc_sent = {}  # terminal id -> session id, for each pane an Esc went out to

    def raised(r, touched, where, e):
        """http.client's too: nothing more is typed, and the step goes on. An Esc or a
        POST can reach the pane even when its reply is torn, so a pane anything went out
        to is FAILED and never looked at or typed into again (as aftercare does); one
        nothing went out to is SKIPPED, as the name step's re-screen reads a raise."""
        nonlocal failures
        tid = r["terminalId"]
        failures += 1
        outcomes[tid] = "failed" if touched else "skipped"
        print(f"  {'FAILED ' if touched else 'SKIPPED'} {tid[:8]}  {r['name'][:50]} - {where['step']} raised "
              f"({type(e).__name__}: {str(e)[:80]}); not typed into{' again' if touched else ''}: read the pane "
              "with --report")
    # Pass 1: every Esc goes out before any wait. A menu left open holds the prompt. The
    # prepare read comes first, so the menu look is the last read before the Esc
    # (ready_to_type's rule); under a prepare the pane is left alone, and ready_to_type
    # refuses the row in pass 3. When a background subagent hit the limit, the menu is all
    # the pane shows, and this Esc alone can let the main session print the limit message
    # the runner moves the session on (plan
    # 2026-11-12-a-limit-hit-by-a-background-subagent-shows-only-the-rate-limit-menu-so-the-pane-is-never-migrated).
    skipped = set()  # rows pass 1 already ended (a raise)
    for r in targets:
        tid, sid = r["terminalId"], r.get("claudeSessionId")
        touched, where = False, {"step": "the look for an open usage-limit menu"}
        state_of[tid] = [touched, where]
        try:
            if cfs.prepare_status()[0] == "clear" and limit_menu_open(screen_text(tid, chars=None) or ""):
                state_of[tid][0], where["step"] = True, "the Esc on its usage-limit menu"
                esc_sent[tid] = sid
                write_keys(tid, b"\x1b")
        except Exception as e:
            raised(r, state_of[tid][0], where, e)
            skipped.add(tid)
    # Pass 2: one wait for the runner's move, for every pane an Esc went out to.
    found, wait_err = {}, None
    live = {t: sid for t, sid in esc_sent.items() if t not in skipped}
    if live:
        try:
            wait_for_handoffs(live, found=found)
        except Exception as e:  # the panes it settled before the raise keep their answer
            wait_err = e
    # Pass 3: each row in its own order.
    for r in targets:
        tid, sid = r["terminalId"], r.get("claudeSessionId")
        if tid in skipped:
            continue
        touched, where = state_of[tid]
        reached = False
        try:
            if tid in esc_sent:
                where["step"] = "the hand-off read after its Esc"
                if tid not in found and wait_err is not None:
                    raise wait_err
                if tid in found:
                    state, why = found[tid]
                else:
                    state, why = dismiss_open_menu(tid, where)
                if state == "handed":
                    outcomes[tid] = "done"
                    print(f"  HANDED  {tid[:8]}  {r['name'][:50]} - {why}")
                    continue
                if state == "gone":  # aftercare's GONE: the pane is not re-screened
                    failures += 1
                    outcomes[tid] = "failed" if sid else "unknown"
                    print(f"  GONE    {tid[:8]}  {r['name'][:50]} - {why}")
                    continue
            where["step"] = "the re-screen"
            ok, msg, _ = ready_to_type(tid, a.idle_min, a.cooldown_hours, own, want="LIMITED")
            reached = ok
            if ok:
                # The POST goes out from here, so a raise from submit (its read of a
                # refused POST's reply included) ends the row FAILED as well.
                touched, where["step"] = True, "the /next-move POST"
                ok, msg = submit(tid, "/next-move")
        except Exception as e:
            raised(r, touched, where, e)
            continue
        if not ok:
            # The Esc unstuck the row: its reset passed, so the re-screen reads STOPPED
            # now, where it read LIMITED at screen time. Nothing is wrong and nothing is
            # typed; the nudge step owns a STOPPED row.
            # Nothing nudges it this pass (the nudge step works from the pre-act screen,
            # where it read LIMITED), so it is skipped, not done: a later pass picks it up.
            if tid in esc_sent and not reached and msg.startswith(RESCREEN_STOPPED):
                outcomes[tid] = "skipped"
                print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - the Esc on its menu let it resume (unstuck); not typed "
                      f"this pass, a later pass picks it up: {msg}")
                continue
            failures += 1
            # reached: the runner refused the POST; touched: an Esc went in before it
            outcomes[tid] = "failed" if reached or touched else "skipped"
            print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - {msg}")
            continue
        outcomes[tid] = "sent_unconfirmed"  # until aftercare reads the handoff back
        print(f"  SENT    {tid[:8]}  {r['name'][:50]} - /next-move")
        sent.append(tid)
    # Each pane's session as the screen read it, before any handoff moved it.
    return failures + aftercare(sent, a, own, wait_s=90, limited=True, outcomes=outcomes,
                                sids={r["terminalId"]: r.get("claudeSessionId") for r in targets})


def answered_unknown(text):
    """True when the pane answered its last /next-move with "Unknown command".
    The reply is searched for after the last /next-move echo, not in a fixed
    window at the screen's end: the input box and status lines Claude Code
    redraws below the reply can run past any such window on a wide pane, and a
    reply left from an earlier /next-move must not read as this one's. Pure.
    Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 5."""
    reply = text.rfind(UNKNOWN_NEXT_MOVE)
    if reply < 0:
        return False
    echo = text.replace(UNKNOWN_NEXT_MOVE, "\0" * len(UNKNOWN_NEXT_MOVE)).rfind("/next-move")
    return echo < reply


def running_now(text):
    """cfs.running_turn, over the text after the last Unknown command reply when
    there is one: that reply ends no turn, so a spinner older than it is not a
    turn running now. Pure."""
    reply = text.rfind(UNKNOWN_NEXT_MOVE)
    return cfs.running_turn(text if reply < 0 else text[reply + len(UNKNOWN_NEXT_MOVE):])


def last_screen_state(text):
    """A short description of a pane's last screen, for a PENDING row. Pure."""
    if text is None:
        return "no screen read"
    lines = [ln.strip() for ln in text.splitlines() if set(ln.strip()) - set("\u2500\u2501 ")]
    # ascii(): the row prints to whatever console runs this, cp1252 included.
    return f"last screen line {ascii(lines[-1][:80])}" if lines else "screen blank"


def aftercare(tids, a, own, wait_s=60, limited=False, outcomes=None, sids=None, watch=None):
    """Watch panes this run just typed into, and print one row for each. A pane
    that closes with its session in a new pane was moved by the runner to
    another account (HANDED); one that closes with its session nowhere is GONE,
    whether a sweep finds it gone or it closes while its limit menu's hand-off
    is awaited; one whose limit menu stays open was not moved, and the menu is
    dismissed with Esc (FAILED). An "Unknown command: /next-move" reply gets the command
    from origin/main (REDIRECT). A redirected pane stays watched, since the
    redirect is a prompt too and can open the limit menu. A pane drops out once
    its turn is visibly running (WORKING; under `limited` the panes were
    LIMITED, so that also means the account has capacity again), it closes, or
    it was handled.

    `sids` maps a pane to the Claude session the caller's screen read for it (a
    screen row's claudeSessionId), so a pane the runner moves after its send is
    still followed to its new pane. One registry read fills any pane `sids`
    lacks. The deadline starts after that read, and a sweep that starts always
    finishes, so every pane is looked at at least once however slow the reads
    are. A pane still pending at the deadline gets a PENDING row. A look that
    raises is retried at the next sweep, unless it had already typed into the
    pane (KEYS_WRITTEN): that pane ends FAILED, so nothing is typed into it twice.

    Returns the count of panes whose handling did not complete, PENDING ones
    included. `outcomes`, when given, receives the pass-ledger outcome of each
    pane; a PENDING pane stays sent_unconfirmed. `watch`, when given, receives
    each pane's watch rows as {"verdict", "message"[, "redirected"]}: the nudge
    step merges them into its helper's own --rows-out row (merge_watch), never
    into the outcome."""
    outcomes = {} if outcomes is None else outcomes
    watch = {} if watch is None else watch
    pending = dict.fromkeys(tids)  # ordered: the rows come out in the order the panes were sent
    given = sids or {}
    sids = {tid: given.get(tid) or "" for tid in pending}
    if not all(sids.values()):
        try:
            rows = cfs.open_rows()
        except Exception:  # http.client's too: such a pane is still watched by its screen
            rows = {}
        sids.update({tid: session_id(rows, tid) for tid, sid in sids.items() if not sid})
    redirected, failures, swept, errors, last = set(), 0, False, {}, {}
    start = time.time()

    def seen(tid, verdict, msg):  # the watch row this pane ended on, for merge_watch
        watch[tid] = dict({"verdict": verdict, "message": msg[:300]}, **({"redirected": True} if tid in redirected
                                                                          else {}))
    deadline = start + wait_s
    while pending and (not swept or time.time() < deadline):
        swept = True
        time.sleep(5)
        try:
            listed = {t["id"]: t for t in cfs.terminals()}
        except Exception:  # http.client's too: this sweep reads the screens only
            listed = None
        for tid in list(pending):
            # A look whose read raises keeps the pane for the next sweep, and its
            # PENDING row names the failure; one pane never ends the others' watch.
            # A look that raised AFTER it typed into the pane ends it FAILED instead:
            # looking again could type into it a second time.
            errors.pop(tid, None)
            typed = KEYS_WRITTEN.get(tid, 0)
            try:
                if listed is not None and tid not in listed:
                    new = successor(sids[tid], tid)
                    failures += not new
                    outcomes[tid] = "done" if new else "failed" if sids[tid] else "unknown"
                    if new:
                        msg = f"the runner resumed the session in pane {new[:8]} under another account"
                    elif sids[tid]:
                        msg = "the pane closed, and no other pane holds its session"
                    else:
                        msg = "the pane closed; its session id is unknown, so no other pane could be checked for it"
                    print(f"  {'HANDED ' if new else 'GONE   '} {tid[:8]}  - {msg}")
                    seen(tid, "HANDED" if new else "GONE", msg)
                    del pending[tid]
                    continue
                full = screen_text(tid, chars=None)
                if full is None:
                    continue
                text = full[-3000:]
                last[tid] = text
                if limit_menu_open(full):
                    state, msg = await_handoff(tid, sids[tid])
                    if state == "gone":  # closed during the wait: the same GONE reading as above
                        failures += 1
                        outcomes[tid] = "failed" if sids[tid] else "unknown"
                        print(f"  GONE    {tid[:8]}  - {msg}")
                        seen(tid, "GONE", msg)
                        del pending[tid]
                        continue
                    handed = state == "handed"
                    failures += not handed
                    outcomes[tid] = "done" if handed else "failed"
                    print(f"  {'HANDED ' if handed else 'FAILED '} {tid[:8]}  - {msg}")
                    seen(tid, "HANDED" if handed else "FAILED", msg)
                    del pending[tid]
                elif tid not in redirected and answered_unknown(text):
                    if listed is None:
                        # The redirect's prompt check replays the pane at its width,
                        # which only its /terminals row gives: the next sweep retries.
                        errors[tid] = "the /terminals read failed, so the redirect's prompt check could not run"
                        continue
                    ok, msg = ready_for_redirect(tid, listed[tid])
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
                        seen(tid, "FAILED", f"no /next-move here, and the redirect was not sent: {msg}")
                        del pending[tid]
                elif running_now(text):
                    outcomes[tid] = "done"  # the prompt reached a session that is now working
                    msg = "the prompt started a turn" + (": this account has capacity again" if limited else "")
                    print(f"  WORKING {tid[:8]}  - {msg}")
                    seen(tid, "WORKING", msg)
                    del pending[tid]
            except Exception as e:  # http.client's too
                err = f"{type(e).__name__}: {str(e)[:80]}"
                if KEYS_WRITTEN.get(tid, 0) > typed:
                    failures += 1
                    outcomes[tid] = "failed"
                    msg = (f"a look raised after it typed into the pane ({err}); not looked at again, so nothing "
                           "is typed twice: read the pane with --report")
                    print(f"  FAILED  {tid[:8]}  - {msg}")
                    seen(tid, "FAILED", msg)
                    del pending[tid]
                else:
                    errors[tid] = err
    waited = round(time.time() - start)
    for tid in pending:
        failures += 1
        outcomes.setdefault(tid, "sent_unconfirmed")
        saw = ("redirected, then no HANDED, FAILED or WORKING" if tid in redirected
               else "no HANDED, REDIRECT, FAILED or WORKING")
        why = f" (its last look failed: {errors[tid]})" if tid in errors else ""
        msg = (f"{saw} reading within {waited} s{why}; {last_screen_state(last.get(tid))}; read the pane again "
               "with --report")
        print(f"  PENDING {tid[:8]}  - {msg}")
        seen(tid, "PENDING", msg)
    return failures


def ready_for_redirect(tid, term):
    """(ok, why) for typing the redirect. The full re-screen cannot be used:
    "Unknown command" ends no turn, so the spinner line above it still reads as
    a running turn. Instead the reply must be the last thing on screen, the
    output still, and the prompt empty. Claude's optional session survey can
    print after the reply; it is dismissed with 0 first. A prepare that started
    since the prompt this follows stops it before any key reaches the pane.
    `term` is the pane's /terminals row: the prompt is replayed at its width."""
    prep, prep_text = cfs.prepare_status()
    if prep != "clear":
        return False, prep_text
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
    prep, prep_text = cfs.prepare_status()  # the survey wait and the sample above took seconds
    if prep != "clear":
        return False, prep_text
    raw = cfs.raw_tail(tid)
    # The same final look ready_to_type refuses on: an open usage-limit menu takes the
    # redirect's Enter as a choice, and prompt_state does not count a dim option label,
    # so a dim-labelled menu would read as an empty prompt just below.
    if raw is not None and limit_menu_open(cfs.ANSI_RE.sub("", raw)):
        return False, "Claude's usage-limit menu is open just before the send, so nothing is typed into it"
    state, typed = (cfs.prompt_state(raw, cols=cfs.wrap_cols(term)) if raw is not None
                    else ("unknown", "screen unreadable"))
    if state != "empty":
        return False, f"input prompt not empty ({state}: {str(typed)[:50]!r})"
    return True, ""


def nudged_panes(rows_path, out, res):
    """(full terminal ids, unresolved id prefixes) of the panes one
    nudge-stopped-sessions.py --send run typed /next-move into, in the order it
    sent them. Its --rows-out file names each send's full id at send time
    (action nudge, outcome done or sent_unconfirmed), so a pane the runner
    moved since still reaches the watch. That file's writer warns and never
    raises, so a NUDGED or SENT? line whose row is missing is matched against
    this pass's screen rows instead; a prefix neither resolves is returned
    unresolved. Reads no runner door."""
    events, _ = read_rows_file(rows_path) if rows_path else ([], 0)
    sent = [ev["terminal_id"] for ev in events if ev.get("action") == "nudge" and isinstance(ev.get("terminal_id"), str)
            and ev.get("action_outcome") in ("done", "sent_unconfirmed")]
    screened = [r["terminalId"] for r in res]
    unresolved = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) < 2 or parts[0] not in ("NUDGED", "SENT?") or any(t.startswith(parts[1]) for t in sent):
            continue
        hits = [t for t in screened if t.startswith(parts[1])]
        if len(hits) == 1:
            sent.append(hits[0])
        else:
            unresolved.append(parts[1])
    return list(dict.fromkeys(sent)), unresolved


def watch_nudged(rows_out, out, res, a, own):
    """aftercare() over the panes one nudge helper run sent /next-move to
    (nudged_panes), each with the Claude session this pass's screen read for
    it. `rows_out` is the --rows-out argument the run was given. A sent pane
    whose full id cannot be resolved cannot be watched, so it gets a PENDING row
    saying so. Returns the count that did not complete."""
    tids, unresolved = nudged_panes(rows_out[1] if len(rows_out) == 2 else None, out, res)
    for prefix in unresolved:
        print(f"  PENDING {prefix[:8]}  - sent, but neither its --rows-out row nor this pass's screen gives its full "
              "terminal id, so it was not watched; read the pane again with --report")
    watch = {}
    failures = aftercare(tids, a, own, sids={r["terminalId"]: r.get("claudeSessionId") for r in res}, watch=watch)
    merge_watch(rows_out, watch)
    return len(unresolved) + failures


def merge_watch(rows_out, watch):
    """Merge each pane's watch row into the nudge helper's own --rows-out row as
    detail.watch, rewriting that file. The row's action_outcome is never
    touched: apply_events lets the last event win, so a PENDING written as an
    outcome would overwrite the helper's read-back `done`. `rows_out` is the
    --rows-out argument pair the helper was given; nothing to merge, no file."""
    if len(rows_out) != 2 or not watch:
        return
    path = rows_out[1]
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
    except OSError:
        return
    out = []
    for line in lines:
        try:
            row = json.loads(line)
        except ValueError:
            out.append(line)
            continue
        if isinstance(row, dict) and row.get("action") == "nudge" and row.get("terminal_id") in watch \
                and isinstance(row.get("detail"), dict):
            row["detail"] = dict(row["detail"], watch=watch[row["terminal_id"]])
            line = json.dumps(row, separators=(",", ":"))
        out.append(line)
    try:
        with open(path, "w", encoding="utf-8") as f:
            f.write("".join(ln + "\n" for ln in out))
    except OSError as e:
        print(f"WARNING: --rows-out {path}: the watch rows were not merged ({e})", file=sys.stderr)


# ---------------------------------------------------------------- names
# Claude Code's permission-mode indicators, as its status line draws them.
# Captures of 2.1.309 show "bypass permissions on", "accept edits on", "plan
# mode on", "manual mode on" and "don't ask on"; "auto mode on" is matched too,
# but no capture shows it. The right boundary keeps prose after an indicator
# ("bypass once", "auto mode only") from reading as the last one; on
# 2026-10-24 it dropped none of the 427 indicators in 104 live runner panes.
# That measurement covered bypass status lines only: every one of those panes
# was in bypass mode.
# The status-line glyph is not required: those panes carried only the bypass
# one, so whether the manual/plan glyph survives a redraw in the raw buffer is
# unmeasured.
MODE_LINE = re.compile(r"(bypass|accept\s*edits|plan\s*mode|auto\s*mode|manual\s*mode|don['\u2019]?t\s*ask)"
                       r"\s*(?:permissions\s*)?on(?![a-z])", re.I)
# An indicator folded to one word (case, spaces, apostrophe dropped) -> its mode.
MODE_WORDS = {"bypass": "bypass", "acceptedits": "accept edits", "planmode": "plan", "automode": "auto",
              "manualmode": "manual", "dontask": "don't ask"}
# The modes the runner never auto-accepts in: plan approval, and an explicit
# allow-list posture (plan
# 2026-10-23-runner-sessions-stop-at-permission-dialogs-so-accept-them-by-default-with-a-tenant-switch, D3).
NO_AUTO_ACCEPT = ("plan", "don't ask")
# permission_mode() when the pane's screen could not be read, which is not the
# same as a tail read with no indicator in it (None).
MODE_UNREAD = "UNKNOWN"
# session_hold() for a pane the runner no longer lists: not a permission hold.
NOT_LISTED = "terminal no longer listed"


def last_mode(text):
    """The mode of the LAST permission-mode indicator in `text` (a MODE_WORDS
    value), or None when there is none. A session switched out of a mode keeps
    the old status text in its scrollback, so the last indicator decides, not
    any one."""
    found = MODE_LINE.findall(text or "")
    return MODE_WORDS.get(re.sub(r"[\s'\u2019]", "", found[-1]).lower()) if found else None


def permission_mode(tid):
    """last_mode() of the pane's tail (None when it holds no indicator), or
    MODE_UNREAD when the screen cannot be read. The status line is redrawn
    differentially, so a short tail can miss it: a 30000-character tail held it
    on 40 of 44 STOPPED panes measured on 2026-10-18."""
    raw = cfs.raw_tail(tid, 30000)
    return MODE_UNREAD if raw is None else last_mode(cfs.ANSI_RE.sub("", raw))


def dial_verdict(dial):
    """(active, why) for a /health `permissionAutoAccept` value: active only
    when it reads answered == true and level == "on". Pure, so the selftest
    can pin it."""
    if not isinstance(dial, dict):
        return False, f"auto-accept dial not answered on (permissionAutoAccept is a {type(dial).__name__})"
    answered, level = dial.get("answered"), dial.get("level")
    if answered is True and level == "on":
        return True, ""
    return False, f"auto-accept dial not answered on (answered={answered!r}, level={level!r})"


def health_dial(body):
    """(active, why) for a GET /health body. Only a success envelope whose
    `data` object lacks `permissionAutoAccept` is a runner build without the
    dial; a missing or non-object `data`, or an error envelope, is UNKNOWN.
    Neither is active. Pure, so the selftest can pin it."""
    from envelope import require_key  # lib/ is on sys.path: close-finished-sessions.py put it there
    if not isinstance(body, dict) or body.get("success") is not True:
        return False, "auto-accept dial UNKNOWN (/health answered no success envelope)"
    try:
        data, _ = require_key(body, "data", "GET /health")
    except cfs.EnvelopeUnknown:
        data = None
    if not isinstance(data, dict):
        return False, f"auto-accept dial UNKNOWN (/health data is {type(data).__name__})"
    if "permissionAutoAccept" not in data:
        return False, ("auto-accept dial not answered on (/health has no permissionAutoAccept: "
                       "a runner build without it)")
    return dial_verdict(data["permissionAutoAccept"])


def auto_accept_dial():
    """(active, why) for the runner's permission auto-accept dial, read from
    GET /health once per run. Any read failure is UNKNOWN and not active."""
    try:
        return health_dial(cfs.read("/health", "."))
    except Exception as e:  # any failure, http.client's included, fails closed before a session is typed into
        return False, f"auto-accept dial UNKNOWN (/health unreadable: {str(e)[:80]})"


def name_hold(mode, hook, dial):
    """Why the name step must skip a session, or "" when /name may be typed.
    `mode` is permission_mode(), `hook` the terminal's `permissionRequestHook`
    (None when the field is missing), `dial` auto_accept_dial()'s (active, why).

    A bypass session is named, as it always was. Any other session is named
    only when the runner will answer a permission dialog /name raises: its
    mode is one the runner accepts in, its carrier registered the hook, and the
    dial reads answered on. Every condition that fails is named. Pure, so the
    selftest can pin every arm."""
    if mode == "bypass":
        return ""
    held = []
    if mode == MODE_UNREAD:
        held.append("permission mode UNKNOWN (the pane's screen could not be read)")
    elif mode is None:
        held.append("no permission-mode indicator in the pane's tail")
    elif mode in NO_AUTO_ACCEPT:
        held.append(f"{mode} mode (the runner never auto-accepts there)")
    if hook != "registered":
        held.append("auto-accept not registered for this session (permissionRequestHook "
                    + ("field missing: a runner build without it)" if hook is None else f"{hook!r})"))
    if not dial[0]:
        held.append(dial[1])
    return "; ".join(held)


def session_hold(tid, dial):
    """name_hold() for one pane: its status line and, outside bypass mode, its
    /terminals row. Raises when that row cannot be read; the caller holds."""
    mode = permission_mode(tid)
    if mode == "bypass":
        return ""
    term = next((t for t in cfs.terminals() if t["id"] == tid), None)
    if term is None:
        return NOT_LISTED
    return name_hold(mode, term.get("permissionRequestHook"), dial)


def custom_title(path):
    """The last /rename title recorded in a transcript, or None. /rename writes
    a `custom-title` record; the runner's resumeName falls back to Claude's own
    ai-title, so it cannot say whether a session was ever named."""
    title = None
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
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


def turn_done(tid, term, row, sent_ms):
    """(done, final_text): the reply to a prompt sent at `sent_ms` is written
    and the pane is back at an empty, idle prompt. `term` and `row` are the
    pane's rows in the name poll's /terminals and registry reads for this
    round; the prompt is replayed at the /terminals row's width."""
    tx = cfs.transcript_facts(row) if row else None
    if not tx or not tx.get("final_text_at") or tx["final_text_at"] < sent_ms or tx.get("pending_tool"):
        return False, ""
    raw = cfs.raw_tail(tid)
    if raw is None or cfs.running_turn(cfs.ANSI_RE.sub("", raw)) or \
            cfs.prompt_state(raw, cols=cfs.wrap_cols(term))[0] != "empty":
        return False, ""
    return True, tx.get("final_text") or ""


def hold_line(tid, dial):
    """Why /name must not be typed into this pane, or "" when it may be. The
    one place the hold is worded, so a report and a run cannot disagree about it.
    Reads only; any failure, http.client's included, holds the session."""
    try:
        hold = session_hold(tid, dial)
    except Exception as e:
        hold = f"UNKNOWN: the pane or its /terminals row could not be read ({str(e)[:80]})"
    if not hold:
        return ""
    return hold if hold == NOT_LISTED else f"/name could stop at a permission dialog: {hold}"


def name_report(targets):
    """The report-mode name step: the hold check a run applies, so a report names
    only the sessions a run would, and says why it would skip the rest."""
    dial = auto_accept_dial() if targets else (False, "")
    for r in targets:
        why = hold_line(r["terminalId"], dial)
        verb, tail = ("would skip", f" - {why}") if why else ("would name", "")
        print(f"  {verb} {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]}{tail}")


def name_sessions(targets, a, own, outcomes=None):
    """Run /name in each target, then type the /rename line it suggests.
    Returns (named terminal ids, failures). `outcomes`, when given, receives
    the pass-ledger outcome of each row the poll settled itself (a pane gone
    after its limit menu), which PASS.named prefers to its own reading; such
    a row counts as a failure, as the limit step and the watch count it."""
    outcomes = {} if outcomes is None else outcomes
    sent = {}  # tid -> (sent_ms, transcript path, row)
    failures = 0
    dial = auto_accept_dial() if targets else (False, "")
    for r in targets:
        tid = r["terminalId"]
        # /name runs git and gh. Outside bypass mode a session can stop at a
        # permission dialog for one of them, and triage would leave that dialog
        # open in a session it does not own, so such a session is named only
        # when the runner answers its dialogs (name_hold). Checked first, so it
        # adds nothing to the gap between the empty-prompt check and the send.
        why = hold_line(tid, dial)
        if why:
            print(f"  SKIPPED {tid[:8]}  {r['name'][:50]} - {why}")
            continue
        try:
            ok, msg, tx = ready_to_type(tid, a.idle_min, a.cooldown_hours, own)
        except Exception as e:  # http.client's too: nothing is sent, and the run goes on
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
    named, ran, deadline, read_err = [], set(), time.time() + a.name_timeout, None
    while sent and time.time() < deadline:
        time.sleep(5)
        # One /terminals read and one registry read per round, shared by every
        # pane: a registry read is slow on a loaded runner, so a read per pane
        # could outlast --name-timeout. A round whose read fails is read
        # again on the next one.
        try:
            terms = {t["id"]: t for t in cfs.terminals()}
            rows = cfs.open_rows()
        except Exception as e:  # http.client's too
            read_err = e
            continue
        read_err = None
        for tid, (at, path, r) in list(sent.items()):
            try:
                result = poll_one(tid, at, path, r, ran, a, own, terms, rows)
            except Exception as e:  # http.client's too: reported SENT?, and the run goes on
                result = ("SENT?  ", r["name"], f"/name was typed; a runner read failed before the title was read back: {e}")
            if result is None:
                continue
            del sent[tid]
            label, shown, msg, *settled = result
            failures += label in ("FAILED ", "SENT?  ") or bool(settled)
            named += [tid] if label == "NAMED  " else []
            if settled:
                outcomes[tid] = settled[0]
            print(f"  {label} {tid[:8]}  {shown[:50]} - {msg}")
    for tid, (_, _, r) in sent.items():
        failures += 1
        why = (f"/name still running after {a.name_timeout}s, or waiting at a dialog" if read_err is None else
               f"/name was typed; the runner could not be read before the title was read back ({str(read_err)[:120]})")
        print(f"  SENT?   {tid[:8]}  {r['name'][:50]} - {why}; not renamed")
    return named, failures


def poll_one(tid, at, path, r, ran, a, own, terms, rows):
    """One look at a session /name was typed into. `terms` ({id: /terminals
    row}) and `rows` (the registry) are the round's reads, shared by every pane
    it polls. None while it is still working; otherwise (label, name shown,
    message[, outcome]), after typing the /rename when the reply suggested
    one; the fourth item is a pass-ledger outcome the poll settled itself. A
    usage-limit menu means the runner should move the session to another
    account; that is waited for, never chosen: HANDED when it moves, GONE when
    the pane closes with its session nowhere (failed, or unknown with no
    session id, as the limit step and the watch record it), FAILED when it
    does not move."""
    if tid not in terms:
        return "GONE   ", r["name"], "the pane closed while /name ran"
    if limit_menu_open(screen_text(tid, chars=None) or ""):
        # The session the screen read for the pane, as the watch after a send uses.
        sid = r.get("claudeSessionId") or session_id(rows, tid)
        state, msg = await_handoff(tid, sid)
        if state == "gone":  # read as the limit step and the watch read it, and its outcome settled here
            return "GONE   ", r["name"], msg, "failed" if sid else "unknown"
        return ("HANDED " if state == "handed" else "FAILED "), r["name"], msg
    if tid not in ran:
        row = rows.get(tid)
        tx = cfs.transcript_facts(row) if row else None
        p_at, text = (tx or {}).get("last_prompt"), (tx or {}).get("last_prompt_text") or ""
        if p_at and p_at >= at - 1000 and NAME_RAN.search(text):
            ran.add(tid)
        elif time.time() * 1000 - at > 60000:
            return "SENT?  ", r["name"], "/name did not run within 60s"
        return None
    done, reply = turn_done(tid, terms[tid], rows.get(tid), at)
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
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    for line in (p.stdout + p.stderr).splitlines():
        print("  " + line)
    return p.returncode, p.stdout


def common(a):
    return (["--idle-min", str(a.idle_min)] + (["--only", a.only] if a.only else [])
            + (["--port", a.port] if a.port else []))


CLOSE_REFUSED_UNREADABLE = ("close-finished-sessions.py --close will refuse: GET /restart-readiness is unreadable, "
                            "so nothing closes")


def would_close(res, ready_err=None):
    """The rows close-finished-sessions.py --close would close at screen time:
    each row's close-screen verdict (nss.screen keeps it as r["screen"]) in that
    script's own CLOSABLE set, so the report predicts what the closer does.
    With readiness unreadable (`ready_err` set) the closer refuses --close
    outright, EXITED-FINISHED rows included, so none would close."""
    return [] if ready_err else [r for r in res if r["screen"] in cfs.CLOSABLE]


def list_custody(res):
    """Print, for the operator, each row the close screen read CUSTODY: finished,
    but its directory is not a linked worktree (a primary checkout, the workspace
    root, no repository) or could not be read (served finished-criteria condition
    3). No step closes or nudges such a row, so the close step names every one,
    as the nudge step names STUCK, in report and act mode alike."""
    for r in res:
        if r["screen"] == "CUSTODY":
            why = next((x for x in r.get("reasons") or [] if x.startswith("custody")), "; ".join(r.get("reasons") or []))
            print(f"  CUSTODY      {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]} - {why[:160]}; "
                  "for the operator")


def operator_rows(res):
    """(label, row) for each row the nudge step lists before it sends: STUCK and
    a HELD row carrying a finish_stranded code (a finished session a screen
    rule holds, cfs.stranded_code), which are never nudged and are for the
    operator, and CONTRADICTED, which is nudged so /next-move reconciles it.
    Pure."""
    out = []
    for r in res:
        if r["verdict"] == "STUCK":
            out.append(("STUCK", r))
        elif r["verdict"] == "HELD" and r.get("reason_code"):
            out.append(("HELD", r))
        elif r["verdict"] == "STOPPED" and r.get("screen") == "CONTRADICTED":
            out.append(("CONTRADICTED", r))
    return out


def layout_argv(a, pages, res, skip, ready_err=None):
    """The layout step's argv for runner-layout.py. In report mode it previews
    the trim after this run's own predicted closes: the close step's would-close
    terminals (would_close) go in as --assume-closed. An act pass passes none
    (its close step has already run, so /terminals lists only what is left),
    and neither does a pass whose close step is skipped, which closes nothing.
    Nor does a pass whose readiness read failed (`ready_err`): the closer would
    refuse. `pages` is the page ids under --only (empty: every page). Plan
    2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 7."""
    where = ["--page", ",".join(pages)] if pages else ["--all-pages"]
    closing = [] if a.act or "close" in skip else [r["terminalId"] for r in would_close(res, ready_err)]
    return (["fit", *where, "--trim-only"] + ([] if a.act else ["--dry-run"])
            + (["--assume-closed", ",".join(closing)] if closing else []) + (["--port", a.port] if a.port else []))


def close_argv(a, rows_out=()):
    """The close step's argv for close-finished-sessions.py. The selftest parses
    it with that script's own parser (cfs.build_parser), so a flag the helper
    does not take fails CI here instead of the first acting pass."""
    return ["--close"] + common(a) + list(rows_out)


# Everything this script calls on its two imported siblings. The selftest
# checks they still exist, so a rename there fails CI here rather than at the
# first --act run.
USES_NSS = ("judge", "row_facts", "screen", "external_signals", "STOPPED_FROM")
USES_CFS = ("API", "ANSI_RE", "CLOSABLE", "print_stranded", "EnvelopeUnknown", "READ_ERRORS", "build_parser", "classify",
            "git_rev_parse_dirs", "live_session_ids", "load", "open_rows", "output_moving", "page_names",
            "prepare_status", "proc_facts", "proc_table", "prompt_state", "raw_tail", "read", "readiness",
            "running_turn", "finish_since", "terminals", "transcript_facts", "wrap_cols")


def selftest_prepare_hold():
    """main() --act under each prepare verdict, every door stubbed and nothing
    able to type; then ready_to_type under a prepare that started mid-run.
    Returns the number of failures."""
    import contextlib
    import io
    g = globals()
    failures = 0
    # nss.screen keeps every row's close-screen verdict as "screen"; the close step reads it.
    limited = {"terminalId": "l1", "verdict": "LIMITED", "screen": "LIMITED", "tab": "x", "name": "n", "reasons": []}
    stopped = {"terminalId": "s1", "verdict": "STOPPED", "screen": "ACTIVE", "tab": "x", "name": "n", "reasons": []}
    class Recorder:
        def __getattr__(self, _name):
            return lambda *a, **k: []
    keys_g = ("PASS", "run_helper", "hand_off_limited", "name_sessions", "aftercare", "custom_title", "screen_text",
              "write_keys", "ready_to_type", "limit_menu_open")
    keys_cfs = ("prepare_status", "terminals", "open_rows", "transcript_facts", "readiness", "classify", "proc_table",
                "output_moving", "raw_tail")
    keys_nss = ("screen", "external_signals", "judge", "row_facts")
    saved_g = {k: g[k] for k in keys_g}
    saved_cfs = {k: getattr(cfs, k) for k in keys_cfs}
    saved_nss = {k: getattr(nss, k) for k in keys_nss}
    saved_argv = sys.argv
    acted = []
    try:
        g["PASS"] = Recorder()
        g["run_helper"] = lambda script, args: (acted.append(script.split("-")[0]) or (0, ""))
        g["hand_off_limited"] = lambda *a, **k: (acted.append("limit") or 0)
        g["name_sessions"] = lambda *a, **k: (acted.append("name") or ([], 0))
        g["aftercare"] = lambda *a, **k: 0
        g["custom_title"] = lambda path: None
        cfs.terminals = lambda: []
        cfs.open_rows = lambda: {"s1": {}}
        cfs.transcript_facts = lambda row: {"path": "p"}
        nss.screen = lambda *a, **k: ([limited, stopped], [], None)
        for prep, want_rc, want in ((("held", "held text"), 0, ["close"]),
                                    (("unknown", "unknown text"), 3, []),
                                    (("clear", ""), 0, ["close", "limit", "name", "nudge"])):
            acted.clear()
            cfs.prepare_status = lambda prep=prep: prep
            sys.argv = ["triage-sessions.py", "--act", "--skip", "layout"]
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                rc = main()
            if rc != want_rc or acted != want:
                print(f"FAIL main --act under prepare {prep[0]}: rc {rc}, acted {acted}; want rc {want_rc}, {want}")
                failures += 1
        # ready_to_type: a prepare that started after the pass's read stops it
        # before the final prompt look, whichever verdict the row is wanted in.
        cfs.terminals = lambda: [{"id": "t1"}]
        cfs.readiness = lambda: ({}, None)
        cfs.open_rows = lambda: {"t1": {}}
        cfs.classify = lambda *a: ("ACTIVE", [])
        cfs.proc_table = lambda: {}
        cfs.transcript_facts = lambda row: None
        nss.external_signals = lambda *a: None
        nss.row_facts = lambda *a: {}
        cfs.prepare_status = lambda: ("held", "held text")
        cfs.output_moving = lambda tid: False
        def tail(tid):
            raise AssertionError("the final prompt look reached under a held prepare")
        cfs.raw_tail = tail
        for want in ("STOPPED", "LIMITED"):
            nss.judge = lambda *a, want=want: (want, [])
            ok, msg, _ = ready_to_type("t1", 0, 48, "", want=want)
            if ok or "held text" not in msg:
                print(f"FAIL ready_to_type({want}) under a mid-run prepare: {ok} {msg!r}")
                failures += 1
        # No key reaches a pane under a prepare: not the redirect's survey key,
        # and not the Esc the limit step sends to an open menu.
        keys = []
        g["write_keys"] = lambda tid, data: keys.append(data)
        g["screen_text"] = lambda tid, chars=3000: UNKNOWN_NEXT_MOVE + " " + SURVEY
        ok, msg = ready_for_redirect("t1", {"id": "t1"})
        if ok or "held text" not in msg or keys:
            print(f"FAIL ready_for_redirect under a prepare: {ok} {msg!r}, keys {keys}")
            failures += 1
        # A prepare that starts between the redirect's two reads stops it too.
        reads = iter([("clear", ""), ("held", "held text")])
        cfs.prepare_status = lambda: next(reads)
        g["screen_text"] = lambda tid, chars=3000: UNKNOWN_NEXT_MOVE
        ok, msg = ready_for_redirect("t1", {"id": "t1"})
        if ok or "held text" not in msg:
            print(f"FAIL ready_for_redirect under a prepare started mid-check: {ok} {msg!r}")
            failures += 1
        cfs.prepare_status = lambda: ("held", "held text")
        g["screen_text"] = lambda tid, chars=3000: UNKNOWN_NEXT_MOVE + " " + SURVEY
        g["limit_menu_open"] = lambda text: True
        g["ready_to_type"] = lambda *a, **k: (False, "held text", None)
        with contextlib.redirect_stdout(io.StringIO()):
            saved_g["hand_off_limited"]([limited], argparse.Namespace(idle_min=0, cooldown_hours=48), "")
        if keys:
            print(f"FAIL hand_off_limited sent keys under a prepare: {keys}")
            failures += 1
    finally:
        sys.argv = saved_argv
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
        for k, v in saved_nss.items():
            setattr(nss, k, v)
    return failures


def selftest_name_outcomes():
    """main() --act with only the name step doing work: the outcomes name_sessions
    settles itself reach PASS.named, which prefers them to its own reading of what
    submit() recorded (a row it settled "failed" would otherwise read as done or
    sent_unconfirmed). Every door is stubbed and nothing can type. Returns failures."""
    import contextlib
    import io
    g = globals()
    stopped = {"terminalId": "n1", "verdict": "STOPPED", "screen": "ACTIVE", "tab": "x", "name": "n", "reasons": []}
    got = []

    class Recorder:
        def named(self, targets, named, typed, outcomes=None):
            got.append((list(targets), dict(outcomes) if outcomes is not None else None))

        def __getattr__(self, _name):
            return lambda *a, **k: []
    keys_g = ("PASS", "run_helper", "name_sessions", "custom_title")
    saved_g = {k: g[k] for k in keys_g}
    saved_cfs = {k: getattr(cfs, k) for k in ("prepare_status", "terminals", "open_rows", "transcript_facts")}
    saved_screen, saved_argv = nss.screen, sys.argv

    def fake_name_sessions(targets, a, own, outcomes=None):
        outcomes["n1"] = "failed"
        return [], 1
    try:
        g["PASS"], g["run_helper"] = Recorder(), lambda script, args: (0, "")
        g["name_sessions"], g["custom_title"] = fake_name_sessions, lambda path: None
        cfs.prepare_status, cfs.terminals = lambda: ("clear", ""), lambda: []
        cfs.open_rows, cfs.transcript_facts = lambda: {"n1": {}}, lambda row: {"path": "p"}
        nss.screen = lambda *a, **k: ([dict(stopped)], [], None)
        sys.argv = ["triage-sessions.py", "--act", "--skip", "close,limit,layout"]
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            main()
    finally:
        sys.argv = saved_argv
        nss.screen = saved_screen
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
    if not got or got[0][1] != {"n1": "failed"}:
        print(f"FAIL the name step's settled outcomes did not reach PASS.named: {got}")
        return 1
    return 0


def selftest_redirect_menu():
    """ready_for_redirect's final look refuses an open usage-limit menu whose option
    label is dim (prompt_state reads that screen as an empty prompt), as ready_to_type
    does, and still passes a plain idle prompt. Every runner read is stubbed. Returns failures."""
    g = globals()
    failures = 0
    saved_g = {k: g[k] for k in ("screen_text", "write_keys")}
    saved_cfs = {k: getattr(cfs, k) for k in ("prepare_status", "output_moving", "raw_tail", "wrap_cols")}
    try:
        g["screen_text"], g["write_keys"] = lambda tid, chars=3000: UNKNOWN_NEXT_MOVE, lambda tid, data: None
        cfs.prepare_status, cfs.output_moving = lambda: ("clear", ""), lambda tid: False
        cfs.wrap_cols = lambda term: None
        idle = "\x1b[10;1H\u276f \x1b[10;3H\x1b[?25h"
        for case, raw, want_ok in (("a dim-labelled menu", cfs.LIMIT_MENU_RAW_DIM, False), ("the plain menu", cfs.LIMIT_MENU_RAW, False),
                                   ("an idle prompt", idle, True)):
            cfs.raw_tail = lambda tid, raw=raw: raw
            ok, msg = ready_for_redirect("t1", {"id": "t1"})
            if ok != want_ok or (not ok and "usage-limit menu is open" not in msg):
                print(f"FAIL ready_for_redirect on {case}: {ok} {msg!r}, want ok={want_ok}")
                failures += 1
    finally:
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
    return failures


def typing_steps_held(prep, skip):
    """The steps a restart prepare holds, in run order. Pure. These three type
    into a pane: /next-move would end a paused session's pause, and on a LIMITED
    row it would hand the session to another account, which the prepare promises
    not to do. Closing finished tabs and trimming grids type nothing."""
    return [s for s in ("limit", "name", "nudge") if s not in skip] if prep == "held" else []


def selftest():
    failures = 0
    for prep, skip, want in (("held", set(), ["limit", "name", "nudge"]), ("held", {"name"}, ["limit", "nudge"]),
                             ("clear", set(), []), ("unknown", set(), [])):
        if typing_steps_held(prep, skip) != want:
            print(f"FAIL typing_steps_held({prep!r}, {skip!r}) = {typing_steps_held(prep, skip)}, want {want}")
            failures += 1
    for mod, names in ((nss, USES_NSS), (cfs, USES_CFS)):
        for missing in [n for n in names if not hasattr(mod, n)]:
            print(f"FAIL {mod.__name__}.{missing} no longer exists")
            failures += 1
    modes = [("x bypass permissions on y", "bypass"), ("bypass permissions on ... accept edits on", "accept edits"),
             ("accept edits on ... bypasspermissionson", "bypass"), ("no indicator", None),
             ("plan mode on", "plan"), ("\u23f5\u23f5 don't ask on (shift+tab to cycle)", "don't ask"),
             ("don\u2019t ask on", "don't ask"), ("  manual mode on \u00b7 ? for shortcuts", "manual"),
             ("auto mode on", "auto"), ("\u276f 1. Yes, and use auto mode\n   2. No", None),
             ("bypass permissions on ... plan mode on (shift+tab to cycle)", "plan"),
             # Prose printed after an indicator never reads as one.
             ("\u23f8 plan mode on (shift+tab to cycle)\nSwitch to auto mode only if needed", "plan"),
             ("\u23f8 plan mode on\nrun it in bypass once, then stop", "plan"),
             ("\u23f8 manual mode on\nthe plan mode one-pager is onto main", "manual"),
             ("\u23f5\u23f5 don't ask on\naccept edits onward", "don't ask")]
    for text, want in modes:
        got = last_mode(text)
        if got != want:
            print(f"FAIL last_mode({text!r}) = {got!r}, want {want!r}")
            failures += 1
    # name_hold: (mode, permissionRequestHook, dial, what the skip names; () = named).
    on, off = dial_verdict({"answered": True, "level": "on"}), dial_verdict({"answered": True, "level": "off"})
    holds = [
        ("bypass", None, (False, "dial UNKNOWN"), ()),  # bypass is named exactly as before
        ("manual", "registered", on, ()), ("accept edits", "registered", on, ()), ("auto", "registered", on, ()),
        ("manual", "absent", on, ("not registered",)),
        ("manual", None, on, ("not registered", "field missing")),  # a runner build without the field
        ("manual", "Registered", on, ("not registered",)),
        ("manual", "registered", off, ("dial not answered on", "'off'")),
        ("manual", "registered", dial_verdict({"answered": False, "level": "on"}), ("dial not answered on",)),
        ("manual", "registered", dial_verdict({"answered": "true", "level": "on"}), ("dial not answered on",)),
        ("manual", "registered", dial_verdict({"level": "on"}), ("dial not answered on",)),
        ("manual", "registered", dial_verdict({"answered": True}), ("dial not answered on",)),
        ("manual", "registered", dial_verdict({"answered": True, "level": "ON"}), ("dial not answered on",)),
        ("manual", "registered", dial_verdict(None), ("dial not answered on",)),
        ("plan", "registered", on, ("plan mode",)),
        ("don't ask", "registered", on, ("don't ask mode",)),
        (None, "registered", on, ("no permission-mode indicator",)),
        (MODE_UNREAD, "registered", on, ("permission mode UNKNOWN",)),
        (MODE_UNREAD, None, (False, "auto-accept dial UNKNOWN (x)"),
         ("permission mode UNKNOWN", "field missing", "dial UNKNOWN")),
    ]
    for mode, hook, dial, want in holds:
        got = name_hold(mode, hook, dial)
        if bool(got) != bool(want) or any(w not in got for w in want):
            print(f"FAIL name_hold({mode!r}, {hook!r}, {dial!r}) = {got!r}, want {want or 'named'}")
            failures += 1
    # The reads behind it, against stubbed runner doors: /health is read at
    # data.permissionAutoAccept, and the terminal row only outside bypass mode.
    import http.client
    real = cfs.read, cfs.raw_tail, cfs.terminals
    paths = []

    def health(body):
        def read(path, key, timeout=60):
            paths.append(path)
            return cfs.load(body, door="GET " + path, source="selftest").require_key(key)[0]
        return read

    def unreachable(*_a, **_k):
        raise urllib.error.URLError("selftest: refused")

    def torn(*_a, **_k):  # not one of cfs.READ_ERRORS
        raise http.client.IncompleteRead(b"{")
    screens = {"t-bypass": "bypass permissions on", "t-manual": "manual\x1b[1m mode on", "t-plan": "plan mode on",
               "t-gone": "manual mode on", "t-blank": "no status line here", "t-old": "manual mode on",
               "t-dark": None}  # raw_tail's answer for a screen it could not read
    rows = [{"id": "t-manual", "permissionRequestHook": "registered"},
            {"id": "t-plan", "permissionRequestHook": "registered"},
            {"id": "t-blank", "permissionRequestHook": "registered"},
            {"id": "t-dark", "permissionRequestHook": "registered"},
            {"id": "t-old"}]  # spawned by a runner build without the field
    reads = [
        (health(b'{"success":true,"data":{"permissionAutoAccept":{"answered":true,"level":"on"}}}'), True, ""),
        (health(b'{"success":true,"data":{"permissionAutoAccept":{"answered":true,"level":"off"}}}'), False,
         "not answered on"),
        (health(b'{"success":true,"data":{"buildId":"x"}}'), False, "a runner build without it"),
        (health(b'{"success":true,"permissionAutoAccept":{"answered":true,"level":"on"},"data":{}}'), False,
         "a runner build without it"),
        (health(b'{"success":true}'), False, "UNKNOWN"),
        (health(b'{"success":true,"data":null}'), False, "UNKNOWN"),
        (health(b'{"success":false,"error":"boom","data":{}}'), False, "UNKNOWN"),
        (health(b"<html>"), False, "UNKNOWN"),
        (unreachable, False, "UNKNOWN"),
        (torn, False, "UNKNOWN"),
    ]
    sessions = [("t-bypass", on, ""), ("t-manual", on, ""), ("t-manual", off, "dial not answered on"),
                ("t-plan", on, "plan mode"), ("t-gone", on, NOT_LISTED),
                ("t-blank", on, "no permission-mode indicator"), ("t-dark", on, "permission mode UNKNOWN"),
                ("t-old", on, "permissionRequestHook field missing")]
    try:
        for stub, want_on, want_why in reads:
            cfs.read = stub
            try:
                got_on, why = auto_accept_dial()
            except Exception as e:
                got_on, why = None, f"raised {e!r}"
            if got_on != want_on or want_why not in why:
                print(f"FAIL auto_accept_dial() = {(got_on, why)!r}, want {want_on} naming {want_why!r}")
                failures += 1
        cfs.raw_tail = lambda tid, chars=30000: screens[tid]
        cfs.terminals = unreachable  # a bypass session never reads its row
        try:
            bypass_named = session_hold("t-bypass", (False, "x")) == ""
        except cfs.READ_ERRORS:
            bypass_named = False
        if not bypass_named:
            print("FAIL session_hold: a bypass session must be named without reading /terminals")
            failures += 1
        cfs.terminals = lambda: rows
        for tid, dial, want in sessions:
            got = session_hold(tid, dial)
            if bool(got) != bool(want) or want not in got:
                print(f"FAIL session_hold({tid!r}) = {got!r}, want {want or 'named'!r}")
                failures += 1
    finally:
        cfs.read, cfs.raw_tail, cfs.terminals = real
    if any(p != "/health" for p in paths):
        print(f"FAIL auto_accept_dial read {sorted(set(paths))}, want only /health")
        failures += 1
    failures += selftest_prepare_hold()
    failures += selftest_name_outcomes()
    failures += selftest_redirect_menu()
    step_failures, steps = selftest_name_sessions()
    failures += step_failures
    close_failures, close_rows, close_argvs, listings = selftest_close_step()
    failures += close_failures
    watch_failures, watch_cells = watch_selftest()
    failures += watch_failures
    reply_failures, reply_cells = reply_selftest()
    failures += reply_failures
    width_failures, width_cells = width_selftest()
    failures += width_failures
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
    # Plan 2026-11-12-a-limit-hit-by-a-background-subagent-shows-only-the-rate-limit-menu-so-the-pane-is-never-migrated,
    # Phase 2: the measured screen, whose menu draws its border after the key hint, reads open; a hint followed
    # by any other text is a menu left in scrollback.
    for text, want in ((menu, True), ("What do you want to do?\n 1. Yes\n 2. No", False),
                       (cfs.LIMIT_MENU_SCREEN, True), (cfs.LIMIT_MENU_SCREEN + "\r\u276f ", False),
                       (menu + "\n\u25cf Resumed.", False)):
        if limit_menu_open(text) != want:
            print(f"FAIL limit menu({text[-30:]!r}) != {want}")
            failures += 1
    # One reading of the menu: triage's helpers are the screen verdict's own (D2).
    if not all(mine is theirs for mine, theirs in ((LIMIT_MENU, cfs.LIMIT_MENU), (MENU_OPEN, cfs.MENU_OPEN),
                                                    (limit_menu_open, cfs.limit_menu_open))):
        print("FAIL the limit-menu helpers are not close-finished-sessions.py's own")
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
    print(f"selftest: {len(cases)} name cases, {len(modes)} mode cases, {len(holds)} hold cases, "
          f"{len(reads)} dial reads, {len(sessions) + 1} session holds, {steps} name-step runs, "
          f"{len(tabs)} tab cases, {close_rows} close-prediction rows, {close_argvs} close-argv parses, "
          f"{listings} listing checks (custody x2, held section, nudge x2), {watch_cells} watch cells, {reply_cells} reply/merge cells, {width_cells} prompt-width cells, "
          f"{failures} failure(s)")
    return 1 if failures else 0


def selftest_name_sessions():
    """Drive name_sessions() itself against stubbed runner doors. The
    ready_to_type stub records each row that reaches it and refuses it, and
    submit raises, so no run can type into a pane; cfs.API points at a closed
    port as a backstop. Returns (failures, runs)."""
    import contextlib
    import http.client
    import io
    failures = 0
    g = globals()
    saved = cfs.API, cfs.read, cfs.raw_tail, cfs.terminals, g["ready_to_type"], g["submit"]
    screens = {"t-bypass": "\u23f5\u23f5 bypass permissions on (shift+tab to cycle)",
               "t-manual": "\u23f8 manual\x1b[1m mode on",  # the ANSI strip must run before the match
               "t-old": "\u23f8 manual mode on", "t-plan": "\u23f8 plan mode on", "t-gone": "\u23f8 manual mode on",
               "t-blank": "no status line here", "t-dark": None}  # None: raw_tail could not read the screen
    rows = [{"id": "t-manual", "permissionRequestHook": "registered"}, {"id": "t-old"},
            {"id": "t-plan", "permissionRequestHook": "registered"},
            {"id": "t-blank", "permissionRequestHook": "registered"},
            {"id": "t-dark", "permissionRequestHook": "registered"}]  # no t-bypass row: bypass never reads one
    on = b'{"success":true,"data":{"permissionAutoAccept":{"answered":true,"level":"on"}}}'
    off = b'{"success":true,"data":{"permissionAutoAccept":{"answered":true,"level":"off"}}}'

    def torn(*_a, **_k):  # not one of cfs.READ_ERRORS
        raise http.client.IncompleteRead(b"{")
    # (case, /health body or None to raise, /terminals, targets, rows that reach ready_to_type,
    #  {tid: what its SKIPPED line names}, /health reads, whether the re-screen raises)
    runs = [
        ("dial on", on, lambda: rows, ["t-bypass", "t-manual", "t-old", "t-plan", "t-blank", "t-dark", "t-gone"],
         {"t-bypass", "t-manual"},
         {"t-old": "permissionRequestHook field missing", "t-plan": "plan mode",
          "t-blank": "no permission-mode indicator", "t-dark": "permission mode UNKNOWN",
          "t-gone": "- " + NOT_LISTED}, 1, False),  # not listed: no permission-dialog prefix
        ("dial off", off, lambda: rows, ["t-bypass", "t-manual"], {"t-bypass"},
         {"t-manual": "dial not answered on"}, 1, False),
        ("/terminals fails", on, torn, ["t-bypass", "t-manual"], {"t-bypass"}, {"t-manual": "UNKNOWN"}, 1, False),
        ("/health fails", None, lambda: rows, ["t-manual", "t-old"], set(),
         {"t-manual": "dial UNKNOWN", "t-old": "permissionRequestHook field missing"}, 1, False),
        ("re-screen fails", on, lambda: rows, ["t-bypass"], {"t-bypass"},
         {"t-bypass": "UNKNOWN at re-screen, not sent"}, 1, True),
        ("no targets", on, lambda: rows, [], set(), {}, 0, False),
    ]
    try:
        cfs.API = "http://127.0.0.1:9"
        cfs.raw_tail = lambda tid, chars=30000: screens[tid]
        for case, body, terms, tids, want_reached, want_skips, want_reads, ready_raises in runs:
            reached, paths = [], []

            def read(path, key, timeout=60, body=body):
                paths.append(path)
                if body is None:
                    raise http.client.IncompleteRead(b"{")
                return cfs.load(body, door="GET " + path, source="selftest").require_key(key)[0]

            def ready(tid, *_a, reached=reached, raises=ready_raises, **_k):
                reached.append(tid)
                if raises:  # not one of cfs.READ_ERRORS
                    raise http.client.IncompleteRead(b"{")
                return False, "selftest stub", None

            def typed(tid, message):
                raise AssertionError(f"name_sessions tried to type {message!r} into {tid}")
            cfs.read, cfs.terminals, g["ready_to_type"], g["submit"] = read, terms, ready, typed
            out = io.StringIO()
            a = argparse.Namespace(idle_min=15, cooldown_hours=None, name_timeout=0)
            try:
                with contextlib.redirect_stdout(out):
                    name_sessions([{"terminalId": t, "name": t} for t in tids], a, "")
            except Exception as e:
                print(f"FAIL name_sessions [{case}] raised {e!r}")
                failures += 1
                continue
            lines = out.getvalue().splitlines()
            # The report path applies the same hold: it would name exactly the
            # rows a run lets through to ready_to_type, and says why for the rest.
            run_paths, paths[:] = list(paths), []
            rep = io.StringIO()
            try:
                with contextlib.redirect_stdout(rep):
                    name_report([{"terminalId": t, "name": t, "tab": "tab"} for t in tids])
            except Exception as e:
                print(f"FAIL name_report [{case}] raised {e!r}")
                failures += 1
                continue
            rlines = rep.getvalue().splitlines()
            would = {t for t in tids if any(f"would name {t[:8]}" in ln for ln in rlines)}
            if would != (want_reached if not ready_raises else set(tids)):
                print(f"FAIL name_report [{case}] would name {sorted(would)}, want {sorted(want_reached)}: {rlines}")
                failures += 1
            if not ready_raises:  # a re-screen failure is not a hold, so the report cannot name it
                for tid, why in want_skips.items():
                    if not any(f"would skip {tid[:8]}" in ln and why in ln for ln in rlines):
                        print(f"FAIL name_report [{case}] printed no 'would skip' {tid} naming {why!r}: {rlines}")
                        failures += 1
            if len(paths) != want_reads or any(p != "/health" for p in paths):
                print(f"FAIL name_report [{case}] read {paths}, want /health {want_reads} time(s)")
                failures += 1
            if set(reached) != want_reached:
                print(f"FAIL name_sessions [{case}] reached ready_to_type for {sorted(reached)}, "
                      f"want {sorted(want_reached)}")
                failures += 1
            for tid, why in want_skips.items():
                if not any(f"SKIPPED {tid[:8]}" in ln and why in ln for ln in lines):
                    print(f"FAIL name_sessions [{case}] printed no SKIPPED {tid} naming {why!r}: {lines}")
                    failures += 1
            if len(run_paths) != want_reads or any(p != "/health" for p in run_paths):
                print(f"FAIL name_sessions [{case}] read {run_paths}, want /health {want_reads} time(s)")
                failures += 1
    finally:
        cfs.API, cfs.read, cfs.raw_tail, cfs.terminals, g["ready_to_type"], g["submit"] = saved
    return failures, len(runs)


def selftest_close_step():
    """The close step against close-finished-sessions.py itself (plan Phase 2,
    N2 and D2). Its report names exactly the rows that script's --close would
    close - each row's close-screen verdict in cfs.CLOSABLE - and none of them is
    a row coord does not read finished (a closeout phrase, a nothing-owed footer,
    a runner mark beside waiting_human, working or no status). Its acting header
    names the screen-time count, and the argv it runs the helper with parses with
    that script's own parser, for every option this script takes. The verdicts
    come from cfs.classify over stubbed reads (no runner, no process table), and
    main() runs with every door stubbed and nothing able to close or type.
    The close step names each CUSTODY row for the operator in both modes, and
    the nudge step's listing names a contradicted finish as nudged, and STUCK
    for the operator. Returns (failures, rows, argv parses, listing checks)."""
    import contextlib
    import io
    failures = 0
    g = globals()
    now = time.time() * 1000
    m = 60000
    idle = {"final_text": "Fixed the bug and pushed it.", "final_text_at": now - 60 * m,
            "last_prompt": now - 62 * m, "last_prompt_text": "fix the bug", "last_asst": now - 60 * m,
            "last_activity": now - 60 * m, "pending_tool": False, "last_turn_open": now - 62 * m,
            "last_turn_open_text": "fix the bug", "task_prompts": 1, "cwd": cfs.LINKED_CWD}
    shut = dict(idle, final_text="PR #3 landed.\n\nSAFE TO CLOSE")
    footer = dict(idle, final_text="Done.\n\n**POLICY_COMPLIANCE**\n- **Final state:** this session owes nothing.")
    mark = {"finished": True, "finishedAt": int(now - 61 * m)}
    by_time = {"sessionStatus": "finished", "sessionStatusSince": int(now - 61 * m)}
    table = [  # (terminal id, claude alive, registry row, readiness row, transcript, closable under the policy)
        ("c0000001", True, {}, by_time, idle, True),
        ("c0000002", True, mark, {"sessionStatus": "finished"}, idle, True),
        ("c0000003", False, mark, {}, idle, True),  # claude has exited: EXITED-FINISHED
        ("c0000004", True, {}, {"sessionStatus": "working"}, shut, False),
        ("c0000005", True, {}, {"sessionStatus": "waiting_human"}, shut, False),
        ("c0000006", True, {}, {}, footer, False),
        ("c0000007", True, mark, {"sessionStatus": "waiting_human"}, idle, False),
        ("c0000008", True, mark, {"sessionStatus": "working"}, idle, False),
        ("c0000009", True, mark, {}, idle, False),
        ("c0000010", True, {}, by_time, dict(idle, cwd="/w/repo"), False),  # a primary checkout: CUSTODY
        # a turn inside coord's 1 s stamp band: close keeps it, nudge holds it (finish_stranded.in_band)
        ("c0000011", True, {}, {"sessionStatus": "finished", "sessionStatusSince": int(now - 62 * m) + 500}, idle,
         False),
    ]
    txs = {f"s-{tid}": tx for tid, _, _, _, tx, _ in table}
    stubs = {"proc_facts": lambda tbl, pid, windows=None: ([11], [], []),
             "live_session_ids": lambda claudes: set(txs), "raw_tail": lambda tid, chars=30000: "screen",
             "running_turn": lambda text: False, "prompt_state": lambda raw, *, cols: ("empty", ""),
             "transcript_facts": lambda row: dict(txs[row["claudeSessionId"]]),
             "git_rev_parse_dirs": cfs.fake_git_dirs}  # the custody probe: no git runs
    saved_cfs = {k: getattr(cfs, k) for k in list(stubs) + ["terminals", "open_rows", "prepare_status", "API"]}
    saved_g = {k: g[k] for k in ("PASS", "run_helper")}
    saved_screen, saved_argv = nss.screen, sys.argv

    class Recorder:
        def __getattr__(self, _name):
            return lambda *a, **k: []
    res = []
    try:
        for k, v in stubs.items():
            setattr(cfs, k, v)
        ready = {f"s-{tid}": rp for tid, _, _, rp, _, _ in table}
        for tid, alive, row, rp, tx, _ in table:
            reg = dict(row, claudeSessionId=f"s-{tid}")
            term = {"id": tid, "isAlive": alive, "pid": 10 if alive else 99}
            verdict, why = cfs.classify(term, reg, {10: (1, ["bash"])}, 15, "", ready)
            code, why = cfs.coded_row(why, None)  # as cfs.screen_all lifts it
            judged, reasons = nss.judge(verdict, why, nss.row_facts(reg, ready), tx, now, None)
            res.append({"terminalId": tid, "verdict": judged, "screen": verdict, "reasons": reasons, "name": tid,
                        "tab": "t", "account": "a", "claudeSessionId": f"s-{tid}", "reason_code": code})
        policy = {tid for tid, _, _, _, _, ok in table if ok}
        closer = {r["terminalId"] for r in res if r["screen"] in cfs.CLOSABLE}  # what --close targets
        if closer != policy:
            print(f"FAIL close screen: --close would target {sorted(closer)}; only {sorted(policy)} are finished in "
                  f"coord (or exited): {[(r['terminalId'], r['screen']) for r in res]}")
            failures += 1
        g["PASS"], cfs.terminals, cfs.open_rows = Recorder(), lambda: [], lambda: {}
        cfs.prepare_status = lambda: ("clear", "")
        nss.screen = lambda *a, **k: ([dict(r) for r in res], [], None)
        out = io.StringIO()
        sys.argv = ["triage-sessions.py", "--skip", "limit,name,layout,nudge"]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            main()
        said = [ln.split() for ln in out.getvalue().splitlines() if ln.strip().startswith("would close ")]
        named = {w[2] for w in said}
        if named != closer or any(w[-1] != f"({next(r['screen'] for r in res if r['terminalId'] == w[2])})"
                                  for w in said):
            print(f"FAIL close report: would close {sorted(named)}, the closer's own screen closes {sorted(closer)}: "
                  f"{out.getvalue()!r}")
            failures += 1
        # A CUSTODY row (c0000010, a primary checkout) is named for the operator in both modes: no step
        # closes or nudges it.
        custody = {r["terminalId"] for r in res if r["screen"] == "CUSTODY"}

        def custody_listed(text):
            return {ln.split()[1] for ln in text.splitlines() if ln.strip().startswith("CUSTODY ")
                    and "custody: not a linked worktree" in ln and ln.rstrip().endswith("; for the operator")}
        if custody != {"c0000010"} or custody_listed(out.getvalue()) != custody:
            print(f"FAIL close report: CUSTODY rows {sorted(custody)}, named for the operator "
                  f"{sorted(custody_listed(out.getvalue()))}: {out.getvalue()!r}")
            failures += 1
        # Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run Phase 2: after == close, the
        # finished rows a screen rule holds, each with its finish_stranded code; no uncoded row.
        coded = {r["terminalId"]: r["reason_code"] for r in res if r.get("reason_code")}
        want_coded = {"c0000007": "finish_stranded.coord_status", "c0000008": "finish_stranded.coord_status",
                      "c0000011": "finish_stranded.in_band"}
        held = out.getvalue().split(f"== FINISHED, HELD BY A SCREEN RULE (not closed): {len(want_coded)}\n", 1)
        shown = {ln.split()[0]: ln for ln in (held[1].splitlines() if len(held) == 2 else []) if ln.startswith("  c")}
        if coded != want_coded or set(shown) != set(want_coded) or not all(
                f"] {code}: " in held[1] for code in want_coded.values()):
            print(f"FAIL close report: the held section lists {sorted(shown)}, coded {coded}; want {want_coded}: "
                  f"{out.getvalue()!r}")
            failures += 1
        # Acting: the header names the screen-time count, and the helper's argv is one it accepts.
        ran = []
        g["run_helper"] = lambda script, args: ran.append((script, list(args))) or (0, "")
        out = io.StringIO()
        sys.argv = ["triage-sessions.py", "--act", "--skip", "limit,name,layout,nudge", "--only", "c0", "--port",
                    "9877", "--idle-min", "7"]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            main()
        if f"== close ({len(closer)} closable at screen time; close-finished-sessions.py re-screens each row " \
                "before its DELETE)" not in out.getvalue() or [s for s, _ in ran] != ["close-finished-sessions.py"]:
            print(f"FAIL close --act: header or helper run wrong: {out.getvalue()!r} {ran}")
            failures += 1
        if custody_listed(out.getvalue()) != custody:
            print(f"FAIL close --act: CUSTODY rows {sorted(custody)}, named for the operator "
                  f"{sorted(custody_listed(out.getvalue()))}: {out.getvalue()!r}")
            failures += 1
        # Every option this script takes, set at once, still builds an argv the helper's parser accepts.
        ap = build_parser()
        every = []
        for act in ap._actions:
            flag = next((o for o in act.option_strings if o.startswith("--")), None)
            if flag and flag != "--help":
                every += [flag] if act.nargs == 0 else [flag, "1" if act.type in (int, float) else "x"]
        argvs = [args for _, args in ran] + [close_argv(ap.parse_args(every), ["--rows-out", "rows.jsonl"])]
        for args in argvs:
            try:
                with contextlib.redirect_stderr(io.StringIO()):
                    cfs.build_parser().parse_args(args)
            except SystemExit:
                print(f"FAIL close step argv {args} is refused by close-finished-sessions.py's own parser")
                failures += 1
        # The nudge step's listing (plan Phase 6): a finish the session's own record contradicts is listed
        # as nudged, so /next-move reconciles it; only STUCK is left for the operator.
        stuck = {"terminalId": "c0000099", "verdict": "STUCK", "screen": "ACTIVE", "name": "stuck", "tab": "t",
                 "account": "a", "reasons": ["3 /next-move nudges in a row made no progress"]}
        nss.screen = lambda *a, **k: ([dict(r) for r in res] + [dict(stuck)], [], None)
        out = io.StringIO()
        sys.argv = ["triage-sessions.py", "--skip", "close,limit,name,layout"]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            main()
        lines = [ln.split() for ln in out.getvalue().splitlines()]
        contradicted = {r["terminalId"] for r in res if r["screen"] == "CONTRADICTED" and r["verdict"] == "STOPPED"}
        listed = {w[1] for w in lines if w[:1] == ["CONTRADICTED"] and "nudged:" in w}
        nudged = {w[2] for w in lines if w[:2] == ["would", "nudge"]}
        if not contradicted or listed != contradicted or not contradicted <= nudged or \
                not any(w[:2] == ["STUCK", "c0000099"] for w in lines) or "c0000099" in nudged:
            print(f"FAIL nudge listing: contradicted {sorted(contradicted)}, listed as nudged {sorted(listed)}, "
                  f"would nudge {sorted(nudged)}: {out.getvalue()!r}")
            failures += 1
        # Phase 2: a HELD row carrying a finish_stranded code is listed for the operator, with its code, and
        # never nudged; an uncoded HELD row is not listed.
        held_coded = {r["terminalId"] for r in res if r["verdict"] == "HELD" and r.get("reason_code")}
        listed_held = {w[1] for w in lines if w[:1] == ["HELD"] and any(x.startswith("finish_stranded.") for x in w)}
        uncoded_held = {r["terminalId"] for r in res if r["verdict"] == "HELD" and not r.get("reason_code")}
        if held_coded != {"c0000007", "c0000011"} or listed_held != held_coded or held_coded & nudged \
                or any(w[:1] == ["HELD"] and w[1] in uncoded_held for w in lines):
            print(f"FAIL nudge listing: coded HELD {sorted(held_coded)}, listed {sorted(listed_held)}, uncoded HELD "
                  f"{sorted(uncoded_held)}: {out.getvalue()!r}")
            failures += 1
    finally:
        sys.argv = saved_argv
        nss.screen = saved_screen
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
    failures += layout_step_selftest(res)
    return failures, len(table), len(argvs), 5


def layout_step_selftest(res):
    """Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run
    Phase 7: the layout step's argv, through main() with every door stubbed. In
    report mode it carries the close step's would-close terminals as
    --assume-closed; with --skip close, and in an act pass, it carries none; and
    --assume-closed only ever rides a --dry-run, which runner-layout.py requires.
    Returns failures."""
    import contextlib
    import io
    failures = 0
    g = globals()
    closer = [r["terminalId"] for r in res if r["screen"] in cfs.CLOSABLE]
    saved_g = {k: g[k] for k in ("PASS", "run_helper")}
    saved_cfs = {k: getattr(cfs, k) for k in ("terminals", "open_rows", "prepare_status", "API")}
    saved_screen, saved_argv = nss.screen, sys.argv

    class Recorder:
        def __getattr__(self, _name):
            return lambda *a, **k: []
    try:
        g["PASS"], cfs.terminals, cfs.open_rows = Recorder(), lambda: [], lambda: {}
        cfs.prepare_status = lambda: ("clear", "")
        cfs.API = "http://127.0.0.1:9"
        for argv, want, err in ((["--skip", "limit,name,nudge"], closer, None),  # report mode, close step on
                                (["--skip", "close,limit,name,nudge"], [], None),  # close skipped: nothing closes
                                (["--act", "--skip", "limit,name,nudge"], [], None),  # act: its closes already ran
                                # readiness unreadable: the closer refuses --close, so nothing would close
                                (["--skip", "limit,name,nudge"], [], "GET /restart-readiness: timed out")):
            nss.screen = lambda *a, err=err, **k: ([dict(r) for r in res], [], err)
            ran = []
            g["run_helper"] = lambda script, args: ran.append((script, list(args))) or (0, "")
            sys.argv = ["triage-sessions.py"] + argv
            out = io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                main()
            layout = [args for script, args in ran if script == "runner-layout.py"]
            got = []
            if layout and "--assume-closed" in layout[0]:
                got = layout[0][layout[0].index("--assume-closed") + 1].split(",")
            if not closer or len(layout) != 1 or sorted(got) != sorted(want) \
                    or ("--assume-closed" in layout[0] and "--dry-run" not in layout[0]):
                print(f"FAIL layout step argv under {argv} (readiness error {err!r}): {layout}; "
                      f"want --assume-closed {sorted(want)}")
                failures += 1
            text = out.getvalue()
            if err and ("== close (0 closable)" not in text or CLOSE_REFUSED_UNREADABLE not in text
                        or "would close " in text):
                print(f"FAIL close report with readiness unreadable: {text!r}")
                failures += 1
    finally:
        sys.argv = saved_argv
        nss.screen = saved_screen
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
    return failures


class FakeClock:
    """time.time() and time.sleep() for the watch selftest: a sleep moves the
    clock instead of waiting, so a 60 s watch deadline passes in no real time."""

    def __init__(self, now=1_800_000_000.0):
        self.now = now

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


WATCH_LABELS = ("HANDED", "GONE", "FAILED", "REDIRECT", "WORKING", "PENDING")


def watch_rows(text):
    """{8-character pane id: [watch row labels, in order]} in printed output."""
    rows = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] in WATCH_LABELS:
            rows.setdefault(parts[1], []).append(parts[0])
    return rows


def watch_selftest():
    """The watch after a limit or nudge send, driven through main() --act, and
    the name step's poll, driven through name_sessions(). Every runner door is
    stubbed, cfs.API points at a closed port as a backstop, and the clock is
    fake, so nothing reaches a runner and no real time passes. Returns
    (failures, cells)."""
    import contextlib
    import http.client
    import io
    g = globals()
    bad = []
    real_aftercare = aftercare
    keys_g = ("PASS", "run_helper", "submit", "write_keys", "post_keys", "ready_to_type", "custom_title", "aftercare",
              "time", "hold_line", "auto_accept_dial")
    keys_cfs = ("API", "terminals", "open_rows", "raw_tail", "prepare_status", "readiness", "transcript_facts",
                "output_moving", "prompt_state", "read", "page_names", "classify", "proc_table")
    saved_g = {k: g[k] for k in keys_g}
    saved_cfs = {k: getattr(cfs, k) for k in keys_cfs}
    saved_screen, saved_argv = nss.screen, sys.argv
    tmp = tempfile.mkdtemp(prefix="triage-watch-")
    working, idle = "\u273b Working\u2026 (esc to interrupt)", "\u276f "
    unknown = "\u276f /next-move\n  " + UNKNOWN_NEXT_MOVE + "\n"
    outcome_of = {"NUDGED": "done", "SENT?": "sent_unconfirmed", "SKIPPED": "skipped"}
    clock = FakeClock()

    class Rec:
        """PASS for these runs: real --rows-out paths, the limit step's outcomes kept, nothing recorded."""

        def __init__(self):
            self.dir, self.n, self.limit = tempfile.mkdtemp(dir=tmp), 0, None

        def rows_out(self, step):
            self.n += 1
            return ["--rows-out", os.path.join(self.dir, f"{self.n}-{step}.jsonl")]

        def handed_off(self, _targets, outcomes):
            self.limit = dict(outcomes)

        def __getattr__(self, _name):
            return lambda *a, **k: None

    def screen_rows(verdict, panes):
        return [{"terminalId": t, "verdict": verdict, "tab": "tab", "name": t, "reasons": [], "account": "acct",
                 "claudeSessionId": sid} for t, sid in panes]

    def act(step, res, listed, registry, screens, sends=(), written=None, slow=0.0):
        """main() --act with every step but `step` skipped, over the screen rows
        `res`. Once the step has sent, /terminals lists `listed`, the registry
        holds `registry` ({pane: session}) and each registry read moves the
        clock `slow` s. For nudge, the stubbed helper prints a row per
        (pane, label) in `sends` and writes the --rows-out row of the panes in
        `written` (default: all of them). Returns (exit code, output, reads by
        (phase, door), screen reads per pane once the watch began, prompts
        typed, keys written, the limit step's outcomes)."""
        state = {"phase": "screen"}  # -> "step" at main's prepare read, -> "watch" when aftercare starts
        reads, looked, typed, keyed, rec = {}, {}, [], [], Rec()
        rows_written = {t for t, _ in sends} if written is None else written

        def count(door):
            reads[(state["phase"], door)] = reads.get((state["phase"], door), 0) + 1

        def terminals():
            count("terminals")
            return [{"id": t} for t in listed]

        def open_rows():
            count("open_rows")
            clock.sleep(slow)
            return {t: {"claudeSessionId": sid} for t, sid in registry.items()}

        def prepare():
            state["phase"] = "step" if state["phase"] == "screen" else state["phase"]
            return "clear", ""

        def tail(tid, chars=30000):
            shown = screens.get(tid)
            if isinstance(shown, list):  # one screen per look once the watch began, the last one kept
                shown = shown[min(looked.get(tid, 0), len(shown) - 1)]
            if state["phase"] == "watch":
                looked[tid] = looked.get(tid, 0) + 1
                if isinstance(shown, Exception):  # a read the watch's own look must survive
                    raise shown
            return None if isinstance(shown, Exception) else shown

        def watched(*args, **kw):
            state["phase"] = "watch"
            return real_aftercare(*args, **kw)

        def helper(script, args):
            if not script.startswith("nudge-stopped-sessions"):
                raise AssertionError(f"{script} ran in a watch cell")
            path = args[args.index("--rows-out") + 1]
            for tid, label in sends:
                if tid in rows_written:
                    cfs.append_rows_out(path, {"terminal_id": tid, "action": "nudge",
                                               "action_outcome": outcome_of[label], "detail": {}})
            return 0, "".join(f"  {label:<7} {tid[:8]}  tab             acct           {tid} - selftest\n"
                              for tid, label in sends)

        cfs.terminals, cfs.open_rows, cfs.prepare_status, cfs.raw_tail = terminals, open_rows, prepare, tail
        g["PASS"], g["run_helper"], g["aftercare"] = rec, helper, watched
        g["submit"] = lambda tid, message: typed.append((tid, message)) or (True, "")
        g["write_keys"] = lambda tid, keys: keyed.append((tid, keys))
        nss.screen = lambda *_a, **_k: ([dict(r) for r in res], [], None)
        sys.argv = ["triage-sessions.py", "--act", "--skip", ",".join(s for s in STEPS if s != step)]
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            try:
                rc = main()
            except Exception as e:  # reported as this cell's exit, so the cells after it still run
                rc = f"main() raised {type(e).__name__}: {e}"
        return rc, out.getvalue(), reads, looked, typed, keyed, rec.limit

    cells = 0
    try:
        cfs.API = "http://127.0.0.1:9"
        cfs.readiness = lambda: ({}, None)
        cfs.transcript_facts = lambda row: None
        cfs.output_moving = lambda tid, seconds=3: False
        cfs.prompt_state = lambda raw, *, cols: ("empty", "")  # keyword-only, like the real one
        cfs.page_names = lambda: {}

        def no_read(*_a, **_k):
            raise AssertionError("a watch cell read GET /health")
        cfs.read = no_read
        g["ready_to_type"] = lambda tid, *_a, **_k: (True, "", None)
        g["time"] = clock
        # A slow registry: each read takes longer than the watch's wait split
        # over the batch. Every sent pane is still looked at and gets exactly one
        # row, the first one's turn showing only at its second look, so the wait
        # counts from after the setup read; the session ids come from the screen,
        # so setup reads the registry once for the panes the screen gave none, and
        # not at all when it gave every one; and no /terminals read resolves the
        # sent panes after the batch.
        for step, n, from_screen in (("nudge", 0, False), ("nudge", 1, False), ("nudge", 13, False),
                                     ("nudge", 13, True), ("limit", 0, False), ("limit", 1, False),
                                     ("limit", 13, False)):
            cells += 1
            case = f"slow registry, {step} batch of {n}" + (" with screen ids" if from_screen else "")
            wait = 60 if step == "nudge" else 90
            ids = [f"w{i:07d}-{step}" for i in range(n)]
            res = screen_rows("STOPPED" if step == "nudge" else "LIMITED",
                              [(t, f"s-{t}" if from_screen else None) for t in ids])
            screens = dict.fromkeys(ids, working)
            screens.update({t: [idle, working] for t in ids[:1]})
            rc, text, reads, looked, typed, keyed, _ = act(
                step, res, listed=ids, registry={t: f"s-{t}" for t in ids}, screens=screens,
                sends=[(t, "NUDGED") for t in ids], slow=wait / max(n, 1) + 1)
            rows, setup = watch_rows(text), reads.get(("watch", "open_rows"), 0)
            want_setup = 1 if n and not from_screen else 0
            if rows != {t[:8]: ["WORKING"] for t in ids} or any(looked.get(t, 0) < 1 for t in ids):
                bad.append(f"{case}: watch rows {rows}, screens read {looked}; want one WORKING row per pane, "
                           "each pane looked at")
            if setup != want_setup or reads.get(("step", "terminals"), 0) or rc or keyed:
                bad.append(f"{case}: setup read the registry {setup} time(s) (want {want_setup}), /terminals "
                           f"{reads.get(('step', 'terminals'), 0)} time(s) after the sends (want 0), exit {rc}, "
                           f"keys {keyed}")
        # One sweep always runs and always finishes: a watch with no time left
        # still looks at every pane once.
        cells += 1
        zero = [f"z{i:07d}-zero" for i in range(2)]
        cfs.terminals = lambda: [{"id": t} for t in zero]
        cfs.open_rows = lambda: {t: {"claudeSessionId": f"s-{t}"} for t in zero}
        cfs.raw_tail = lambda tid, chars=30000: working
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            left = real_aftercare(zero, argparse.Namespace(), "", wait_s=0)
        if watch_rows(out.getvalue()) != {t[:8]: ["WORKING"] for t in zero} or left:
            bad.append(f"a watch with no time left: rows {watch_rows(out.getvalue())}, {left} not completed; "
                       "want one sweep over every pane")
        # A pane the runner moved after its send, and one that closed: both
        # reach the watch, and the screen's session id finds the moved one's
        # new pane (HANDED); nothing holds the other's (GONE). A closed pane
        # whose session id neither the screen nor the registry knows is GONE
        # with that said, not "no other pane holds its session".
        for step in ("nudge", "limit"):
            cells += 1
            moved, gone, stay, new, orphan = (f"{c}0000001-{step}" for c in "mgsno")
            res = screen_rows("STOPPED" if step == "nudge" else "LIMITED",
                              [(moved, "s-moved"), (gone, "s-gone"), (stay, "s-stay"), (orphan, None)])
            rc, text, reads, looked, typed, keyed, outcomes = act(
                step, res, listed=[stay, new], registry={new: "s-moved", stay: "s-stay"},
                screens={stay: working, new: working}, sends=[(t, "NUDGED") for t in (moved, gone, stay, orphan)])
            rows, lines = watch_rows(text), text.splitlines()
            said = (any(ln.startswith(f"  HANDED  {moved[:8]}") and f"pane {new[:8]}" in ln for ln in lines)
                    and any(ln.startswith(f"  GONE    {gone[:8]}") and "no other pane holds" in ln for ln in lines)
                    and any(ln.startswith(f"  GONE    {orphan[:8]}") and "session id is unknown" in ln for ln in lines))
            if rows != {moved[:8]: ["HANDED"], gone[:8]: ["GONE"], stay[:8]: ["WORKING"], orphan[:8]: ["GONE"]} \
                    or not said:
                bad.append(f"moved mid-batch, {step}: watch rows {rows}; want HANDED to {new[:8]}, GONE, WORKING, "
                           f"GONE (session id unknown): {lines}")
            if step == "limit" and outcomes != {moved: "done", gone: "failed", stay: "done", orphan: "unknown"}:
                bad.append(f"moved mid-batch, limit: outcomes {outcomes}")
        # A pane that never resolves reads PENDING at the deadline, counted as
        # not completed with its outcome left sent_unconfirmed; so does a pane
        # whose redirect was typed and that then showed nothing more.
        for step in ("nudge", "limit"):
            cells += 1
            quiet, asked = f"q0000001-{step}", f"u0000001-{step}"
            res = screen_rows("STOPPED" if step == "nudge" else "LIMITED", [(quiet, "s-quiet"), (asked, "s-asked")])
            rc, text, reads, looked, typed, keyed, outcomes = act(
                step, res, listed=[quiet, asked], registry={quiet: "s-quiet", asked: "s-asked"},
                screens={quiet: idle, asked: unknown}, sends=[(quiet, "NUDGED"), (asked, "NUDGED")])
            lines, rows = text.splitlines(), watch_rows(text)
            said = (any(ln.startswith(f"  PENDING {quiet[:8]}  - no HANDED, REDIRECT, FAILED or WORKING reading "
                                      "within ") and ln.endswith("read the pane again with --report") for ln in lines)
                    and any(ln.startswith(f"  PENDING {asked[:8]}  - redirected, then no HANDED, FAILED or WORKING")
                            for ln in lines))
            redirects = [t for t, m in typed if m == NEXT_MOVE_FROM_MAIN]
            if rows != {quiet[:8]: ["PENDING"], asked[:8]: ["REDIRECT", "PENDING"]} or not said or rc != 1 \
                    or redirects != [asked]:
                bad.append(f"never resolves, {step}: watch rows {rows}, exit {rc}, redirected {redirects}: {lines}")
            if step == "limit" and outcomes != {quiet: "sent_unconfirmed", asked: "sent_unconfirmed"}:
                bad.append(f"never resolves, limit: outcomes {outcomes}, want both sent_unconfirmed")
        # A look whose read raises (a torn body, which raw_tail does not catch)
        # keeps the pane for the next sweep and never ends the others' watch;
        # a pane whose every look fails reads PENDING, naming the failure.
        cells += 1
        once, always, fine = "e0000001-raise", "e0000002-raise", "e0000003-raise"
        torn = http.client.IncompleteRead(b"{")
        res = screen_rows("STOPPED", [(once, "s-1"), (always, "s-2"), (fine, "s-3")])
        rc, text, reads, looked, typed, keyed, _ = act(
            "nudge", res, listed=[once, always, fine], registry={once: "s-1", always: "s-2", fine: "s-3"},
            screens={once: [torn, working], always: [torn], fine: working},
            sends=[(t, "NUDGED") for t in (once, always, fine)])
        rows = watch_rows(text)
        if rows != {once[:8]: ["WORKING"], always[:8]: ["PENDING"], fine[:8]: ["WORKING"]} or rc != 1 \
                or not any(ln.startswith(f"  PENDING {always[:8]}") and "its last look failed: IncompleteRead" in ln
                           for ln in text.splitlines()):
            bad.append(f"a look that raises: watch rows {rows}, exit {rc}")
        # A look that raised AFTER it typed into the pane - the survey's 0 before a redirect, then a torn
        # screen read; the Esc on a limit menu the runner did not move, whose reply was torn - ends the pane
        # FAILED, naming the failure, and the pane is never looked at again, so nothing is typed twice. The
        # real write_keys runs and counts each write; only its POST is stubbed.
        cells += 1
        sv, mn, fine = "v0000001-typed", "m0000001-typed", "f0000001-typed"
        menu = ("You've hit your weekly limit\nWhat do you want to do?\n  \u276f1. Stop and wait for limit to reset\n"
                "   2. Resume when the limit resets\n Enter to confirm \u00b7 Esc to cancel")
        posted, seen = [], {}

        def typed_tail(tid, chars=30000):
            seen[tid] = seen.get(tid, 0) + 1
            if tid == sv and seen[tid] == 3:  # the survey check's re-read, after its 0 went out
                raise http.client.IncompleteRead(b"{")
            return {sv: unknown + "  " + SURVEY + "\n", mn: menu, fine: working}[tid]

        def post(tid, keys):
            posted.append((tid, keys))
            if keys == b"\x1b":  # the Esc reached the runner, and its reply did not come back
                raise http.client.IncompleteRead(b"{")
        g["write_keys"], g["post_keys"] = saved_g["write_keys"], post
        cfs.terminals = lambda: [{"id": t} for t in (sv, mn, fine)]
        cfs.raw_tail = typed_tail
        cfs.prepare_status = lambda: ("clear", "")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            left = real_aftercare([sv, mn, fine], argparse.Namespace(), "", wait_s=60,
                                  sids={sv: "s-v", mn: "s-m", fine: "s-f"})
        rows, lines = watch_rows(out.getvalue()), out.getvalue().splitlines()
        if rows != {sv[:8]: ["FAILED"], mn[:8]: ["FAILED"], fine[:8]: ["WORKING"]} or left != 2 \
                or posted != [(sv, b"0"), (mn, b"\x1b")] \
                or not all(any(ln.startswith(f"  FAILED  {t[:8]}") and "raised after it typed" in ln
                               and "IncompleteRead" in ln for ln in lines) for t in (sv, mn)):
            bad.append(f"a look that raises after typing: watch rows {rows}, {left} not completed, keys posted "
                       f"{posted}; want FAILED, FAILED, WORKING with one 0 and one Esc: {lines}")
        g["post_keys"] = saved_g["post_keys"]
        # Plan 2026-11-12-a-limit-hit-by-a-background-subagent-shows-only-the-rate-limit-menu-so-the-pane-is-never-migrated,
        # Phase 2: the limit step over a LIMITED pane parked on an open usage-limit menu. It gets one Esc, and
        # the step waits for the runner's move before it types anything. Moved (the pane leaves /terminals and
        # another pane holds its session): HANDED, done, nothing typed and no second Esc, with the real
        # re-screen in place, which would read the closed pane SKIPPED. Closed with its session nowhere: GONE,
        # failed (unknown with no session id), again with the real re-screen in place and never reaching it.
        # Not moved, the menu gone: the re-screen and /next-move follow, as for any LIMITED row, unless a draft
        # sits at the prompt, which the real re-screen's real prompt read refuses. Not moved, the menu still
        # open - drawn as a raw stream whose option label is dim, which the real prompt read takes for an empty
        # prompt, or full-width on a 1000-column pane, which only a reader of the whole tail sees - a second
        # Esc, then the real re-screen refuses the row because the menu is open, so nothing is typed; it is
        # failed, not skipped, since Escs went in. A prepare that holds at the wait's last look leaves the menu
        # open: no second Esc, and the re-screen refuses. A runner call that raises - the read after the Esc,
        # the Esc POST, the second Esc POST, the re-screen, the /next-move POST - ends the row FAILED naming
        # that call, and the step goes on; a raise in the look before any Esc ends it SKIPPED. In every run
        # each Esc follows the menu look directly: the prepare read comes before the look, never between the
        # look and the key. The real write_keys counts each write; only its POST is stubbed.
        open_cases = ("stays open", "second Esc raises", "held at the second look", "stays open, 1000 columns")
        saved_judge = nss.judge

        def menu_cell(case, parked, heir):
            """One hand_off_limited run over `parked`. Returns (not completed, output lines, keys posted,
            prompts typed, panes re-screened, outcomes, the prepare/look/esc events in order)."""
            posted, prompts, rescreened, outcomes, events, flag = [], [], [], {}, [], {"rescreen": False}
            sid = None if case == "gone, no session id" else "s-parked"
            after = {"moved": [heir], "gone": [], "gone, no session id": []}.get(case, [parked])
            menu = {"stays open": cfs.LIMIT_MENU_RAW_DIM,
                    "stays open, 1000 columns": cfs.limit_menu_full_width(1000)}.get(case, cfs.LIMIT_MENU_SCREEN)
            draft = (cfs.EMPTY_PROMPT_SCREEN + "\x1b[?25l\x1b[H\r\x1b[2C\x1b[17Bhalf\x1b[8Gtyped\x1b[14Gdraft"
                     "\r\x1b[47C\x1b[20;1H\x1b[18;19H\x1b[?25h")  # the prompt cases' differential draft

            def menu_terminals():
                if (posted and case == "read raises") or flag["rescreen"]:  # a read after the Esc
                    raise (http.client.IncompleteRead(b"{") if case == "read raises"
                           else urllib.error.URLError("timed out"))
                return [{"id": t} for t in (after if posted else [parked])]

            def post(tid, keys):
                posted.append((tid, keys))
                events.append("esc")
                if case == "Esc raises" or (case == "second Esc raises" and len(posted) == 2):
                    raise http.client.IncompleteRead(b"{")  # the Esc reached the runner; its reply did not come back

            def tail(tid, chars=30000):
                events.append("look")
                if case == "the menu look raises" and not posted:
                    raise http.client.IncompleteRead(b"{")
                if case == "re-screen raises" and posted:
                    flag["rescreen"] = True  # await_handoff's last look; the re-screen's first read raises
                    return idle
                if case == "a draft at the prompt" and posted:
                    return draft
                if case in open_cases or not posted:
                    return menu
                return working if prompts else idle

            def prepare():
                events.append("prepare")
                return ("held", "held text") if case == "held at the second look" and posted else ("clear", "")

            def submit_stub(tid, message):
                prompts.append((tid, message))
                if case == "the /next-move POST raises":
                    raise http.client.IncompleteRead(b"{")  # e.read() of a refused POST, cut short
                return True, ""
            cfs.terminals, cfs.prepare_status, cfs.raw_tail = menu_terminals, prepare, tail
            cfs.open_rows = lambda: {t: {"claudeSessionId": sid} for t in (after if posted else [parked])}
            # The re-screen's verdict for a pane whose menu stays open, so its final look is reached.
            cfs.proc_table = lambda: {}
            cfs.classify = lambda *_a, **_k: ("LIMITED", ["an open usage-limit menu ends the screen"])
            if case == "the Esc unstuck it":  # its reset passed while the menu was up: the re-screen reads STOPPED
                cfs.classify = lambda *_a, **_k: ("STOPPED", [])
                nss.judge = lambda *_a, **_k: ("STOPPED", ["its usage limit reset"])
            g["write_keys"], g["post_keys"] = saved_g["write_keys"], post
            g["ready_to_type"] = saved_g["ready_to_type"] if case in (
                "moved", "gone", "gone, no session id", "stays open", "re-screen raises", "a draft at the prompt",
                "held at the second look", "stays open, 1000 columns", "the Esc unstuck it") else (
                lambda tid, *_a, **_k: rescreened.append(tid) or (True, "", None))
            cfs.prompt_state = (saved_cfs["prompt_state"] if case in ("stays open", "a draft at the prompt",
                                                                      "stays open, 1000 columns") else stub_prompt)
            g["submit"] = submit_stub
            g["aftercare"] = real_aftercare
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                try:
                    left = hand_off_limited([{"terminalId": parked, "name": "parked", "claudeSessionId": sid}],
                                            argparse.Namespace(idle_min=15, cooldown_hours=None), "", outcomes)
                except Exception as e:  # a raise out of the step is this cell's failure, not the selftest's end
                    left = f"hand_off_limited raised {type(e).__name__}: {e}"
            cfs.prompt_state = stub_prompt
            nss.judge = saved_judge
            return left, out.getvalue().splitlines(), posted, prompts, rescreened, outcomes, events
        heir, ready_stub, stub_prompt, esc = "h0000001-menu", g["ready_to_type"], cfs.prompt_state, b"\x1b"
        refused_open = "Claude's usage-limit menu is open just before the send"
        for i, case in enumerate(("moved", "gone", "gone, no session id", "not moved", "stays open", "read raises",
                                  "Esc raises", "second Esc raises", "re-screen raises", "the menu look raises",
                                  "a draft at the prompt", "held at the second look", "stays open, 1000 columns",
                                  "the /next-move POST raises", "the Esc unstuck it"), 1):
            cells += 1
            parked = f"p{i:07d}-menu"
            left, lines, posted, prompts, rescreened, outcomes, events = menu_cell(case, parked, heir)
            p = parked[:8]
            want = {  # (not completed, outcome, prompts, re-screened, keys posted, line prefixes)
                "moved": (0, "done", [], [], [esc], [f"  HANDED  {p}  parked - the runner resumed the session in pane "
                                                       f"{heir[:8]} under another account"]),
                "gone": (1, "failed", [], [], [esc], [f"  GONE    {p}  parked - the pane closed, but no other pane "
                                                        "holds its session"]),
                "gone, no session id": (1, "unknown", [], [], [esc], [f"  GONE    {p}  parked - the pane closed; its "
                                                                       "session id is unknown"]),
                "not moved": (0, "done", [(parked, "/next-move")], [parked], [esc],
                              [f"  SENT    {p}  parked - /next-move", f"  WORKING {p}  - "]),
                "stays open": (1, "failed", [], [], [esc, esc], [f"  SKIPPED {p}  parked - {refused_open}"]),
                "read raises": (1, "failed", [], [], [esc], [f"  FAILED  {p}  parked - the hand-off read after its "
                                                               "Esc raised (IncompleteRead"]),
                "Esc raises": (1, "failed", [], [], [esc], [f"  FAILED  {p}  parked - the Esc on its usage-limit "
                                                              "menu raised (IncompleteRead"]),
                "second Esc raises": (1, "failed", [], [], [esc, esc], [f"  FAILED  {p}  parked - the second Esc on "
                                                                         "its usage-limit menu raised (IncompleteRead"]),
                "re-screen raises": (1, "failed", [], [], [esc], [f"  FAILED  {p}  parked - the re-screen raised "
                                                                    "(URLError"]),
                "the menu look raises": (1, "skipped", [], [], [], [f"  SKIPPED {p}  parked - the look for an open "
                                                                    "usage-limit menu raised (IncompleteRead"]),
                "a draft at the prompt": (1, "failed", [], [], [esc], [f"  SKIPPED {p}  parked - input prompt not "
                                                                         "empty just before the send (text"]),
                "held at the second look": (1, "failed", [], [], [esc], [f"  SKIPPED {p}  parked - re-screen: held "
                                                                           "text"]),
                "stays open, 1000 columns": (1, "failed", [], [], [esc, esc], [f"  SKIPPED {p}  parked - "
                                                                                f"{refused_open}"]),
                "the /next-move POST raises": (1, "failed", [(parked, "/next-move")], [parked], [esc],
                                               [f"  FAILED  {p}  parked - the /next-move POST raised (IncompleteRead"]),
                # the Esc unstuck it (the re-screen reads STOPPED): not a failure, nothing typed
                "the Esc unstuck it": (0, "skipped", [], [], [esc],
                                       [f"  SKIPPED {p}  parked - the Esc on its menu let it resume (unstuck); not "
                                        "typed this pass"])
            }[case]
            if (left, outcomes.get(parked), prompts, rescreened) != want[:4] \
                    or posted != [(parked, k) for k in want[4]] \
                    or not all(any(ln.startswith(w) for ln in lines) for w in want[5]) \
                    or not all(i and events[i - 1] == "look" for i, ev in enumerate(events) if ev == "esc"):
                bad.append(f"a pane parked on the limit menu, {case}: {left} not completed, outcomes {outcomes}, "
                           f"keys posted {posted}, prompts {prompts}, re-screened {rescreened}, events {events}; "
                           f"want {want}: {lines}")
        # Every Esc goes out before any wait, and one shared deadline covers the wait: two panes
        # whose menus stay open and which the runner never moves cost one 30 s wait between them,
        # not one each, and both Escs are sent before the first /terminals read.
        cells += 1
        pair = ["c0000001-menu", "c0000002-menu"]
        order, started = [], clock.now
        cfs.terminals = lambda: order.append("read") or [{"id": t} for t in pair]
        cfs.prepare_status = lambda: ("clear", "")
        cfs.raw_tail = lambda tid, chars=30000: cfs.LIMIT_MENU_SCREEN
        g["post_keys"] = lambda tid, keys: order.append(("esc", tid))
        g["write_keys"] = saved_g["write_keys"]
        g["ready_to_type"] = lambda *_a, **_k: (False, "held", None)
        pair_out = {}
        with contextlib.redirect_stdout(io.StringIO()):
            hand_off_limited([{"terminalId": t, "name": "pair", "claudeSessionId": f"s-{t}"} for t in pair],
                             argparse.Namespace(idle_min=15, cooldown_hours=None), "", pair_out)
        first_read = order.index("read") if "read" in order else len(order)
        if order[:2] != [("esc", pair[0]), ("esc", pair[1])] or first_read < 2 or clock.now - started > 45:
            bad.append(f"two panes parked on the limit menu: order {order[:6]}, {clock.now - started:.0f}s elapsed; "
                       "want both Escs before the first /terminals read and one shared 30s wait")
        g["post_keys"], g["ready_to_type"] = saved_g["post_keys"], ready_stub
        cfs.classify, cfs.proc_table = saved_cfs["classify"], saved_cfs["proc_table"]
        # await_handoff's state as its other two readers take it, read as hand_off_limited reads it, over the
        # menu drawn full-width on a 1000-column pane, which only a reader of the whole stripped tail sees. In
        # aftercare, a pane whose menu stays open is FAILED after one more Esc, and one that closes while its
        # hand-off is awaited, with its session nowhere, is GONE (failed: its session id is known). The name
        # step's poll reads a pane the runner moved as HANDED, one that closed with its session nowhere as
        # GONE, and one whose menu stays open as FAILED, after one more Esc.
        cells += 1
        stay, gone2, keyed2, outs2, sweeps = "a0000001-menu", "a0000002-menu", [], {}, []
        wide = cfs.limit_menu_full_width(1000)

        def state_terminals():  # gone2 is listed for aftercare's sweep only, then closes during its wait
            sweeps.append(1)
            return [{"id": stay}] + ([{"id": gone2}] if len(sweeps) == 1 else [])
        cfs.terminals, cfs.prepare_status, cfs.open_rows = state_terminals, (lambda: ("clear", "")), (lambda: {})
        cfs.raw_tail = lambda tid, chars=30000: wide
        g["write_keys"] = lambda tid, keys: keyed2.append((tid, keys))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            left = real_aftercare([stay, gone2], argparse.Namespace(), "", wait_s=60, outcomes=outs2,
                                  sids={stay: "s-stay", gone2: "s-gone"})
        lines2 = out.getvalue().splitlines()
        polled, poll = [], "q0000001-poll"
        for heir2, listed2 in (("h0000002-poll", []), ("", []), ("", [poll])):
            cfs.terminals = lambda listed2=listed2: [{"id": t} for t in listed2]
            cfs.open_rows = lambda heir2=heir2: {heir2: {"claudeSessionId": "s-poll"}} if heir2 else {}
            polled.append(poll_one(poll, 0, "", {"name": "polled", "claudeSessionId": "s-poll"}, set(),
                                   argparse.Namespace(), "", {poll: {}}, {})[0])
        if (left, outs2, keyed2, polled) != (2, {stay: "failed", gone2: "failed"}, [(stay, esc), (poll, esc)],
                                              ["HANDED ", "GONE   ", "FAILED "]) \
                or not any(ln.startswith(f"  FAILED  {stay[:8]}") for ln in lines2) \
                or not any(ln.startswith(f"  GONE    {gone2[:8]}") and "no other pane holds its session" in ln
                           for ln in lines2):
            bad.append(f"await_handoff's state as aftercare and the poll read it: {left} not completed, outcomes "
                       f"{outs2}, keys {keyed2}, poll labels {polled}; want 2, FAILED and GONE both failed, an Esc "
                       f"each to the open menus, HANDED, GONE, FAILED: {lines2}")
        # The sent panes come from the nudge helper's --rows-out rows, so a pane
        # this pass's screen never listed is watched too. That file's writer
        # warns and never raises: a sent pane with no row is matched by its
        # printed prefix against the screen rows; a skipped row is never
        # watched; a prefix nothing resolves reads PENDING.
        cells += 1
        a1, b1, x1, y1, k1 = "a0000001-out", "b0000001-out", "x0000001-out", "y0000001-out", "k0000001-out"
        res = screen_rows("STOPPED", [(a1, "s-a"), (b1, "s-b"), (k1, "s-k")])
        rc, text, reads, looked, typed, keyed, _ = act(
            "nudge", res, listed=[a1, b1, x1, y1, k1],
            registry={a1: "s-a", b1: "s-b", x1: "s-x", y1: "s-y", k1: "s-k"},
            screens=dict.fromkeys((a1, b1, x1, y1, k1), working),
            sends=[(a1, "NUDGED"), (b1, "SENT?"), (x1, "NUDGED"), (y1, "NUDGED"), (k1, "SKIPPED")],
            written={a1, y1, k1})
        rows = watch_rows(text)
        if rows != {a1[:8]: ["WORKING"], b1[:8]: ["WORKING"], x1[:8]: ["PENDING"], y1[:8]: ["WORKING"]} or rc != 1 \
                or not any(ln.startswith(f"  PENDING {x1[:8]}") and "not watched" in ln for ln in text.splitlines()):
            bad.append(f"sent panes from --rows-out, a row lost: watch rows {rows}, exit {rc}")
        # The name step's poll: one /terminals read and one registry read per
        # round, for every pane. Two rounds over three panes: /name runs in the
        # first, the reply and title read back in the second.
        cells += 1
        names = [f"n{i:07d}-name" for i in range(3)]
        polled = {"terminals": 0, "open_rows": 0}

        def count_terminals():
            polled["terminals"] += 1
            return [{"id": t} for t in names]

        def count_rows():
            polled["open_rows"] += 1
            return {t: {"claudeSessionId": f"s-{t}"} for t in names}
        cfs.terminals, cfs.open_rows = count_terminals, count_rows
        cfs.raw_tail = lambda tid, chars=30000: "\u23f5\u23f5 bypass permissions on (shift+tab to cycle)"
        cfs.read = lambda path, key, timeout=60: {"success": True,
                                                  "data": {"permissionAutoAccept": {"answered": True, "level": "on"}}}
        cfs.transcript_facts = lambda row: {"path": "p", "last_prompt": 10 ** 13, "final_text_at": 10 ** 13,
                                            "last_prompt_text": "<command-name>/name</command-name>",
                                            "final_text": "named"}
        g["ready_to_type"] = lambda tid, *_a, **_k: (True, "", {"path": f"{tid}.jsonl"})
        g["submit"] = lambda tid, message: (True, "")
        g["custom_title"] = lambda path: "a title"
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            got, n_failed = name_sessions([{"terminalId": t, "name": t, "claudeSessionId": f"s-{t}"} for t in names],
                                          argparse.Namespace(idle_min=15, cooldown_hours=None, name_timeout=10), "")
        if got != names or n_failed or polled != {"terminals": 2, "open_rows": 2}:
            bad.append(f"name poll over {len(names)} panes in 2 rounds: named {got}, {n_failed} failed, reads "
                       f"{polled}; want all named and one /terminals and one registry read per round")
        # The name step's poll over a pane whose /name met the usage-limit menu, which then closed during the
        # hand-off wait with its session nowhere: GONE, counted as a failure as the limit step and the watch
        # count it, with its pass-ledger outcome settled by the poll - failed, or unknown with no session id.
        cells += 1
        named3, settled3, lines3 = [], {}, []
        g["hold_line"], g["auto_accept_dial"] = (lambda tid, dial: ""), (lambda: (True, ""))
        g["ready_to_type"] = lambda tid, *_a, **_k: (True, "", {"path": f"{tid}.jsonl"})
        g["submit"], g["write_keys"] = (lambda tid, message: (True, "")), (lambda tid, keys: None)
        cfs.open_rows, cfs.prepare_status = (lambda: {}), (lambda: ("clear", ""))
        cfs.raw_tail = lambda tid, chars=30000: cfs.LIMIT_MENU_SCREEN
        for gone3, sid3 in (("g0000001-name", "s-g"), ("g0000002-name", None)):
            rounds = []

            def name_terminals(gone3=gone3, rounds=rounds):  # listed for the poll's first read, then closed
                rounds.append(1)
                return [{"id": gone3}] if len(rounds) == 1 else []
            cfs.terminals = name_terminals
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                got3, n3 = name_sessions([{"terminalId": gone3, "name": gone3, "claudeSessionId": sid3}],
                                         argparse.Namespace(idle_min=15, cooldown_hours=None, name_timeout=120), "",
                                         settled3)
            named3.append((got3, n3))
            lines3 += out.getvalue().splitlines()
        if named3 != [([], 1), ([], 1)] or settled3 != {"g0000001-name": "failed", "g0000002-name": "unknown"} \
                or not all(any(ln.startswith(f"  GONE    {t[:8]}") for ln in lines3)
                           for t in ("g0000001-name", "g0000002-name")):
            bad.append(f"the name step's poll, a pane gone after its limit menu: (named, failures) {named3}, "
                       f"settled {settled3}; want each counted, failed and unknown: {lines3}")
    except Exception as e:  # a cell that raises is a failure, never a crash of the selftest
        bad.append(f"watch selftest raised {type(e).__name__}: {e}")
    finally:
        sys.argv, nss.screen = saved_argv, saved_screen
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
        shutil.rmtree(tmp, ignore_errors=True)
    for b in bad:
        print("FAIL " + b)
    return len(bad), cells


def reply_selftest():
    """Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run Phase 5:
    the watch reads an "Unknown command" reply wherever it sits after the last
    /next-move echo, a spinner older than that reply is not a running turn, a
    PENDING row names the pane's last screen line, and the nudge step's watch
    rows ride the helper's own --rows-out row as detail.watch without touching
    its outcome. The fixtures are SYNTHESIZED (no temporary runner was used: no
    live reproduction). Returns (failures, cells)."""
    import contextlib
    import io
    bad, cells = [], 0
    rule = "─" * 220
    chrome = f"\n{rule}\n❯ \n{rule}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n"
    # SYNTHESIZED: a wide pane whose input box and status line, redrawn below the reply, put the reply more
    # than 600 characters from the end of the screen.
    wide = "✻ Worked for 2m 3s\n\n❯ /next-move\n  ⎿  " + UNKNOWN_NEXT_MOVE + "\n" + chrome * 2
    # SYNTHESIZED: a spinner frame redrawn after the last turn end and before the reply.
    stale = ("✻ Worked for 2m 3s\n✢ Thinking… (esc to interrupt)\n\n❯ /next-move\n  ⎿  "
             + UNKNOWN_NEXT_MOVE + "\n" + chrome)
    # an earlier /next-move's reply, then a new /next-move echo with no reply yet
    again = "❯ /next-move\n  ⎿  " + UNKNOWN_NEXT_MOVE + "\n❯ /next-move\n" + chrome
    for text, want in ((wide, True), (stale, True), (again, False), (chrome, False)):
        cells += 1
        if answered_unknown(text) != want:
            bad.append(f"answered_unknown({text[:40]!r}...) = {answered_unknown(text)}, want {want}")
    cells += 1
    if UNKNOWN_NEXT_MOVE in wide[-600:]:
        bad.append("the wide fixture must put the reply outside the last 600 characters")
    for text, want in ((stale, False), (stale + "✢ Thinking… (esc to interrupt)\n", True),
                       ("✢ Thinking… (esc to interrupt)\n", True)):
        cells += 1
        if running_now(text) != want:
            bad.append(f"running_now({text[-50:]!r}) = {running_now(text)}, want {want}")
    for text, want in ((None, "no screen read"), (chrome, "last screen line '\\u23f5\\u23f5 bypass permissions on "
                                                          "(shift+tab to cycle)'"), ("\n \n", "screen blank")):
        cells += 1
        if last_screen_state(text) != want:
            bad.append(f"last_screen_state({text!r:.30}) = {last_screen_state(text)!r}, want {want!r}")
    # The watch over the wide and stale-spinner fixtures: one REDIRECT each (never WORKING), then PENDING
    # naming the last screen line, and the watch rows recorded for merge_watch. Every door is stubbed and the
    # clock is fake.
    g = globals()
    for label, screen in (("wide", wide), ("stale", stale)):
        saved_g = {k: g[k] for k in ("ready_for_redirect", "submit", "time", "limit_menu_open")}
        saved_cfs = {k: getattr(cfs, k) for k in ("terminals", "raw_tail", "open_rows")}
        tid = f"r0000001-{label}"
        try:
            g["time"] = FakeClock()
            g["ready_for_redirect"] = lambda t, term: (True, "")
            typed = []
            g["submit"] = lambda t, message, typed=typed: typed.append((t, message)) or (True, "")
            cfs.terminals = lambda tid=tid: [{"id": tid}]
            cfs.raw_tail = lambda t, chars=30000, screen=screen: screen
            cfs.open_rows = lambda tid=tid: {tid: {"claudeSessionId": "s-r"}}
            out, watch = io.StringIO(), {}
            with contextlib.redirect_stdout(out):
                left = aftercare([tid], argparse.Namespace(), "", sids={tid: "s-r"}, watch=watch)
        finally:
            g.update(saved_g)
            for k, v in saved_cfs.items():
                setattr(cfs, k, v)
        cells += 1
        pending = [ln for ln in out.getvalue().splitlines() if ln.startswith(f"  PENDING {tid[:8]}")]
        if watch_rows(out.getvalue()) != {tid[:8]: ["REDIRECT", "PENDING"]} or left != 1 \
                or typed != [(tid, NEXT_MOVE_FROM_MAIN)] or not pending or "last screen line" not in pending[0] \
                or watch.get(tid, {}).get("verdict") != "PENDING" or watch[tid].get("redirected") is not True:
            bad.append(f"the {label} Unknown command reply: rows {watch_rows(out.getvalue())}, left {left}, typed "
                       f"{typed}, watch {watch}: {out.getvalue()!r}")
    # merge_watch: the watch row lands in the helper's own nudge row as detail.watch; its outcome is untouched.
    cells += 1
    tmp = tempfile.mkdtemp(prefix="triage-merge-")
    path = os.path.join(tmp, "nudge.jsonl")
    try:
        cfs.append_rows_out(path, {"terminal_id": tid, "action": "nudge", "action_outcome": "done",
                                   "detail": {"sent": True}})
        cfs.append_rows_out(path, {"terminal_id": "other", "action": "nudge", "action_outcome": "skipped",
                                   "detail": {}})
        merge_watch(["--rows-out", path], {tid: {"verdict": "PENDING", "message": "x"}})
        rows, dropped = read_rows_file(path)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    by = {r["terminal_id"]: r for r in rows}
    if dropped or by[tid]["action_outcome"] != "done" or by[tid]["detail"] != {
            "sent": True, "watch": {"verdict": "PENDING", "message": "x"}} or "watch" in by["other"]["detail"]:
        bad.append(f"merge_watch: rows {rows}, dropped {dropped}")
    for b in bad:
        print("FAIL " + b)
    return len(bad), cells


def width_selftest():
    """The prompt reads around triage's sends (plan Phase 7, D3), over the
    SYNTHETIC idle Windows pane cfs.IDLE_CONPTY_PROMPT_SCREEN, whose empty prompt
    reads empty only when replayed at the pane's width: ready_to_type before a
    send, turn_done in the name poll and ready_for_redirect in the watch, each
    through the real cfs.prompt_state with wrap_cols held on its Windows arm,
    so no cell depends on the host. Then the watch's redirect, which needs the
    pane's /terminals row: a sweep whose /terminals read fails types nothing and
    the next sweep redirects; when every read fails the pane reads PENDING and
    says why. Every door is stubbed, submit only records, cfs.API points at a
    closed port as a backstop, and the clock is fake. Returns (failures, cells)."""
    import contextlib
    import io
    g = globals()
    bad, cells, typed = [], 0, []
    idle, cols = cfs.IDLE_CONPTY_PROMPT_SCREEN, cfs.CONPTY_FIXTURE_COLS
    term = {"id": "w0000001-width", "cols": cols}
    tid, real_wrap = term["id"], cfs.wrap_cols
    keys_cfs = ("API", "terminals", "readiness", "open_rows", "classify", "proc_table", "transcript_facts",
                "output_moving", "prepare_status", "raw_tail", "wrap_cols")
    keys_nss = ("external_signals", "judge", "row_facts")
    keys_g = ("screen_text", "submit", "write_keys", "time")
    saved_cfs = {k: getattr(cfs, k) for k in keys_cfs}
    saved_nss = {k: getattr(nss, k) for k in keys_nss}
    saved_g = {k: g[k] for k in keys_g}
    try:
        cfs.API = "http://127.0.0.1:9"
        cfs.terminals = lambda: [term]
        cfs.readiness = lambda: ({}, None)
        cfs.open_rows = lambda: {tid: {"claudeSessionId": "s-width"}}
        cfs.classify = lambda *a: ("ACTIVE", [])
        cfs.proc_table = lambda: {}
        cfs.transcript_facts = lambda row: {"path": "p", "final_text_at": 2, "final_text": "named",
                                            "pending_tool": False}
        cfs.output_moving = lambda tid, seconds=3: False
        cfs.prepare_status = lambda: ("clear", "")
        cfs.raw_tail = lambda tid, chars=30000: idle
        cfs.wrap_cols = lambda t, windows=None: real_wrap(t, windows=True)
        nss.external_signals = lambda *a, **k: None
        nss.row_facts = lambda *a: {}
        nss.judge = lambda *a: ("STOPPED", [])
        g["screen_text"] = lambda tid, chars=3000: "\u276f /next-move\n  " + UNKNOWN_NEXT_MOVE + "\n"
        g["submit"] = lambda tid, message: typed.append((tid, message)) or (True, "")
        g["write_keys"] = lambda tid, keys: bad.append(f"a width cell wrote {keys!r} into {tid}")
        for name, call, want in (
                ("ready_to_type", lambda: ready_to_type(tid, 0, 48, "")[:2], (True, "")),
                ("turn_done", lambda: turn_done(tid, term, {"claudeSessionId": "s-width"}, 1), (True, "named")),
                ("ready_for_redirect", lambda: ready_for_redirect(tid, term), (True, ""))):
            cells += 1
            try:
                got = call()
            except Exception as e:  # a width passed by position raises TypeError: a failure, never a crash
                got = f"raised {type(e).__name__}: {e}"
            if got != want:
                bad.append(f"{name} over an idle Windows pane read at its width = {got!r}, want {want!r}")
        # The watch's redirect: the pane's row comes from the sweep's own /terminals read.
        for fails, want_typed, want_rows in ((1, [(tid, NEXT_MOVE_FROM_MAIN)], ["REDIRECT", "PENDING"]),
                                             (10 ** 6, [], ["PENDING"])):
            cells += 1
            typed.clear()
            reads = []

            def terminals(fails=fails, reads=reads):
                reads.append(1)
                if len(reads) <= fails:
                    raise urllib.error.URLError("selftest: refused")
                return [term]
            cfs.terminals, g["time"] = terminals, FakeClock()
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                left = aftercare([tid], argparse.Namespace(), "", sids={tid: "s-width"})
            rows = watch_rows(out.getvalue()).get(tid[:8], [])
            why = "the /terminals read failed, so the redirect's prompt check could not run"
            if typed != want_typed or rows != want_rows or left != 1 or \
                    (not want_typed and why not in out.getvalue()):
                bad.append(f"a watch whose first {fails} /terminals read(s) fail: typed {typed}, rows {rows}, "
                           f"{left} not completed; want {want_typed}, {want_rows}: {out.getvalue()!r}")
    finally:
        g.update(saved_g)
        for k, v in saved_cfs.items():
            setattr(cfs, k, v)
        for k, v in saved_nss.items():
            setattr(nss, k, v)
    for b in bad:
        print("FAIL " + b)
    return len(bad), cells


def build_parser():
    """This script's argument parser (--selftest is read before it). The
    selftest sets every option it takes and parses the close step's argv with
    close-finished-sessions.py's own parser."""
    ap = argparse.ArgumentParser()
    ap.add_argument("--act", action="store_true", help="run the steps; without it, report only")
    ap.add_argument("--skip", default="", help=f"comma-separated steps to leave out: {', '.join(STEPS)}")
    ap.add_argument("--max-name", type=int, default=0, help="sessions to name at most (default: no cap)")
    ap.add_argument("--max-nudge", type=int, default=0, help="nudges to send at most (default: no cap)")
    ap.add_argument("--name-timeout", type=int, default=600, help="seconds to wait for /name replies")
    ap.add_argument("--idle-min", type=int, default=15)
    ap.add_argument("--cooldown-hours", type=float, default=None,
                    help="the nudge step's backstop hours (default: nudge-stopped-sessions.py's own)")
    ap.add_argument("--only", default="", help="terminal id prefixes to limit every step to (layout: the pages holding them)")
    ap.add_argument("--tab", action="append", default=[], metavar="NAME",
                    help="limit every step to the sessions in the Terminal-page tab(s) of this name; repeatable")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    return ap


def main():
    for stream in (sys.stdout, sys.stderr):  # a piped Windows console is cp1252
        try:
            stream.reconfigure(encoding="utf-8", errors="backslashreplace")
        except (AttributeError, ValueError, OSError):
            pass
    if "--selftest" in sys.argv[1:]:
        return selftest()
    a = build_parser().parse_args()
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
        except (*cfs.READ_ERRORS, http.client.HTTPException) as e:
            print(f"UNKNOWN: could not read the runner at {cfs.API}: {e}", file=sys.stderr)
            return 3
        print(f"tab {', '.join(repr(n) for n in tabs)}: {len(hit)} tab(s), {len(ids)} terminal(s)")
        if not ids:
            print(f"no terminal on this runner is in those tabs (page id(s) {', '.join(sorted(hit))}); "
                  "nothing to triage")
            return 0
        a.only = ",".join(ids)
    PASS.screening()
    try:
        terms = [t for t in cfs.terminals() if any(t["id"].startswith(o) for o in a.only.split(",") if o)]
        res, warnings, ready_err = nss.screen(a.idle_min, [o for o in a.only.split(",") if o], a.cooldown_hours,
                                              False)
        rows = cfs.open_rows()
    except (*cfs.READ_ERRORS, http.client.HTTPException) as e:
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
    prep, prep_text = cfs.prepare_status()
    if prep != "clear":
        print("WARNING: " + prep_text)
    if a.act and prep == "unknown":
        print(f"UNKNOWN: --act refused - {prep_text}", file=sys.stderr)
        return 3
    for step in typing_steps_held(prep, skip):
        print(f"\n== {step} (held: a restart prepare is in force, so this step types into nothing)")
        PASS.step(step, held="restart prepare in force")
        skip.add(step)

    failed = 0
    if "close" not in skip:
        # The prediction is the closer's own: the close screen's verdict against its CLOSABLE set.
        closable = would_close(res, ready_err)
        PASS.step("close", candidates=len(closable))
        if a.act:
            print(f"\n== close ({len(closable)} closable at screen time; close-finished-sessions.py re-screens "
                  "each row before its DELETE)")
        else:
            print(f"\n== close ({len(closable)} closable)")
            if ready_err:
                print("  " + CLOSE_REFUSED_UNREADABLE)
        list_custody(res)
        if a.act:
            code, _ = run_helper("close-finished-sessions.py", close_argv(a, PASS.rows_out("close")))
            failed += code != 0
            PASS.step("close", exit=code)
        else:
            for r in closable:
                print(f"  would close {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]} ({r['screen']})")
        # Structurally finished rows a screen rule holds, with their finish_stranded
        # code: the close helper's own section, over the same screened rows.
        cfs.print_stranded(res)

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
            name_outcomes = {}
            named, n_failed = name_sessions(targets, a, own, name_outcomes)
            failed += n_failed > 0
            PASS.named(targets, named, dict(SUBMITTED), name_outcomes)
        else:
            name_report(targets)

    if "layout" not in skip:
        print("\n== layout (trim each page to its tabs)")
        # Under --only (and --tab, which sets it), trim only the pages holding those terminals.
        pages = sorted({t.get("pageId") or "default" for t in terms})
        if a.only and not pages:
            print("  no page holds a terminal --only names; nothing to trim")
        else:
            code, _ = run_helper("runner-layout.py", layout_argv(a, pages if a.only else [], res, skip, ready_err))
            failed += code != 0
            PASS.step("layout", pages=len(pages) if a.only else "all", exit=code)

    if "nudge" not in skip:
        print(f"\n== nudge ({counts.get('STOPPED', 0)} STOPPED)")
        PASS.step("nudge", candidates=counts.get("STOPPED", 0))
        # Listed first. STUCK, a session /next-move could not move three times
        # running, is for the operator and never nudged, and so is a HELD row whose
        # finish a screen rule holds (its finish_stranded code). CONTRADICTED, a
        # finish the session's own record contradicts (the close screen's verdict),
        # is nudged, so /next-move reconciles the two signals.
        for label, r in operator_rows(res):
            code = f"{r['reason_code']} " if label == "HELD" else ""
            print(f"  {label:<12} {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['name'][:50]} - "
                  f"{'nudged: ' if label == 'CONTRADICTED' else ''}{code}{'; '.join(r['reasons'])[:160]}")
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
                rows_out = PASS.rows_out("nudge")  # the watch reads the sent panes' full ids from it
                code, out = run_helper("nudge-stopped-sessions.py",
                                       ["--send", "--only", ",".join(named), "--idle-min", "0"] + cap + port
                                       + rows_out)
                failed += code != 0
                sends = sum(line.startswith(("  NUDGED", "  SENT?")) for line in out.splitlines())
                failed += watch_nudged(rows_out, out, res, a, own) > 0
            left = a.max_nudge - sends if a.max_nudge else 0
            if not a.max_nudge or left > 0:
                if named:
                    print("  -- every other STOPPED session")
                rows_out = PASS.rows_out("nudge")
                code, out = run_helper("nudge-stopped-sessions.py",
                                       ["--send"] + cap + common(a) + (["--max", str(left)] if a.max_nudge else [])
                                       + rows_out)
                failed += code != 0
                failed += watch_nudged(rows_out, out, res, a, own) > 0
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
# Verdicts whose finish time (coord's, else the runner marker's) is the time the
# row has been in that state since.
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
    nss.screen()), before any step acted on it. Pure, apart from the clock
    finish_since reads for its plausibility bound. Reason codes are not emitted
    yet: the prose reasons ride in detail.reasons.

    state_since_ms prefers coord's finish time, as close-finished-sessions does:
    a finish made through coord alone leaves no runner marker, and a re-finish on
    a runner build predating the restamp leaves the marker at the first finish.
    It is read only while coord's status IS finished, because sessionStatusSince
    dates whatever status the row is in, and a row FINISHED on the runner marker
    alone may read `working` there. It is cfs.finish_since, so a stamp still
    ahead of this clock proves skew and is not used, the same rule as classify.
    An EXITED-FINISHED row has left /restart-readiness, so it carries no coord
    time and the marker dates it."""
    sid, fin, verdict = r.get("claudeSessionId"), r.get("finishedAt"), r["verdict"]
    since = None
    if verdict in SINCE_FINISHED:
        since = cfs.finish_since(r, r.get("sessionStatus") == "finished")
        if since is None and isinstance(fin, (int, float)) and not isinstance(fin, bool) and fin > 0:
            since = int(fin)
    screen = r.get("screen")
    progress = r.get("progress_since_nudge")
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
        "progress_since_nudge": progress if isinstance(progress, bool) else None,
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
        p = subprocess.run(["git", "hash-object", "--", *paths], capture_output=True, timeout=30,
                           env=nss.git_scope_env(), check=False)
        shas = p.stdout.decode("utf-8").split() if p.returncode == 0 else []
    except (OSError, subprocess.TimeoutExpired, UnicodeDecodeError):
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
    """The runner's gitSha from /health, or None when it cannot be read. Never
    raises: the ledger must not take the run down with it."""
    try:
        sha = cfs.read("/health", "data.gitSha", timeout=20)
    except Exception:  # noqa: BLE001 - READ_ERRORS leaves out a response cut mid-read
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
        self.build_before = self.screened_build = None
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

    def screening(self):
        """Call before the screen: see screened()."""
        self.build_before = runner_build()

    def screened(self, res):
        """The flow metrics compare a pass's build across passes to tell a close
        from a restart, so the build recorded is the one the rows were screened
        on: read on both sides of the screen, and None (unknown) when a restart
        moved it during the screen or a read failed."""
        self.res = res
        after = runner_build()
        self.screened_build = after if after == self.build_before else None

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

    def named(self, targets, named, typed, outcomes=None):
        """The name step's rows. `typed` is what submit() recorded during the
        step ({terminal id: "sent" | "refused"}): done for a title read back,
        skipped for a row never typed into (held by name_hold, or by its
        re-screen), failed for a POST the runner refused, sent_unconfirmed for
        a prompt typed whose title did not read back. `outcomes` holds the
        outcomes the poll settled itself (name_sessions), which win."""
        outcomes = outcomes or {}
        rows = []
        for r in targets:
            tid = r["terminalId"]
            outcome = outcomes.get(tid) or ("done" if tid in named else "skipped" if tid not in typed
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
        build = self.screened_build if build is None else build
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
    globals()["runner_build"] = lambda: None  # the process exits after this selftest
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
    for given, want_progress in [(True, True), (False, False), (None, None), ("yes", None)]:
        got = pass_row(dict(res[0], progress_since_nudge=given), "x")["progress_since_nudge"]
        if got is not want_progress:
            bad.append(f"pass_row(progress_since_nudge={given!r}) = {got!r}")
    fin, held = pass_row(res[1], "x"), pass_row(res[2], "x")
    if (fin["claude_session_id"], fin["account"], fin["state_since_ms"]) != (None, None, 1761270000000):
        bad.append(f"pass_row(FINISHED) = {fin}")
    if (held["state_since_ms"], held["session_name"]) != (None, None):
        bad.append(f"pass_row(HELD) keeps finishedAt or an empty name: {held}")
    # state_since_ms against coord's finish time: (row overrides, want).
    marker, coord_at = 1761270000000, 1761280000000
    since_cases = [
        ({"sessionStatus": "finished", "sessionStatusSince": coord_at}, coord_at),  # newer than the marker: coord's
        ({"sessionStatus": "finished", "sessionStatusSince": marker - 5}, marker - 5),  # older: still coord's
        ({"sessionStatus": "finished", "sessionStatusSince": coord_at, "finishedAt": None}, coord_at),  # coord-only
        ({"sessionStatus": "working", "sessionStatusSince": coord_at}, marker),  # it dates `working`, not the finish
        ({"sessionStatus": "working", "sessionStatusSince": coord_at, "finishedAt": None}, None),
        ({"sessionStatus": "finished"}, marker),  # a runner build serving no time
        ({"sessionStatus": "finished", "sessionStatusSince": None, "finishedAt": None}, None),
        ({"sessionStatus": "finished", "sessionStatusSince": 7}, marker),  # status_since reads it as UNKNOWN
        ({"sessionStatus": "finished", "sessionStatusSince": True, "finishedAt": None}, None),
        ({"sessionStatus": "finished", "sessionStatusSince": int(time.time() * 1000) + 60000}, marker),  # ahead: skew
        ({"sessionStatus": None, "sessionStatusSince": None, "verdict": "EXITED-FINISHED"}, marker),  # its live shape
        ({"sessionStatus": "finished", "sessionStatusSince": coord_at, "verdict": "HELD"}, None),  # not in SINCE_FINISHED
        ({"sessionStatus": "finished", "sessionStatusSince": coord_at, "verdict": "REVIVED"}, None),
    ]
    for over, want_since in since_cases:
        got_since = pass_row(dict(res[1], **over), "x")["state_since_ms"]
        if got_since != want_since:
            bad.append(f"pass_row state_since_ms over {over} = {got_since}, want {want_since}")

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
        # The build recorded is the one the rows were screened on, even when a
        # restart moved the runner before the pass was written; a restart during
        # the screen, or a failed read, leaves it unknown.
        real_build = runner_build
        try:
            for before, after, want_build in [("a", "a", "a"), ("a", "b", None), (None, "a", None), ("a", None, None)]:
                globals()["runner_build"] = lambda v=before: v
                rec.screening()
                globals()["runner_build"] = lambda v=after: v
                rec.screened(res)
                globals()["runner_build"] = lambda: "at-end"
                checks.append((rec.build(1, now=when, versions={})["runner_build"], want_build))
        finally:
            globals()["runner_build"] = real_build
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
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, encoding="utf-8", errors="replace")
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
    print(f"ledger selftest: {3 + len(since_cases)} row cases, {len(checks)} build checks, 3 lock cases, {len(writers) + 2} never-raises "
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
    # A refused POST whose body read is cut short (IncompleteRead) is still the runner's refusal: submit()
    # returns it, so name_sessions, the aftercare redirect and the hand-off step finish the pass.
    class _TornBody(urllib.error.HTTPError):
        def read(self, *_a):
            raise http.client.IncompleteRead(b"{")

    def _refused(*_a, **_k):
        raise _TornBody("http://127.0.0.1:9/x", 500, "boom", {}, None)
    saved_urlopen = urllib.request.urlopen
    urllib.request.urlopen = _refused
    try:
        try:
            got = submit("t-torn", "/name")
        except Exception as e:  # noqa: BLE001
            got = (None, f"raised {type(e).__name__}: {e}")
    finally:
        urllib.request.urlopen = saved_urlopen
        SUBMITTED.pop("t-torn", None)
    if got[0] is not False or not got[1].startswith(nss.SUBMIT_REFUSED):
        bad.append(f"submit() with a refused POST whose body read is cut short = {got}")
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

        def after(tids, a, own, outcomes=None, **_k):
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
    rec.named([{"terminalId": t} for t in ("n-named", "n-manual", "n-refused", "n-typed", "n-gone", "n-anon")],
              ["n-named"], {"n-named": "sent", "n-refused": "refused", "n-typed": "sent", "n-gone": "sent",
                            "n-anon": "sent"}, {"n-gone": "failed", "n-anon": "unknown"})
    got = {r["terminal_id"]: r["action_outcome"] for r in rec.events[0][2]}
    want = {"n-named": "done", "n-manual": "skipped", "n-refused": "failed", "n-typed": "sent_unconfirmed",
            "n-gone": "failed", "n-anon": "unknown"}
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
