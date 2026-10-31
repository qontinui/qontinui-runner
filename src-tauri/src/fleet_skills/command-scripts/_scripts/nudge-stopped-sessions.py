#!/usr/bin/env python3
"""Find the runner's terminal sessions that stopped before finishing; prompt
/next-move into them.

Usage: nudge-stopped-sessions.py [--send] [--cooldown-hours H] [--idle-min N]
                                 [--max N] [--only ID[,ID..]] [--json] [--port N]

A STOPPED session is idle at an empty prompt (not working) and not finished.
The screen is close-finished-sessions.py's, imported from beside this file, so
both overlord commands judge "busy" and "empty prompt" the same way. Of its
verdicts, ACTIVE, REVIVED and FOOTER-ONLY are stopped, and so is a LIMITED
session whose usage-limit reset time has passed. That screen leans to "not
finished" when unsure, which is the safe side for a close and the unsafe side
for a nudge, so this script adds its own holds: any finished signal (the
runner marker, unless a prompt came after it; or coord session_status
finished), coord session_status waiting_human, and a last prompt that was a
/next-move newer than the cooldown.

The report is always printed. --send then types `/next-move` into each STOPPED
row through POST /terminals/{id}/submit-prompt, re-screening the row
immediately before (a 3-second output sample, then one last empty-prompt check
on the final screen) and reading the slash command back from the transcript.
Read-only unless --send. Stdlib only. The command /nudge-stopped-sessions
documents the verdicts and the evidence they rest on.

Exit codes: 0 done (every attempted nudge verified), 1 a nudge was skipped or
did not verify, 3 UNKNOWN: the runner could not be read (nothing screened), or
--send was refused because /restart-readiness could not be read.
"""
import argparse
import datetime as dt
import importlib.util
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

try:
    from zoneinfo import ZoneInfo, ZoneInfoNotFoundError
except ImportError:  # Python < 3.9: a named zone is then unreadable, so the reset is UNKNOWN
    ZoneInfo, ZoneInfoNotFoundError = None, Exception

# The helper runs from a provisioned skill directory, which must not collect a __pycache__.
sys.dont_write_bytecode = True
_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("cfs", os.path.join(_here, "close-finished-sessions.py"))
cfs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cfs)

PROMPT = "/next-move"
STOPPED_FROM = {"ACTIVE", "REVIVED", "FOOTER-ONLY"}
# /next-move as the transcript records a slash command that RAN. Only this form
# verifies a send: pasted text that never ran as a command is a plain message.
NEXT_MOVE_RAN = re.compile(r"<command-name>/next-move</command-name>")
# Either form, for the cooldown hold, where reading too much as a nudge is the safe side.
NEXT_MOVE_ANY = re.compile(r"<command-name>/next-move</command-name>|^/next-move(?![\w-])")
# "You've hit your session limit · resets 5:40pm (Europe/Berlin)". A dated form
# ("resets Oct 20, 5pm") does not match, so its reset reads UNKNOWN and the row
# stays LIMITED.
RESET_RE = re.compile(r"\bresets\s+(\d{1,2})(?::(\d{2}))?\s*([ap]m)\b\s*(?:\(([^)]+)\))?", re.I)
RESET_GRACE_MS = 2 * 60000
# A report whose HEADLINE (its first sentence) says the assignment came out
# with nothing owed: "No follow-up PR is needed for #1550, so I opened none and
# changed nothing." Every accepted phrase names the follow-up itself, because a
# bare "made no changes" or "nothing to do" is just as often the start of a
# refusal ("I made no changes: the push was refused").
COMPLETE_RE = re.compile(
    r"\bno follow-?up (?:PR |pull request |changes? |work )?(?:is |was )?(?:needed|required|owed)\b"
    r"|\bfollow-?up (?:PR |pull request )?(?:for \S+ )?(?:isn't|is not|wasn't|was not) (?:needed|required)\b"
    r"|\bnothing (?:that )?needs a follow-?up\b"
    r"|\bfound (?:nothing|no follow-?up work) to (?:follow up|add)\b|\bfound no follow-?up work\b"
    r"|\bmade no follow-?up changes\b"
    r"|\bnothing (?:is |was )?(?:left )?owed\b", re.I)
# A hedge inside the headline sentence turns it into a partial result.
HEADLINE_HEDGE = re.compile(r"\b(?:but|however|except|though|although|yet|until|unless|once|then|still)\b", re.I)
# Something left for someone to act on, anywhere in the message.
OPERATOR_ITEM_RE = re.compile(
    r"\bneeds? (?:a |an |your |one more )?(?:decision|approval|review|answer|change|fix|vetting)\b"
    r"|\bstill needs?\b|\bopen item\b|\bfor the operator\b|\boperator (?:must|should|needs to)\b|\boperator-run\b"
    r"|\b(?:is|are) blocked\b|\bblocks?\b|\bblocking\b|\bremains? open\b|\bmissing\b"
    r"|\bnot (?:yet )?(?:done|finished|complete|started|implemented)\b|\bunstarted\b|\bunimplemented\b"
    r"|\bdeferred\b|\bremaining\b|\bleft to do\b|\bran out of\b|\bis next\b|\bis red\b"
    r"|\bfailed\b|\brefused\b|\bunable to\b"
    r"|\bcan(?:'t|not) (?:finish|complete|land|push|read|merge|run)\b"
    r"|\bcould(?:n't| not) (?:finish|complete|land|push|read|merge|run)\b", re.I)
SENTENCE_END = re.compile(r"(?<=[.!?])\s|\n")
# A printed reset this close before the message is the same moment, rounded; not tomorrow.
RESET_ROUNDING = dt.timedelta(minutes=5)


# ---------------------------------------------------------------- rules
def limit_reset_ms(final_text, said_ms):
    """Epoch ms at which the limit named in `final_text` (said at `said_ms`)
    resets, or None when the reset time cannot be read."""
    m = RESET_RE.search(final_text[-300:])
    if not m or not said_ms:
        return None
    h12, minute = int(m.group(1)), int(m.group(2) or 0)
    if not 1 <= h12 <= 12 or minute > 59:
        return None
    hour = h12 % 12 + (12 if m.group(3).lower() == "pm" else 0)
    if m.group(4):
        if ZoneInfo is None:
            return None
        try:
            said = dt.datetime.fromtimestamp(said_ms / 1000, ZoneInfo(m.group(4).strip()))
        except (ZoneInfoNotFoundError, ValueError):
            return None
    else:
        said = dt.datetime.fromtimestamp(said_ms / 1000).astimezone()  # the local offset AT said_ms
    reset = said.replace(hour=hour, minute=minute, second=0, microsecond=0)
    if reset < said - RESET_ROUNDING:
        reset += dt.timedelta(days=1)
    return reset.timestamp() * 1000


def completion(verdict, tx):
    """The headline phrase that reports the assignment complete with nothing
    owed, or None. Only for a row the screen read as ACTIVE or FOOTER-ONLY
    with no 'work remains' veto in its final message."""
    if verdict not in ("ACTIVE", "FOOTER-ONLY") or not tx or not tx["final_text"].strip():
        return None
    if tx["last_prompt"] and (tx.get("final_text_at") or 0) < tx["last_prompt"]:
        return None  # the report predates the last instruction (a later turn wrote no text)
    text = cfs.normalize(tx["final_text"])
    # The screen reads its "work remains" phrases near the end only; a
    # completion needs them absent from the whole message.
    if any(r.search(text) for _, r in cfs.NEG_RE) or cfs.phrase_hits(text)[1] or OPERATOR_ITEM_RE.search(text):
        return None
    headline = SENTENCE_END.split(text.strip(), 1)[0]
    if HEADLINE_HEDGE.search(headline):
        return None
    m = COMPLETE_RE.search(headline)
    return m.group(0) if m else None


def judge(verdict, reasons, facts, tx, now_ms, cooldown_h, include_complete=False):
    """(nudge verdict, reasons) for one screened row: STOPPED, a row that needs
    no /next-move (HELD, NO-TASK, COMPLETE), or the screen's own verdict when
    the row is not a stopped session at all.

    `facts` carries the registry's `finished` / `finishedAt` and coord's
    `sessionStatus`. A nudge into a finished session reopens it (/next-move
    reads an owed item it cannot prove as owed), so every finished signal holds
    the row, except a runner marker that a later real prompt revived.
    Coord's finished status holds even then: a re-finish leaves finishedAt
    where it was, so coord is the only sign a revived session finished again."""
    limited = verdict == "LIMITED"
    if limited:
        if tx is None:
            return "LIMITED", reasons + ["transcript unreadable at re-check"]
        reset = limit_reset_ms(tx["final_text"], tx["last_asst"])
        if reset is None:
            return "LIMITED", reasons + ["reset time unreadable"]
        if now_ms < reset + RESET_GRACE_MS:
            return "LIMITED", reasons + [f"resets in {(reset + RESET_GRACE_MS - now_ms) / 60000:.0f}m"]
        reasons = [f"usage limit reset {cfs.fmt_age(reset)} ago"] + reasons
    elif verdict not in STOPPED_FROM:
        return verdict, reasons
    if tx is not None and not tx.get("task_prompts"):
        return "NO-TASK", ["never given a task: no prompt beyond built-in commands Claude Code answered "
                           "itself (/model, /effort, /clear, ...)"] + reasons
    status = facts.get("sessionStatus")
    if status == "waiting_human":
        return "HELD", ["coord session_status=waiting_human: a pending operator question"] + reasons
    last_prompt = (tx or {}).get("last_prompt") or 0
    if facts.get("finished"):
        fa = facts.get("finishedAt") or 0
        if not last_prompt or last_prompt <= fa + 1000:
            return "HELD", [f"runner finished marker ({cfs.fmt_age(fa) if fa else '?'} ago), "
                            "no prompt since"] + reasons
    if status == "finished":
        return "HELD", ["coord session_status=finished (no finish time to compare a revival against)"] + reasons
    # A /next-move answered by a usage-limit notice never ran, so it starts no cooldown.
    if tx and last_prompt and not limited and NEXT_MOVE_ANY.search(tx["last_prompt_text"].strip()):
        if (now_ms - last_prompt) / 3600000 < cooldown_h:
            return "HELD", [f"last prompt was /next-move {cfs.fmt_age(last_prompt)} ago "
                            f"(< {cooldown_h:g}h cooldown)"] + reasons
    done = completion(verdict, tx)
    if done:
        if include_complete:
            return "STOPPED", [f"complete but not marked finished ({done!r}); nudged to record it"] + reasons
        return "COMPLETE", [f"final report says the work is done, nothing owed: {done!r}; "
                            "not marked finished"] + reasons
    return "STOPPED", reasons


def row_facts(row, ready):
    row = row or {}
    return {"finished": bool(row.get("finished")), "finishedAt": row.get("finishedAt"),
            "sessionStatus": (ready.get(row.get("claudeSessionId")) or {}).get("sessionStatus")}


# ---------------------------------------------------------------- screening
def screen(idle_min, only, cooldown_h, include_complete):
    res, warnings, ready_err = cfs.screen_all(idle_min, only)
    rows = cfs.open_rows()
    now = time.time() * 1000
    for r in res:
        row = rows.get(r["terminalId"])
        tx = cfs.transcript_facts(row) if row and r["verdict"] in STOPPED_FROM | {"LIMITED"} else None
        facts = dict(row_facts(row, {}), sessionStatus=r.get("sessionStatus"))
        r["screen"] = r["verdict"]
        r["verdict"], r["reasons"] = judge(r["verdict"], r["reasons"], facts, tx, now, cooldown_h,
                                           include_complete)
    return res, warnings, ready_err


ORDER = ["STOPPED", "COMPLETE", "NO-TASK", "HELD", "LIMITED", "BUSY", "FINISHED", "CLOSEOUT", "EXITED-FINISHED", "EXITED", "UNKNOWN",
         "SHELL", "SELF"]


def print_report(res, warnings):
    for w in warnings:
        print("WARNING: " + w)
    res = sorted(res, key=lambda r: (ORDER.index(r["verdict"]), r["tab"], r["name"]))
    counts = {v: sum(r["verdict"] == v for r in res) for v in ORDER}
    print("verdicts: " + ", ".join(f"{v}={n}" for v, n in counts.items() if n))
    cur = None
    for r in res:
        if r["verdict"] != cur:
            cur = r["verdict"]
            print(f"\n== {cur}")
        screen_v = f"[{r['screen']}] " if r["screen"] != r["verdict"] else ""
        print(f"  {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['account']:<13.13}  {r['name'][:60]}")
        print(f"            - {screen_v}{'; '.join(r['reasons'])[:200]}")


def offset_of(path):
    """The transcript's size now, or None: the read-back reads only what is
    written after it."""
    try:
        return os.path.getsize(path)
    except (OSError, TypeError):
        return None


def ran_since(path, offset, since_ms):
    """True when a /next-move slash command was recorded at or after byte
    `offset` and time `since_ms`. Reading from the offset rather than a fixed
    tail matters: right after the command record, Claude Code writes the
    command's whole expanded body as one meta record, and /next-move's body
    alone is larger than any sensible tail window."""
    try:
        with open(path, "rb") as f:
            f.seek(offset)
            lines = f.read().decode(errors="replace").splitlines()
    except (OSError, TypeError, ValueError):
        return False
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue  # a line the offset cut, or one still being written
        if e.get("type") != "user" or e.get("isMeta") or not isinstance(e.get("message"), dict):
            continue
        c = e["message"].get("content")
        text = c if isinstance(c, str) else " ".join(
            b.get("text", "") for b in c or [] if isinstance(b, dict) and b.get("type") == "text")
        try:
            at = cfs.ms(e["timestamp"]) if e.get("timestamp") else None
        except ValueError:
            continue
        # Only a record that IS the command: a notification or reminder quoting it does not count.
        if at and at >= since_ms and text.lstrip().startswith(("<command-message>", "<command-name>")) \
                and NEXT_MOVE_RAN.search(text):
            return True
    return False


def nudge_one(tid, idle_min, cooldown_h, own, include_complete):
    """Re-screen, then submit /next-move, then read it back.
    Returns (ok, sent, msg); `sent` is True once the POST has gone out."""
    t = next((x for x in cfs.terminals() if x["id"] == tid), None)
    if t is None:
        return False, False, "terminal no longer listed"
    ready, err = cfs.readiness()
    if err:
        return False, False, f"readiness unreadable at re-screen ({err[:80]})"
    row = cfs.open_rows().get(tid)
    verdict, why = cfs.classify(t, row, cfs.proc_table(), idle_min, own, ready)
    tx = cfs.transcript_facts(row) if row else None
    verdict, why = judge(verdict, why, row_facts(row, ready), tx, time.time() * 1000, cooldown_h,
                         include_complete)
    if verdict != "STOPPED":
        return False, False, f"re-screen now says {verdict}: {'; '.join(why)}"
    moving = cfs.output_moving(tid)
    if moving is not False:
        return False, False, "terminal output still moving" if moving else "terminal output could not be sampled"
    # The route pastes onto whatever the input box holds and presses Enter, so
    # the last thing before the POST is one more look at the prompt itself:
    # a draft typed during the re-screen must not ride along.
    raw = cfs.raw_tail(tid)
    # Same width the screen passes: on Windows the replay must auto-wrap at the
    # pane's columns, or the cursor lands off the prompt row and reads as a draft.
    state, typed = cfs.prompt_state(raw, cfs.wrap_cols(t)) if raw is not None else ("unknown", "screen unreadable")
    if state != "empty":
        return False, False, f"input prompt not empty just before the send ({state}: {str(typed)[:50]!r})"
    path = (tx or {}).get("path")
    offset = offset_of(path)
    sent_ms = time.time() * 1000
    url = f"{cfs.API}/terminals/{tid}/submit-prompt"
    # What the runner says it wrote. Each field is read through the envelope
    # helper, so a reply without it is UNKNOWN (None), never a silent False.
    submitted = sanitized = None
    try:
        req = urllib.request.Request(url, method="POST", data=json.dumps({"message": PROMPT}).encode(),
                                     headers={"Content-Type": "application/json"})
        env = cfs.load(urllib.request.urlopen(req, timeout=60).read(), door="POST /terminals/{id}/submit-prompt",
                       source=url)
        reply = "reply read"
        for key in ("data.submitted", "data.sanitized"):
            try:
                value = env.require_key(key)[0]
            except cfs.EnvelopeUnknown:
                reply = f"reply has no {key}"
                continue
            if key == "data.submitted":
                submitted = value
            else:
                sanitized = value
    except urllib.error.HTTPError as e:
        return False, False, f"submit-prompt answered {e.code}: {e.read()[:200]!r}"
    except Exception as e:  # the runner may have typed it anyway: the read-back decides
        reply = f"{type(e).__name__}: {e}"
    if sanitized:
        return False, True, "submit-prompt reports the message was altered on the way to the PTY; not verified"
    for _ in range(15):
        time.sleep(2)
        if offset is not None and ran_since(path, offset, sent_ms - 1000):
            return True, True, "submitted; read back: the transcript recorded the /next-move command"
    sent = "submitted" if submitted else f"submit unconfirmed ({reply})"[:120]
    return False, True, f"{sent}; read back: no /next-move command in the transcript after 30s"


# ---------------------------------------------------------------- selftest
def selftest():
    bad = []
    berlin_1700 = dt.datetime(2026, 10, 18, 17, 0, tzinfo=dt.timezone(dt.timedelta(hours=2))).timestamp() * 1000
    cases = [
        ("You've hit your session limit · resets 5:40pm (Europe/Berlin)", berlin_1700, 40),
        ("You've hit your session limit · resets 6pm (Europe/Berlin)", berlin_1700, 60),
        ("You've hit your session limit · resets 4pm (Europe/Berlin)", berlin_1700, 23 * 60),
        ("You've hit your session limit · resets 12:15am (Europe/Berlin)", berlin_1700, 7 * 60 + 15),
        ("You've hit your session limit · resets 12pm (Europe/Berlin)", berlin_1700, 19 * 60),
        ("You've hit your session limit · resets 4:58pm (Europe/Berlin)", berlin_1700, -2),  # rounded
        ("You've hit your session limit · resets 13pm (Europe/Berlin)", berlin_1700, None),
        ("You've hit your weekly limit · resets Oct 20, 5pm (Europe/Berlin)", berlin_1700, None),
        ("You've hit your session limit · resets 5:40pm (Not/AZone)", berlin_1700, None),
        ("You've hit your session limit", berlin_1700, None),
    ]
    # A Python with zoneinfo but no zone data (Windows without tzdata) cannot
    # name the zone either, so the reset is UNKNOWN there, as with no zoneinfo.
    try:
        zones = ZoneInfo is not None and all(ZoneInfo(z) for z in ("Europe/Berlin", "UTC"))
    except ZoneInfoNotFoundError:
        zones = False
    for text, said, want_min in cases:
        got = limit_reset_ms(text, said)
        got_min = None if got is None else round((got - said) / 60000)
        if not zones and want_min is not None:
            want_min = None
        if got_min != want_min:
            bad.append(f"limit_reset_ms({text!r}) = {got_min}m after, want {want_min}")
    ran = "<command-message>next-move</command-message>\n<command-name>/next-move</command-name>"
    for text, want_ran, want_any in [(ran, True, True), ("/next-move", False, True),
                                     ("/next-move 3a6ed95f --recheck t4h", False, True),
                                     ("/next-move-foo", False, False),
                                     ("<command-name>/next-moves</command-name>", False, False),
                                     ("run /next-move later", False, False),
                                     ("<command-name>/close-finished-sessions</command-name>", False, False)]:
        if bool(NEXT_MOVE_RAN.search(text)) != want_ran or bool(NEXT_MOVE_ANY.search(text)) != want_any:
            bad.append(f"next-move match({text!r}) != ran {want_ran} / any {want_any}")
    now = 1_000_000_000_000
    tx = {"final_text": "done for now", "last_asst": now - 3600000, "last_prompt": now - 3600000,
          "last_prompt_text": ran, "task_prompts": 1, "final_text_at": now - 3600000}
    work = dict(tx, last_prompt_text="fix the bug")
    none = {}
    judge_cases = [
        ("ACTIVE", none, work, "STOPPED"),
        ("FOOTER-ONLY", none, work, "STOPPED"),
        ("ACTIVE", {"sessionStatus": "waiting_human"}, work, "HELD"),
        ("ACTIVE", none, tx, "HELD"),  # nudged 1h ago, inside the cooldown
        ("ACTIVE", none, dict(tx, last_prompt=now - 25 * 3600000), "STOPPED"),  # past the cooldown
        # Finished signals the close screen lets through as ACTIVE / FOOTER-ONLY / REVIVED:
        ("ACTIVE", {"finished": True, "finishedAt": now - 1800000}, work, "HELD"),  # veto phrase after the mark
        ("FOOTER-ONLY", {"sessionStatus": "finished"}, work, "HELD"),
        ("REVIVED", {"sessionStatus": "finished"}, work, "HELD"),
        # kept working after its mark with no new prompt: still finished
        ("REVIVED", {"finished": True, "finishedAt": now - 1800000}, dict(work, last_asst=now), "HELD"),
        # ...and the one real revival: a prompt after the runner mark,
        ("REVIVED", {"finished": True, "finishedAt": now - 7200000}, work, "STOPPED"),
        # unless coord says it finished again (finishedAt does not move on a re-finish).
        ("REVIVED", {"finished": True, "finishedAt": now - 7200000, "sessionStatus": "finished"}, work, "HELD"),
        # a /next-move the limit answered never ran: no cooldown once the limit resets
        # (with no zone data the reset is unreadable, so the row stays LIMITED)
        ("LIMITED", none, dict(tx, final_text="You've hit your session limit \u00b7 resets 1am (UTC)",
                                last_asst=now - 3600000), "STOPPED" if zones else "LIMITED"),
        ("BUSY", none, tx, "BUSY"),
        ("FINISHED", none, tx, "FINISHED"),
        ("EXITED", none, None, "EXITED"),
        ("LIMITED", none, dict(tx, final_text="You've hit your session limit"), "LIMITED"),
    ]
    for verdict, facts, t, want in judge_cases:
        got = judge(verdict, [], facts, t, now, 24)[0]
        if got != want:
            bad.append(f"judge({verdict}, {facts}, prompt={t and t['last_prompt_text'][:20]!r}) = {got}, want {want}")
    # Completion reports. True rows are the headlines of real sessions on
    # 2026-10-18; False rows are reports that still owe work (review round 1).
    complete_cases = [
        ("No follow-up PR is needed for #1550, so I opened none and changed nothing.\n\n- **Wiring:** ok", True),
        ("PR #251 left nothing that needs a follow-up PR, so I opened none and made no changes.", True),
        ("I found nothing to follow up on from #3112, so I opened no PR and changed nothing.", True),
        ("The feature from #1898 is fully wired, so I made no follow-up changes and opened no PR.", True),
        ("I made no follow-up changes and opened no pull request. The follow-up for #8 isn't needed.", True),
        ("I found no follow-up work for PR #4, so I made no changes and opened no pull request.", True),
        ("I reviewed the merged change and found nothing to add, so I opened no code follow-up PR.", True),
        ("No follow-up PR is needed for #1815, so I made no changes.\n\n- **Gaps:** none.", True),
        # still owing work
        ("I made no changes: allocate-worktree.sh refused with exit 10, so I stopped here.", False),
        ("Made no changes; I can't push from this box (credential missing).", False),
        ("I made no changes yet. The plan needs vetting before Phase 2 can start.", False),
        ("Nothing to do until CI on #2535 goes green; then it can be merged.", False),
        ("Nothing to fix in the parser. The bug is in the runner; it remains open.", False),
        ("No follow-up PR is needed for #1550, but #1551 still needs one for the schema bump.", False),
        ("The UI is fully wired, but the backend route is missing, so the button 404s.", False),
        ("Nothing is owed by this session; the operator must run the migration before Phase 3.", False),
        ("No follow-up PR was needed. Pre-existing CI failure on main blocks the merge train.", False),
        ("PR #12 needs one more change. No follow-up PR is needed for #11.", False),
        ("I found nothing to follow up on in qontinui-web.\n- **Needs a decision:** #1922 is still open.", False),
        ("Nothing needs a follow-up PR from me.\n- **Open item for the operator:** run Phase 3a soon.", False),
        ("No follow-up PR is needed.\n\nTell me if you want me to dig into the shim.", False),
        ("No follow-up PR is needed for #1550. I'll check the deploy once the train lands #1551.\n"
         + "- evidence line.\n" * 120, False),  # the veto sits early in a long message
        ("#2781 has landed, and the closing step on #2535 is done.", False),  # no recognised phrase: nudged
        # unfinished work named after the headline (review round 2)
        ("No follow-up PR is needed for #1550. Phase 3 of the plan is next and has not started.", False),
        ("No follow-up PR is needed for #1550; the remaining work is Phase 3, which I have not started.", False),
        ("Nothing is owed on #12. #13 is open and unstarted.", False),
        ("No follow-up PR is needed for the docs. The runner change is unimplemented; I ran out of context.", False),
        ("No follow-up PR is needed for #1550. CI on #1551 is red.", False),
        ("No follow-up PR is needed for #1550. Phase 2 is deferred to a later session.", False),
    ]
    for text, want in complete_cases:
        t = dict(tx, final_text=text, last_prompt_text="PR x just merged. Carry out the follow-up")
        if bool(completion("ACTIVE", t)) != want:
            bad.append(f"completion({text[:50]!r}) != {want}")
    t = dict(tx, final_text=complete_cases[0][0], last_prompt_text="PR x just merged")
    if judge("ACTIVE", [], none, t, now, 24)[0] != "COMPLETE":
        bad.append("judge(complete report) != COMPLETE")
    if judge("ACTIVE", [], none, t, now, 24, include_complete=True)[0] != "STOPPED":
        bad.append("judge(complete report, include_complete) != STOPPED")
    if judge("ACTIVE", [], none, dict(t, final_text_at=t["last_prompt"] - 1000), now, 24)[0] != "STOPPED":
        bad.append("a report older than the last prompt read as COMPLETE")
    if judge("LIMITED", [], none, dict(t, final_text="You've hit your session limit"), now, 24)[0] == "COMPLETE":
        bad.append("a LIMITED row read as COMPLETE")
    # transcript_facts: built-in commands Claude Code answers itself are not tasks.
    import tempfile

    def user(text, **kw):
        return dict({"type": "user", "message": {"content": text}}, **kw)

    def asst(*blocks):
        return {"type": "assistant", "message": {"content": list(blocks)}}

    caveat = user("<local-command-caveat>Caveat: the command below ran locally</local-command-caveat>")
    model = user("<command-name>/model</command-name>\n<command-message>model</command-message>\n"
                 "<command-args></command-args>")
    stdout = user("<local-command-stdout>Set model to Opus</local-command-stdout>")
    stderr = user([{"type": "text", "text": "<local-command-stderr>no such model</local-command-stderr>"}])
    clear = user("<command-name>/clear</command-name>\n<command-message>clear</command-message>")
    compact = user("summary of the earlier conversation", isCompactSummary=True)
    skill = user("<command-message>pvi</command-message>\n<command-name>/pvi</command-name>")
    first, second = user("fix the first bug"), user("fix the second bug")
    report = asst({"type": "text", "text": "No follow-up PR is needed for #1, so I opened none."})
    tool_only = asst({"type": "tool_use", "id": "t1", "name": "Bash", "input": {}})
    transcript_cases = [  # (records, task_prompts, last prompt text, index of the last prompt or None)
        ([caveat, model, stdout], 0, "", None),
        ([clear], 0, "<command-name>/clear", 0),  # moves last_prompt (a revival for close), not a task
        ([first, report, caveat, model, stdout], 1, "fix the first bug", 0),  # a built-in after the work
        ([skill, report], 1, "<command-message>pvi", 0),                       # a skill is a task
        ([first, report, model, stdout, second], 2, "fix the second bug", 4),
        ([first, report, model, compact, stdout], 1, "fix the first bug", 0),  # summary between
        ([first, report, model, stderr], 1, "fix the first bug", 0),           # stderr, list content
        ([skill, stdout], 1, "<command-message>pvi", 0),  # a stdout after a skill does not revert it
    ]
    for k, (records, want_tasks, want_last, want_at) in enumerate(transcript_cases):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "projects", "p"))
            with open(os.path.join(d, "projects", "p", "s.jsonl"), "w") as f:
                for i, e in enumerate(records):
                    f.write(json.dumps(dict(e, timestamp=f"2026-10-18T10:00:{i:02d}Z")) + "\n")
            got = cfs.transcript_facts({"configDir": d, "claudeSessionId": "s"})
        at = None if want_at is None else cfs.ms(f"2026-10-18T10:00:{want_at:02d}Z")
        if (got["task_prompts"], got["last_prompt"]) != (want_tasks, at) or \
                not got["last_prompt_text"].startswith(want_last) or (not want_last and got["last_prompt_text"]):
            bad.append(f"transcript case {k}: task_prompts={got['task_prompts']} last_prompt={got['last_prompt']} "
                       f"text={got['last_prompt_text'][:20]!r}; want {want_tasks} / {at} / {want_last!r}")
        if want_tasks == 0 and judge("ACTIVE", [], none, got, now, 24)[0] != "NO-TASK":
            bad.append(f"transcript case {k}: judge != NO-TASK")
    # A report followed by a new task whose turn made only tool calls is stale.
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "projects", "p"))
        with open(os.path.join(d, "projects", "p", "s.jsonl"), "w") as f:
            for i, e in enumerate([first, report, second, tool_only]):
                f.write(json.dumps(dict(e, timestamp=f"2026-10-18T10:00:{i:02d}Z")) + "\n")
        got = cfs.transcript_facts({"configDir": d, "claudeSessionId": "s"})
    if completion("ACTIVE", got):
        bad.append("a report written before the newest task read as COMPLETE")
    # Read-back: the command record is followed by a meta record far larger than
    # any tail window (the live /next-move send of 2026-10-18 read SENT? this way).
    with tempfile.TemporaryDirectory() as d:
        tp = os.path.join(d, "t.jsonl")
        with open(tp, "w") as f:
            f.write(json.dumps(dict(first, timestamp="2026-10-18T10:00:00Z")) + "\n")
        off = offset_of(tp)
        t0 = cfs.ms("2026-10-18T10:00:01Z")
        with open(tp, "a") as f:
            f.write(json.dumps(user(ran, timestamp="2026-10-18T10:00:02Z")) + "\n")
            f.write(json.dumps(user([{"type": "text", "text": "x" * 600000}], isMeta=True,
                                    timestamp="2026-10-18T10:00:02Z")) + "\n")
            f.write(json.dumps(dict(tool_only, timestamp="2026-10-18T10:00:05Z")) + "\n")
        if not ran_since(tp, off, t0):
            bad.append("read-back missed a /next-move command followed by a large meta record")
        with open(tp, "w") as f:
            f.write(json.dumps(user("/next-move", timestamp="2026-10-18T10:00:02Z")) + "\n")
        if ran_since(tp, 0, t0):
            bad.append("read-back accepted plain /next-move text as a command that ran")
        if ran_since(tp, 0, t0 + 5000):
            bad.append("read-back accepted a record older than the send")
        with open(tp, "w") as f:
            f.write(json.dumps(user("<task-notification>agent summary: ran <command-name>/next-move</command-name>"
                                    "</task-notification>", timestamp="2026-10-18T10:00:02Z")) + "\n")
        if ran_since(tp, 0, t0):
            bad.append("read-back accepted a notification quoting the command")
        if ran_since(None, None, t0):
            bad.append("read-back with no transcript path verified")
    # nudge_one's last look at the prompt replays the pane at the width the close
    # screen uses (wrap_cols), or a Windows pane reads as a draft and never sends.
    seen = []
    stubs = {"terminals": lambda: [{"id": "t1", "cols": 120}], "readiness": lambda: ({}, None),
             "open_rows": lambda: {"t1": {"claudeSessionId": "s"}},
             "classify": lambda *a: ("ACTIVE", []), "proc_table": lambda: {},
             "transcript_facts": lambda row: dict(work, last_prompt=time.time() * 1000 - 3600000),
             "output_moving": lambda tid: False, "wrap_cols": lambda term: ("cols", term["cols"]),
             "prompt_state": lambda raw, cols=None: seen.append((raw, cols)) or ("text", "draft"),
             "API": "http://127.0.0.1:9"}  # a regression past the check must fail here, never send live
    saved = {k: getattr(cfs, k) for k in list(stubs) + ["raw_tail"]}
    try:
        for k, v in stubs.items():
            setattr(cfs, k, v)
        for raw, want_seen, want_msg in [("screen", [("screen", ("cols", 120))], "(text: 'draft')"),
                                         (None, [], "(unknown: 'screen unreadable')")]:
            seen.clear()
            cfs.raw_tail = lambda tid, raw=raw: raw
            got = nudge_one("t1", 15, 24, set(), False)
            if got[:2] != (False, False) or want_msg not in got[2] or seen != want_seen:
                bad.append(f"nudge_one pre-send check (raw={raw!r}) = {got}, prompt_state saw {seen}")
    finally:
        for k, v in saved.items():
            setattr(cfs, k, v)
    for b in bad:
        print("FAIL: " + b)
    print(f"selftest: {len(cases)} reset cases, 7 prompt cases, {len(judge_cases)} judge cases, "
          f"{len(complete_cases)} completion cases, {len(transcript_cases) + 1} transcript cases, "
          f"2 send cases, {len(bad)} failure(s)")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--send", action="store_true", help="prompt /next-move into every STOPPED row")
    ap.add_argument("--cooldown-hours", type=float, default=24.0,
                    help="hold a row whose last prompt was a /next-move newer than this (default 24)")
    ap.add_argument("--include-complete", action="store_true",
                    help="also nudge COMPLETE rows, so /next-move can verify and mark them finished")
    ap.add_argument("--idle-min", type=int, default=15)
    ap.add_argument("--max", type=int, default=0, help="sends to stop after, verified or not (default: no cap)")
    ap.add_argument("--only", default="")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    ap.add_argument("--selftest", action="store_true", help="run the rule table and exit; reads nothing")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    if a.port:
        cfs.API = f"http://127.0.0.1:{a.port}"
    only = [o for o in a.only.split(",") if o]
    try:
        res, warnings, ready_err = screen(a.idle_min, only, a.cooldown_hours, a.include_complete)
    except cfs.READ_ERRORS as e:
        print(f"UNKNOWN: could not read the runner at {cfs.API}: {e}", file=sys.stderr)
        return 3
    if a.json:
        print(json.dumps({"warnings": warnings, "sessions": res}, indent=1))
    else:
        print_report(res, warnings)
    if not a.send:
        return 0
    if ready_err:
        print("\nUNKNOWN: --send refused - GET /restart-readiness could not be read", file=sys.stderr)
        return 3
    targets = [r for r in res if r["verdict"] == "STOPPED"]
    print(f"\nprompting {PROMPT} into up to {a.max or len(targets)} of {len(targets)} terminal(s)")
    own = os.environ.get("QONTINUI_TERMINAL_ID", "")
    failed = sends = 0
    for r in targets:
        if a.max and sends >= a.max:
            break
        try:
            ok, sent, msg = nudge_one(r["terminalId"], a.idle_min, a.cooldown_hours, own, a.include_complete)
        except cfs.READ_ERRORS as e:
            ok, sent, msg = False, False, f"UNKNOWN before the send, not sent: {e}"
        sends += sent  # a SENT? row may have started a turn too, so it uses up the cap
        failed += not ok
        label = "NUDGED " if ok else "SENT?  " if sent else "SKIPPED"
        print(f"  {label} {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['account']:<13.13}  {r['name'][:50]} - {msg}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
