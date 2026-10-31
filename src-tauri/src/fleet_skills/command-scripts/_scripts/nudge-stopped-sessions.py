#!/usr/bin/env python3
"""Find the runner's terminal sessions that stopped before finishing; prompt
/next-move into them.

Usage: nudge-stopped-sessions.py [--send] [--cooldown-hours H] [--idle-min N]
                                 [--max N] [--only ID[,ID..]] [--json] [--port N]
                                 [--no-coord] [--no-gh] [--rows-out PATH]

A STOPPED session is idle at an empty prompt (not working) and not finished.
The screen is close-finished-sessions.py's, imported from beside this file, so
both overlord commands judge "busy" and "empty prompt" the same way. Of its
verdicts, ACTIVE, REVIVED and FOOTER-ONLY are stopped, and so is a LIMITED
session whose usage-limit reset time has passed. That screen leans to "not
finished" when unsure, which is the safe side for a close and the unsafe side
for a nudge, so this script adds its own holds: any finished signal (the
runner marker, or coord session_status finished, unless a prompt came after
every finish time it has, so the newer of the two decides; a coord finish
with no time holds), coord session_status waiting_human, and a last prompt
that was a /next-move that changed nothing.

A row whose last prompt is /next-move is held by what changed, not by a
clock. A /next-move followed by a work-class tool call (an edit, a commit, a
push, a PR write) made progress and is nudged again when it next stops. One
that only classified is futile: held 4 h after the first such nudge, 24 h
after the second, and read STUCK (reported, never nudged) at the third. An open
gate /next-move recorded on its intervention row holds the row until it closes;
a class-B row is held for the operator, a class-E row is a close candidate. A
pull request named in the final message that changed after the nudge releases a
futile hold. --cooldown-hours is the backstop: no futile or class-B hold
outlasts it. Coord is read through the coord-revive skill's `call` verb (the
session's own .mcp.json nonce, nothing minted) and pull requests through `gh`;
an unreadable read leaves the transcript verdict standing, annotated UNKNOWN.
A session finished at a known time, with no prompt since, whose final message
says work remains reads CONTRADICTED: reported, never nudged or closed.

The report is always printed. --send then types `/next-move` into each STOPPED
row through POST /terminals/{id}/submit-prompt, re-screening the row
immediately before (a 3-second output sample, then one last empty-prompt check
on the final screen) and reading the slash command back from the transcript.
Read-only unless --send. Stdlib, plus bash and the coord-revive skill for the
coord reads and `gh` for the PR reads. The command /nudge-stopped-sessions
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
import subprocess
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
from envelope import EnvelopeUnknown, unwrap_mcp_text  # noqa: E402  (cfs put lib/ on sys.path)
try:
    from git_scope import git_scope_env  # noqa: E402  (cfs put lib/ on sys.path)
except ImportError:  # the command-scripts carrier ships lib/envelope.py only: strip the core three
    def git_scope_env(base=None):
        src = os.environ if base is None else base
        return {k: v for k, v in src.items() if k not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR")}

PROMPT = "/next-move"
STOPPED_FROM = {"ACTIVE", "REVIVED", "FOOTER-ONLY"}
# /next-move as the transcript records a slash command that RAN. Only this form
# verifies a send: pasted text that never ran as a command is a plain message.
NEXT_MOVE_RAN = re.compile(r"<command-name>/next-move</command-name>")
# Either form, for the nudge holds, where reading too much as a nudge is the safe side.
# One definition, shared with the transcript's segment counts.
NEXT_MOVE_ANY = cfs.NEXT_MOVE_PROMPT
# The backstop: no hold that waits on nothing outlasts it (--cooldown-hours).
BACKSTOP_H = 48.0
# The futile-nudge holds: after the 1st, after the 2nd. The 3rd reads STUCK.
BACKOFF_H = (4.0, 24.0)
STUCK_STRIKES = len(BACKOFF_H) + 1
# Pull request references in a final message: owner/repo#N, repo#N (the session
# repo's owner) or a bare #N (the session repo). A #N after a word naming a
# lint check, step or phase is not a PR.
PR_REF_RE = re.compile(r"(?<![\w/#.-])(?:([A-Za-z0-9_.-]+/)?([A-Za-z0-9_.-]*[A-Za-z][A-Za-z0-9_.-]*))?#(\d{1,6})\b")
NOT_PR_WORD = re.compile(r"\b(?:checks?|lints?|steps?|phases?|rules?|items?|occurrences?|cases?|rows?|issues?|no\.?)"
                         r"(?:\s+\d+[a-z]?)?\s*$", re.I)
# A file name followed by an anchor (README.md#3) is not a repo.
FILE_NAME = re.compile(r"\.(?:md|py|rs|ts|tsx|js|json|ya?ml|toml|txt|html?|sh|ps1)$", re.I)
MAX_PRS = 5
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
# Above coord-revive.sh's own 60 s call budget, so the door answers first.
DOOR_TIMEOUT_S = 75
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


def remains_phrase(tx):
    """The first "work remains" label in the final message, or None: the
    screen's NEGATIVE phrases and phrase_hits' negatives, over the whole text."""
    text = (tx or {}).get("final_text") or ""
    if not text.strip():
        return None
    norm = cfs.normalize(text)
    hit = next((label for label, r in cfs.NEG_RE if r.search(norm)), None)
    return hit or next(iter(cfs.phrase_hits(text)[1]), None)


def finished_hold(base, tx, marker_ms, now_ms, backstop_h):
    """A finished signal holds the row. When it has a finish time (`marker_ms`,
    at or after the last prompt) and the final message says work remains, the
    two signals disagree: CONTRADICTED, for the operator. With no finish time
    there is nothing to weigh the message against, so the row is only held."""
    phrase = remains_phrase(tx) if marker_ms else None
    if phrase:
        return "CONTRADICTED", [f"{base}, but the final message says work remains ({phrase})"]
    if marker_ms and (now_ms - marker_ms) / 3600000 >= backstop_h:
        base += f"; finished marker stale ({cfs.fmt_age(marker_ms)})"
    return "HELD", [base]


def nudge_hold(tx, now_ms, backstop_h, ext):
    """(verdict, reason) for a row whose last prompt is /next-move, or None
    when the row should be screened as usual (progress, or a hold that ran
    out). `ext` is external_signals()'s dict, or None when not read."""
    last = tx["last_prompt"]
    age_h = (now_ms - last) / 3600000
    unknown = []
    if ext is not None:
        unknown = list(ext.get("unknown") or [])
        if ext.get("open_gate"):
            gid, kind = ext["open_gate"]
            return "HELD", f"awaiting gate {gid} ({kind}), recorded by the last /next-move"
        cls = ext.get("stop_class")
        if cls == "E":
            return "HELD", (f"last /next-move found nothing owed (class E, intervention "
                            f"{ext.get('intervention_id', '?')[:8]}): close candidate, not nudged")
        if cls == "B" and age_h < backstop_h:
            return "HELD", (f"operator decision pending (class B, intervention "
                            f"{ext.get('intervention_id', '?')[:8]}); backstop {backstop_h:g}h")
    note = f"; {'; '.join(unknown)}" if unknown else ""
    work = tx.get("work_tools_since_nudge") or 0
    if work:
        return None, f"last /next-move made progress ({work} work tool call(s)){note}"
    streak = max(tx.get("nudge_streak") or 0, 1)
    if streak >= STUCK_STRIKES:
        return "STUCK", (f"{streak} /next-move nudges in a row made no progress; "
                         f"for the operator, not nudged again{note}")
    hold = min(BACKOFF_H[streak - 1], backstop_h)
    closed = (ext or {}).get("gate_closed")
    if closed:
        return None, (f"gate {closed[0]} ({closed[1]}) recorded by the last /next-move went {closed[2]} "
                      f"(strike {streak}){note}")
    released = (ext or {}).get("pr_changed")
    if released:
        return None, f"PR {released} changed since the last /next-move (strike {streak}){note}"
    if age_h < hold:
        return "HELD", (f"last /next-move made no progress (strike {streak}, {cfs.fmt_age(last)} ago, "
                        f"held {hold:g}h){note}")
    return None, f"no-progress hold expired (strike {streak}, held {hold:g}h){note}"


def judge(verdict, reasons, facts, tx, now_ms, cooldown_h, include_complete=False, ext=None):
    """(nudge verdict, reasons) for one screened row: STOPPED, a row that needs
    no /next-move (HELD, NO-TASK, COMPLETE, STUCK, CONTRADICTED), or the
    screen's own verdict when the row is not a stopped session at all.

    `facts` carries the registry's `finished` / `finishedAt` and coord's
    `sessionStatus` / `sessionStatusSince`. A nudge into a finished session
    reopens it (/next-move reads an owed item it cannot prove as owed), so every
    finished signal holds the row, except one that a later real prompt revived.
    Where the runner serves coord's finish time (status_since; it moves on a
    re-finish once coord restamps a finished->finished report, plan Phase 2,
    and the runner passes the time through, plan Phase 3, while finishedAt
    stays where it was on a runner build predating plan Phase 3), the row is
    held on the NEWER of that and the runner
    marker's: it is revived only by a prompt more than 1 s after every finish
    stamp it has. close-finished-sessions prefers coord's time instead; nudge
    holds on the newer stamp. Without a usable coord time, coord's finished
    status holds even a revived runner marker, since coord is then the only
    sign it finished again (on a runner build predating plan Phase 3, or for a
    re-finish made through coord_report_status only).
    `cooldown_h` is the backstop (None: BACKSTOP_H); `ext` is the coord/PR
    signals for a row whose last prompt is /next-move (None: not read)."""
    backstop_h = BACKSTOP_H if cooldown_h is None else cooldown_h
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
    since = cfs.status_since(facts, now_ms) if status == "finished" else None
    # With coord's time, hold on the NEWER of the two finish stamps: a nudge
    # into a finished session reopens it, so a row is revived only when every
    # stamp it has is older than its last prompt (close prefers coord's instead).
    marked = facts.get("finishedAt") if facts.get("finished") else None
    marked = marked if isinstance(marked, (int, float)) and not isinstance(marked, bool) else 0
    if since is not None and last_prompt <= max(since, marked) + 1000:
        src = f"coord session_status=finished ({cfs.fmt_age(since)} ago)" if since >= marked else \
            f"runner finished marker ({cfs.fmt_age(marked)} ago, newer than coord's {cfs.fmt_age(since)} ago)"
        v, why = finished_hold(f"{src}, no prompt since", tx, max(since, marked), now_ms, backstop_h)
        return v, why + reasons
    if facts.get("finished") and since is None:
        fa = facts.get("finishedAt") or 0
        if not last_prompt or last_prompt <= fa + 1000:
            v, why = finished_hold(f"runner finished marker ({cfs.fmt_age(fa) if fa else '?'} ago), "
                                   "no prompt since", tx, fa, now_ms, backstop_h)
            return v, why + reasons
    if status == "finished" and since is None:
        v, why = finished_hold("coord session_status=finished (no finish time to compare a revival against)",
                               tx, None, now_ms, backstop_h)
        return v, why + reasons
    # A /next-move answered by a usage-limit notice never ran, so it holds nothing.
    if tx and last_prompt and not limited and NEXT_MOVE_ANY.search(tx["last_prompt_text"].strip()):
        v, why = nudge_hold(tx, now_ms, backstop_h, ext)
        if v:
            return v, [why] + reasons
        reasons = [why] + reasons
    done = completion(verdict, tx)
    if done:
        if include_complete:
            return "STOPPED", [f"complete but not marked finished ({done!r}); nudged to record it"] + reasons
        return "COMPLETE", [f"final report says the work is done, nothing owed: {done!r}; "
                            "not marked finished"] + reasons
    return "STOPPED", reasons


# ---------------------------------------------------------------- coord and PR signals
class SignalUnknown(Exception):
    """A coord or gh read that could not be made or not be read. `door_verdict`
    is coord-revive.sh's own whole `coord-revive: <verb> -> <VERDICT> ...` line
    when the door printed one (None otherwise), so a caller can tell a call the
    door never carried from one it may have, without the 80-character cut."""

    def __init__(self, message, door_verdict=None):
        super().__init__(message)
        self.door_verdict = door_verdict


def _bash():
    """bash to spawn: `bash` off Windows; on Windows Git for Windows' bin\\bash.exe
    (its usr\\bin\\bash.exe starts with PATH set to System32 alone, where `bash`
    is WSL's launcher), found beside `git` on PATH or under Program Files.
    None when no Git Bash is found: never System32's or WindowsApps' launcher.
    These are the two common arms of the repository's full bash resolver, which
    the command-scripts skill does not carry."""
    if sys.platform != "win32":
        return "bash"
    import shutil
    roots = []
    git = shutil.which("git")
    if git:
        d = os.path.dirname(os.path.realpath(git))
        roots += [os.path.dirname(d), os.path.dirname(os.path.dirname(d))]
    roots += [os.path.join(r, "Git") for r in (os.environ.get("ProgramFiles"), os.environ.get("ProgramW6432"),
                                                r"C:\Program Files") if r]
    if os.environ.get("LOCALAPPDATA"):
        roots.append(os.path.join(os.environ["LOCALAPPDATA"], "Programs", "Git"))
    for root in roots:
        c = os.path.join(root, "bin", "bash.exe")
        # Only a Git for Windows tree: it carries cmd\git.exe beside bin\bash.exe.
        if os.path.isfile(c) and os.path.isfile(os.path.join(root, "cmd", "git.exe")):
            return c
    return None


def coord_door():
    """argv prefix for `coord-revive.sh call`, found beside this script: from
    the provisioned skills tree (_scripts/../../coord-revive/) or from the
    repository (scripts/../.claude/skills/coord-revive/). $NSS_COORD_DOOR names
    an executable taking the same `call <tool> <json>` argv instead. None when
    neither exists."""
    override = os.environ.get("NSS_COORD_DOOR")
    if override:
        return [override]
    for rel in (("..", "..", "coord-revive"), ("..", ".claude", "skills", "coord-revive")):
        c = os.path.normpath(os.path.join(_here, *rel, "coord-revive.sh"))
        if os.path.isfile(c):
            bash = _bash()
            return [bash, c.replace("\\", "/")] if bash else None
    return None


def coord_call(tool, args):
    """One coord MCP tool over the session's own .mcp.json nonce. The body,
    or SignalUnknown naming why it could not be read."""
    door = coord_door()
    if door is None:
        raise SignalUnknown("no coord-revive.sh beside this script, or no Git Bash to run it")
    payload = json.dumps(args, separators=(",", ":"))
    env = None
    if sys.platform == "win32" and len(door) == 2:
        # msys bash drops an argv entry holding double quotes, so pass the JSON in the environment.
        env = {**os.environ, "NSS_TOOL_ARGS": payload}
        cmd = [door[0], "-c", 'exec "$BASH" "$0" call "$1" "$NSS_TOOL_ARGS"', door[1], tool]
    else:
        cmd = door + ["call", tool, payload]
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace",
                           timeout=DOOR_TIMEOUT_S, env=env)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise SignalUnknown(f"{tool}: {type(e).__name__}") from None
    if p.returncode != 0:
        tail = ((p.stderr or p.stdout).strip().splitlines() or ["no output"])[-1]
        verdicts = [ln.strip() for ln in (p.stderr or "").splitlines() if ln.startswith("coord-revive: ")]
        raise SignalUnknown(f"{tool}: door exit {p.returncode} ({tail[:80]})", verdicts[-1] if verdicts else None)
    try:
        body = unwrap_mcp_text(json.loads(p.stdout), door=tool)
    except (ValueError, EnvelopeUnknown) as e:
        raise SignalUnknown(f"{tool}: {str(e)[:100]}") from None
    if not isinstance(body, dict):
        raise SignalUnknown(f"{tool}: body is not an object")
    return body


def regrade_gate_ids(evidence):
    """The ids of the re-grade gate alone (a 4h timer, not a wait)."""
    regrade = (evidence or {}).get("regrade")
    f = regrade.get("gate_id") if isinstance(regrade, dict) else None
    return [g for g in re.split(r"[\s,]+", f.strip().lower()) if g] if isinstance(f, str) else []


def recorded_gate_ids(evidence):
    """The gate ids /next-move recorded on an intervention row: its re-grade
    gate and its wait gates. A field may hold several ids joined by commas."""
    ids = []
    regrade = (evidence or {}).get("regrade")
    wait = (evidence or {}).get("wait_gates")
    fields = [regrade.get("gate_id")] if isinstance(regrade, dict) else []
    fields += [w.get("gate_id") for w in wait if isinstance(w, dict)] if isinstance(wait, list) else []
    for f in fields:
        if isinstance(f, str):
            ids += [g for g in re.split(r"[\s,]+", f.strip().lower()) if g]
    return list(dict.fromkeys(ids))


def gate_signals(session_id, nudged_ms, ext):
    """Fill `ext` from the newest intervention row for this Claude session:
    its stop class, and the first of its recorded gates still open. Only a row
    observed at or after the last /next-move counts; an older one says nothing
    about that nudge."""
    body = coord_call("coord_overlord_interventions", {"claude_code_session_id": session_id, "limit": 1})
    rows = body.get("interventions")
    if not isinstance(rows, list):
        raise SignalUnknown("coord_overlord_interventions: no interventions list")
    if not rows:
        ext["unknown"].append("no intervention row for this session in this tenant")
        return
    row = rows[0]
    try:
        observed = cfs.ms(row.get("observed_at") or row.get("recorded_at") or "")
    except ValueError:
        raise SignalUnknown("coord_overlord_interventions: unreadable observed_at") from None
    if observed < nudged_ms - 60000:
        ext["unknown"].append("the newest intervention row predates the last /next-move")
        return
    ext["stop_class"] = row.get("stop_class")
    ext["intervention_id"] = str(row.get("intervention_id") or row.get("id") or "")
    regrades = set(regrade_gate_ids(row.get("evidence")))
    for gid in recorded_gate_ids(row.get("evidence")):
        if not UUID_RE.match(gid):
            ext["unknown"].append(f"gates UNKNOWN (gate {gid[:8]} recorded without its full id)")
            continue
        gates = coord_call("coord_gate_list", {"gate_id": gid, "open_only": False, "include_archived": True})
        found = gates.get("gates")
        if not isinstance(found, list):
            raise SignalUnknown("coord_gate_list: no gates list")
        if not found:
            ext["unknown"].append(f"gates UNKNOWN (gate {gid[:8]} not found in this tenant)")
            continue
        g = found[0]
        if g.get("verdict") == "open" and g.get("muted"):
            ext["unknown"].append(f"gate {gid[:8]} is open but muted, so it holds nothing")
        elif g.get("verdict") == "open":
            ext["open_gate"] = (gid[:8], g.get("predicate_kind") or "?")
            return
        else:
            try:
                closed = cfs.ms(g.get("cleared_at") or "")
            except ValueError:
                closed = None
            # The re-grade gate clears on its 4h timer: that is a clock, not a change.
            if closed and closed > nudged_ms and not ext.get("gate_closed") and gid not in regrades:
                ext["gate_closed"] = (gid[:8], g.get("predicate_kind") or "?", g.get("verdict"))


def session_repo(cwd):
    """owner/name of the repository the session works in, or None."""
    if not cwd:
        return None
    try:
        url = subprocess.run(["git", "-C", cwd, "remote", "get-url", "origin"], capture_output=True,
                             text=True, timeout=20, env=git_scope_env()).stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        return None
    m = re.search(r"[:/]([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+?)(?:\.git)?/?$", url)
    return m.group(1) if m else None


def pr_refs(text, repo):
    """Up to MAX_PRS owner/repo#N references named in `text`, in order. `repo`
    is the session's owner/name (or a callable returning it, read only when a
    reference needs it); a reference it cannot resolve is dropped."""
    refs = []
    text = text or ""
    for m in PR_REF_RE.finditer(text):
        owner, name, n = m.group(1), m.group(2), m.group(3)
        if not name and NOT_PR_WORD.search(text[max(0, m.start() - 20):m.start()]):
            continue
        if name and FILE_NAME.search(name):
            continue
        if owner:
            full = f"{owner}{name}"
        else:
            if callable(repo):
                repo = repo()
            if not repo:
                continue
            full = f"{repo.split('/')[0]}/{name}" if name else repo
        ref = f"{full}#{n}"
        if ref not in refs:
            refs.append(ref)
        if len(refs) == MAX_PRS:
            break
    return refs


def pr_moved_at(pr):
    """Epoch ms of the newest change that could unblock a waiting session: the
    PR merged or closed, or a commit landed on its head. Comments, labels and
    reviews move `updatedAt` too, so it is not read. None when none is known."""
    times = [pr.get("mergedAt"), pr.get("closedAt")]
    commits = pr.get("commits")
    if isinstance(commits, list) and commits and isinstance(commits[-1], dict):
        times.append(commits[-1].get("committedDate"))
    known = [cfs.ms(t) for t in times if isinstance(t, str) and t]
    return max(known) if known else None


def pr_signals(tx, since_ms, ext):
    """Set ext["pr_changed"] to the first referenced PR that merged, closed or
    got a new commit after `since_ms` (the end of the /next-move turn, so the
    turn's own pushes do not count). A gh failure is UNKNOWN for that PR and
    releases nothing. $NSS_GH names the gh executable (tests use a fake)."""
    gh = os.environ.get("NSS_GH") or "gh"
    for ref in pr_refs(tx.get("final_text"), lambda: session_repo(tx.get("cwd"))):
        repo, n = ref.rsplit("#", 1)
        try:
            p = subprocess.run([gh, "pr", "view", n, "--repo", repo, "--json", "state,mergedAt,closedAt,commits"],
                               capture_output=True, text=True, timeout=30)
            moved = pr_moved_at(json.loads(p.stdout)) if p.returncode == 0 else False
        except (OSError, subprocess.TimeoutExpired, ValueError, KeyError, TypeError, AttributeError):
            moved = False
        if moved is False:
            ext["unknown"].append(f"PR state UNKNOWN ({ref})")
        elif moved and moved > since_ms:
            ext["pr_changed"] = ref
            return


def external_signals(tx, row, use_coord=True, use_gh=True):
    """The coord and PR signals for a row whose last prompt is /next-move, as
    the dict judge() reads; None for any other row. Every read that fails is
    named in ext["unknown"] and never stands in for "no gate" or "no change"."""
    if not tx or not tx.get("last_prompt") or not NEXT_MOVE_ANY.search((tx.get("last_prompt_text") or "").strip()):
        return None
    ext = {"unknown": []}
    sid = (row or {}).get("claudeSessionId")
    if use_coord and sid:
        try:
            gate_signals(sid, tx["last_prompt"], ext)
        except SignalUnknown as e:
            ext["unknown"].append(f"gates UNKNOWN ({e})")
    elif use_coord:
        ext["unknown"].append("gates UNKNOWN (no Claude session id)")
    else:
        ext["unknown"].append("gates UNKNOWN (--no-coord)")
    if use_gh and not ext.get("open_gate") and not (tx.get("work_tools_since_nudge") or 0):
        pr_signals(tx, max(tx["last_prompt"], tx.get("last_asst") or 0), ext)
    return ext


def row_facts(row, ready):
    row = row or {}
    rp = ready.get(row.get("claudeSessionId")) or {}
    return {"finished": bool(row.get("finished")), "finishedAt": row.get("finishedAt"),
            "sessionStatus": rp.get("sessionStatus"), "sessionStatusSince": rp.get("sessionStatusSince")}


# ---------------------------------------------------------------- screening
def screen(idle_min, only, cooldown_h, include_complete, use_coord=True, use_gh=True):
    res, warnings, ready_err = cfs.screen_all(idle_min, only)
    rows = cfs.open_rows()
    now = time.time() * 1000
    for r in res:
        row = rows.get(r["terminalId"])
        tx = cfs.transcript_facts(row) if row and r["verdict"] in STOPPED_FROM | {"LIMITED"} else None
        facts = dict(row_facts(row, {}), sessionStatus=r.get("sessionStatus"),
                     sessionStatusSince=r.get("sessionStatusSince"))
        ext = external_signals(tx, row, use_coord, use_gh) if r["verdict"] in STOPPED_FROM else None
        r["screen"] = r["verdict"]
        r["verdict"], r["reasons"] = judge(r["verdict"], r["reasons"], facts, tx, now, cooldown_h,
                                           include_complete, ext)
    return res, warnings, ready_err


ORDER = ["STOPPED", "COMPLETE", "NO-TASK", "HELD", "STUCK", "CONTRADICTED", "LIMITED", "BUSY", "FINISHED", "CLOSEOUT", "EXITED-FINISHED", "EXITED", "UNKNOWN",
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


def nudge_one(tid, idle_min, cooldown_h, own, include_complete, use_coord=True, use_gh=True):
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
    ext = external_signals(tx, row, use_coord, use_gh) if verdict in STOPPED_FROM else None
    verdict, why = judge(verdict, why, row_facts(row, ready), tx, time.time() * 1000, cooldown_h,
                         include_complete, ext)
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
    import tempfile
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
                                     ("/next-move 3a6ed95f --regrade wait-land-x", False, True),
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
        ("ACTIVE", none, tx, "HELD"),  # nudged 1h ago, no progress: the 4h hold
        ("ACTIVE", none, dict(tx, last_prompt=now - 25 * 3600000), "STOPPED"),  # past the 4h hold
        # Finished signals the close screen lets through as ACTIVE / FOOTER-ONLY / REVIVED:
        ("ACTIVE", {"finished": True, "finishedAt": now - 1800000}, work, "HELD"),  # veto phrase after the mark
        ("FOOTER-ONLY", {"sessionStatus": "finished"}, work, "HELD"),
        ("REVIVED", {"sessionStatus": "finished"}, work, "HELD"),
        # kept working after its mark with no new prompt: still finished
        ("REVIVED", {"finished": True, "finishedAt": now - 1800000}, dict(work, last_asst=now), "HELD"),
        # ...and the one real revival: a prompt after the runner mark,
        ("REVIVED", {"finished": True, "finishedAt": now - 7200000}, work, "STOPPED"),
        # unless coord says it finished again (finishedAt does not move on a re-finish
        # on a runner build predating plan Phase 3).
        ("REVIVED", {"finished": True, "finishedAt": now - 7200000, "sessionStatus": "finished"}, work, "HELD"),
        # a /next-move the limit answered never ran: no hold once the limit resets
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
    # The /next-move holds: progress, futility, strike backoff, the backstop,
    # STUCK, and the coord / PR signals that hold or release them.
    h = 3600000
    fut = dict(tx, work_tools_since_nudge=0, nudge_streak=1)
    prog = dict(tx, work_tools_since_nudge=2, nudge_streak=0)
    hold_cases = [  # (tx, ext, cooldown, want, reason fragment)
        (prog, None, 48, "STOPPED", "made progress"),
        (fut, None, 48, "HELD", "no progress (strike 1"),
        (dict(fut, last_prompt=now - 5 * h), None, 48, "STOPPED", "hold expired (strike 1"),
        (dict(fut, nudge_streak=2, last_prompt=now - 5 * h), None, 48, "HELD", "strike 2"),
        (dict(fut, nudge_streak=2, last_prompt=now - 25 * h), None, 48, "STOPPED", "hold expired (strike 2"),
        (dict(fut, nudge_streak=2, last_prompt=now - 5 * h), None, 3, "STOPPED", "held 3h"),  # backstop caps it
        (dict(fut, last_prompt=now - 49 * h), None, 48, "STOPPED", "hold expired"),
        (dict(fut, nudge_streak=3), None, 48, "STUCK", "3 /next-move nudges"),
        (dict(fut, nudge_streak=3, last_prompt=now - 99 * h), None, 48, "STUCK", "not nudged again"),
        (prog, {"unknown": [], "open_gate": ("1e08a531", "pr_merged")}, 48, "HELD", "awaiting gate 1e08a531"),
        (fut, {"unknown": [], "stop_class": "B", "intervention_id": "abcdef12-x"}, 48, "HELD", "class B"),
        (dict(fut, last_prompt=now - 49 * h), {"unknown": [], "stop_class": "B", "intervention_id": "a"}, 48,
         "STOPPED", "hold expired"),
        (prog, {"unknown": [], "stop_class": "E", "intervention_id": "abcdef12"}, 48, "HELD", "close candidate"),
        (fut, {"unknown": [], "stop_class": "A"}, 48, "HELD", "no progress"),
        (fut, {"unknown": ["gates UNKNOWN (door exit 3)"]}, 48, "HELD", "gates UNKNOWN (door exit 3)"),
        (prog, {"unknown": ["gates UNKNOWN (x)"]}, 48, "STOPPED", "gates UNKNOWN (x)"),
        (fut, {"unknown": [], "gate_closed": ("1e08a531", "pr_merged", "cleared")}, 48, "STOPPED",
         "gate 1e08a531 (pr_merged) recorded by the last /next-move went cleared"),
        (dict(fut, nudge_streak=3), {"unknown": [], "gate_closed": ("a", "b", "cleared")}, 48, "STUCK", "3 /next"),
        (fut, {"unknown": [], "pr_changed": "o/r#7"}, 48, "STOPPED", "PR o/r#7 changed"),
        (fut, {"unknown": ["PR state UNKNOWN (o/r#7)"]}, 48, "HELD", "PR state UNKNOWN"),
        (dict(fut, nudge_streak=3), {"unknown": [], "pr_changed": "o/r#7"}, 48, "STUCK", "made no progress"),
    ]
    for t, ext, cool, want, frag in hold_cases:
        got, why = judge("ACTIVE", [], none, t, now, cool, False, ext)
        if got != want or frag not in "; ".join(why):
            bad.append(f"hold(streak={t.get('nudge_streak')}, work={t.get('work_tools_since_nudge')}, "
                       f"ext={ext}) = {got} {why}, want {want} with {frag!r}")
    # The segment counts behind them (close-finished-sessions.nudge_facts).
    def seg(nudge, work=0, final="ok"):
        return {"nudge": nudge, "work": work, "final": final}
    limit_msg = "You've hit your session limit \u00b7 resets 5pm"
    for segs, want in [
        ([seg(False, 3), seg(True, 1)], (1, 0)),                              # progress
        ([seg(False, 3), seg(True)], (0, 1)),                                 # futile, strike 1
        ([seg(True), seg(True), seg(True)], (0, 3)),                          # three in a row -> STUCK
        ([seg(True), seg(True), seg(True, 2)], (2, 0)),                       # progress after two: reset
        ([seg(True), seg(False), seg(True)], (0, 1)),                         # a real prompt ends the streak
        ([seg(True, 1), seg(True), seg(True)], (0, 2)),                       # progress before ends it too
        ([seg(True), seg(True, 0, limit_msg)], (0, 1)),                       # a limited nudge is skipped
        ([seg(False)], (0, 0)),
        ([], (0, 0)),
    ]:
        if cfs.nudge_facts(segs) != want:
            bad.append(f"nudge_facts({segs}) = {cfs.nudge_facts(segs)}, want {want}")
    # A limit-answered /next-move never ran: the screen reads it LIMITED and it holds nothing.
    if judge("LIMITED", [], none, dict(fut, final_text="You've hit your session limit"), now, 48)[0] != "LIMITED":
        bad.append("a limit-answered /next-move was held instead of staying LIMITED")
    for block, want in [({"type": "tool_use", "name": "Edit", "input": {}}, True),
                        ({"type": "tool_use", "name": "Bash", "input": {"command": "git -C x commit -m y"}}, True),
                        ({"type": "tool_use", "name": "Bash", "input": {"command": "gh pr create --title t"}}, True),
                        ({"type": "tool_use", "name": "Bash", "input": {"command": "git log --oneline"}}, False),
                        ({"type": "tool_use", "name": "Bash", "input": {"command": "git -C d push origin x"}}, True),
                        ({"type": "tool_use", "name": "Bash", "input": {"command": "git status; echo commit"}}, False),
                        ({"type": "tool_use", "name": "Bash", "input": {"command": "gh pr view 3"}}, False),
                        ({"type": "tool_use", "name": "Read", "input": {}}, False),
                        ({"type": "tool_use", "name": "Write", "input": {"file_path": "/x/.qontinui/scratch/row.json"}},
                         False),
                        ({"type": "tool_use", "name": "Write", "input": {"file_path": "/tmp/e1.json"}}, False),
                        ({"type": "tool_use", "name": "Edit", "input": {"file_path": "/w/repo/scripts/x.py"}}, True),
                        ({"type": "tool_use", "name": "Edit", "input": {"file_path": "/w/repo/src/temp/x.py"}}, True),
                        ({"type": "tool_use", "name": "Write", "input": {"file_path": "/w/.agent-scratch/n.md"}}, False),
                        ({"type": "tool_use", "name": "mcp__coord-mcp__coord_overlord_record", "input": {}}, False),
                        ({"type": "tool_use", "name": "mcp__coord-mcp__coord_register_gate", "input": {}}, False)]:
        if cfs.is_work_tool(block) != want:
            bad.append(f"is_work_tool({block['name']} {block['input']}) != {want}")
    # Phase 4: a finished signal with a finish time beside a "work remains"
    # message is CONTRADICTED; a coord finish with no time to weigh it against is only HELD.
    still = dict(work, final_text="The deploy is still running; I'll check it in an hour.")
    for facts, t, want in [
        ({"finished": True, "finishedAt": now - 42 * h}, dict(still, last_prompt=now - 43 * h), "CONTRADICTED"),
        ({"sessionStatus": "finished"}, still, "HELD"),
        ({"sessionStatus": "finished"}, dict(work, final_text="PR #3 landed. SAFE TO CLOSE."), "HELD"),
        ({"finished": True, "finishedAt": now - 1800000}, work, "HELD"),
    ]:
        if judge("ACTIVE", [], facts, t, now, 48)[0] != want:
            bad.append(f"finished hold({facts}, {t['final_text'][:30]!r}) != {want}")
    # Coord's finish time (sessionStatusSince) against the last prompt (`sw` was
    # prompted 1h before `sn`, a plausible clock): after it is a revival, none
    # after it a finish; beside the runner marker the NEWER stamp decides;
    # anything status_since reads as UNKNOWN holds on the no-time reason. A
    # fragment tuple must all appear in the reasons.
    sn = 1_790_000_000_000
    sw = dict(work, last_prompt=sn - h, last_asst=sn - h, final_text_at=sn - h)
    ss = dict(sw, final_text=still["final_text"])
    lp = sw["last_prompt"]
    no_time = "coord session_status=finished (no finish time to compare a revival against)"
    by_time = ("coord session_status=finished (", "ago), no prompt since")
    by_marker = ("runner finished marker (", "newer than coord's", "ago), no prompt since")
    cf = {"sessionStatus": "finished"}
    since_cases = [  # (screen verdict, facts, tx, want, reason fragment(s))
        ("REVIVED", dict(cf, sessionStatusSince=sn - 2 * h), sw, "STOPPED", ""),
        ("REVIVED", dict(cf, sessionStatusSince=sn - 2 * h), ss, "STOPPED", ""),  # whatever the message says
        ("ACTIVE", dict(cf, sessionStatusSince=sn - 1800000), sw, "HELD", by_time),
        ("REVIVED", dict(cf, sessionStatusSince=lp - 500), sw, "HELD", by_time),  # within the 1 s tolerance
        ("REVIVED", dict(cf, sessionStatusSince=lp - 1000), sw, "HELD", by_time),  # exact edge: prompt == since + 1 s
        ("REVIVED", dict(cf, sessionStatusSince=lp - 1001), sw, "STOPPED", ""),  # just past it
        ("ACTIVE", dict(cf, sessionStatusSince=sn - 1800000), dict(sw, last_prompt=None), "HELD", by_time),
        ("ACTIVE", dict(cf, sessionStatusSince=sn - 1800000), ss, "CONTRADICTED", by_time + ("work remains",)),
        ("ACTIVE", cf, ss, "HELD", no_time),
        ("REVIVED", cf, sw, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=None), sw, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=str(sn - 2 * h)), sw, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=float(sn - 2 * h)), ss, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=True), sw, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=0), sw, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=-1), sw, "HELD", no_time),
        ("REVIVED", dict(cf, sessionStatusSince=1_000_000_000), sw, "HELD", no_time),  # seconds, not ms
        ("REVIVED", dict(cf, sessionStatusSince=sn + 10 * 60000), sw, "HELD", no_time),  # in the future
        ("REVIVED", dict(cf, sessionStatusSince=sn + 2 * 60000), sw, "HELD", by_time),  # skew inside the allowance
        # the runner marker and coord's time disagree: the newer stamp holds, either way round
        ("REVIVED", dict(cf, finished=True, finishedAt=sn - 2 * h, sessionStatusSince=sn - 1800000), sw,
         "HELD", by_time),  # a re-finish after the revival
        ("ACTIVE", dict(cf, finished=True, finishedAt=sn - 1800000, sessionStatusSince=sn - 2 * h), sw,
         "HELD", by_marker),  # coord missed the re-finish: still held
        ("ACTIVE", dict(cf, finished=True, finishedAt=sn - 1800000, sessionStatusSince=sn - 2 * h), ss,
         "CONTRADICTED", by_marker),
        ("REVIVED", dict(cf, finished=True, finishedAt=lp - 500, sessionStatusSince=sn - 2 * h), sw,
         "HELD", by_marker),  # the marker inside the 1 s tolerance
        ("ACTIVE", dict(cf, finished=True, finishedAt=sn - 1200000, sessionStatusSince=sn - 1800000), sw,
         "HELD", by_marker),  # both after the prompt: the newer one is named
        # every stamp older than the prompt is the only revival
        ("REVIVED", dict(cf, finished=True, finishedAt=sn - 2 * h, sessionStatusSince=sn - 90 * 60000), sw,
         "STOPPED", ""),
        ("REVIVED", dict(cf, finished=True, finishedAt=None, sessionStatusSince=sn - 2 * h), sw, "STOPPED", ""),
        # the runner marker alone keeps its 1 s band
        ("REVIVED", {"finished": True, "finishedAt": lp - 1000}, sw, "HELD", "runner finished marker ("),
        ("REVIVED", {"finished": True, "finishedAt": lp - 1001}, sw, "STOPPED", ""),
        # a time on a status that is not finished is no finish time: the marker decides
        ("ACTIVE", {"finished": True, "finishedAt": sn - 2 * h, "sessionStatus": "working",
                    "sessionStatusSince": sn - 1800000}, sw, "STOPPED", ""),
    ]
    for verdict, facts, t, want, frag in since_cases:
        got, why = judge(verdict, [], facts, t, sn, 48)
        if got != want or not all(f in "; ".join(why) for f in ((frag,) if isinstance(frag, str) else frag)):
            bad.append(f"finish time judge({verdict}, {facts}, prompt={t.get('last_prompt')}) = {got} {why}, "
                       f"want {want} with {frag!r}")
    stale = judge("ACTIVE", [], {"finished": True, "finishedAt": now - 50 * h}, dict(work, last_prompt=now - 51 * h),
                  now, 48)
    if stale[0] != "HELD" or "finished marker stale" not in stale[1][0]:
        bad.append(f"a 50h-old finished marker = {stale}, want HELD with 'finished marker stale'")
    # The gate ids an intervention row records, and the PR references a message names.
    full = "9ac4eb79-1751-4498-97c6-e94c8eae92f8"
    ev = {"regrade": {"gate_id": full}, "wait_gates": [{"gate_id": "1e08a531,418aa291"}, {"gate_id": None}]}
    if recorded_gate_ids(ev) != [full, "1e08a531", "418aa291"] or recorded_gate_ids({"regrade": None}) != []:
        bad.append(f"recorded_gate_ids = {recorded_gate_ids(ev)}")
    refs = pr_refs("Waiting on acme/api#3213 and #2051; gadgets#9 and x#y. #2051 again; check #64 passed, "
                   "step #3 too, Phase 2 #3, issue #44, see README.md#3.", "acme/widgets")
    if refs != ["acme/api#3213", "acme/widgets#2051", "acme/gadgets#9"]:
        bad.append(f"pr_refs = {refs}")
    asked = []
    if pr_refs("only acme/api#1 here", lambda: asked.append(1) or "acme/widgets") != ["acme/api#1"] or asked:
        bad.append("pr_refs resolved the session repo for a message with no bare reference")
    for pr, want in [({"mergedAt": "2026-10-18T10:00:00Z"}, cfs.ms("2026-10-18T10:00:00Z")),
                     ({"commits": [{"committedDate": "2026-10-18T09:00:00Z"}, {"committedDate": "2026-10-18T11:00:00Z"}],
                       "closedAt": None}, cfs.ms("2026-10-18T11:00:00Z")),
                     ({"state": "OPEN", "commits": []}, None)]:
        if pr_moved_at(pr) != want:
            bad.append(f"pr_moved_at({pr}) = {pr_moved_at(pr)}, want {want}")
    if pr_refs("see #12", None) != [] or len(pr_refs(" ".join(f"#{i}" for i in range(9)), "o/r")) != MAX_PRS:
        bad.append("pr_refs: a bare ref with no repo, or the cap")
    # external_signals over a fake coord door: an open gate holds; a failing door is UNKNOWN, never "no gate".
    # The fake door is a /bin/sh script, so this block needs a POSIX shell.
    if sys.platform != "win32":
        with tempfile.TemporaryDirectory() as d:
            door = os.path.join(d, "door")
            nudged = now - h
            row_at = dt.datetime.fromtimestamp((nudged + 60000) / 1000, dt.timezone.utc).isoformat()
            reply = {"interventions": [{"observed_at": row_at, "stop_class": "A", "intervention_id": "i1",
                                        "evidence": {"regrade": {"gate_id": full}}}]}
            gate = {"gates": [{"verdict": "open", "muted": False, "predicate_kind": "time_elapsed"}]}
            with open(door, "w") as f:
                f.write("#!/bin/sh\ncase \"$2\" in\n"
                        f"coord_overlord_interventions) echo '{json.dumps({'content': [{'type': 'text', 'text': json.dumps(reply)}]})}';;\n"
                        f"coord_gate_list) echo '{json.dumps({'content': [{'type': 'text', 'text': json.dumps(gate)}]})}';;\n"
                        "*) exit 3;;\nesac\n")
            os.chmod(door, 0o755)
            ntx = dict(fut, last_prompt=nudged, cwd=None)
            saved_env = os.environ.get("NSS_COORD_DOOR")
            try:
                os.environ["NSS_COORD_DOOR"] = door
                got = external_signals(ntx, {"claudeSessionId": "s"}, use_gh=False)
                if (got or {}).get("open_gate") != (full[:8], "time_elapsed"):
                    bad.append(f"external_signals(open gate) = {got}")
                with open(door, "w") as f:
                    f.write("#!/bin/sh\necho 'refused' >&2; exit 3\n")
                got = external_signals(ntx, {"claudeSessionId": "s"}, use_gh=False)
                if not got or got.get("open_gate") or not any("gates UNKNOWN" in u for u in got["unknown"]):
                    bad.append(f"external_signals(failing door) = {got}")
                if external_signals(work, {"claudeSessionId": "s"}) is not None:
                    bad.append("external_signals read coord for a row whose last prompt is not /next-move")
                got = external_signals(ntx, {"claudeSessionId": "s"}, use_coord=False, use_gh=False)
                if got != {"unknown": ["gates UNKNOWN (--no-coord)"]}:
                    bad.append(f"external_signals(--no-coord) = {got}")
                # An empty interventions list says so; a closed gate after the nudge releases.
                closed_gate = {"gates": [{"verdict": "cleared", "muted": False, "predicate_kind": "pr_merged",
                                          "cleared_at": row_at}]}
                for interventions, gates, check in [
                        ([], gate, lambda g: "no intervention row for this session in this tenant" in g["unknown"]),
                        ([dict(reply["interventions"][0], evidence={"wait_gates": [{"gate_id": full}]})], closed_gate,
                         lambda g: g.get("gate_closed") == (full[:8], "pr_merged", "cleared")),
                        # the re-grade gate clearing on its timer is not a change
                        (reply["interventions"],
                         {"gates": [dict(closed_gate["gates"][0], predicate_kind="time_elapsed")]},
                         lambda g: not g.get("gate_closed")),
                        # a WAIT gate on a time predicate clearing is one
                        ([dict(reply["interventions"][0], evidence={"wait_gates": [{"gate_id": full}]})],
                         {"gates": [dict(closed_gate["gates"][0], predicate_kind="time_elapsed")]},
                         lambda g: g.get("gate_closed") == (full[:8], "time_elapsed", "cleared"))]:
                    with open(door, "w") as f:
                        f.write("#!/bin/sh\ncase \"$2\" in\n"
                                f"coord_overlord_interventions) echo '{json.dumps({'content': [{'type': 'text', 'text': json.dumps({'interventions': interventions})}]})}';;\n"
                                f"coord_gate_list) echo '{json.dumps({'content': [{'type': 'text', 'text': json.dumps(gates)}]})}';;\n"
                                "*) exit 3;;\nesac\n")
                    got = external_signals(ntx, {"claudeSessionId": "s"}, use_gh=False)
                    if not got or not check(got):
                        bad.append(f"external_signals(interventions={len(interventions)}, gates={gates}) = {got}")
                # A PR that got a commit after the /next-move turn releases a futile hold; a gh failure is UNKNOWN.
                gh = os.path.join(d, "gh")
                for body, rc, want in [({"state": "OPEN", "commits": [{"committedDate": "2030-01-01T00:00:00Z"}]}, 0,
                                        "pr_changed"),
                                       ({"state": "OPEN", "commits": [{"committedDate": "2001-01-01T00:00:00Z"}]}, 0,
                                        None),
                                       ({}, 1, "PR state UNKNOWN (acme/api#5)")]:
                    with open(gh, "w") as f:
                        f.write(f"#!/bin/sh\necho '{json.dumps(body)}'\nexit {rc}\n")
                    os.chmod(gh, 0o755)
                    os.environ["NSS_GH"] = gh
                    ext = {"unknown": []}
                    pr_signals(dict(ntx, final_text="waiting on acme/api#5"), now - h, ext)
                    ok = (ext.get("pr_changed") == "acme/api#5" if want == "pr_changed" else
                          want in ext["unknown"] if want else not ext.get("pr_changed") and not ext["unknown"])
                    if not ok:
                        bad.append(f"pr_signals(gh rc {rc}, {body}) = {ext}, want {want}")
            finally:
                os.environ.pop("NSS_GH", None)
                if saved_env is None:
                    os.environ.pop("NSS_COORD_DOOR", None)
                else:
                    os.environ["NSS_COORD_DOOR"] = saved_env
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
    # The segment fields: an Edit after a /next-move is progress, and cwd is read.
    nudge_rec = user(ran)
    edit = asst({"type": "tool_use", "id": "t2", "name": "Edit", "input": {}})
    coord_rec = asst({"type": "tool_use", "id": "t3", "name": "mcp__coord-mcp__coord_overlord_record", "input": {}})
    for records, want in [([first, report, nudge_rec, edit, report], (1, 0)),
                          ([first, report, nudge_rec, coord_rec, report], (0, 1)),
                          ([first, report, nudge_rec, coord_rec, report, nudge_rec, coord_rec], (0, 2)),
                          ([first, report, nudge_rec, coord_rec, model, stdout], (0, 1))]:  # a built-in after
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "projects", "p"))
            with open(os.path.join(d, "projects", "p", "s.jsonl"), "w") as f:
                for i, e in enumerate(records):
                    f.write(json.dumps(dict(e, timestamp=f"2026-10-18T10:00:{i:02d}Z", cwd="/w")) + "\n")
            got = cfs.transcript_facts({"configDir": d, "claudeSessionId": "s"})
        if (got["work_tools_since_nudge"], got["nudge_streak"]) != want or got["cwd"] != "/w":
            bad.append(f"transcript nudge fields = {got['work_tools_since_nudge']}, {got['nudge_streak']}, "
                       f"cwd={got['cwd']!r}; want {want}, '/w'")
    # Work a /next-move delegated to a subagent is progress: its Edit sits in <sid>/subagents/.
    agent_call = asst({"type": "tool_use", "id": "t4", "name": "Agent", "input": {}})
    for sub_at, want in [("2026-10-18T10:00:09Z", (1, 0)), ("2026-10-18T09:00:00Z", (0, 1))]:
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "projects", "p", "s", "subagents"))
            with open(os.path.join(d, "projects", "p", "s.jsonl"), "w") as f:
                for i, e in enumerate([first, report, nudge_rec, agent_call, report]):
                    f.write(json.dumps(dict(e, timestamp=f"2026-10-18T10:00:{i:02d}Z")) + "\n")
            with open(os.path.join(d, "projects", "p", "s", "subagents", "a.jsonl"), "w") as f:
                f.write(json.dumps(dict(edit, timestamp=sub_at)) + "\n")
            got = cfs.transcript_facts({"configDir": d, "claudeSessionId": "s"})
        if (got["work_tools_since_nudge"], got["nudge_streak"]) != want:
            bad.append(f"subagent work at {sub_at} = {got['work_tools_since_nudge']}, {got['nudge_streak']}; "
                       f"want {want}")
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
        # The /next-move branch: a futile nudge an hour ago is re-screened HELD and never sent.
        cfs.transcript_facts = lambda row: dict(fut, last_prompt=time.time() * 1000 - 3600000)
        got = nudge_one("t1", 15, 24, set(), False, use_coord=False, use_gh=False)
        if got[:2] != (False, False) or "re-screen now says HELD" not in got[2] or "gates UNKNOWN (--no-coord)" not in got[2]:
            bad.append(f"nudge_one(/next-move, no progress) = {got}")
    finally:
        for k, v in saved.items():
            setattr(cfs, k, v)
    # Both readers carry coord's time to judge(): row_facts from /restart-readiness,
    # screen() from the close screen's row. Dropped, the row reads "no finish time".
    rp = {"s": {"sessionStatus": "finished", "sessionStatusSince": 123}}
    if row_facts({"claudeSessionId": "s"}, rp).get("sessionStatusSince") != 123:
        bad.append(f"row_facts dropped sessionStatusSince: {row_facts({'claudeSessionId': 's'}, rp)}")
    real_now = time.time() * 1000
    stubs = {"screen_all": lambda idle, only: ([{"terminalId": "t1", "verdict": "REVIVED", "reasons": [],
                                                 "sessionStatus": "finished",
                                                 "sessionStatusSince": int(real_now - 1800000)}], [], None),
             "open_rows": lambda: {"t1": {"claudeSessionId": "s"}},
             "transcript_facts": lambda row: dict(work, last_prompt=real_now - 3600000)}
    saved = {k: getattr(cfs, k) for k in stubs}
    try:
        for k, v in stubs.items():
            setattr(cfs, k, v)
        got = screen(15, None, 48, False, use_coord=False, use_gh=False)[0][0]
        if got["verdict"] != "HELD" or "no prompt since" not in "; ".join(got["reasons"]):
            bad.append(f"screen() did not judge by coord's finish time: {got['verdict']} {got['reasons']}")
    finally:
        for k, v in saved.items():
            setattr(cfs, k, v)
    for b in bad:
        print("FAIL: " + b)
    print(f"selftest: {len(cases)} reset cases, 7 prompt cases, {len(judge_cases)} judge cases, "
          f"{len(hold_cases)} /next-move hold cases, {len(since_cases) + 2} finish-time cases, "
          f"{len(complete_cases)} completion cases, {len(transcript_cases) + 1} transcript cases, "
          f"2 send cases, {len(bad)} failure(s)")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--send", action="store_true", help="prompt /next-move into every STOPPED row")
    ap.add_argument("--cooldown-hours", type=float, default=BACKSTOP_H,
                    help="backstop: no hold that waits on nothing outlasts this many hours (default 48)")
    ap.add_argument("--no-coord", action="store_true",
                    help="do not read coord (intervention rows, gates); every /next-move row says gates UNKNOWN")
    ap.add_argument("--no-gh", action="store_true", help="do not read pull request state with gh")
    ap.add_argument("--include-complete", action="store_true",
                    help="also nudge COMPLETE rows, so /next-move can verify and mark them finished")
    ap.add_argument("--idle-min", type=int, default=15)
    ap.add_argument("--max", type=int, default=0, help="sends to stop after, verified or not (default: no cap)")
    ap.add_argument("--only", default="")
    ap.add_argument("--rows-out", metavar="PATH",
                    help="append one JSON line per attempted nudge to PATH (triage-sessions.py's pass ledger)")
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
        res, warnings, ready_err = screen(a.idle_min, only, a.cooldown_hours, a.include_complete,
                                          not a.no_coord, not a.no_gh)
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
            ok, sent, msg = nudge_one(r["terminalId"], a.idle_min, a.cooldown_hours, own, a.include_complete,
                                      not a.no_coord, not a.no_gh)
        except cfs.READ_ERRORS as e:
            ok, sent, msg = False, False, f"UNKNOWN before the send, not sent: {e}"
        sends += sent  # a SENT? row may have started a turn too, so it uses up the cap
        failed += not ok
        label = "NUDGED " if ok else "SENT?  " if sent else "SKIPPED"
        print(f"  {label} {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['account']:<13.13}  {r['name'][:50]} - {msg}")
        if a.rows_out:
            cfs.append_rows_out(a.rows_out, {
                "terminal_id": r["terminalId"], "action": "nudge", "action_outcome": nudge_outcome(ok, sent, msg),
                "detail": {"screened_as": r["verdict"], "sent": sent, "message": msg[:300]}})
    return 1 if failed else 0


# nudge_one's message when the runner answered the submit-prompt POST with an
# HTTP error: the one refusal that reached the runner before anything was typed.
SUBMIT_REFUSED = "submit-prompt answered "


def nudge_outcome(ok, sent, msg):
    """The pass-ledger outcome of one nudge_one result: done once the command
    read back, sent_unconfirmed once the POST went out, failed when the runner
    refused the POST, skipped when the re-screen held the row before it."""
    if ok:
        return "done"
    if sent:
        return "sent_unconfirmed"
    return "failed" if msg.startswith(SUBMIT_REFUSED) else "skipped"


if __name__ == "__main__":
    sys.exit(main())
