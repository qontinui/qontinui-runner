#!/usr/bin/env python3
"""Screen the runner's terminal sessions for finished ones; optionally close them.

Usage: close-finished-sessions.py [--close] [--idle-min N]
                                  [--only ID[,ID..]] [--json] [--port N]
                                  [--rows-out PATH]

The report is always printed. --close then closes the CLOSABLE rows, FINISHED
and EXITED-FINISHED, re-screening each one immediately before its close and
reading the close back. A live session is FINISHED only when coord's
session_status reads `finished` (served finished-criteria condition 1), so a
finish that only the final reply states (CLOSEOUT, FOOTER-FINISHED), or a
runner mark beside any other coord status (CONTRADICTED, or UNKNOWN when the
runner serves none), is reported and never closed. A finish is read by
finish_state, the one rule nudge-stopped-sessions.py reads too: any turn after
it reopens it (REVIVED), and a finish its own final message contradicts is
CONTRADICTED. A finished session whose directory is not a linked worktree is
CUSTODY (finished-criteria condition 3), never closed.
Read-only unless --close. Stdlib only. The command /close-finished-sessions
documents every verdict and the evidence it rests on.

Exit codes: 0 done (every attempted close verified), 1 a close was skipped or
did not verify, 3 UNKNOWN: the runner could not be read (nothing screened), or
--close was refused because /restart-readiness could not be read.

close-finished-sessions.py --own-tasks [--idle-min N] [--json] lists the calling
session's own background tasks, each STRAY (a wait loop that can never end) or
LIVE, from /proc alone. /finish-session runs it. Exit 0 none live, 1 a live one,
3 UNKNOWN (no /proc, or no claude above the caller).
"""
import argparse
import bisect
import collections
import contextlib
import datetime as dt
import glob
import http.client
import io
import itertools
import json
import os
import re
import sqlite3
import subprocess
import sys
import time
import unicodedata
import urllib.error
import urllib.request

# The helper runs from a provisioned skill directory, which must not collect a __pycache__.
sys.dont_write_bytecode = True
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))
from envelope import EnvelopeUnknown, load  # noqa: E402
try:
    from git_scope import git_scope_env  # noqa: E402  (lib/ is on sys.path above)
except ImportError:  # the command-scripts carrier ships lib/envelope.py only: strip the core three
    def git_scope_env(base=None):
        src = os.environ if base is None else base
        return {k: v for k, v in src.items() if k not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR")}

API = "http://127.0.0.1:" + (os.environ.get("QONTINUI_RUNNER_API_PORT") or "9876")
READ_ERRORS = (urllib.error.URLError, OSError, ValueError, EnvelopeUnknown)

# ---------------------------------------------------------------- phrases
# Closeout PHRASES are a screen, not a verdict: a close rests on coord's
# session_status reading finished (finished-criteria condition 1) plus the
# idleness proof. A phrase is load-bearing only where no structured finish time
# exists - a coord-only finish whose time the runner does not serve, a runner-
# marked session revived and then closed out again with no coord time - and
# where it tells a phrase-only finish (CLOSEOUT, never closed) from an
# unfinished session, so it is matched against a WHITELIST of
# the grammar the fleet's own closeout steps print, and anything hedged is a no.
# A missed yes leaves a tab open; a missed no closes a live session.
#
# "Safe to close": /unattended Step 4.9 prints `SAFE TO CLOSE - <n> trees
# checked, nothing only on disk; hand-off declared for <k>` (or `NOT SAFE TO
# CLOSE - <k> trees`). Accepted: the phrase opening its line (after a bullet or
# one allowlisted label), then nothing, or a separator and one of the report
# clauses below, with no hedge anywhere in that sentence. Every other mention of
# the phrase vetoes the whole message.
SAFE_RE = re.compile(r"safe[ _-]to[ _-]close", re.I)
SAFE_LINE_START = re.compile(
    r"(?:^|\n)[ \t>*-]*(?:(?:hand-off safety|step 4\.9(?: verdict)?|verdict|safe-to-close verdict)[ \t]*:[ \t]*)?$",
    re.I)
HEDGE = re.compile(
    r"\b(?:not|never|except|but|unless|once|until|when|if|after|as soon as|then|assuming|provided|though|"
    r"however|first|must|should|pending|still|yet|blocked|awaiting|needs?|probably|maybe|unclear|unknown|"
    r"uncommitted|unpushed|unverified|unreachable|dirty|failed|failing"
    r"|continu(?!ation (?:report\)|(?:work )?outcome (?:[\u2014\u2013:] ?gate \S+,? outcome_recorded: ?work_completed\b"
    r"|[\u2014\u2013:] ?not applicable\b|on gate \S+ (?:was|is) recorded as work_completed\b)))\w*"
    r"|by mistake|mistakenly)\b"
    r"|n't\b|\?", re.I)  # "continuing" hedges; /unattended's "continuation report/outcome" does not


def sentence_at(text, start):
    """The rest of the sentence that begins at `start`."""
    return re.split(r"(?<=[.!?])\s|\n", text[start:start + 400], maxsplit=1)[0]


def sentence_around(text, start):
    """The WHOLE sentence holding text[start]: its part before `start` as well as the rest."""
    return re.split(r"(?<=[.!?])\s|\n", text[max(0, start - 400):start])[-1] + sentence_at(text, start)


# A hedge word that is itself NEGATED reports a zero, not a doubt: "no uncommitted changes, unpushed commits or
# stashes", "nothing unpushed", "neither has uncommitted changes", "without uncommitted work", "nothing dirty".
# Masked only as a NOUN LIST right after the negator, every word from the closed vocabulary below. ONE item may
# be followed by punctuation, `and` or `anywhere` ("nothing unpushed, so", "no stashes and ..."; never a
# preposition or here/there, which scope the zero - "no unpushed commits on main" - nor "so"). And a sentence whose zero was
# masked must not then name local-only state ("no uncommitted changes, 3 commits ahead of origin"): see hedged(). SEVERAL items are a list only when the last is joined by `or`/`nor` - which distributes
# the negation - and the list then ends at punctuation or the line end. `and` never continues a list ("no stashes,
# and dirty files in the tree" says the files are dirty): it ends the masked item, so the hedge after it still
# vetoes. "no uncommitted changes, unpushed commits remain" masks the first item alone and vetoes on "unpushed";
# "nothing pushed, uncommitted work in tree" masks nothing ("pushed" is not in the vocabulary). A repeated
# negator ("no unpushed commits and no stashes") is two single items.
# Plan 2026-11-08-finished-sessions-held-open-by-idle-chrome-and-hedge-words, Phase 2.
_ZERO_WORD = (r"(?:uncommitted|unpushed|unverified|unmerged|dirty|failed|failing|stashed|stray|leftover|left-over"
              r"|local|background|allocated|open|changes?|commits?|work|files?|edits?|stash(?:es)?|branch(?:es)?"
              r"|trees?|worktrees?|refs?|process(?:es)?|claims?|PRs?|jobs?|tests?|checks?|tasks?|items?)")
_ZERO_ITEM = r"%s(?:[ \t]+%s){0,2}" % (_ZERO_WORD, _ZERO_WORD)
_NEGATOR = (r"\b(?:no|without|zero|nothing(?:[ \t]+(?:is|was|left))?|neither(?:[ \t]+(?:has|have|is|holds?))?"
            r"|none(?:[ \t]+(?:is|are|has|have))?)[ \t]+")
_LIST_END = r"(?=[ \t]*(?:[.,;:)\u2014\u2013]|$))"


def _negated_hedge(single_end):
    return re.compile(
        _NEGATOR + r"(?:%s(?:[ \t]*,[ \t]*(?:no[ \t]+)?%s)*[ \t]*,?[ \t]+(?:or|nor)[ \t]+(?:no[ \t]+)?%s%s|%s%s)"
        % (_ZERO_ITEM, _ZERO_ITEM, _ZERO_ITEM, _LIST_END, _ZERO_ITEM, single_end), re.I | re.M)


NEGATED_HEDGE = _negated_hedge(r"(?=[ \t]*(?:[.,;:)\u2014\u2013]|$)|[ \t]+(?:and|anywhere)\b)")
# Before a mention, only a list closed by punctuation is masked ("clean and nothing unpushed, so it's safe to
# close"): a list running on into the mention ("nothing dirty here is safe to close") keeps its negator for
# SAFE_NEG_BEFORE.
NEGATED_HEDGE_CLOSED = _negated_hedge(r"(?=[ \t]*[.,;:)\u2014\u2013])")
# A parenthetical that explains a ZERO hand-off count in the report ("hand-off declared for 0 (the scratch clone
# was never allocated, so there was nothing to declare)") is not a hedge. Only that shape is allowlisted - a
# clone/worktree/tree/checkout never allocated, created or reserved, optionally followed by "so there was nothing
# to declare" and NOTHING else - and any other note keeps its hedge: "(the fix commit was never created)", "(the
# PR was never opened)", "(the worktree was never created, so the fix sits in the shared checkout)", "(not yet
# declared)", "(coord unreachable)" veto.
ZERO_HANDOFF_NOTE = re.compile(r"(\bhand-?off declared for 0[ \t]*\()([^()\n]*)(\))", re.I)
ZERO_NOTE_OK = re.compile(
    r"[ \t]*(?:the |a |an |this session's )?(?:scratch |agent |sibling )?(?:clone|worktree|tree|checkout|allocation)s?"
    r" (?:was |were )?never (?:allocated|created|reserved)(?:[ \t]*[,;]?[ \t]*(?:so|and)?[ \t]*(?:there (?:was|were|is) )?"
    r"(?:nothing|no hand-?off) to (?:declare|hand off))?[ \t.]*", re.I)


def _mask_zero_note(m):
    return m.group(1) + " " * len(m.group(2)) + m.group(3) if ZERO_NOTE_OK.fullmatch(m.group(2)) else m.group(0)


NO_HANDOFF_NEEDED = re.compile(r"\bno (?:worktree )?hand-?offs?(?: declarations?)? (?:was |were )?needed\b", re.I)


def unnegated(text, closed=False):
    """`text` with negated hedge lists and zero-count notes blanked to spaces (lengths kept). `closed` masks
    only lists that end at punctuation."""
    # The report clause "no (worktree) hand-off was needed" first, whole, so the list mask cannot split it.
    text = NO_HANDOFF_NEEDED.sub(lambda m: " " * len(m.group(0)), text)
    text = (NEGATED_HEDGE_CLOSED if closed else NEGATED_HEDGE).sub(lambda m: " " * len(m.group(0)), text)
    return ZERO_HANDOFF_NOTE.sub(_mask_zero_note, text)


# Once a zero is masked, the REST of the sentence must be a plain clean report, by construction rather than by a
# list of bad words: before the mask, the masked hedge word vetoed "no uncommitted changes, 3 commits ahead of
# origin" or "... ; the push was rejected" by coincidence, and no list of local-state words can name every way
# work is left owed. So after the report clauses and the "safe to close" phrase itself are removed, the rest is
# split into clauses (at punctuation, "and", "so", "with"), and EVERY clause must match one of these closed clean
# clause shapes - a grammar, not a bag of words, so "commits left on the branch" or "a stash exists" cannot be
# assembled from clean-sounding words. Anything else keeps the veto the masked word used to give.
_SUBJ = r"(?:(?:the|this|that|its|my|both|all|each|every) )?(?:(?:repo|coord|runner|web|notes|shared|scratch|session|agent) )?"
_TREE = r"(?:worktrees?|trees?|checkouts?|repos?|repositor(?:y|ies)|clones?)"
CLEAN_CLAUSES = re.compile(r"(?:" + r"|".join((
    r"",
    r"(?:also|yes|clean|checked|none touched|nothing|everything clean|step 4|9|9 verdict|verdict|hand-off safety"
    r"|worktrees?|worktree check|repo|complete|github pr metadata)",
    r"no (?:stray |leftover |background )*(?:processes|claims)(?: (?:from|of) (?:this|the) session)?",
    r"(?:the|this) session had one (?:piece|unit) of work",
    r"(?:now )?recorded in (?:that|a|the) (?:finding|gate|plan)",
    r"(?:the label is )?" + _SUBJ + _TREE + r"(?: i checked)? (?:is|are|was|were) (?:left )?(?:clean|unchanged)",
    r"(?:it|they|both|all|everything|that) (?:is|are|was|were) (?:all )?(?:clean|pushed|merged|landed)",
    r"(?:(?:the|both|all|its|my|every|each) )?(?:commits?|branch(?:es)?|tags?) (?:is|are|was|were) (?:all )?"
    r"(?:pushed|merged|landed)(?: to origin)?",
    r"(?:which|that) (?:is|are|was|were) (?:pushed|merged|landed)",
    # the subject of a masked zero ("this session has [no worktrees]", "there are [no uncommitted changes]")
    r"(?:(?:(?:this|the) session|it|i|they|(?:the|this|each|the one) (?:(?:repo|coord|runner|web|notes|shared|scratch|session|agent) )?" + _TREE + r"(?: i checked)?"
    r"|there) )?(?:has|had|have|holds|owns|left|made|allocated|touched|edited|created|is|are|was|were)",
    r"(?:it's|it is)",
    r"\d+ (?:repository )?(?:trees?|worktrees?)(?: (?:checked|touched|allocated|edited))?",
    r"(?:(?:this|the) session|it|i) only changed coord state(?: and github pr metadata)?",
    r"(?:the )?only change to the (?:shared |repo )?" + _TREE + r" is (?:one|a) tag",
    r"i (?:checked|made|allocated|touched|edited|created)(?: \d+ (?:repository )?(?:trees?|worktrees?)| no| nothing)?",
    r"(?:i )?left (?:it|them|the " + _TREE + r") in place(?: for the platform to reclaim)?",
    r"(?:the " + _TREE + r" (?:is|are) )?left in place for the platform to reclaim",
    r"(?:it|they) (?:was|were) handed off",
    r"(?:from|of|by) (?:this|the) session",
    r"nothing (?:that )?(?:exists|is left|left) only on (?:this machine's )?disk",
    r"no (?:worktree )?hand-?offs? (?:was|were) needed",
)) + r")", re.I)
# "[I] pushed nothing" / "nothing pushed" is never a clean state, though each of its words is.
_NOTHING_PUSHED = re.compile(r"\b(?:nothing|none|no|not)\b (?:\w+ )?(?:is |was |were |got |been )?(?:pushed|merged|landed)\b"
                             r"|\b(?:pushed|merged|landed) (?:nothing|none)\b", re.I)
_REPO_WORD = re.compile(r"\b(?:qontinui[\w.-]*|multistate)\b", re.I)


def _benign_rest(masked):
    """Whether `masked` (a sentence with its zeros blanked) holds nothing but clean-state reporting."""
    rest = SAFE_RE.sub(" ", masked)
    rest = re.sub(SAFE_CLAUSE, " ", rest, flags=re.I)
    if _NOTHING_PUSHED.search(rest):
        return False
    rest = _REPO_WORD.sub("repo", rest)
    for clause in re.split(r"[,;:.!?()\u2014\u2013]+|\s-\s|\b(?:and|so|with|which is pushed)\b", rest, flags=re.I):
        clause = re.sub(r"\s+", " ", clause).strip().lstrip("-*># ").strip()
        if not CLEAN_CLAUSES.fullmatch(clause):
            return False
    return True


# Clean-state report clauses whose negation describes the session's clean state, not a condition on the claim
# (plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 4, N3). Each is ENUMERATED and
# blanked before any hedge test, and nothing else is. Not a generic "negation in a parenthetical" mask: "1 tree
# checked (not pushed)" and "(never committed)" still veto, and so does any other hedge left beside a masked clause.
# "nothing unpushed" is not listed: the negated-hedge mask above reads it, and then requires the rest of its
# sentence to be clean-state reporting ("nothing unpushed, the tree has edits" vetoes).
CLEAN_STATE = re.compile(
    r"\bhand-?off not declared\b"
    r"|\bnothing that isn't already on the remote\b"
    r"|\bnothing that exists only on (?:this machine's )?disk\b"
    r"|\bnever a registered worktree\b"
    r"|\bI didn't commit or edit any files\b"
    r"|\(a scratchpad clone, not an allocated worktree\)"
    r"|\(no PR or branch from this session, nothing to declare\)", re.I)


def hedged(text):
    """A hedge in `text` that is not a negated report of a zero - or, once a zero was masked, anything in the rest
    of the sentence that is not plain clean-state reporting. An enumerated CLEAN_STATE clause is blanked first."""
    text = CLEAN_STATE.sub(lambda m: " " * len(m.group(0)), text)
    masked = unnegated(text)
    # The clause grammar applies only where the mask removed a real hedge word: a zero with no hedge in it ("No
    # open claims; PR #12 merged") never vetoed, and must not start to.
    return HEDGE.search(masked) or (masked != text and HEDGE.search(text) and not _benign_rest(masked))


# One clause of the report after the phrase. The report's first sentence must be
# a list of these, so "2 trees checked, 1 has work only on disk" and "yes;
# operator approval is outstanding" are not a yes: their second clause is not
# one of the report clauses.
SAFE_CLAUSE = (
    r"(?:yes"
    r"|\d+ (?:repository )?(?:trees?|worktrees?) (?:checked|touched|allocated|edited|written)"
    r"(?: or (?:edited|touched))?(?: by this session)?(?: \([^()\n]*\))?"
    r"|no (?:allocated |session )?(?:worktrees?|trees?|repository trees?|working trees?|commits)(?: (?:were|was))?"
    r"(?: (?:allocated|touched|edited|checked|created)(?: or (?:edited|touched))?| to hand off)?(?: by this session)?"
    r"|nothing (?:left |of this session's )?(?:exists |is )?(?:left )?only on (?:this machine's )?disk"
    r"|no work (?:exists |is )?(?:left )?only on (?:this machine's )?disk"
    r"|no (?:files?|edits?|changes?)(?: of mine)? (?:exists? |is |are )?(?:left )?only on (?:this machine's )?disk"
    r"|nothing to hand off"
    r"|hand-?off declared for \d+(?: \([^()\n]*\))?"
    r"|hand-?off not declared(?: \([^()\n]*\))?"
    r"|no (?:worktree )?hand-?offs?(?: declarations?)? (?:was |were )?needed)")
SAFE_REPORT_LINE = re.compile(r"%s(?:[ \t]*(?:[,;]|\band\b|\bso\b)(?:[ \t]*(?:and|so)\b)?[ \t]*%s)*[ \t]*\.?[ \t]*$"
                              % (SAFE_CLAUSE, SAFE_CLAUSE), re.I)
ZERO_CHECKED = re.compile(r"(?:^|[,;]\s*)0 (?:repository )?(?:trees?|worktrees?) checked\b", re.I)
NO_TREES = re.compile(r"\bno (?:allocated )?(?:worktrees?|trees?)\b", re.I)
# A negation or condition in the SAME clause, right before the phrase: "not yet
# safe to close", "isn't safe to close", "Nothing here is safe to close", "is it
# safe to close", "after #12 lands it is safe to close". A sentence or clause
# break ends the reach, so "Clauses not followed: none.\n\nSAFE TO CLOSE" is not one.
SAFE_NEG_BEFORE = re.compile(
    r"(?:(?:\bnot|n't|\bnever"
    r"|\bnothing(?!(?<=\bleft nothing)[ \t]+only on (?:this machine's )?disk, so it was\b)|\bnone)\b"
    r"[^.!?:;\n]{0,25}"
    r"|\bis it\s+|\b(?:will|would|could|might|may) be\s+(?:\w+\s+)?"
    r"|\b(?:whether|until|once|when|if|before|after|as soon as)\b[^.!?:;\n]{0,40})$", re.I)
# "Safe to close: no (PR open)" is a no; "SAFE TO CLOSE: no worktrees allocated" opens a report, and so
# does the one full phrase "no work exists only on disk" - never "no work was pushed" or "no work is done".
SAFE_NEG_AFTER = re.compile(
    r"[ \t]*(?:\?|[:=\u2014\u2013-][ \t]*(?:not|false|negative|n/a|unknown|unclear|pending|no"
    r"(?![ \t]+(?:(?:allocated|session)[ \t]+)?(?:worktrees?|trees?|repository|working|commits|hand-?offs?)\b)"
    r"(?![ \t]+work (?:exists |is )?(?:left )?only on (?:this machine's )?disk\b)"
    r"(?![ \t]+(?:files?|edits?|changes?)(?: of mine)? (?:exists? |is |are )?(?:left )?only on "
    r"(?:this machine's )?disk\b))\b)", re.I)


def neg_after(line):
    """SAFE_NEG_AFTER on the rest of a mention's line, except a `: no` that opens a negated hedge list
    ("Safe to close: no uncommitted changes or unpushed commits") - a report of zeros, not a no."""
    m = SAFE_NEG_AFTER.match(line)
    if m and m.group(0)[-2:].lower() == "no":
        z = NEGATED_HEDGE.match(line, m.end() - 2)
        if z and HEDGE.search(z.group(0)):
            return None
    return m


# The line before a mention introduces a quotation of the format, not a report.
QUOTED_EXAMPLE = re.compile(r"(?:format|example|e\.g\.|template|looks like|such as)[^\n]*:[ \t]*\n"
                            r"(?:[ \t]*\n)*$", re.I)
DISK_LEFT = re.compile(r"\bonly on (?:this machine's )?disk\b", re.I)
# "no work / no files of mine / no edits are left" before "only on disk" is nothing left, never work left.
NO_WORK_BEFORE_DISK = re.compile(r"\bno (?:work|files?|edits?|changes?)(?: of mine)? (?:exists? |is |are )?"
                                 r"(?:left )?$", re.I)
# The label form `**Safe to close?** SAFE TO CLOSE - <report>`: a bare "Safe to close?" opening its line and
# followed at once by a FULL yes - the bare headline ending its sentence, or the headline and a report the
# grammar accepts - is a label, not a question, and is dropped. So is the label answered by the headline and a
# COUNTED report (`**Safe to close? SAFE TO CLOSE** - 0 trees: this session edited no files ...`) when nothing on
# the rest of the line hedges, promises or names work still owed: the question is answered in place, and the
# headline is then judged like any other (an unlisted report is neither a yes nor a veto). Anything else keeps its `?`,
# so "Safe to close? Unclear", "Safe to close? Safe to close, I think." and "Safe to close? NOT SAFE TO CLOSE" veto.
# Markdown emphasis around the label and headline is allowed (phrase_hits strips it first anyway).
SAFE_LABEL = re.compile(r"(?m)^([ \t>*-]*)safe[ _-]to[ _-]close\?\**[ \t]+\**(safe[ _-]to[ _-]close)\b([^\n]*)$",
                        re.I)


def _drop_safe_label(m):
    rest = m.group(3)
    sep = re.match(r"\**[ \t]*[:=\u2014\u2013-][ \t]*", rest)
    report = sentence_at(rest, sep.end()) if sep else ""
    full_yes = (re.fullmatch(r"\**[ \t]*[.!]?[ \t]*\**[ \t]*", rest) or re.match(r"\**[ \t]*[.!][ \t]", rest)
                or (sep and SAFE_REPORT_LINE.match(report)
                    and not (ZERO_CHECKED.search(report) and not NO_TREES.search(report))))
    # The rest of the line must not take it back ("Safe to close? Safe to close. Not really though.").
    full_yes = full_yes and not (hedged(rest) or neg_hits(rest))
    # Answered in place by a ZERO-tree report the grammar does not list ("- 0 trees: this session edited no files"):
    # the label goes only when nothing on the rest of the line hedges, promises, names work still owed or names
    # local state ("2 local commits", "3 commits ahead").
    zero = re.match(r"\**(?:[ \t]*[:\u2014\u2013]|[ \t]+-)[ \t]+0 (?:repository )?(?:trees?|worktrees?)"
                    r"(?: \w+)?[ \t]*[:,;]", rest)
    answered = (zero
                and not (hedged(rest) or neg_hits(rest) or promise_hits(rest) or OWED_WORDS.search(rest))
                and _benign_rest(unnegated(rest)))  # and, by construction, nothing but clean-state reporting
    # Answered in place by ANY report after a spaced separator that the grammar does not list ("- Safe to close?
    # SAFE TO CLOSE - 2 trees reviewed, nothing only on disk."): it reads like the colon form - the label goes, for
    # the safe_to_close read and the closing-question test alike, and the headline is then neither, not a veto -
    # when nothing on the line hedges, promises or names work owed or local state. A hedged report ("..., but #12
    # is open.") keeps its `?` and vetoes. A ZERO-tree report is `answered`'s alone, whose clause grammar is
    # stricter. Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 4.
    reported = (not zero and re.match(r"\**(?:[ \t]*[:=—–]|[ \t]+-)[ \t]+\S", rest)
                and not (hedged(rest) or neg_hits(rest) or promise_hits(rest) or OWED_WORDS.search(rest)))
    return m.group(1) + m.group(2) + rest if full_yes or answered or reported else m.group(0)


OWED_WORDS = re.compile(r"\b(?:owed?|owes|remains?|remaining|next|todo|to-do|later|will|needs?|fix(?:es)?"
                        r"|local|ahead|behind|edits|commits?|changes|modified|untracked|staged"
                        r"|broke|broken|breaks|errors?|regress\w*|red|failing|fails?|in progress|running"
                        r"|mid-flight|waiting|unfinished|incomplete|queued|underway|half)\b|'ll\b", re.I)


# A quoted check name ('so the "safe to close?" check passes') is not the session's own answer: a short quoted
# span holding a mention is neither, and only such spans leave the closing-question test. Every other quoted
# span stays, so 'One question for you: "should I merge #12 now?"' still vetoes, and so does a quoted answer
# that hedges apart from its `?` ('the check printed "NOT SAFE TO CLOSE"'). Plan
# 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 4.
def safe_quotes(text):
    """(start, end) of each quoted span, at most 40 characters inside, that holds a "safe to close" mention
    and no hedge but a `?`."""
    return [(q.start(), q.end()) for q in QUOTED_SPANS.finditer(text)
            if q.end() - q.start() <= 42 and SAFE_RE.search(q.group(0))
            and not HEDGE.search(q.group(0).replace("?", " "))]


def safe_to_close(text):
    """(any_yes, any_no) over every "safe to close" in `text`.

    Each mention is a YES (it matches the closeout grammar, unhedged), a NO (an
    explicit negative: negated or conditional before it, a `?` or `: no` after
    it, a hedge on its line, work "only on disk" not preceded by "nothing", or a
    headline taken back by the line after it), or neither. Neither is not a yes
    - a row that needs a phrase does not get one - and it is not a veto either:
    an unusual wording must not override a runner finished marker. Any NO vetoes
    the whole message."""
    yes = no = False
    quoted = safe_quotes(text)
    for m in SAFE_RE.finditer(text):
        span = next(((a, b) for a, b in quoted if a <= m.start() < b), None)
        if span:
            # A quoted check name is neither, unless its own sentence hedges outside the quote
            # ('The "safe to close" check couldn't confirm ...' still vetoes).
            if hedged(sentence_around(text[:span[0]] + " " + text[span[1]:], span[0])):
                no = True
            continue
        line_start = text.rfind("\n", 0, m.start()) + 1
        before = text[line_start:m.start()]
        line = text[m.end():].split("\n", 1)[0]
        whole = before + m.group(0) + sentence_at(line, 0)  # the mention's own sentence
        disk = [d for d in DISK_LEFT.finditer(whole)
                if "nothing" not in whole[max(0, d.start() - 45):d.start()].lower()
                and not NO_WORK_BEFORE_DISK.search(whole[:d.start()])]
        if (SAFE_NEG_BEFORE.search(unnegated(text[max(0, m.start() - 60):m.start()], closed=True)) or neg_after(line)
                or hedged(whole) or disk):
            no = True
            continue
        if QUOTED_EXAMPLE.search(text[max(0, line_start - 300):line_start]):
            continue  # "The headline format is:\n- SAFE TO CLOSE - ..." quotes the grammar: never a yes
        if not SAFE_LINE_START.search(text[max(0, m.start() - 60):m.start()]):
            continue  # a mention inside a sentence: neither
        if re.fullmatch(r"[ \t]*[.!]?[ \t]*", line):
            # The bare headline, or "Safe to close." on its own line: the line
            # after it must not take it back ("But first: approve #12").
            following = next((ln for ln in text[m.end():].split("\n")[1:] if ln.strip()), "")
            if hedged(following) or neg_hits(following):
                no = True
            else:
                yes = True
            continue
        after = re.match(r"[ \t]*[.!][ \t]", line)
        if after:  # "Safe to close. <the rest of its line>": that rest must not take it back
            rest = line[after.end():]
            if hedged(rest) or neg_hits(rest):
                no = True
            else:
                yes = True
            continue
        sep = re.match(r"[ \t]*[:=\u2014\u2013-][ \t]*", line)
        report = sentence_at(line, sep.end()) if sep else ""
        if sep and SAFE_REPORT_LINE.match(report) and not (ZERO_CHECKED.search(report)
                                                           and not NO_TREES.search(report)):
            yes = True
    return yes, no


# An EXPLICIT negative headline: the one wording that still vetoes a row whose runner marker /unattended set
# (see unattended_trusted). A line opening with NOT SAFE TO CLOSE, Step 4.9's `UNKNOWN - <k> trees could not
# be checked`, or `Safe to close: false|not|unknown` or a bare `no` (one ending the line or followed by punctuation
# or a conjunction: `Safe to close: no - PR open`, `? No.`), after a bullet, emphasis or one allowlisted label - and
# "not / isn't / is not safe to close" and "unsafe to close" anywhere at all, in any case (a quoted or past-tense
# one errs toward keeping the tab). Also: a `Safe to close:` answer of `probably not`, `maybe` and the like; a
# `Safe to close:` line that goes on to `but`/`except`/`unless`; the upper-case short form `NOT SAFE`;
# `Session finished: no`; and `Session left UNFINISHED` and `verdict: INCOMPLETE`, which contradict the marker
# outright. Labels are any `<word> verdict:`. Runs of blanks are folded to one space before matching.
# "Safe to close: no session worktrees; ..." opens a report of zeros, not a no.
_LABEL = r"(?:hand-off safety|step 4\.9(?: verdict)?|(?:[\w-]+ )?verdict|safe-to-close verdict|safe[ _-]to[ _-]close ?\?)"
NEG_HEADLINE = re.compile(
    r"(?im)^[ >#*+-]*(?:" + _LABEL + r" ?:? ?)?"
    r"(?:unknown ?(?:[:(\u2014\u2013-] ?)?\d+ (?:repository )?(?:trees?|worktrees?) could not be checked"
    r"|safe[ _-]to[ _-]close ?[?:=\u2014\u2013-] ?(?:(?:probably |likely |maybe |perhaps )?(?:false|not|unknown|unclear"
    r"|uncertain|nope|negative|pending|n/a)\b|maybe\b|possibly\b|perhaps\b"
    r"|(?:only )?(?:once|after|until|when|if|unless|before|as soon as)\b"
    r"|no\b(?= ?(?:$|[^\w ])| (?:because|since|as|while|until|yet|not|and|but)\b))"
    r"|safe[ _-]to[ _-]close ?[:=\u2014\u2013-][^\n]*\b(?:but|except|unless)\b"
    r"|(?:[\w-]+ )?(?:closeable|closable|ready to close) ?: ?(?:no|false|not)\b)")
# A negator wrapped onto the headline's own line ("this is not\nSAFE TO CLOSE - ..."), in any case.
NEG_WRAPPED = re.compile(r"(?i)\b(?:not|no longer|never|isn't|is not|aren't)[ \t]*\n[ \t>#*-]*safe[ _-]to[ _-]close\b")
# The same negatives spelled anywhere, read over the text with ALL whitespace (newlines too) folded to one space.
NEG_ANYWHERE = re.compile(
    r"(?i)(?:\b(?:not|no longer|never|nothing|none)|n't)(?: [\w'-]+){0,5}? safe[ _-]to[ _-]close\b"
    r"|\bsafe[ _-]to[ _-]close ?[:=\u2014\u2013-] ?(?:\w+ )?(?:not|nah|nope)\b|\bsafe[ _-]to[ _-]close\b[^.!?;]{0,20}n't think so"
    r"|'ll be (?:\w+ )?safe[ _-]to[ _-]close\b"
    r"|\b(?:session|tab|terminal|pane|this|it) (?:should |must |will |needs? to )?(?:remain|stay|be kept) open\b"
    r"|\b(?:keep|leave) (?:this|the|it|that)(?: session| tab| terminal| pane| window)? open\b"
    r"|\b(?:once|after|until|when|if|as soon as)\b[^.!?;]{0,40}\bsafe[ _-]to[ _-]close\b"
    r"|\bsafe[ _-]to[ _-]close ?[:=\u2014\u2013-] ?(?:no|false|not|unknown|unclear|n/a|pending|negative)\b"
    r"(?! (?:session |allocated )?(?:worktrees?|trees?|work|files?|edits?|changes?|commits|repository|working|hand-?offs?)\b)"
    r"(?! (?:uncommitted|unpushed|unverified|unmerged|dirty|failed|failing|stashed|stray|leftover|left-over|local"
    r"|background|open|stash(?:es)?|claims?|PRs?)\b)"  # "Safe to close: no uncommitted changes ..." reports zeros
    r"|\b(?:session|work|it|this|that|phase \d+)(?: \w+)? (?:is |was |remains )?(?:left )?(?:unfinished|incomplete)\b"
    r"|\bunsafe\b[^.!?;]{0,30}\b(?:clos|exit)\w*|\b(?:clos|exit)\w*[^.!?;]{0,30}\bunsafe\b"
    r"|\bnot (?:yet )?(?:ready|safe|ok|okay) to (?:close|exit|end)\b|\bhold off (?:on )?(?:closing|exiting)\b"
    r"|\b(?:do not|don't|must not|mustn't) (?:yet )?(?:exit|end|kill)\b"
    r"|\bsession (?:is |was )?not (?:yet )?(?:marked )?(?:finished|done)\b"
    r"|\b(?:marked|finished)\b[^.!?;]{0,40}\b(?:by mistake|mistakenly|wrongly|in error)\b"
    r"|\b(?:finish(?:ed)? mark|marked finished)\b[^.!?;]{0,20}\b(?:premature|too early)\b"
    r"|\bshould(?: not|n't) have (?:been )?(?:marked|finished)\b|\b(?:marking|finishing) it[^.!?;]{0,20}\bwas a mistake\b"
    r"|\breopening (?:this|the) session\b|\bunless\b[^.!?;]{0,40}\bsafe[ _-]to[ _-]close\b"
    r"|(?:^|[.;] |\u00b6 )(?:[\w-]+ )?(?:closeable|closable|ready to close|safe-to-close verdict) ?: ?(?:no|false|not)\b"
    r"|\b(?:by mistake|mistakenly|wrongly)\b[^.!?;]{0,40}\b(?:marked|finished)\b"
    r"|n't think [^.]{0,20}safe[ _-]to[ _-]close\b|\bsafe[ _-]to[ _-]close ?\? ?(?:no|not)\b"
    r"|\b(?:will|would|could|might|may) be (?:\w+ )?safe[ _-]to[ _-]close\b"
    r"|\bsafe[ _-]to[ _-]close (?:only )?(?:once|after|until|when|if|unless|as soon as)\b"
    r"|\bunsafe[ _-]to[ _-]close\b|\bsession (?:is |was )?(?:left )?unfinished\b"
    r"|\b(?:session|work|it|this|that) (?:is |was )?not (?:yet |quite )?(?:done|finished|complete|completed)\b"
    r"|\b(?:I'm|I am|we're|we are) not (?:yet |quite )?(?:done|finished|complete)\b"
    r"|(?:^|[.!?:;] |\u00b6 )not (?:yet )?(?:done|finished|complete)\b|\bnot (?:done|finished|complete) yet\b"
    r"|\b(?:should|must|needs? to) stay open\b"
    r"|\b(?:must not|mustn't|should not|shouldn't|do not|don't|cannot|can't) (?:yet )?(?:be )?closed?\b"
    r"|\b(?:keep|leave) (?:this|the|it)(?: session| tab)? open\b"
    r"|\bsession (?:marked )?finished ?[?:=\u2014\u2013-] ?(?:no|not|false)\b"
    r"|\bverdict ?[:\u2014\u2013-]? ?(?:is |was )?incomplete\b")
NEG_HEADLINE_UPPER = re.compile(r"\bNOT SAFE\b")  # case matters: the short headline token


def _folded(text):
    """normalize(), with runs of spaces and tabs folded to one space."""
    return re.sub(r"[ \t]+", " ", normalize(text))


def negative_headline(text):
    """The explicit negative headline in `text`, else None. Read over the WHOLE message, code and quotes
    included, so a quoted headline errs toward keeping the tab."""
    norm = _folded(text)
    # Line breaks fold to a space, so "**Safe to close?**\nNo - ..." and "NOT\nSAFE TO CLOSE" stay one negative,
    # EXCEPT a break after a label whose value is a bare zero ("Peer messages: none") or before the Step 4.9
    # headline: those become a stop token, so "none" never negates the headline on the next line.
    flat = re.sub(r"(?im)(:[ \t]*(?:none|nothing|n/a|no|zero)[ \t]*)\n|\n(?=[ \t>#*-]*safe[ _-]to[ _-]close[ \t]*[\u2014\u2013:-])",
                  lambda m: (m.group(1) or "") + " \u00b6 ", norm)
    flat = re.sub(r"\s+", " ", flat)
    # The upper-case NOT SAFE token is unambiguous across ANY line break ("NOT\nSAFE TO CLOSE - ...").
    m = (NEG_HEADLINE.search(norm) or NEG_ANYWHERE.search(flat) or NEG_WRAPPED.search(norm)
         or NEG_HEADLINE_UPPER.search(re.sub(r"\s+", " ", norm)))
    return m.group(0).strip()[:60] if m else None


# The reply must also BE the report /unattended writes after its gate passed - a line OPENING at column 0 (no
# indent, bullet or blockquote; code fences are stripped first) in one of the shapes it prints: the Step 4.9
# headline `SAFE TO CLOSE - <n> trees checked|touched|allocated` (or `- no worktrees`), Step 4's lead line
# `Unattended verdict: COMPLETE` (after at most one `<Label>:`), Step 5's `Session marked finished - carried by`,
# or /finish-session's `Session finished - <session id>` - before the marker is trusted over its wording. A
# subagent shares its parent's session id, so a stray "unattended:" finish can land on a parent that is still
# talking. This narrows that case (labels, bullets, indents, fences and introduced quotes are refused - see
# own_report) but cannot close it: a verbatim column-0 paste with no introducing line still passes, which needs
# a second failure first (/unattended Step 5 forbids a subagent to run /finish-session at all).
UNATTENDED_REPORT = re.compile(
    r"^(?:#+ )?(?:Step 4\.9(?: verdict)?: ?)?SAFE[ _-]TO[ _-]CLOSE ?[:\u2014\u2013-] ?"
    r"(?:\d+ (?:repository )?(?:trees?|worktrees?) (?:checked|touched|allocated)|no (?:session |allocated )?worktrees?)"
    r"|^(?:#+ )?(?:Closeout: ?)?Unattended verdict:? ?COMPLETE\b"
    r"|^(?:#+ )?Session marked finished ?[\u2014\u2013:] ?carried by [^,\n]+, read back\b"
    r"|^(?:#+ )?Session finished ?[\u2014\u2013:] ?[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b",
    re.M)
# A line introducing a quotation ("The closeout subagent returned:", "Subagent result:", "From the /unattended
# subagent:") makes EVERY report shape after it a quote, to the end of the message: /unattended's report runs to
# several paragraphs, so a pasted one cannot be bounded by its first blank line.
REPORT_QUOTED = re.compile(r"(?i)\b(?:returned|reported|reports|report|result|results|output|said|says|wrote|printed"
                           r"|summary|found|finished"
                           r"|came back|subagent|agent|closeout|format|example|e\.g\.|template|looks like|such as)\b"
                           r"[^\n]*:[ \t]*$"
                           # ...and a heading or sentence naming a subagent's result without a colon:
                           # "## Subagent result", "Here is what the closeout subagent found."
                           r"|^(?:#+ [^\n]*\b(?:subagent|agent|result|results|report|summary|output)\b[^\n]*"
                           r"|[^\n]*\bsub-?agent\b[^\n]*\b(?:found|returned|reported|said|wrote|result|report|verdict)"
                           r"\b[^\n]*)$")


def own_report(text):
    """Whether `text` carries one of /unattended's report shapes as its OWN line: column 0, outside a code fence,
    and before any line introducing a quotation (a parent pasting its subagent's report)."""
    for ln in normalize(FENCED_BLOCKS.sub(" ", text)).split("\n"):
        if REPORT_QUOTED.search(ln):
            return False
        if UNATTENDED_REPORT.match(ln):
            return True
    return False


FENCED_BLOCKS = re.compile(r"^[ \t]*```[^\n]*\n.*?^[ \t]*```[ \t]*$", re.S | re.M)
# Asks of the operator that a trusted row keeps as vetoes beside TRUST_KEEPS (the "states the next step" label,
# which is dropped, also carries these two).
OPERATOR_ASK = re.compile(r"\bneed your\b|\btell me\b|\blet me know\b", re.I)
# Vetoes a trusted row KEEPS: the operator is being asked to act, or the reply says the session is not finished.
# What it drops is the hedge, work-remains and promise wording the report carries about work it already settled.
# ("session finished: no" is not kept as phrase_hits spells it - that pattern also reads "...marked the session
# finished. No closer will ..." as a no - but negative_headline keeps the real `Session finished: no`.)
TRUST_KEEPS = {"its closing paragraphs ask a question", "asks the operator to act", "waiting on the operator",
               "not marking finished"}


# Other closeout statements: /finish-session's report. Matched only at a line
# start or as a plain statement, and only when the sentence carries no hedge.
NOT_BEFORE = r"(?<!not )(?<!n't )(?<!never )"
POSITIVE = [
    ("Session finished:", r"(?:^|\n)[ \t>-]*session (?:marked )?finished[ \t]*[:.\u2014\u2013-]"),
    ("marked finished", NOT_BEFORE + r"\bsession (?:is |was |has been |is now )?marked (?:as )?finished\b"),
    ("marked the session finished",
     NOT_BEFORE + r"\b(?:I |I've |have )?marked (?:the |this |my )?session (?:as )?finished\b"),
    # A post-merge follow-up session ends by saying there is nothing to follow up.
    ("no follow-up needed",
     r"\b(?:needs? no follow-?up|no follow-?up(?: PR| work)? (?:is |was )?(?:needed|required|owed))\b"),
    # Re-closeouts: a session woken after its mark that says so again (plan
    # 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 4, N4). Read under a hedge check
    # over the WHOLE sentence (phrase_hits), so "Once CI lands, nothing is left to do." is not one.
    ("owes nothing", NOT_BEFORE + r"\bowes nothing(?: further| more| else)?\b"),
    ("nothing left to do", NOT_BEFORE + r"\bnothing (?:is )?left to do\b"),
    ("stays marked finished", NOT_BEFORE + r"\bstays marked finished\b"),
]
RECLOSEOUT = {"owes nothing", "nothing left to do", "stays marked finished"}
# A footer entry naming a terminal state: running checks behind a gate or in the train do not keep a session
# unfinished (served session-protocol Step 3; finished-criteria session-finished-at-green-not-at-merged).
TERMINAL_STATE = re.compile(r"\bbehind gates?\b|\bgated\b|\bin-train\b|\bwatched by gate [0-9a-f][0-9a-f-]*\b",
                            re.I)
# The session-protocol footer closes EVERY turn of a policy-following session,
# including turns of a conversation that is still going, so alone it is weak.
FOOTER_RE = re.compile(r"\bPOLICY_?COMPLIANCE\b|\bPOLICY_?GAPs?\b|\bClauses applied\b", re.I)
FOOTER_HEAD_RE = re.compile(r"(?:^|\n)[ \t#>*-]*(?:policy[ _]?(?:compliance|summary|footer)|clauses applied)\b",
                            re.I)
# A footer that STATES the session owes nothing. That statement is the finish
# signal: per served policy finished-criteria a session is finished when nothing
# it can act on remains, and work that is gated, deferred with a trigger or owned
# by another PR is not owed by it. So "in-train" or "deferred" beside a
# nothing-owed statement is fine; a hedge beside it is not.
NOTHING_OWED_RE = re.compile(
    r"\bnothing (?:further |else |more )?(?:is |was |remains )?(?:owed|owing|left to do)\b"
    r"|\b(?:this |the )?session owes nothing\b|\bowes? nothing\b"
    r"|\bnothing (?:this |the )?session owes\b|\bno follow-?ups?(?: items?)?\b", re.I)
# The inline footer shape: "POLICY_COMPLIANCE: #1818 is landed. ..." with no
# nothing-owed sentence. Only accepted when nothing else in the footer hedges.
FOOTER_LANDED_RE = re.compile(r"#\d+ (?:is|was|has been) (?:now )?(?:landed|merged)\b[^\n]{0,200}", re.I)
# Words that take a nothing-owed statement back, or say the session is not done.
FOOTER_HEDGE_RE = re.compile(
    r"\b(?:yet|still|but|once|unless|awaiting|waiting|(?:is|are) next|next steps?|to be (?:filed|done|run)|not (?:marked )?"
    r"(?:finished|done)|stays? open|remain(?:s|ing)? to|failing|red)\b", re.I)
# Text near the end of the final message saying work remains.
NEGATIVE = [
    ("I'll / we'll <next step>",
     r"\b(?:I|we)(?:'ll| will) (?:check|watch|wait|mark|report|come back|resume|re-?check|poll|keep|continue|"
     r"follow up|look)\b"),
    ("will mark finished later", r"\b(?:will|going to|then|and) mark (?:the |this )?session finished\b"),
    ("not marking finished", r"\b(?:not|n't|never) (?:be )?(?:marking|marked|mark) (?:the |this |my )?session "
                             r"(?:as )?finished\b"),
    ("when X finishes, I'll",
     r"\bwhen (?:that|the|it|this) [^.]{0,80}\b(?:finishes|lands|completes|merges)\b[^.]{0,40}\bI'?ll\b"),
    ("still running/waiting", r"\bstill (?:running|waiting)\b"),
    ("waiting for it", r"\bwaiting for it\b"),
    ("states the next step",
     r"\bI'?m going to\b|\bnext step\b|(?:^|\n)[ \t-]*remaining:|\bneed your\b|\btell me\b"),
    ("asks the operator to act",
     r"\b(?:reply|answer) (?:yes|no|with)\b|\blet me know\b|\bover to you\b|\byour call\b"
     r"|\byou (?:decide|choose|pick)\b|\b(?:want|shall|should) (?:me|I) to\b|\bI'll do it\b"),
    ("waiting on the operator",
     r"\b(?:waiting (?:for|on)|awaiting|blocked on) (?:your|the operator'?s?|an? operator) "
     r"(?:answer|reply|decision|approval|input)?"),
]
# Pronoun-free promises (follow-up edge 67ecc4d7): "Doing it now." owes work as surely as "I'll do it".
# Read by promise_hits over the CODE- AND QUOTE-FREE tail only (a quoted "doing it now" or a log line in a
# fence is not the session speaking), and never by safe_to_close: a SAFE TO CLOSE that says what is running
# now must not turn into "not safe to close".
PROMISE_WORDS = [
    ("says it is not done yet", re.compile(r"\bnot (?:done|finished) yet\b|\bnot yet (?:done|finished)\b", re.I)),
    ("claims work as still its own", re.compile(r"\bstill (?:on me|mine)\b|\bis mine still\b", re.I)),
]
# Read per SENTENCE (split after . ! ? ; : and newlines, which stay in the sentence). The end-or-continuation
# anchor after "now" is what keeps "Running the suite now passes 284/284." and "it is now reverted" quiet.
_SENT_PREFIX = r"^[ \t]*(?:(?:[#>*+-]+|\[[ xX]\]|\d+[.)])[ \t]*)*"
_DOING = (r"(?:doing|re-?running|running|starting|kicking off|kicked off|working on|deploying|handling"
          r"|rebuilding|fixing|pushing|merging|on it)")
_END = r"[ \t]*(?:[.!;:]|\.\.\.|\u2026)?[ \t]*$"
# Three shapes. (1) verb + an OBJECT + now, then the sentence ends (any terminator, ; and : included, since
# the split keeps them) or continues (`,` `(` a dash, `and`): "Running it now; results shortly.", "Doing it
# now, then marking finished." (2) a BARE verb + now ending the sentence on . or ! only: "Running now." --
# a bare "Running now:" / "Starting now, the hook ..." is a status label or an adverb, not a promise.
# (3) "now <verb>". Plus "<subject> is running now." whose subject is not nothing/none/no ...
STARTS_NOW = re.compile(
    _SENT_PREFIX + r"(?:" + _DOING + r"[ \t]+(?!now\b)\S[^\n]{0,39}?[ \t]+now\b"
    r"(?:" + _END + r"|[ \t]*[,(\u2014\u2013-]|[ \t]+and\b)"
    r"|" + _DOING + r"[ \t]+now[ \t]*(?:[.!]|\.\.\.|\u2026)?[ \t]*$"
    r"|now " + _DOING + r"\b)", re.I)
IS_RUNNING_NOW = re.compile(
    r"^(?![^\n]*\b(?:nothing|none|no)\b)[^\n]*\b(?:is|are)[ \t]+(?:still[ \t]+)?"
    r"(?:running|deploying|rebuilding|building|merging|pushing)[ \t]+now" + _END, re.I)
SENTENCE_SPLIT = re.compile(r"(?<=[.!?;:\u2026])[ \t]+|\n")
# A fenced block only when BOTH fences open a line; an unpaired ``` hides nothing after it.
CODE_SPANS = re.compile(r"^[ \t]*```[^\n]*\n.*?^[ \t]*```[ \t]*$|`[^`\n]*`", re.S | re.M)
QUOTED_SPANS = re.compile(r'"[^"\n]*"|(?<!\w)\'[^\'\n]*\'(?!\w)')


def strip_code(raw):
    """`raw` with fenced blocks and inline code removed -- BEFORE normalize, which deletes the backticks."""
    return CODE_SPANS.sub(" ", raw)


def promise_hits(text):
    """The pronoun-free promise labels in `text` (already code-free and normalized); quoted spans skipped."""
    text = QUOTED_SPANS.sub(" ", text)
    hits = [label for label, p in PROMISE_WORDS if p.search(text)]
    if any(STARTS_NOW.search(sent) or IS_RUNNING_NOW.search(sent) for sent in SENTENCE_SPLIT.split(text)):
        hits.append("starts work now")
    return hits
# A gate, finding or plan that NAMES something "as the next step" is a durable record of who owes it, not
# the session saying it will act ("- The re-check gate names the rebuild as the next step."). Masked for the
# "states the next step" arm only, and only when that record is the WHOLE unit it sits in - its bullet (with
# any continuation lines), or else its whole paragraph - as one sentence: the noun followed at once by a
# recording verb, nothing after the span but an optional ".", and no word in the unit naming anyone inside
# the session or a condition (RECORDED_NEXT_EXCLUDE). Anything else beside it ("... as the next step. Doing it
# now.", "Once the gate closes I deploy as the next step") keeps the veto.
RECORDED_NEXT = re.compile(
    r"\b(?:gates?|findings?|plans?|dossiers?|records?)[ \t]+(?:names?|records?|lists?|tracks?)\b"
    r"[^.,:;\n]{0,80}\bas the next step\b", re.I)
RECORDED_NEXT_EXCLUDE = re.compile(
    r"\b(?:I|I'\w+|me|my|myself|mine|we|us|our|this session|the session|this agent|Claude"
    r"|after|once|when|until|if|before|then)\b", re.I)
BULLET = re.compile(r"[ \t]*(?:[-*+>]|\d+[.)])[ \t]+")


def _record_unit(text, start, end):
    """The bullet (plus its continuation lines) or, for a non-bullet line, the paragraph holding text[start:end],
    as (unit_start, unit_end)."""
    ls = text.rfind("\n", 0, start) + 1
    if BULLET.match(text, ls):
        us = ls
    else:
        blank = [b.end() for b in re.finditer(r"\n[ \t]*\n", text[:start])]
        us = blank[-1] if blank else 0
    ue = text.find("\n", end)
    while ue != -1:
        nxt = text.find("\n", ue + 1)
        line = text[ue + 1:nxt if nxt != -1 else len(text)]
        if not line.strip() or BULLET.match(line):
            break
        ue = nxt
    return us, (ue if ue != -1 else len(text))


def _mask_recorded_next(text):
    def mask(m):
        us, ue = _record_unit(text, m.start(), m.end())
        unit = BULLET.sub("", text[us:ue], count=1) if BULLET.match(text, us) else text[us:ue]
        whole = re.fullmatch(r"[ \t]*[^.!?,:;()\n]*%s[ \t]*\.?[ \t]*" % re.escape(m.group(0)), unit.strip("\n"))
        return " " if whole and not RECORDED_NEXT_EXCLUDE.search(unit) else m.group(0)
    return RECORDED_NEXT.sub(mask, text)


def _mask_gated_footer(text):
    """`text` with "still running/waiting" masked inside a policy-footer entry (its bullet, else its paragraph)
    that also names a TERMINAL_STATE, and only when nothing else in that entry hedges. Read only under an
    unhedged SAFE TO CLOSE yes; every other NEGATIVE arm, promise and question still reads the unmasked text.
    Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 4 step 5."""
    heads = list(FOOTER_HEAD_RE.finditer(text))
    if not heads:
        return text
    foot = heads[0].start()

    def mask(m):
        if m.start() < foot:
            return m.group(0)
        us, ue = _record_unit(text, m.start(), m.end())
        us = max(us, foot)
        unit = text[us:ue]
        # Only THIS match leaves the entry: a second "still ..." in it is a hedge and keeps the veto.
        rest = TERMINAL_STATE.sub(" ", text[us:m.start()] + " " + text[m.end():ue])
        return " " if TERMINAL_STATE.search(unit) and not hedged(rest) else m.group(0)
    return STILL_RUNNING.sub(mask, text)


def neg_hits(text, gated_footer=False):
    """The NEGATIVE labels found in `text`, with a recorded next step masked for "states the next step" and,
    when `gated_footer`, a gated footer entry masked for "still running/waiting"."""
    def read(label):
        if label == "states the next step":
            return _mask_recorded_next(text)
        if label == "still running/waiting" and gated_footer:
            return _mask_gated_footer(text)
        return text
    return [label for label, p in NEG_RE if p.search(read(label))]


POS_RE = [(label, re.compile(p, re.I)) for label, p in POSITIVE]
NEG_RE = [(label, re.compile(p, re.I)) for label, p in NEGATIVE]
STILL_RUNNING = dict(NEG_RE)["still running/waiting"]
LIMIT_RE = re.compile(r"You've hit your (?:session|weekly|usage) limit", re.I)
# The record Claude Code writes after a built-in command it answered itself.
LOCAL_OUTPUT = ("<local-command-stdout", "<local-command-stderr")
CLEAR = ("<command-name>/clear</command-name>",)
# User-typed turns that are NOT a new instruction.
NON_PROMPT = ("<task-notification>", "This session is being continued", "<local-command",
              "Caveat:", "<system-reminder>", "[Request interrupted")


def normalize(text):
    """Fold curly quotes and drop markdown emphasis (`*`, backticks), so
    `**Safe to close:** no` and `isn\u2019t` read like their plain spellings.
    Underscores stay: they are part of POLICY_COMPLIANCE."""
    text = text.replace("\u2019", "'").replace("\u2018", "'").replace("\u201c", '"').replace("\u201d", '"')
    return re.sub(r"[*`]+", "", text)


# A question that asks for a decision or an action is never "answered in place".
ACTION_QUESTION = re.compile(r"^[ \t>-]*(?:should|shall|want|do you|which|would you)\b"
                             r"|\b(?:merge|proceed|close|delete|rebase|confirm)\b[^?\n]*\?", re.I)


def phrase_hits(text):
    """(positive labels, negative labels) for a final assistant message. The
    "safe to close" vetoes are read over the WHOLE message, because /unattended
    puts its headline first and its detail after."""
    bare = SAFE_LABEL.sub(_drop_safe_label, normalize(strip_code(text)))[-2500:]
    text = SAFE_LABEL.sub(_drop_safe_label, normalize(text))
    tail = text[-2500:]
    yes, no = safe_to_close(text)
    pos = ["SAFE TO CLOSE"] if yes else []
    foot = FOOTER_HEAD_RE.search(tail)
    for label, p in POS_RE:
        # A re-closeout is read in the reply above the policy footer only: a footer that says the session owes
        # nothing is footer_nothing_owed's to read (FOOTER-FINISHED), never a closeout phrase.
        for m in p.finditer(tail[:foot.start()] if foot and label in RECLOSEOUT else tail):
            # The WHOLE sentence holding the phrase, so a hedge before it counts too ("Once CI lands, nothing
            # is left to do."). The phrase may itself contain a hedge word ("needs no follow-up").
            start = m.start() + len(m.group(0)) - len(m.group(0).lstrip("\n"))
            if not HEDGE.search(sentence_around(tail, start).replace(m.group(0).lstrip("\n"), " ")):
                pos.append(label)
                break
    neg = ((["not safe to close"] if no else []) + neg_hits(tail[-1000:], gated_footer=yes and not no)
           + promise_hits(bare[-1000:]))
    if re.search(r"\bsession finished[ \t]*[:.\u2014\u2013-]?[ \t]*(?:no|not)\b", tail, re.I):
        neg.append("session finished: no")
    # The policy footer follows the real ending; a question sits above it. Cut at
    # the LAST footer heading, then drop rules and list markers before testing.
    heads = list(FOOTER_HEAD_RE.finditer(text))
    body = text[:heads[-1].start()] if heads else text
    for a, b in reversed(safe_quotes(body)):  # a quoted check name asks nothing
        body = body[:a] + " " + body[b:]
    body = re.sub(r"(?:\s|^)(?:-{3,}|\*{3,}|_{3,})\s*$", "", body.rstrip())
    # A question answered in place ("Was the deploy serving? Yes.") asks nothing.
    body = "\n".join(
        ln if ACTION_QUESTION.search(ln) else
        re.sub(r"\?[ \t]*(?:yes|no|none|n/a)[ \t]*[.;](?![ \t]*(?:or\b|/))", ".", ln, flags=re.I)
        for ln in body.split("\n"))
    closing = "\n\n".join(re.split(r"\n[ \t]*\n", body.strip())[-3:])
    if "?" in closing or re.search(r"(?:^|\n)[ \t>-]*(?:should|shall|want|do you|which|would you)\b[^\n]*\?",
                                    body, re.I):
        neg.append("its closing paragraphs ask a question")
    return pos, neg


def footer_nothing_owed(text):
    """The sentence in the footer that says the session owes nothing, else None.
    Only text from the first footer heading on counts. Any hedge word anywhere
    in the footer voids it, because a hedge there means the statement is
    conditional."""
    text = normalize(text)
    heads = list(FOOTER_HEAD_RE.finditer(text))
    if not heads:
        return None
    foot = text[heads[0].start():]
    m = NOTHING_OWED_RE.search(foot) or FOOTER_LANDED_RE.search(foot)
    if not m or FOOTER_HEDGE_RE.search(foot):
        return None
    return foot[max(0, m.start() - 40):m.end() + 40].replace("\n", " ").strip()


# Regression table for --selftest: final-message text -> True (a closeout),
# False (not a closeout), or "veto" (not a closeout AND an explicit veto - the
# stronger claim, and the one that stops a close on the runner-marker path, where
# no phrase is needed). Every row is a misreading an earlier pattern let through.
PHRASE_CASES = [
    ("**Safe to close:** no \u2014 PR open", "veto"),
    ("**Safe to close: NO**", "veto"),
    ("Safe to close: no (PR #12 still in the train)", "veto"),
    ("Safe to close: no\nPR #12 is open", "veto"),
    ("Safe to close: not yet \u2014 CI running", "veto"),
    ("It is not yet safe to close: PR open", "veto"),
    ("This isn\u2019t safe to close yet", "veto"),
    ("Is it safe to close? Not until the build lands.", "veto"),
    ("NOT SAFE TO CLOSE \u2014 2 blockers.\n" + "detail line\n" * 15 + "Once #12 merges it will be safe to close.",
     "veto"),
    ("the session is marked finished once #1804 is green", False),
    ("PR #1850 needs no follow-up, and I changed nothing.", True),
    ("The test run finished clean. No follow-up PR is needed. I left the worktree in place.", True),
    ("This needs no follow-up once CI is green.", False),
    ("It needs no follow-up, but #12 is still open and I'll check it tomorrow.", "veto"),
    ("Does this need follow-up? No follow-up needed if you agree.", "veto"),
    ("Session finished: no \u2014 blocked", "veto"),
    ("I'm not marking the session finished, and I haven't registered a gate.", "veto"),
    ("this session is finished once #1804 is green and in the merge train.", False),
    ("Which do you want, A or B?", "veto"),
    ("I will check back after CI.", "veto"),
    ("We\u2019ll watch the merge queue.", "veto"),
    ("Done. Blocked on the operator's decision about Q3.", "veto"),
    ("Is this safe to close?\n\nPOLICY_COMPLIANCE: clauses applied: none.", "veto"),
    ("Are we safe to close? CI is still red.", "veto"),
    ("I don't think it's safe to close yet; PR #12 is open.", "veto"),
    ("Nothing here is safe to close.", "veto"),
    ("Safe to close: yes, once CI is green.", "veto"),
    ("Safe to close? Unclear \u2014 the build log is truncated.", "veto"),
    ("SAFE TO CLOSE \u2014 except the PR still needs your approval.", "veto"),
    ("Probably safe to close.", "veto"),
    ("Safe to close? I believe so.", "veto"),
    ("Safe to close: no changes are pushed yet, so not really.", "veto"),
    ("Which option, A or B?\n\n**POLICY_COMPLIANCE**\n- Clauses applied: none.", "veto"),
    ("Session finished \u2014 but I still need your approval on #12.", "veto"),
    ("SAFE TO CLOSE: I allocated no trees, and the only commit went in through plumbing.", False),
    ("SAFE TO CLOSE \u2014 this session allocated no worktrees, and nothing exists only on disk.", False),
    # "0 trees checked" alone may mean nothing could be checked (coord unreachable): a no.
    ("Step 4.9 verdict: SAFE TO CLOSE \u2014 0 trees checked, nothing only on disk.", False),
    ("Step 4.9 verdict: SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk.", True),
    ("**SAFE TO CLOSE**\n\nNo worktrees.", True),
    ("The only local change was a git fetch, so it is safe to close.\n- Findings: none.", False),
    ("I don't think it is safe to close.", "veto"),
    ("Probably it is safe to close.", "veto"),
    ("After #12 lands it is safe to close.", "veto"),
    ("As soon as you approve #12, it is safe to close.", "veto"),
    ("Approve #12. Then it is safe to close.", "veto"),
    ("If the operator approves: safe to close.", "veto"),
    ("After #12 merges: safe to close.", "veto"),
    ("Not yet: safe to close.", "veto"),
    ("Safe to close: 1 tree has uncommitted work.", "veto"),
    ("Safe to close: 0 of 2 trees verified.", False),
    ("Safe to close: nothing is pushed.", False),
    ("Safe to close: nothing has been verified.", False),
    ("Safe to close: this session has unpushed commits on a local branch.", "veto"),
    ("Safe to close: I left 2 uncommitted files in the worktree.", "veto"),
    ("Safe to close: yes \u2014 after you merge #12.", "veto"),
    ("Safe to close: yes, provided CI passes.", "veto"),
    ("Safe to close: yes, assuming CI passes.", "veto"),
    ("Safe to close: yes, though the gate is open for your approval.", "veto"),
    ("Which do you want, A or B?\n\nPOLICY_COMPLIANCE: ok", "veto"),
    ("I recorded a POLICY_GAP for X.\n\nShould I also file Y?\n\n---\n\n**POLICY_COMPLIANCE**\n- none", "veto"),
    ("SAFE TO CLOSE \u2014 1 tree checked (the shared checkout, read-only, clean); nothing exists only on disk.",
     True),
    ("SAFE TO CLOSE \u2014 2 trees checked, 1 has work only on disk.", "veto"),
    ("SAFE TO CLOSE \u2014 0 trees checked (coord unreachable).", "veto"),
    ("Safe to close: yes; operator approval on #12 is outstanding.", False),
    ("SAFE TO CLOSE\n\nBut first: approve #12 in the merge train.", "veto"),
    ("SAFE TO CLOSE \u2014 1 tree checked; its branch has commits absent from origin.", False),
    ("Want me to open the PR? Reply yes and I'll do it.", "veto"),
    ("Which one?\n\n**Policy summary**\n- none", "veto"),
    ("Shall I merge it?\n\n## Policy compliance\n- none", "veto"),
    ("Should I proceed?\n\nClauses applied: none.", "veto"),
    ("Session finished. Over to you for the merge decision.", "veto"),
    ("I marked the session finished. Next: you decide whether to merge #12.", "veto"),
    ("SAFE TO CLOSE \u2014 0 trees touched, nothing only on disk; hand-off declared for 0.", True),
    ("SAFE TO CLOSE \u2014 7 trees checked, nothing only on disk; hand-off declared for 6", True),
    ("**Safe to close:** nothing exists only on disk, and no worktree hand-off was needed.", True),
    ("Should I also delete the branch?\n\nSummary: PR #12 open, CI green.", "veto"),
    ("Was the deploy serving before the move? Yes. #1916 merged.\n\nSAFE TO CLOSE \u2014 1 tree checked, "
     "nothing only on disk.", True),
    ("Safe-to-close: no \u2014 PR #12 is open.", "veto"),
    ("SAFE_TO_CLOSE: false", "veto"),
    ("Safe to close: false \u2014 1 tree is dirty.", "veto"),
    ("Next I'm going to start Phase 3.", "veto"),
    ("Remaining: Phase 3 is not started.", "veto"),
    ("I need your decision on A vs B.", "veto"),
    ("Do you want A or B. Tell me which.", "veto"),
    ("The session was marked finished by mistake; I am continuing.", False),
    ("The headline format is:\n- SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk.", False),
    ("Example output:\n\nSAFE TO CLOSE \u2014 2 trees checked, nothing only on disk.\n\nI have not run it yet.", False),
    ("SAFE TO CLOSE \u2014 1 tree checked (unreachable)", "veto"),
    ("Merge #12 now? Yes or No", "veto"),
    ("Proceed with the deploy? Yes/No", "veto"),
    ("Rebase onto main? yes / no", "veto"),
    ("Close the stale PRs too? No action taken until you confirm.", "veto"),
    ("Step 4.9 output:\nNOT SAFE TO CLOSE \u2014 2 trees hold work only on disk.", "veto"),
    ("Step 4.9 prints:\n\nSafe to close: no \u2014 PR #12 open.", "veto"),
    ("Here is what Step 4.9 prints:\nNOT SAFE TO CLOSE \u2014 1 tree dirty.\n\nPOLICY_COMPLIANCE: ok", "veto"),
    ("SAFE TO CLOSE \u2014 2 trees checked, nothing only on disk; hand-off declared for 2.", True),
    ("- Clauses not followed: none.\n\nSAFE TO CLOSE. I made no worktrees.", True),
    ("Treat it as not told.\n\nSAFE TO CLOSE: 1 tree checked, nothing only on disk.", True),
    ("- Safe to close. No leftover processes from this session.", True),
    ("It will be safe to close once #12 lands.", "veto"),
    ("Safe to close? No - the PR is still open.", "veto"),
    ("SAFE TO CLOSE: no worktrees allocated or edited, nothing exists only on disk.", True),
    ("**Safe to close:** nothing exists only on disk, and no worktree hand-off was needed.", True),
    ("I left no worktrees and no stray processes; the one local ref I created was deleted. Safe to close.", False),
    ("**Safe to close: yes.** I checked two working trees.", True),
    ("**Hand-off safety:** safe to close \u2014 1 tree checked, nothing only on disk.", True),
    ("- **Session finished.** The runner marked it locally.", True),
    ("The session is marked finished on the runner.", True),
    # Plan 2026-10-23-wind-down-idle-check-mistakes-the-completed-turn, Phase 4.
    ("Safe to close: no work exists only on disk", True),
    ("Safe to close: no work exists only on disk.", True),
    ("Safe to close: no work is left only on this machine's disk.", True),
    ("SAFE TO CLOSE \u2014 1 tree checked, no work exists only on disk.", True),
    ("Safe to close: no work was pushed", "veto"),
    ("Safe to close: no work is done yet", "veto"),
    ("**Safe to close?** SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk.", True),
    ("**Safe to close?** SAFE TO CLOSE \u2014 no worktrees allocated, nothing exists only on disk.", True),
    ("**Safe to close?** NOT SAFE TO CLOSE \u2014 1 tree dirty.", "veto"),
    ("**Safe to close?** SAFE TO CLOSE \u2014 once CI is green.", "veto"),
    ("**Safe to close?** Safe to close if CI passes.", "veto"),
    ("This session left nothing only on disk, so it was safe to close.", False),
    # A gate that NAMES the next step is a record of who owes it, not the session saying it will act.
    ("- The finding and the re-check gate name the rebuild as the next step.\n\n"
     "Session marked finished.\n\nSAFE TO CLOSE. No trees were allocated and nothing exists only on disk.", True),
    # ...but a condition anywhere in that sentence is read as the session's own promise (the safe direction).
    ("- The finding and the re-check gate name the rebuild as the next step once its owner is proven dead.\n\n"
     "Session marked finished.\n\nSAFE TO CLOSE. No trees were allocated and nothing exists only on disk.", "veto"),
    ("Next step: Phase 3, which the gate tracks.", "veto"),
    ("The gate is open; I'll check it as the next step.", "veto"),
    # The recorded-next-step mask is third person only: a promise beside a gate/plan word still vetoes.
    ("Session marked finished.\nThe plan is done, I'm going to rebase as the next step.", "veto"),
    ("Session marked finished.\nPer the finding, I'm going to file the PR as the next step.", "veto"),
    ("Session marked finished.\nAfter the gate clears, I will run Phase 5 as the next step.", "veto"),
    ("Session marked finished.\nThe plan: I run Phase 5 as the next step.", "veto"),
    ("Session marked finished.\nOnce the gate closes I deploy as the next step.", "veto"),
    ("SAFE TO CLOSE.\n- The re-check gate names the rebuild as the next step.", True),
    ("- Safe to close? Safe to close, I think.", "veto"),
    ("Safe to close? Safe to close-ish.", "veto"),
    ("nothing only on disk is safe to close", "veto"),
    ("Nothing only on disk, it is safe to close.", "veto"),
    ("Session marked finished.\nThe plan lists me running migrations as the next step.", "veto"),
    ("Session marked finished.\nThe plan names my session's rebuild as the next step.", "veto"),
    ("Session marked finished.\nThe gate records that this session deploys as the next step.", "veto"),
    ("Session marked finished.\nThe plan names us as owners of the deploy as the next step.", "veto"),
    ("Session marked finished.\nThe plan names the deploy that our team runs as the next step.", "veto"),
    ("Session marked finished.\nThe plan names Phase 5 which the session runs now as the next step.", "veto"),
    ("Session marked finished.\nFindings record the deploy as the next step, then me.", "veto"),
    ("Session marked finished.\nRecords list the deploy as the next step\u2014I'll handle it.", "veto"),
    ("Safe to close. Not really though, CI is red.", "veto"),
    ("Safe to close! Except the stash.", "veto"),
    ("Safe to close? Safe to close. Not really though, CI is red.", "veto"),
    # The mask holds only when the record is its whole bullet or paragraph.
    ("Session marked finished.\nThe gate names the rebuild as the next step. Doing it now.", "veto"),
    ("Session marked finished.\nThe gate names the rebuild as the next step. Starting on it right away.", "veto"),
    ("Session marked finished.\nThe plan names the deploy as the next step, starting now.", "veto"),
    ("Session marked finished.\nThe gate names the rebuild (mine) as the next step.", "veto"),
    ("Session marked finished.\nGates name the deploy as the next step (assigned to myself).", "veto"),
    ("Session marked finished.\nThe plan tracks Phase 5 as the next step and myself as owner.", "veto"),
    ("Session marked finished.\nThe gate names the rebuild as the next step for this agent.", "veto"),
    ("Session marked finished.\nThe plan names Claude as the owner of the deploy as the next step.", "veto"),
    ("Session marked finished.\nThe gate names the rebuild as the next step.\nDoing it now.", "veto"),
    # Pronoun-free promises (follow-up edge 67ecc4d7): no I/we, still owed.
    ("Doing it now.", "veto"),
    ("Running Phase 5 now.", "veto"),
    ("Not done yet - the rebuild is still mine.", "veto"),
    ("Session marked finished.\nDoing it now.", "veto"),
    ("Session marked finished.\nRunning Phase 5 now.", "veto"),
    ("Session marked finished.\nNot done yet - the rebuild is still mine.", "veto"),
    ("Session marked finished.\n\n- The gate names the rebuild as the next step.\n- Doing it now.", "veto"),
    # Review round: the same promises on ONE line, mid-line, behind a list/heading/checkbox marker.
    ("Session marked finished. Doing it now.", "veto"),
    ("All landed. Running Phase 5 now.", "veto"),
    ("The rebuild is next; doing it now.", "veto"),
    ("Session marked finished.\n- [ ] Deploying it now.", "veto"),
    ("# Rebuilding the index now", "veto"),
    ("On it now.", "veto"),
    ("Session marked finished. Now doing the rebuild.", "veto"),
    ("Session marked finished. Handling the migration now!", "veto"),
    ("Not yet done - the deploy.", "veto"),
    ("Session marked finished. The rebuild is still on me.", "veto"),
    ("The deploy is mine still.", "veto"),
    # ...while "now" describing a quiet state is not a promise.
    ("Nothing is running now.", False),
    ("SAFE TO CLOSE. Running the suite now shows 284 green.", True),
    ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk. Running now: nothing.", True),
    ("SAFE TO CLOSE: 1 tree checked, nothing only on disk.\nNothing is running now.", True),
    ("Step 4.9 verdict: SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk. The train is quiet now.", True),
]

# Second review round: terminators the split keeps (; : ... \u2026), a continuation after "now", wider verbs,
# "<subject> is running now", and an unpaired ``` that must hide nothing after it -- bare and after the
# finished marker.
_PROMISES_R2 = [
    "Running it now; results shortly.", "Running Phase 5 now: tests first.", "Running it now...", "Doing it now\u2026",
    "Use ``` fences for logs.\nDoing it now.", "Doing it now, then marking finished.",
    "Running Phase 5 now and will report back.", "Doing that now (Phase 5).", "Fixing the lint now.",
    "Pushing the branch now.", "Merging #12 now.", "Re-running CI now.", "Rerunning the suite now.",
    "Kicked off the rebuild now.", "The rebuild is running now.",
]
PHRASE_CASES += [(t, "veto") for t in _PROMISES_R2] + [("Session marked finished.\n" + t, "veto") for t in _PROMISES_R2]

# Plan 2026-11-08-finished-sessions-held-open-by-idle-chrome-and-hedge-words, Phase 2: a negated hedge, a note
# explaining a zero hand-off, an emphasised label and "no files of mine ... only on disk" read as the reports they
# are - synthetic strings modelled on the closeouts the old grammar vetoed. The vetoes after them must hold.
PHRASE_CASES += [
    ("SAFE TO CLOSE: 1 tree checked, nothing only on disk; hand-off declared for 0 (the scratch clone was never "
     "allocated, so there was nothing to declare).", True),
    ("**SAFE TO CLOSE** \u2014 no files of mine exist only on disk.", True),
    ("SAFE TO CLOSE \u2014 no files exist only on disk.", True),
    ("SAFE TO CLOSE: no edits are left only on disk.", True),
    ("SAFE TO CLOSE \u2014 1 tree checked; nothing exists only on disk except the runner's .claude/ command set; "
     "hand-off declared for 1.", "veto"),
    ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (not yet declared).", "veto"),
    ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (coord unreachable).", "veto"),
    ("Safe to close: no uncommitted changes, unpushed commits remain.", "veto"),
    ("Safe to close: nothing pushed, uncommitted work in the tree.", "veto"),
    ("Safe to close: no uncommitted changes, but 2 unpushed commits.", "veto"),
    ("Safe to close: no uncommitted changes, not yet pushed.", "veto"),
    ("Safe to close: no unpushed commits, unless the push failed.", "veto"),
    ("Safe to close: no work exists only on disk, but uncommitted files remain.", "veto"),
    ("Safe to close: no files were pushed", "veto"),
    ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: but CI is still red.", "veto"),
    ("**Safe to close? SAFE TO CLOSE** \u2014 1 tree checked, nothing only on disk, once CI is green.", "veto"),
    ("Safe to close? Safe to close \u2014 I think so?", "veto"),
    ('The "safe to close" check couldn\'t confirm that nothing is still owed.', "veto"),
    ("Coord still counts withdrawn gates as unfinished work, which means the session can never read as safe to "
     "close. I overrode that check with --force.", "veto"),
    ("Whether the session is safe to close is UNKNOWN: coord's obligations check timed out.", "veto"),
    ("Safe to close: no", "veto"),
    ("NOT SAFE TO CLOSE \u2014 1 tree", "veto"),
    ("Safe to close: not yet \u2014 the push is still running.", "veto"),
]
NO_VETO_CASES_PHASE2 = [
    "Safe to close: 1 tree checked; nothing exists only on disk (no uncommitted changes, unpushed commits or stashes).",
    "Safe to close: the worktree is clean, with no unpushed commits and no stashes.",
    "SAFE TO CLOSE: 2 trees checked, neither has uncommitted changes, and both commits are pushed.",
    "Safe to close: this session has no worktrees, claims or uncommitted work.",
    "Safe to close: this session has no allocated worktrees and no unpushed work.",
    "Safe to close: the tree is clean, without uncommitted work.",
    "Safe to close: nothing dirty, nothing only on disk.",
    "Safe to close: no failed checks, and nothing only on disk.",
    "- Worktree: clean and nothing unpushed, so it's safe to close.",
    "- Safe to close: 1 tree checked: clean, nothing unpushed, nothing that exists only on disk.",
    "- Worktrees: none touched, so nothing is uncommitted, unpushed or stashed. Safe to close: 0 trees checked, "
    "nothing exists only on disk.",
    "SAFE TO CLOSE \u2014 the worktree is clean and has no unpushed commits.",
    "**Safe to close? SAFE TO CLOSE** \u2014 0 trees: this session edited no files; it only changed coord state. "
    "No worktree hand-off was needed.",
    "SAFE TO CLOSE. The one worktree I checked has no uncommitted changes and no unpushed commits.",
    "Safe to close: no uncommitted changes or unpushed commits.",
    "- SAFE TO CLOSE: no session worktrees; the shared checkout is clean.",
    # review round 11: a zero with no hedge word in it never vetoed and must not start to
    "SAFE TO CLOSE. No open claims; PR #12 merged as 4f3a2c1.",
    "SAFE TO CLOSE. No stray processes; the gate watches PR #3.",
    "SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; no open PRs, PR #12 merged.",
    "SAFE TO CLOSE. No hand-off was needed: PR #12 merged.",
    "Unattended verdict: COMPLETE. Two units (the finding, the continuation report), all recorded. Safe to close.",
]
PHRASE_CASES += [("Safe to close: no changes", "veto"), ("Safe to close: no, uncommitted changes.", "veto"),
                 ("Safe to close: no uncommitted changes yet.", "veto"),
                 ("I am continuing the work. Safe to close.", "veto"),
                 # review round 1: `and` never continues a negated list; a counted label answer must not owe work
                 ("SAFE TO CLOSE: no uncommitted changes and failed to push the branch.", "veto"),
                 ("Safe to close: no stashes, and dirty files in the tree.", "veto"),
                 ("Safe to close: no unpushed commits, and uncommitted changes in qontinui-web.", "veto"),
                 ("Safe to close: no uncommitted changes and unpushed commits on main.", "veto"),
                 ("Safe to close: no claims and failed checks on PR #3.", "veto"),
                 ("Safe to close: no worktrees and dirty files in the shared checkout.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 I'll push it next.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 the PR is owed a fix", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: work remains on the branch.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the fix will land later.", "veto"),
                 ("SAFE TO CLOSE. The continuation is running in this session.", "veto"),
                 ("SAFE TO CLOSE. The continuation job is in progress.", "veto"),
                 ("SAFE TO CLOSE. Continuation in progress here.", "veto"),
                 # review round 2: a zero hand-off note saying the WORK was never done is no zero
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (the fix was "
                  "not committed)", "veto"),
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (couldn't push "
                  "the branch)", "veto"),
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (the worktree "
                  "was never pushed)", "veto"),
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (the branch is "
                  "not on origin)", "veto"),
                 ("Safe to close: nothing dirty here is safe to close", "veto"),
                 # review round 3
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (the fix commit "
                  "was never created).", "veto"),
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (the PR was "
                  "never opened).", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 3 trees checked: web clean, schemas has 2 local commits.",
                  "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: schemas is 3 commits ahead of origin.", "veto"),
                 ("Safe to close? SAFE TO CLOSE \u2014 2 trees touched: both have local edits.", "veto"),
                 ("Safe to close: no failed checks so far.", "veto"),
                 ("Safe to close: no unpushed commits on main from me.\nThe feature branch has 2.", "veto"),
                 ("Safe to close: neither has uncommitted changes from the rebase.", "veto"),
                 # review round 4: a masked zero beside local-only state; a note naming where work sits
                 ("Safe to close: no uncommitted changes, 3 commits ahead of origin.", "veto"),
                 ("Safe to close: no uncommitted changes and 2 local commits on the branch.", "veto"),
                 ("SAFE TO CLOSE: 1 tree checked, nothing dirty, the fix commit sits on a local branch only.", "veto"),
                 ("Safe to close: nothing unpushed here, 2 stashes kept.", "veto"),
                 ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 0 (the worktree "
                  "was never created, so the fix sits in the shared checkout).", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the migration broke prod.", "veto"),
                 # review round 5
                 ("Safe to close: no unpushed commits, 3 modified files in qontinui-web.", "veto"),
                 ("Safe to close: nothing unpushed, the tree has edits.", "veto"),
                 ("Safe to close: no uncommitted changes, 1 commit only in this clone.", "veto"),
                 ("Safe to close: no uncommitted changes, 1 unpublished commit.", "veto"),
                 ("Safe to close: no uncommitted changes, 2 commits to push.", "veto"),
                 ("Safe to close: no uncommitted changes and nothing pushed.", "veto"),
                 ("Safe to close: zero failed, 2 failing.", "veto"),
                 ("Safe to close: nothing unpushed here. Schemas has 2 unpushed commits.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: tests are failing on main.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the deploy is in progress.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the migration is running in prod.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the release job is mid-flight.", "veto"),
                 ("SAFE TO CLOSE. The continuation outcome: Phase 2 in progress here.", "veto"),
                 # review round 8: placing or counting words beside a masked zero; a watched PR in progress
                 ("Safe to close: no uncommitted changes; zero commits on origin.", "veto"),
                 ("Safe to close: no uncommitted changes, nothing on origin.", "veto"),
                 ("Safe to close: nothing dirty, the commit exists only in this worktree.", "veto"),
                 ("Safe to close: no unpushed commits, 2 commits on main in the shared checkout.", "veto"),
                 ("Safe to close: no uncommitted changes or unpushed commits: 2 on main.", "veto"),
                 ("Safe to close: no uncommitted changes; a hand-off is required for the worktree.", "veto"),
                 ("SAFE TO CLOSE.\nPR #3 checks in progress; a gate watches the merge.", True),
                 # review round 9: the clean remainder is a clause grammar, not a bag of clean words
                 ("Safe to close: no uncommitted changes; commits left on the branch.", "veto"),
                 ("Safe to close: no uncommitted changes; the commit is in the worktree.", "veto"),
                 ("Safe to close: no unpushed commits, the tree has changes.", "veto"),
                 ("Safe to close: no unpushed commits; the files are edited in the shared checkout.", "veto"),
                 ("Safe to close: nothing dirty; a stash exists.", "veto"),
                 ("Safe to close: no dirty files; the branch has commits.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the stash has work.", "veto"),
                 ("Safe to close: no uncommitted changes; I pushed nothing.", "veto"),
                 ("SAFE TO CLOSE. The continuation report: Phase 2 is underway in this session.", "veto"),
                 ("SAFE TO CLOSE. The continuation outcome is that I resume Phase 2 here.", "veto"),
                 # review round 10: only /unattended's printed continuation shapes; no free words in clean clauses
                 ("SAFE TO CLOSE. Continuation report, Phase 2 resumes in this session.", "veto"),
                 ("SAFE TO CLOSE. Continuation outcome \u2014 Phase 2 resumes in this session.", "veto"),
                 ("SAFE TO CLOSE\nContinuation outcome: work_abandoned \u2014 1 unit DROPPED.", "veto"),
                 ("Safe to close: no uncommitted changes; i made commits.", "veto"),
                 ("Safe to close: no uncommitted changes; hand-offs were needed.", "veto"),
                 ("Safe to close: no uncommitted changes; the broken worktree is unchanged.", "veto"),
                 ("Safe to close: 2 trees checked; one worktree has no uncommitted changes.", "veto"),
                 ("Safe to close: no uncommitted changes; the only change to the prod database is one tag", "veto"),
                 # review round 6
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: waiting on the release job.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the backfill is unfinished.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: deploy half done.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: CI is queued on #3.", "veto"),
                 ("Safe to close: no uncommitted changes, the fix sits in my worktree.", "veto"),
                 ("Safe to close: nothing dirty, the branch exists only here.", "veto"),
                 ("Safe to close: no failed checks, the build is broken.", "veto"),
                 ("Safe to close: no failing tests, 3 errors in the build.", "veto"),
                 ("Nothing dirty, the rollout is running, safe to close.", "veto"),
                 ("SAFE TO CLOSE \u2014 no uncommitted changes, rollout in flight", "veto"),
                 # review round 7: the rest of a masked sentence must be clean-state reporting, by construction
                 ("Safe to close: no uncommitted changes; the push was rejected by the hook.", "veto"),
                 ("Safe to close: nothing unpushed, the remote diverged.", "veto"),
                 ("Safe to close: no dirty files, the PR has conflicts.", "veto"),
                 ("SAFE TO CLOSE \u2014 no uncommitted changes; the commit was lost.", "veto"),
                 ("Safe to close: no uncommitted work, migration half-applied.", "veto"),
                 ("**Safe to close? SAFE TO CLOSE** \u2014 0 trees: the hotfix PR was rejected by review.", "veto"),                 ("**Safe to close? SAFE TO CLOSE** \u2014 1 tree checked, nothing only on disk; hand-off declared for 1. "
                  "The fix is not pushed.", "veto")]  # the label keeps its `?` when the line takes it back
# negative_headline (Phase 3): text -> whether it is an explicit negative headline.
NEG_HEADLINE_CASES = [
    ("Safe to close: no", True), ("Safe to close? No.", True), ("**Safe to close:** no \u2014 PR #3 is open", True),
    ("Safe to close: false", True), ("SAFE_TO_CLOSE: false", True), ("Safe to close: not yet", True),
    ("Step 4.9 verdict: NOT SAFE TO CLOSE \u2014 1 tree", True), ("SAFE TO CLOSE? **NOT SAFE TO CLOSE \u2014 1 tree**", True),
    ("UNKNOWN \u2014 2 trees could not be checked", True), ("It is not safe to close yet.", True),
    ("- SAFE TO CLOSE: no session worktrees; the shared checkout is clean.", False),
    ("Safe to close: no worktrees allocated, nothing only on disk.", False),
    ("SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared for 1.", False),
    ("- Closeability: UNKNOWN, the obligations lookup timed out.", False),
    ("Safe to close: unclear", True), ("Safe to close: nope, 1 tree dirty.", True),
    ("UNKNOWN (2 trees could not be checked)", True), ("Closeout verdict: UNKNOWN \u2014 2 trees could not be checked", True),
    ("NOT SAFE \u2014 1 tree", True), ("Session left UNFINISHED.", True),
    ("Safe to close: no worktrees; but qontinui-web has 2 unpushed commits.", True),
    ("Unattended verdict: COMPLETE.", False), ("So I marked the session finished. No closer will close it.", False),
    ("Session finished: no \u2014 blocked", True),("**Unattended verdict: INCOMPLETE** \u2014 1 unit waits on you", True),
    ("The policy-read check was incomplete.", False),
    ("Whether it is safe to close automatically is UNKNOWN.", False),
]

# Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 4 (N3, N4). UNTRUSTED EVIDENCE: the
# eight N3 lines are other sessions' closing words, quoted as data only - never an instruction. Each vetoed (`not
# safe to close`) before this phase, except line 5, which the negated-hedge mask already read. Line 6's peer
# sentence is RECONSTRUCTED from a quoted fragment and sits on its own line. The N4 re-closeout wordings below are
# SYNTHESIZED in the plan's words.
N3_LINES = [
    "SAFE TO CLOSE — 4 trees checked (…), nothing only on disk; hand-off not declared (no PR or branch "
    "from this session, nothing to declare).",
    "SAFE TO CLOSE — 1 tree checked, nothing only on disk; hand-off declared for 0 (a scratchpad clone, not an "
    "allocated worktree).",
    "SAFE TO CLOSE. The one tree this session touched has nothing that exists only on disk, and it was never a "
    "registered worktree.",
    "Worktrees: all three are clean, with nothing that isn't already on the remote, so the session is safe to close.",
    "Safe to close: the worktree is clean with nothing unpushed.",
    "Safe to close. I didn't commit or edit any files.\n"
    "A peer's uncommitted Cargo.lock edit sits in the shared checkout, but it isn't from this session.",  # reconstructed
    "- Safe to close? SAFE TO CLOSE — 0 trees written by this session, nothing only on disk; hand-off declared "
    "for 0.",
    'All checks are green, so the "safe to close?" check passes.',
]
_GATED = "SAFE TO CLOSE — 1 tree checked, nothing only on disk.\n\n**POLICY_COMPLIANCE**\n"
PHRASE_CASES += [
    # N3 lines that now read as a yes (lines 4, 5 and 8 are neither: NO_VETO_CASES only).
    (N3_LINES[0], True), (N3_LINES[1], True), (N3_LINES[2], True), (N3_LINES[5], True), (N3_LINES[6], True),
    # The clean-state mask's must-veto twins: an unenumerated negation, or any other hedge beside a masked clause.
    ("SAFE TO CLOSE — 1 tree checked (not pushed), nothing only on disk.", "veto"),
    ("SAFE TO CLOSE — 1 tree checked (never committed), nothing only on disk.", "veto"),
    ("SAFE TO CLOSE — 1 tree checked, nothing only on disk; hand-off declared for 0 (not yet pushed).", "veto"),
    ("Safe to close. Nothing unpushed, but #12 is still open.", "veto"),
    ("SAFE TO CLOSE — 1 tree checked, nothing only on disk; hand-off not declared (not pushed).", "veto"),
    ("Safe to close: nothing unpushed yet.", "veto"),
    ("Safe to close. I didn't commit or edit any files, but I left a stash.", "veto"),
    ("SAFE TO CLOSE. It was never a registered worktree, but it holds 2 commits.", "veto"),
    ("SAFE TO CLOSE\n\nNothing unpushed until #12 lands.", "veto"),
    ("SAFE TO CLOSE\n\nNothing unpushed.", True),
    # The self-answered label reads like the colon form; a hedged report keeps its `?`.
    ("- Safe to close? SAFE TO CLOSE — 2 trees reviewed, nothing only on disk.", False),
    ("- Safe to close? SAFE TO CLOSE — 2 trees reviewed, but #12 is open.", "veto"),
    ("- Safe to close? SAFE TO CLOSE — 2 trees reviewed, 3 local commits.", "veto"),
    # Quoted check names are neither; any other quoted question, or a hedged quoted answer, still vetoes.
    ('One question for you: "should I merge #12 now?"', "veto"),
    ('The "safe to close?" check passes. Should I merge #12?', "veto"),
    ('The check printed "NOT SAFE TO CLOSE".', "veto"),
    ('The "safe to close?" check has not run yet.', "veto"),
    # N4 re-closeouts...
    ("This was a scheduled reminder, and the closeout it was for already finished, so nothing is left to do.", True),
    ("This session owes nothing further, and it stays marked finished.", True),
    # ...and their hedged twins, a hedge BEFORE the phrase included.
    ("Once CI lands, nothing is left to do.", False),
    ("If #12 merges, this session owes nothing further.", False),
    ("After the operator approves #12, it stays marked finished.", False),
    ("Nothing is left to do until CI lands.", False),
    ("This session owes nothing yet.", False),
    ("Nothing is left to do, but #12 is still open.", False),
    # A gated footer under an unhedged SAFE TO CLOSE: a terminal state masks "still running" in that entry only.
    (_GATED + "- **Final state:** finished with CI still running, behind gates.\n- Clauses applied: finish-to-zero.",
     True),
    (_GATED + "- Final state: #12 in-train, CI still running.", True),
    (_GATED + "- Final state: CI still running, watched by gate 4f2a-77.", True),
    (_GATED + "- Final state: finished with CI still running, behind gates.\n- I'll check the gate tomorrow.", "veto"),
    (_GATED + "- Final state: CI still running, behind gates, but #12 is open.", "veto"),
    (_GATED + "- Final state: CI still running.\n- Gates: behind gates.", "veto"),
    (_GATED + "- Final state: CI still running, gated; still waiting on review.", "veto"),
    (_GATED + "- Final state: CI still running, gated. Want me to merge?", "veto"),
    ("SAFE TO CLOSE — 1 tree checked, nothing only on disk.\nCI is still running, behind gates.\n\n"
     "**POLICY_COMPLIANCE**\n- none", "veto"),
    ("Safe to close: no.\n\n**POLICY_COMPLIANCE**\n- Final state: CI still running, behind gates.", "veto"),
    # ...and only under a SAFE TO CLOSE yes: with none (or one the grammar reads as neither) it still vetoes.
    ("Work is done.\n\n**POLICY_COMPLIANCE**\n- Final state: CI still running, behind gates.", "veto"),
    ("The work is done, so it is safe to close.\n\n**POLICY_COMPLIANCE**\n- Final state: CI still running, gated.",
     "veto"),
]

# Real closeouts from finished sessions in wordings the grammar does not list.
# They need not read as a yes, but they must NOT veto a runner finished marker.
NO_VETO_CASES = [
    "SAFE TO CLOSE \u2014 1 tree checked, nothing left only on disk; hand-off declared for 1. The worktree is "
    "clean, nothing is unpushed.",
    "SAFE TO CLOSE \u2014 0 trees allocated or edited by this session, nothing only on disk; hand-off declared for 0.",
    "- SAFE TO CLOSE: 0 trees allocated, nothing left only on disk, nothing to hand off.",
    "Step 4.9 verdict: SAFE TO CLOSE \u2014 0 trees checked, nothing only on disk. I allocated no worktrees.",
    "SAFE TO CLOSE \u2014 1 tree checked (the notes repo, shared; nothing of this session's is only on disk); "
    "peer state: 3 staged files belong to another session; hand-off declared for 0.",
    "SAFE TO CLOSE: no worktrees allocated and no commits, so nothing of this session's exists only on disk.",
    "The only local change was a git fetch, so it is safe to close.",
    "This session left nothing only on disk, so it was safe to close.",
    "Nothing is running now.",
    "SAFE TO CLOSE \u2014 nothing is running now and nothing exists only on disk.",
    "The rebuild is done and nothing is pending now.",
    "Running now: nothing.",
    "running now: 0 jobs",
    "Running the suite now passes 284/284.",
    "The branch is fine; it is now reverted.",
    "Working on main is now forbidden.",
    'The plan says "Doing it now." about phase 2.',
    "The log read 'running it now' at 12:00.",
    "Output:\n```\nDoing it now.\n```\nAll landed.",
    "The banner `Running Phase 5 now.` is gone.",
    "SAFE TO CLOSE. Running the suite now shows 284 green.",
    "Starting now, the hook blocks raw cargo.",
    "No jobs are running now.",
] + NO_VETO_CASES_PHASE2 + N3_LINES + [
    "- Safe to close? SAFE TO CLOSE — 2 trees reviewed, nothing only on disk."]


# windDown block -> True (vetoes a close) / False (leaves it to this script's own checks).
WIND_DOWN_CASES = [
    (None, False), ({}, False), ({"eligibility": "eligible", "reason": None}, False),
    ({"eligibility": "eligible", "reason": None, "idleEvidence": "glyph:x"}, False),  # evidence alone: no veto
    ({"eligibility": "ineligible", "reason": "grid_busy"}, True),          # the older build's vocabulary
    ({"eligibility": "ineligible", "reason": "sideband_working"}, True),
    ({"eligibility": "ineligible", "reason": "record_busy"}, True),        # the newer build's vocabulary
    ({"eligibility": "ineligible", "reason": "record_waiting"}, True),
    ({"eligibility": "ineligible", "reason": "record_shell"}, True),
    ({"eligibility": "ineligible", "reason": "record_unknown"}, True),
    ({"eligibility": "ineligible", "reason": "transcript_age_unknown"}, True),
    ({"eligibility": "ineligible", "reason": "prompt_not_ready", "idleEvidence": "no_prompt"}, True),
    ({"eligibility": "ineligible", "reason": "not_finished"}, False),
    ({"eligibility": "ineligible", "reason": "live_children"}, False),
    ({"eligibility": "unknown", "reason": "work_status_unknown"}, False),
    ({"eligibility": "unknown", "reason": "grid_unknown"}, False),
    ({"eligibility": "ineligible", "reason": "some_future_reason"}, True),  # unrecognised: fail closed
]


WIN_PANE_EXE = r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"
WIN_PANE = WIN_PANE_EXE + r''' -NoLogo -NoExit -Command ". 'D:\t\qontinui-shell-integration.ps1'"'''
WIN_TOOL_BASH = r'''"C:\Program Files\Git\bin\bash.exe" -c -l "export TEMP='C:\Temp' && eval 'sleep 60'"'''


FT = "Review found nothing to follow up.\n\n**POLICY_COMPLIANCE**\n"
# The custody probe's stand-in for the selftests (git_rev_parse_dirs, finished-criteria
# condition 3): git's answer per directory. A directory no case set up fails loudly.
LINKED_CWD = "/w/agent-worktrees/a1/repo"
GIT_ANSWERS = {
    LINKED_CWD: (0, "/w/repo/.git/worktrees/a1\n/w/repo/.git\n", ""),  # a linked worktree
    "/w/repo": (0, ".git\n.git\n", ""),  # a primary checkout, at its top (git answers relative paths)
    "/w/repo/sub": (0, "/w/repo/.git\n/w/repo/.git\n", ""),  # ...and below its top
    "/w/no-repo": (128, "", "fatal: not a git repository (or any of the parent directories): .git\n"),
    "/w/broken": (128, "", "fatal: detected dubious ownership in repository at '/w/broken'\n"),
    "/w/slow": None,  # git could not be run, or did not finish
}


# A session's own scheduled wake (a /loop or ScheduleWakeup turn), as a META user record: it opens a turn
# (turn_open) but is never a prompt. Synthetic, for the selftests.
SCHEDULED_WAKE = "Scheduled wake-up: re-run the deploy check."


def fake_git_dirs(cwd, timeout=None):
    if cwd not in GIT_ANSWERS:
        raise AssertionError(f"the custody probe asked git about {cwd!r}, which no selftest case set up")
    return GIT_ANSWERS[cwd]
# A replayed Linux pane whose input prompt is empty (the selftests' idle screen).
EMPTY_PROMPT_SCREEN = "\x1b[H\r\x1b[17B\u276f\xa0\r\x1b[50C\x1b[2Bfor agents\x1b[K\x1b[20;1H\x1b[18;3H\x1b[?25h"
# Two SYNTHETIC Windows (ConPTY) panes, CONPTY_FIXTURE_COLS wide, whose input prompt is empty: the
# streams of plan 2026-11-07-triage-sessions-every-page-run-defects-aftercare-veto-reader-revival-and-
# concurrent-passes, Phase 1, published beside that plan as <its stem>.phase1-streams.json (blob 4250c279).
# Their escape structure follows one read-only, structure-only read of a live scrolled pane; every
# printable character is neutral text or Claude Code's own chrome. ConPTY writes a full-width row with
# no line break, so both read empty only when the replay wraps at the width.
CONPTY_FIXTURE_COLS = 50
# A pane scrolled up: the sticky header (the last sent prompt, row 1), the scroll pill, the input box (its
# top rule, the empty input row 4, its bottom rule), the status line; then a redraw of the header after the
# status line, its grey set BEFORE the cursor-home that places it, as ConPTY sends it. That header is the
# stream's last marker.
SCROLLED_PROMPT_SCREEN = (
    "\x1b[H\u276f synthetic prompt text" + " " * 27 + "\x1b[msynthetic reply"
    + "\x1b[38;2;255;255;255m\x1b[48;2;55;55;55m Jump to bottom (ctrl+End) \u2193 \x1b[m" + " " * 6 + "\r\n"
    + "\u2500" * 38 + " synthetic \u2500\x1b[m\u276f\xa0\x1b[K\x1b[38;2;136;136;136m\r\n" + "\u2500" * 50
    + "\x1b[m  \x1b[38;2;255;107;128m\u23f5\u23f5 bypass permissions on \x1b[38;2;153;153;153m(shift+tab to cycle)"
    + "\x1b[K\x1b[4;3H\x1b[?25h"
    + "\x1b[?25l\x1b[m\x1b[38;2;80;80;80m\x1b[48;2;55;55;55m\x1b[H\u276f synthetic prompt text" + " " * 27
    + "\x1b[4;3H\x1b[?25h")
# An idle pane, not scrolled: one transcript row, the input box (the empty input row 3), the status line.
IDLE_CONPTY_PROMPT_SCREEN = (
    "\x1b[Hsynthetic reply text" + " " * 30 + "\r\n"
    + "\u2500" * 38 + " synthetic \u2500\x1b[m\u276f\xa0\x1b[K\x1b[38;2;136;136;136m\r\n" + "\u2500" * 50
    + "\x1b[m  \x1b[38;2;255;107;128m\u23f5\u23f5 bypass permissions on \x1b[38;2;153;153;153m(shift+tab to cycle)"
    + "\x1b[K\x1b[3;3H\x1b[?25h")
FOOTER_CASES = [
    (FT + "- **Final state:** #1816 is landed and nothing further is owed.\n- **Clauses applied:** x", True),
    (FT + "- Final state: nothing owed.\n- POLICY_GAPs: none.", True),
    (FT + "- **Final state:** this session owes nothing.", True),
    (FT + "- **Final state:** #3201 is landed. There are no follow-ups.\n- **Clauses applied:** x", True),
    (FT + "- **Final state:** #1848 is landed. Nothing is gated or deferred.\n- Clauses applied: "
          "`finish-to-zero`, with nothing owed after the review.", True),
    (FT + "- **Final state:** #12 is in-train; nothing further is owed.", True),
    (FT + "- **Final state:** gated (gate_id 4f2a); nothing further is owed.", True),
    (FT + "- **Final state:** Phase 2 deferred until #1925 merges.\n- **Clauses applied:** `finish-to-zero` "
          "(nothing this session owes remains).", True),
    (FT + "| Item | State |\n|---|---|\n| Gap review | Done |\n\n- **Clauses applied:** finish-to-zero "
          "(the session owes nothing further here).", True),
    ("No follow-up needed.\n\n**POLICY_COMPLIANCE:** #1818 is landed. I applied `session-protocol` Step 2, "
     "finish-to-zero. No policy gaps, no operator questions, and no clauses declined.", True),
    (FT + "- **Final state:** nothing further is owed, but #12 is still open.", False),
    (FT + "- **Final state:** nothing further is owed once CI is green.", False),
    (FT + "- Final state: nothing owed yet.", False),
    (FT + "- Final state: nothing owed for this step; step 2 is next.", False),
    (FT + "- Final state: nothing owed.\n- Follow-ups: 3 remain to be filed.", False),
    (FT + "- Final state: nothing owed.\n  #14 still pending in the queue.", False),
    (FT + "- Final state: PR #5 awaiting merge; nothing further is owed.", False),
    (FT + "- **Session not marked finished**, so nothing further is owed here.", False),
    (FT + "- **Clauses applied:** finish-to-zero.", False),
    (FT + "- Final state: #9 deferred.\n- Operator questions: asked 012a003a.", False),
    ("**POLICY_COMPLIANCE:** #1818 is landed. #1819 is gated on a review, but not yet reviewed.", False),
    ("**POLICY_COMPLIANCE:** #1818 is landed, but #1819 is still open.", False),
    ("**POLICY_COMPLIANCE:** the change is done. No policy gaps.", False),
    ("nothing further is owed.\n\nPOLICY_COMPLIANCE\n- Clauses applied: x", False),
    ("Final state: nothing owed.", False),
]


def selftest():
    bad = []
    for text in NO_VETO_CASES:
        neg = phrase_hits(text)[1]
        if neg:
            bad.append(f"must not veto a finished marker: {text[:60]!r} -> neg={neg}")
    for text, want in PHRASE_CASES:
        pos, neg = phrase_hits(text)
        if want == "veto" and not neg:
            bad.append(f"veto expected: {text[:60]!r} -> pos={pos} neg={neg}")
        elif (bool(pos) and not neg) != bool(want is True):
            bad.append(f"{'closeout' if want else 'not a closeout'} expected: {text[:60]!r} -> pos={pos} neg={neg}")
    for text in NO_VETO_CASES_PHASE2:
        if negative_headline(text):
            bad.append(f"a Phase 2 zero report must not read as a negative headline: {text[:60]!r} -> "
                       f"{negative_headline(text)!r}")
    for text, want in NEG_HEADLINE_CASES:
        if bool(negative_headline(text)) != want:
            bad.append(f"negative_headline {text!r} expected {want}, got {negative_headline(text)!r}")
    for text, want in FOOTER_CASES:
        got = footer_nothing_owed(text) is not None
        if got != want:
            bad.append(f"footer final state {'accepted' if got else 'rejected'}, expected "
                       f"{'accepted' if want else 'rejected'}: {text[-90:]!r}")
        if want and phrase_hits(text)[1]:
            bad.append(f"an accepted footer must not carry a veto: {text[:60]!r} -> {phrase_hits(text)[1]}")
    for text, want in [("Thought for 12s", False), ("Waited for 30s", False),
                       ("\u273b Cooked for 2s \u00b7 done 4:42 PM", True), ("\u273b Sauteedfor1m 21s", True)]:
        if bool(TURN_END.search(text)) != want:
            bad.append(f"TURN_END {text!r} expected {want}")
    for argv, want in [(["bash", "--rcfile", "/tmp/x"], True), (["-bash"], True), (["zsh", "-l"], True),
                       (["bash", "long-job.sh"], False), (["bash", "-c", "sleep 9"], False),
                       (win_argv(WIN_TOOL_BASH), False)]:
        if is_bare_shell(argv, windows=False) != want:
            bad.append(f"is_bare_shell {argv} expected {want}")
    for argv, want in [(win_argv(WIN_PANE), True), ([r"C:\WINDOWS\system32\cmd.exe"], True),
                       (["cmd.exe", "/d"], True), (["powershell.exe", "-NoLogo", "-NoExit"], True),
                       (["powershell.exe", "-NoExit", "-Command", "Start-Job x"], False),
                       (["pwsh.exe", "-File", "x.ps1"], False), (["cmd.exe", "/d", "/s", "/c", "x"], False),
                       (["cmd.exe", "/d", "/R"], False), (["cmd.exe", "job.bat"], False),
                       (["powershell.exe", UNREADABLE_CMDLINE], False), (["cmd.exe", UNREADABLE_CMDLINE], False),
                       (win_argv(WIN_TOOL_BASH), False)]:
        if is_bare_shell(argv, windows=True) != want:
            bad.append(f"is_bare_shell windows {argv} expected {want}")
    for argv in (["pwsh", "-NoProfile"], ["cmd.exe"]):  # a Linux pane running these is running something
        if is_bare_shell(argv, windows=False):
            bad.append(f"is_bare_shell {argv} must not be bare off Windows")
    argv_cases = [(WIN_PANE, [WIN_PANE_EXE, "-NoLogo", "-NoExit", "-Command",
                              r". 'D:\t\qontinui-shell-integration.ps1'"]),
                  (r'"C:\Program Files\x.exe" a\\"b c" f\\\"g \x', [r"C:\Program Files\x.exe", "a\\b c", 'f\\"g', r"\x"]),
                  # "" inside quotes is a literal quote AND ends the quoted run
                  ('x "a""b c"', ["x", 'a"b', "c"]), ('x "d""e" f', ["x", 'd"e f']),
                  ('cmd.exe /c "bash -c ""sleep 9"""', ["cmd.exe", "/c", 'bash -c "sleep', '9"']),
                  ('x a"b"c "" """"', ["x", "abc", "", '"']), ("", []), ("  solo  ", ["", "solo"]), ("solo", ["solo"])]
    for line, want in argv_cases:
        if win_argv(line) != want:
            bad.append(f"win_argv {line!r} expected {want}, got {win_argv(line)}")
        native = shell32_argv(line) if WINDOWS and line.strip() else None
        if native is not None and native != win_argv(line):
            bad.append(f"win_argv {line!r} = {win_argv(line)} but shell32 says {native}")
    # A Windows pane: powershell > wrapper claude.exe > claude.exe > an MCP server under cmd.exe.
    table = {1: (0, win_argv(WIN_PANE)), 2: (1, [r"C:\t\claude.exe"]), 3: (2, [r"C:\b\claude.exe"]),
             4: (3, [r"C:\WINDOWS\system32\cmd.exe", "/d", "/s", "/c", "python shim.py"]),
             5: (4, [r"\??\C:\WINDOWS\system32\conhost.exe", "0x4"]), 6: (4, ["python.exe", "shim.py"])}
    claudes, shells, _ = proc_facts(table, 1, windows=True)
    if sorted(claudes) != [2, 3] or shells:
        bad.append(f"windows idle pane: claudes={claudes} shells={shells}")
    table[7] = (3, win_argv(WIN_TOOL_BASH))
    if proc_facts(table, 1, windows=True)[1] != [7]:
        bad.append(f"windows tool bash not seen: {proc_facts(table, 1, windows=True)}")
    if proc_facts(table, 1, windows=False)[1]:
        bad.append("the windows tool-shell rule leaked into the Linux arm")
    del table[7]
    table[2], table[3] = (1, ["powershell.exe", "-NoProfile"]), (2, ["node.exe", "build.js"])
    if proc_facts(table, 1, windows=True)[0] or not proc_facts(table, 1, windows=True)[2]:
        bad.append(f"windows pane with no claude but a job running: {proc_facts(table, 1, windows=True)}")
    # Off Windows, a cmd/pwsh left under an exited pane is running something, not idle.
    if proc_facts({1: (0, ["bash", "-l"]), 2: (1, ["pwsh", "-NoProfile"])}, 1, windows=False)[2] != [2]:
        bad.append("off Windows, a pwsh under the pane must count as a running job")
    # claude exited and only the pane shell and its console host remain: nothing else runs.
    if proc_facts({1: (0, win_argv(WIN_PANE)), 2: (1, [r"\??\C:\WINDOWS\system32\conhost.exe", "0x4"])},
                  1, windows=True) != ([], [], []):
        bad.append("a conhost under an exited pane must not count as a running job")
    # Pane 10 was born at 50 s. Pid 11 names it as parent but is 45 s older, so it is
    # an orphan of a dead process whose pid 10 reused: cut. Pid 12 is a real child
    # with no readable command line. Pid 13 has no creation time and pid 14 is only
    # 0.5 s "older" (jitter): neither link is provably stale, so both are kept.
    sec = STALE_LINK_MARGIN
    wt = win_table([{"p": 10, "pp": 1, "t": 50 * sec, "n": "powershell.exe", "c": WIN_PANE},
                    {"p": 11, "pp": 10, "t": 5 * sec, "n": "node.exe", "c": "node old.js"},
                    {"p": 12, "pp": 10, "t": 60 * sec, "n": "claude.exe", "c": None},
                    {"p": 13, "pp": 10, "t": 0, "n": "node.exe", "c": "node a.js"},
                    {"p": 14, "pp": 10, "t": 50 * sec - sec // 2, "n": "node.exe", "c": "node b.js"},
                    {"p": 0, "pp": 0, "t": 0}])
    if sorted(subtree(wt, 10)) != [10, 12, 13, 14] or wt[12][1] != ["claude.exe", UNREADABLE_CMDLINE] \
            or wt[0][0] == 0:
        bad.append(f"win_table stale-parent cut / name fallback / self-parent: {wt}")
    if sorted(subtree({1: (2, []), 2: (1, [])}, 1)) != [1, 2]:
        bad.append("subtree does not terminate on a parent cycle")
    empty, prompt_cases = EMPTY_PROMPT_SCREEN, 0
    for raw, want in [(empty, "empty"),
                      ("\x1b[26;1H\r\x1b[2B\x1b[39m\u276f\xa0\x1b[2mrun /vet-imp on the plan\r\x1b[47C\x1b[22m"
                       "\x1b[K\x1b[30;1H\x1b[28;3H\x1b[?25h", "empty"),
                      (empty + "\x1b[?25l\x1b[H\r\x1b[2C\x1b[17Bhalf\x1b[8Gtyped\x1b[14Gdraft\r\x1b[47C"
                       "\x1b[20;1H\x1b[18;19H\x1b[?25h", "text"),
                      ("\x1b[9;1H\u276f\xa0Yes, I trust this folder\r\x1b[9;3H\x1b[?25h", "text"),
                      # differential draft, then Home: the cursor is back at the input start
                      (empty + "\x1b[?25l\x1b[H\r\x1b[2C\x1b[17Bhalf\x1b[8Gtyped\x1b[14Gdraft\r\x1b[47C"
                       "\x1b[20;1H\x1b[18;3H\x1b[?25h", "text"),
                      # multi-line draft, cursor at the start of the second input row
                      (empty + "\x1b[?25l\x1b[19;3Hsecond line\x1b[19;3H\x1b[?25h", "text"),
                      # a draft only on the second input row, cursor back on the first
                      ("\x1b[H\r\x1b[17B\u276f\xa0\x1b[19;3Hrest of draft\x1b[20;1H\u2500\u2500\u2500"
                       "\x1b[18;3H\x1b[?25h", "text"),
                      # OSC title, hyperlink and a private-prefix CSI leave no cells
                      ("\x1b[H\r\x1b[17B\u276f\xa0\x1b]0;title\x07\x1b]8;;http://x\x1b\\\x1b[>0q\x1b[18;3H\x1b[?25h",
                       "empty"),
                      # a relative move after the last absolute position
                      (empty + "\x1b[?25l\x1b[18;3H\x1b[5C\x1b[?25h", "text"),
                      ("\x1b[9;1H\u276f\xa0\x0f\x0f\x0f\r\x1b[9;3H\x1b[?25h", "empty"),
                      # the sticky header repeats the last prompt on a grey background after the input row
                      (empty + "\x1b[?25l\x1b[H\x1b[48;5;237m\x1b[38;5;239m\u276f run /babysit-prs #1\x1b[39m"
                       "\x1b[49m\x1b[20;1H\x1b[18;3H\x1b[?25h", "empty"),
                      (empty + "\x1b[?25l", "unknown"), ("no prompt here", "unknown")]:
        prompt_cases += 1
        if prompt_state(raw, cols=None)[0] != want:
            bad.append(f"prompt_state {raw!r} expected {want}, got {prompt_state(raw, cols=None)}")
    # ConPTY (Windows) writes a border that fills the width with no line break, so
    # the marker after it is on the next row only once the replay auto-wraps.
    wrapped = ("\x1b[H" + "─" * 10 + "❯\xa0\x1b[K\r\n" + "─" * 10 + "  status\x1b[K"
               "\x1b[2;3H\x1b[?25h")
    # The SYNTHETIC ConPTY panes (plan Phase 7, N6 and D3): a scrolled pane's sticky
    # header is the stream's last marker, and the input box is read instead.
    scrolled, w = SCROLLED_PROMPT_SCREEN, CONPTY_FIXTURE_COLS
    box_input, top_rule, bottom_rule = "\u276f\xa0\x1b[K", "\u2500" * 38 + " synthetic \u2500", "\u2500" * w
    draft = scrolled.replace(box_input, "\u276f\xa0synthetic draft\x1b[K", 1)

    def decoy(grey):
        """An old prompt redrawn under a rule after the box, a rule below it; the caret is not in its box."""
        return (scrolled + "\x1b[?25l\x1b[m\x1b[8;1H" + "\u2500" * w + "\x1b[m" + grey
                + "\u276f synthetic old prompt".ljust(w) + "\x1b[m" + "\u2500" * w + "\x1b[4;3H\x1b[?25h")
    for raw, cols, want in [(wrapped, 10, "empty"), (wrapped.replace("❯\xa0", "❯\xa0draft"), 10, "text"),
                            (wrapped, None, "text"),
                            # five double-width characters fill the same 10 columns
                            (wrapped.replace("─" * 10 + "❯", "界" * 5 + "❯", 1), 10, "empty"),
                            (scrolled, w, "empty"),
                            (scrolled, None, "text"),  # ConPTY rows are placed only at the width
                            (IDLE_CONPTY_PROMPT_SCREEN, w, "empty"), (IDLE_CONPTY_PROMPT_SCREEN, None, "text"),
                            (draft.replace("\x1b[4;3H", "\x1b[4;18H"), w, "text"),  # a draft, the caret after it
                            (draft, w, "text"),  # a draft, the caret sent Home
                            (scrolled.replace(box_input, box_input + "\r\nsynthetic second line\x1b[K", 1), w,
                             "text"),  # a draft on the box's second input row
                            # the header redrawn with no background at all: the box still decides
                            (scrolled.replace("\x1b[48;2;55;55;55m\x1b[H", "\x1b[H"), w, "empty"),
                            (scrolled.replace("\x1b[48;2;55;55;55m\x1b[H", "\x1b[H\x1b[48;2;55;55;55m"), w,
                             "empty"),  # its grey set after the cursor-home
                            (scrolled + "\x1b[?25l", w, "unknown"),
                            # no rules, so no box: the last marker not on a background, its grey
                            # read from before the cursor-home that places it
                            (scrolled.replace(top_rule, "synthetic row".ljust(w))
                             .replace(bottom_rule, "synthetic row".ljust(w)), w, "empty"),
                            # a dialog's selection line where the box was
                            (scrolled.replace(top_rule, "synthetic dialog".ljust(w))
                             .replace(box_input, "\u276f\xa01. synthetic option\x1b[K", 1)
                             .replace(bottom_rule, "  2. synthetic option".ljust(w)), w, "text"),
                            # an old prompt redrawn after the box: on a grey background, and on none
                            (decoy("\x1b[48;2;55;55;55m"), w, "empty"), (decoy(""), w, "empty")]:
        prompt_cases += 1
        if prompt_state(raw, cols=cols)[0] != want:
            bad.append(f"prompt_state cols={cols} {raw!r} expected {want}, got {prompt_state(raw, cols=cols)}")
    # Drafts that must read as text (plan Phase 7 review, B1 and N2). A draft typed after the frame,
    # caret sent Home: the frame's truecolor status line once read as dim (the 2 of 38;2;r;g;b), so
    # the draft read as a dim suggestion - plain, after ESC[39m, after ESC[m, after ESC[m + truecolor.
    mk, nb, idle_c = PROMPT_MARK, "\xa0", IDLE_CONPTY_PROMPT_SCREEN
    drafts = [(frame + f"\x1b[?25l\x1b[{row};3H{pre}abc\x1b[{row};3H\x1b[?25h", w)
              for frame, row in ((scrolled, 4), (idle_c, 3))
              for pre in ("", "\x1b[39m", "\x1b[m", "\x1b[m\x1b[38;2;100;150;255m")]
    drafts += [
        (idle_c + f"\x1b[?25l\x1b[3;3Ha {mk} b\x1b[3;3H\x1b[?25h", w),  # a marker typed into it, caret Home
        # a draft ending in a marker, drawn inside the frame, caret right after it: the row's own
        # marker stands left of the typed one
        (scrolled.replace(box_input, f"{mk}{nb}see {mk} \x1b[K", 1).replace("\x1b[4;3H", "\x1b[4;9H"), w),
        (idle_c.replace(box_input, f"{mk}{nb}{mk} \x1b[K", 1).replace("\x1b[3;3H", "\x1b[3;5H"), w),
        (idle_c + f"\x1b[?25l\x1b[3;3Hsee {mk} \x1b[3;9H\x1b[?25h", w),  # the same, typed after the frame
        # no box on the screen: the leftmost marker on the row is the row's own
        (empty + f"\x1b[?25l\x1b[18;3Hsee {mk} \x1b[18;9H\x1b[?25h", None),
        # a second input row that starts with a typed marker, caret after it: only a marker directly
        # under a rule line is the box's
        (scrolled.replace(box_input, box_input + f"\r\n{mk} \x1b[K", 1).replace("\x1b[4;3H", "\x1b[5;3H"), w)]
    for raw, cols in drafts:
        prompt_cases += 1
        if prompt_state(raw, cols=cols)[0] != "text":
            bad.append(f"a draft read {prompt_state(raw, cols=cols)}, want text: cols={cols} {raw[-90:]!r}")
    # Ten markers typed into the row of a screen with no box: the walk back to the row's own marker
    # stops after BOX_SEARCH_MARKS steps and reads text.
    prompt_cases += 1
    got = prompt_state(empty + "\x1b[?25l\x1b[18;3H" + f"{mk} " * 10 + "\x1b[18;23H\x1b[?25h", cols=None)
    if got != ("text", "prompt markers typed into the input row"):
        bad.append(f"ten typed markers read {got}, want text: prompt markers typed into the input row")
    # The box search replays at most BOX_SEARCH_MARKS (8) markers, however often a dialog is
    # redrawn (each older marker costs a replay of the rest of the window), then reads as before:
    # 10 placements in all, with the fallback's own and its one look at the marker before it.
    # Literal numbers, so raising the bound fails here too.
    redrawn = ("\x1b[?25l\x1b[16;1H" + "\u2500" * w + "\x1b[17;1HDo you want to proceed?\x1b[K\x1b[18;1H"
               "\u276f 1. Yes\x1b[K\x1b[19;1H   2. No\x1b[K\x1b[18;3H\x1b[?25h") * 40
    prompt_cases += 1
    placed, real_place = [], _place_mark
    globals()["_place_mark"] = lambda raw, i, cols, anchors: placed.append(i) or real_place(raw, i, cols, anchors)
    try:
        got = prompt_state(redrawn, cols=w)
    finally:
        globals()["_place_mark"] = real_place
    if got[0] != "text" or len(placed) > 10 or BOX_SEARCH_MARKS != 8:
        bad.append(f"a dialog redrawn 40 times read {got} after {len(placed)} placements with BOX_SEARCH_MARKS "
                   f"{BOX_SEARCH_MARKS}, want text after at most 10, with 8")
    # The width is required, by keyword: a caller that omits it fails, never misreads (D3).
    for args in ((empty,), (empty, None), (empty, w)):
        prompt_cases += 1
        try:
            got = prompt_state(*args)
        except TypeError:
            continue
        bad.append(f"prompt_state(raw{''.join(f', {a!r}' for a in args[1:])}) returned {got}; "
                   "a call without cols= must raise TypeError")
    c = Cursor(1, 1, 10)
    c.feed("\x1b[20Cx")  # cursor-forward stops at the right margin; it never wraps
    if (1, 10) not in c.cells:
        bad.append(f"cursor-forward past the margin must clamp to it: {c.cells}")
    c = Cursor(1, 1)
    c.feed("界x")  # with no width (Linux) every character is one column, as measured
    if (1, 2) not in c.cells:
        bad.append(f"a wide character must take one column when no width is set: {c.cells}")
    # SGR: an extended colour's sub-parameters are not attributes (review B1). A truecolor 38;2;r;g;b
    # is not dim and an underline colour 58;2;r;g;b no background; SGR 2 is dim until 22 or a reset.
    sgr_cases = 0
    for seq, want in (("\x1b[38;2;2;2;2m", False), ("\x1b[2m", True), ("\x1b[2;38;2;1;2;3m", True),
                      ("\x1b[38;5;2m", False), ("\x1b[48;2;0;0;2m", False), ("\x1b[58;2;2;2;2m", False),
                      ("\x1b[2m\x1b[22m", False), ("\x1b[2m\x1b[m", False), ("\x1b[2m\x1b[39m", True)):
        sgr_cases += 1
        c = Cursor(1, 1)
        c.feed(seq)
        if c.dim != want:
            bad.append(f"SGR {seq!r} left dim={c.dim}, want {want}")
    sgr_cases += 1
    c = Cursor(1, 1, CONPTY_FIXTURE_COLS)
    c.feed(IDLE_CONPTY_PROMPT_SCREEN)
    if c.dim:
        bad.append("replaying a frame whose status line is truecolor left the replay dim")
    for raw, want in (("\x1b[58;2;41;41;41m" + PROMPT_MARK, False), ("\x1b[48;2;1;2;3m" + PROMPT_MARK, True),
                      ("\x1b[38;2;45;45;45m" + PROMPT_MARK, False), ("\x1b[48;5;237m\x1b[49m" + PROMPT_MARK, False)):
        sgr_cases += 1
        if on_background(raw, len(raw) - 1) != want:
            bad.append(f"on_background {raw!r} expected {want}")
    if wrap_cols({"cols": 102}, windows=True) != 102 or wrap_cols({"cols": 102}, windows=False) is not None:
        bad.append("the prompt wrap must apply on Windows only")
    if win_table([{"p": 5, "pp": 1, "t": 1, "n": "bash.exe", "c": " -c sleep"}])[5][1][0] != "bash.exe":
        bad.append("an empty argv[0] must be named by the image")
    try:  # the report must survive a cp1252 pipe: reconfigure ran before this
        "❯".encode(sys.stdout.encoding or "ascii", sys.stdout.errors or "strict")
    except (UnicodeEncodeError, LookupError):
        bad.append(f"stdout ({sys.stdout.encoding}, errors={sys.stdout.errors}) would die on a non-ASCII name")
    for wd, want in WIND_DOWN_CASES:
        got = wind_down_veto(wd)
        if (got is not None) != want:
            bad.append(f"wind_down_veto {wd} expected {'a veto' if want else 'no veto'}, got {got!r}")
    if "waiting on a human" not in (wind_down_veto({"reason": "record_waiting"}) or "") or \
            "prompt check" not in (wind_down_veto({"reason": "prompt_not_ready"}) or ""):
        bad.append("record_waiting and prompt_not_ready must be named distinctly in the report")
    if "glyph" not in (wind_down_veto({"reason": "prompt_not_ready", "idleEvidence": "glyph:x"}) or ""):
        bad.append("a veto must quote the runner's idleEvidence")
    two = newest_open_rows([
        {"terminalId": "t", "state": "open", "claudeSessionId": "new", "openedAt": 20},
        {"terminalId": "t", "state": "open", "claudeSessionId": "old", "openedAt": 10},
        {"terminalId": "t", "state": "closed", "claudeSessionId": "gone", "openedAt": 30}])
    if two["t"]["claudeSessionId"] != "new":
        bad.append("a terminal with two open rows must resolve to the newest openedAt")
    finish_cases, condition_one_cases = finish_time_cases(bad)
    roster_checks = roster_cases(bad)
    stranded_n = stranded_cases(bad)
    opener_n = opener_cases(bad)
    compaction_n = compaction_cases(bad)
    stray_cases = stray_selftest(bad, empty) + end_strays_selftest(bad) + zombie_live_selftest(bad)
    for row, want in [({"account": {"label": "paktis"}, "configDir": "/h/.claude-x"}, "paktis"),
                      ({"account": {"label": "unknown"}, "configDir": "/h/.claude-lukas"}, "lukas"),
                      ({"account": {"label": "unknown"}, "configDir": os.path.expanduser("~/.claude") + "/"}, "default"),
                      ({"account": {"label": "unknown"}, "configDir": "~/.claude"}, "default"),
                      ({"account": {"label": "unknown"}, "configDir": "/other/place/.claude"}, ".claude"),
                      ({"account": {"label": "unknown"}, "configDir": None}, "unknown"),
                      ({"account": {"label": "unknown"}, "configDir": "/h/acct-x"}, "acct-x"),
                      ({"account": {"label": "unknown"}, "configDir": "C:\\u\\.claude-win\\"}, "win"),
                      ({}, "?")]:
        if account_of(row) != want:
            bad.append(f"account_of {row} expected {want}, got {account_of(row)}")
    home = os.path.expanduser("~/.claude")
    for row, live, want in [({"configDir": "/h/.claude-a"}, "/h/.claude-b", "/h/.claude-b"),
                            ({"configDir": None}, "", home), ({"configDir": "/h/.claude-a"}, None, "/h/.claude-a")]:
        got = with_live_config_dir(row, {}, 1, read=lambda _t, _p, v=live: v)["configDir"]
        if got != want:
            bad.append(f"with_live_config_dir {row} live={live!r} expected {want}, got {got}")
    if with_live_config_dir(None, {}, 1, read=lambda _t, _p: "/h/.claude-b") is not None:
        bad.append("with_live_config_dir(None) must stay None")
    def cwd_of(v):
        def read():
            if isinstance(v, Exception):
                raise v
            return v
        return read
    for cd, cwd, want in [("", "/w", ""), ("/h/.claude-a/", "/w", "/h/.claude-a"),
                          ("~/.claude-a", "/w", os.path.expanduser("~/.claude-a")),
                          ("acct", "/w/p", "/w/p/acct"), ("../.claude-b", "/w/p", "/w/.claude-b"),
                          ("acct", "/w/p (deleted)", None), ("acct", OSError("gone"), None)]:
        got = resolve_config_dir(cd, cwd_of(cwd))
        # Paths compare by os.path.normpath on both sides, so the separator the OS
        # writes does not decide a case; "" (the default dir) and None (no cwd) stay exact.
        same = got == want if not (got and want) else os.path.normpath(got) == os.path.normpath(want)
        if not same:
            bad.append(f"resolve_config_dir {cd!r} cwd={cwd!r} expected {want!r}, got {got!r}")
    for line in bad:
        print("SELFTEST FAIL: " + line)
    print(f"selftest: {len(PHRASE_CASES)} phrase cases, {len(WIND_DOWN_CASES)} wind-down reason cases, "
          f"{len(NO_VETO_CASES)} no-veto cases, {finish_cases} finish-time cases + turn-end and shell "
          f"cases, {prompt_cases} prompt cases, {sgr_cases} SGR cases, {condition_one_cases} condition-1 cases, "
          f"{roster_checks} verdict-roster checks, {stranded_n} finish_stranded cases, {opener_n} opener-class cases, {compaction_n} compaction cases, "
          f"{stray_cases} stray-shell cases, {len(bad)} failure(s)")
    return 1 if bad else 0


def stranded_cases(bad):
    """Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run Phase 2:
    a row whose structured finish signal was read but a screen rule holds carries
    a finish_stranded code; nothing else does, and an unreadable input carries
    none. classify() runs on an idle, bound pane in a linked worktree with the
    process, screen, git and transcript reads stubbed (as finish_time_cases).
    Literal codes, never STRANDED_CODES. Appends failures; returns the count."""
    now = time.time() * 1000
    m = 60000
    lp = int(now - 62 * m)
    after = int(now - 61 * m)
    base = {"final_text": "Fixed the bug in parser.py and pushed it.", "final_text_at": now - 60 * m,
            "last_prompt": lp, "last_prompt_text": "fix the bug", "last_asst": now - 60 * m,
            "last_activity": now - 60 * m, "pending_tool": False, "last_turn_open": lp,
            "last_turn_open_text": "fix the bug", "cwd": LINKED_CWD}
    remains = dict(base, final_text="The deploy is still running; I'll check it in an hour.")
    worked = dict(base, last_asst=now - 30 * m, last_activity=now - 30 * m)
    marker = {"finished": True, "finishedAt": after}
    cf = {"sessionStatus": "finished", "sessionStatusSince": after}
    cases = [  # (registry row, readiness row, transcript, want verdict, want code)
        # a finish its final message contradicts, on both arms
        (marker, {"sessionStatus": "finished"}, remains, "CONTRADICTED", "finish_stranded.phrase_veto"),
        ({}, cf, remains, "CONTRADICTED", "finish_stranded.phrase_veto"),
        # a runner mark coord's readable status does not confirm
        (marker, {"sessionStatus": "waiting_human"}, base, "CONTRADICTED", "finish_stranded.coord_status"),
        (marker, {"sessionStatus": "working"}, base, "CONTRADICTED", "finish_stranded.coord_status"),
        # kept working past the finish with no new closeout, on both arms
        ({}, cf, worked, "REVIVED", "finish_stranded.kept_working"),
        (marker, {"sessionStatus": "working"}, worked, "REVIVED", "finish_stranded.kept_working"),
        # ...but coord's finish with no time to weigh the work against is unconfirmed: no code
        (marker, {"sessionStatus": "finished"}, worked, "REVIVED", None),
        # a turn inside coord's 1 s stamp band: close keeps it, nudge holds it
        ({}, dict(cf, sessionStatusSince=lp + 500), base, "REVIVED", "finish_stranded.in_band"),
        # no code: no finish signal, a revival by a prompt, a closable row, coord's finish with no time
        ({}, {"sessionStatus": "working"}, remains, "ACTIVE", None),
        ({}, dict(cf, sessionStatusSince=lp - 5000), base, "REVIVED", None),
        ({"finished": True, "finishedAt": lp - 5000}, {"sessionStatus": "finished"}, base, "REVIVED", None),
        ({}, cf, base, "FINISHED", None),
        ({}, {"sessionStatus": "finished"}, base, "REVIVED", None),
        # a mark with no usable time is an unknown input: it fires no detector
        ({"finished": True, "finishedAt": None}, {"sessionStatus": "working"}, base, "REVIVED", None),
        ({"finished": True, "finishedAt": None}, {"sessionStatus": "waiting_human"}, remains, "ACTIVE", None),
        # (with no prompt at all, a timeless mark reads every reply as work past it: still no code)
        ({"finished": True, "finishedAt": None}, {"sessionStatus": "working"},
         dict(base, last_prompt=None, last_turn_open=None), "REVIVED", None),
    ]
    g = globals()
    stubs = {"proc_facts": lambda table, pid, windows=WINDOWS: ([11], [], []),
             "live_session_ids": lambda claudes: {"s1"}, "raw_tail": lambda tid, chars=30000: "screen",
             "running_turn": lambda text: False, "prompt_state": lambda raw, *, cols: ("empty", ""),
             "wrap_cols": lambda term, windows=None: None, "git_rev_parse_dirs": fake_git_dirs}
    names = list(stubs) + ["transcript_facts", "terminals", "open_rows", "page_names", "proc_table", "readiness",
                           "screen_one"]
    saved = {k: g[k] for k in names}
    term = {"id": "t1", "isAlive": True, "pid": 10}
    n = 0
    try:
        g.update(stubs)
        for row, rp, tx, want, code in cases:
            g["transcript_facts"] = lambda r, tx=tx: dict(tx)
            got, why = classify(term, dict(row, claudeSessionId="s1"), {10: (1, ["bash"])}, 15, "", {"s1": rp})
            n += 1
            lines = [w for w in why if w.startswith("finish_stranded.")]
            if got != want or stranded_code(why) != code or len(lines) != (1 if code else 0):
                bad.append(f"stranded: classify(row={row}, readiness={rp}, final={tx['final_text'][:24]!r}) = "
                           f"{got} code {stranded_code(why)!r} {why}; want {want} code {code!r}")
        # an unreadable transcript, or an unreadable screen, carries no code
        for k, v, want in (("transcript_facts", lambda r: None, "UNKNOWN"),
                           ("raw_tail", lambda tid, chars=30000: None, "BUSY")):
            g["transcript_facts"] = lambda r: dict(remains)
            g[k] = v
            got, why = classify(term, dict(marker, claudeSessionId="s1"), {10: (1, ["bash"])}, 15, "", {"s1": cf})
            g.update(stubs)
            n += 1
            if got != want or stranded_code(why) is not None or "finish_stranded" in "; ".join(why):
                bad.append(f"stranded: an unreadable {k} gave {got} {why}; want {want} with no code")
        # screen_all lifts the code onto the row, and drops it when readiness is unreadable
        coded = ["kept working 31m after its finish mark",
                 "finish_stranded.kept_working: it kept working past its finish with no new closeout"]
        for err, want_code, want_why in ((None, "finish_stranded.kept_working", coded),
                                         ("boom", None, coded[:1])):
            g.update({"terminals": lambda: [term], "open_rows": lambda: {"t1": {"claudeSessionId": "s1"}},
                      "page_names": dict, "proc_table": dict, "readiness": lambda err=err: ({}, err),
                      "screen_one": lambda *a, **k: ("REVIVED", list(coded))})
            out = screen_all(15, None)[0]
            n += 1
            if [r.get("reason_code") for r in out] != [want_code] or out[0]["reasons"] != want_why:
                bad.append(f"stranded: screen_all(readiness err={err!r}) = {out}; want reason_code {want_code!r}")
    finally:
        g.update(saved)
    # the report section lists each coded row with its code and the finish reason, and only those
    rows = [{"terminalId": "t1234567", "verdict": "CONTRADICTED", "tab": "x", "account": "a", "name": "held one",
             "reasons": ["coord session_status=finished (1h ago), but the final message says work remains (x)",
                         "finish_stranded.phrase_veto: a finish its own final message contradicts"],
             "reason_code": "finish_stranded.phrase_veto"},
            {"terminalId": "t7654321", "verdict": "ACTIVE", "tab": "x", "account": "a", "name": "plain one",
             "reasons": ["no finished marker, no closeout phrase"], "reason_code": None}]
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        print_report(rows, [])
    section = out.getvalue().split("== FINISHED, HELD BY A SCREEN RULE (not closed): 1\n", 1)
    n += 1
    if len(section) != 2 or "[CONTRADICTED] finish_stranded.phrase_veto: coord session_status=finished (1h ago), " \
            "but the final message says work remains" not in section[1] or "t1234567" not in section[1] \
            or "t7654321" in section[1]:
        bad.append(f"stranded: print_report's held section is wrong: {out.getvalue()!r}")
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        print_stranded([dict(rows[1])])
    n += 1
    if out.getvalue() != "\n== FINISHED, HELD BY A SCREEN RULE (not closed): 0\n":
        bad.append(f"stranded: an uncoded screen must print a read zero: {out.getvalue()!r}")
    return n


def finish_time_cases(bad):
    """classify() on an idle, bound pane (the process, screen and transcript
    reads stubbed) for each way coord's finish time (sessionStatusSince) and the
    runner marker meet the last prompt, then transcript_facts' `last_turn_open`
    on written transcripts. Appends failures to `bad`; returns the case count."""
    import tempfile
    now = time.time() * 1000
    m = 60000
    lp = int(now - 62 * m)  # the last real prompt
    plain, closing = "Fixed the bug in parser.py and pushed it.", "PR #3 landed.\n\nSAFE TO CLOSE"
    # The session works in a linked worktree (finished-criteria condition 3) unless a case says otherwise;
    # GIT_ANSWERS stands in for `git rev-parse` per directory, so no case runs git.
    base = {"final_text": plain, "final_text_at": now - 60 * m, "last_prompt": lp, "last_prompt_text": "fix the bug",
            "last_asst": now - 60 * m, "last_activity": now - 60 * m, "pending_tool": False,
            "last_turn_open": lp, "last_turn_open_text": "fix the bug", "cwd": LINKED_CWD}
    shut = dict(base, final_text=closing)
    cf = {"sessionStatus": "finished"}
    before, after = int(now - 90 * m), int(now - 61 * m)  # coord stamps either side of the prompt

    def at(ms_):
        return dict(cf, sessionStatusSince=int(ms_))
    marker = {"finished": True, "finishedAt": before}
    wake = "<task-notification><task-id>b1</task-id> background build finished"
    prompted = ("coord finished", "then prompted")
    ua = {"finished": True, "finishedAt": after, "finishReason": "unattended: 3 units, all landed or watched"}
    hedgy = dict(base, final_text="PR #3 is pushed; its checks are still running and a gate watches the merge.\n"
                                  "- Whether coord can auto-close this session is UNKNOWN: the obligations check "
                                  "timed out.\n\nSAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off "
                                  "declared for 1.")
    # Plan Phase 6 fixtures: a session's own scheduled wake, a Session Bus delivery, a well-formed No.
    mark = {"finished": True, "finishedAt": after}
    woke = "Woke on schedule; the check it was set for already ran, so there is nothing to do here."
    bus = ("<system-reminder>Session Bus stuck_pr_nudge message. Message: PR #7 has had no review for three "
           "days</system-reminder>")
    no = "Pushed the fix to #12.\n\n**Safe to close:** no - PR #12 is still open."
    cases = [  # (registry row, readiness row, transcript, want, reason fragment(s): all must appear, "!x" must not)
        ({}, at(before), base, "REVIVED", prompted),
        ({}, at(before), shut, "REVIVED", "predates that prompt"),
        ({}, at(after), base, "FINISHED", "coord session_status=finished ("),
        ({}, at(after), shut, "FINISHED", "no prompt since"),
        # the band leans to keeping the tab: within 1 s either side of the prompt is a revival
        ({}, at(lp + 1500), base, "FINISHED", "no prompt since"),
        ({}, at(lp + 1000), base, "FINISHED", "no prompt since"),  # exact edge: prompt == since - 1 s
        ({}, at(lp + 500), base, "REVIVED", prompted),
        ({}, at(lp - 500), base, "REVIVED", prompted),
        ({}, at(lp - 1500), base, "REVIVED", prompted),
        # with no usable time, the coord-only rule stands: closeout phrase or not
        ({}, cf, shut, "FINISHED", "coord session_status=finished, closeout phrase"),
        ({}, cf, base, "REVIVED", "not a closeout"),
        ({}, dict(cf, sessionStatusSince=None), base, "REVIVED", "not a closeout"),
        ({}, dict(cf, sessionStatusSince=str(after)), base, "REVIVED", "not a closeout"),
        ({}, dict(cf, sessionStatusSince=float(after)), base, "REVIVED", "not a closeout"),
        ({}, dict(cf, sessionStatusSince=True), shut, "FINISHED", "closeout phrase"),
        ({}, dict(cf, sessionStatusSince=0), base, "REVIVED", "not a closeout"),
        ({}, dict(cf, sessionStatusSince=-1), base, "REVIVED", "not a closeout"),
        ({}, dict(cf, sessionStatusSince=1_000_000_000), base, "REVIVED", "not a closeout"),  # seconds, not ms
        ({}, at(now + 10 * m), base, "REVIVED", "not a closeout"),  # in the future
        # a stamp ahead of this clock proves an unbounded skew, so it is not compared
        # with the prompt at all: the coord-only rule decides (closeout phrase or not)
        ({}, at(now + 2 * m), base, "REVIVED", "not a closeout"),
        ({}, at(now + 2 * m), shut, "FINISHED", "closeout phrase"),
        # revived after the runner mark, then closed out: coord's stamp decides whether it re-finished
        (marker, at(after), shut, "FINISHED", "coord session_status=finished ("),
        (marker, at(before - m), shut, "REVIVED", "predates that prompt"),
        # with no coord time and a mark older than the last turn, the finish is unconfirmed, and whether coord's
        # finish came after that turn is UNKNOWN (condition 4 cannot be ruled out): withheld whatever the reply
        # says, a closeout or a nothing-owed footer alike; coord reading anything else leaves the closeout
        # unrecorded (condition 1)
        (marker, cf, shut, "REVIVED", ("no finish time", "reopened after its runner finish mark", "then prompted",
                                       "!closeout phrase")),
        (marker, cf, dict(base, final_text=FT + "- **Final state:** this session owes nothing."), "REVIVED",
         ("no finish time", "reopened after its runner finish mark")),
        (marker, cf, dict(shut, last_turn_open=now - 30 * m, last_turn_open_text=wake, last_prompt=before - m),
         "REVIVED", ("no finish time", "reopened after its runner finish mark", "a turn opened")),
        # ...while a mark after the last turn still settles it
        ({"finished": True, "finishedAt": after}, cf, shut, "FINISHED", "runner finished marker"),
        # a mark with no usable time beside coord's untimed finish dates nothing: never closed, and its reason
        # says so rather than claiming a reopening it cannot date
        ({"finished": True, "finishedAt": None}, cf, shut, "UNKNOWN",
         ("no usable time either", "!then prompted", "!marked finished ?")),
        ({"finished": True, "finishedAt": 0}, cf, dict(base, final_text=FT + "- **Final state:** this session owes "
                                                       "nothing."), "UNKNOWN", ("no usable time either",)),
        (marker, {"sessionStatus": "working"}, shut, "CLOSEOUT",
         ("closed out again", "reads working, not finished (finished-criteria condition 1)")),
        # the runner marker says finished after the prompt; coord's earlier stamp wins
        ({"finished": True, "finishedAt": after}, at(before), base, "REVIVED", prompted),
        # the runner marker alone keeps its own 1 s band (prompt > finishedAt + 1 s revives); not revived,
        # it closes only beside coord's finished status (condition 1): with none served it is UNKNOWN
        ({"finished": True, "finishedAt": lp - 1000}, {}, base, "UNKNOWN",
         ("runner finished marker", "UNKNOWN (the runner served none), so not finished (finished-criteria condition 1)")),
        ({"finished": True, "finishedAt": lp - 1000}, cf, base, "FINISHED", ("runner finished marker", "!condition 1")),
        ({"finished": True, "finishedAt": lp - 1001}, {}, base, "REVIVED", "marked finished"),
        # and no runner finishReason rides on a coord-sourced line
        ({"finished": True, "finishedAt": after, "finishReason": "done"}, at(after), base, "FINISHED",
         ("no prompt since", "!(done)")),
        ({"finished": True, "finishedAt": after, "finishReason": "done"}, cf, base, "FINISHED",
         ("runner finished marker (", " ago: done), no prompt since")),
        # finished-criteria condition 1 names each status it read (3bbebf70 was a closeout beside waiting_human)
        ({}, {"sessionStatus": "waiting_human"}, shut, "CLOSEOUT",
         ("closeout phrase without a finished marker", "reads waiting_human, not finished (finished-criteria condition 1)")),
        ({"finished": True, "finishedAt": after}, {"sessionStatus": "waiting_human"}, base, "CONTRADICTED",
         ("runner finished marker", "reads waiting_human, not finished (finished-criteria condition 1)")),
        ({}, {}, dict(base, final_text=FT + "- **Final state:** this session owes nothing."), "FOOTER-FINISHED",
         ("footer final state, no marker", "UNKNOWN (the runner served none)")),
        ({}, cf, dict(base, final_text=FT + "- **Final state:** this session owes nothing."), "FINISHED",
         "coord session_status=finished, footer final state"),
        # a turn opened by something that is not a prompt (a wake) also keeps it open
        ({}, at(after), dict(base, last_turn_open=now - 40 * m, last_turn_open_text=wake), "REVIVED",
         ("a turn opened", "task-notification")),
        ({}, at(after), dict(base, last_turn_open=after - 500, last_turn_open_text=wake), "REVIVED", "a turn opened"),
        ({}, at(after), dict(base, last_turn_open=after - 1500, last_turn_open_text=wake), "FINISHED",
         "no prompt since"),
        # a finish whose own final message says work remains is contradicted (plan Phase 6), never closed
        ({}, at(after), dict(base, final_text="The deploy is still running; I'll check it in an hour."),
         "CONTRADICTED", ("coord session_status=finished (", "but the final message says work remains")),
        ({}, at(now - 40 * m), dict(base, last_prompt=now - 50 * m), "ACTIVE", "has no reply"),
        # a turn 30m after coord's stamp with no prompt and no closeout keeps it open (finished-criteria cond. 4)
        ({}, at(after), dict(base, last_asst=now - 30 * m, last_activity=now - 30 * m), "REVIVED",
         "after coord's finish time"),
        ({}, at(after), dict(shut, last_asst=now - 30 * m, last_activity=now - 30 * m), "FINISHED", "no prompt since"),
        # a time on a status that is not finished is no finish time, and the status contradicts the mark
        ({"finished": True, "finishedAt": after}, {"sessionStatus": "working", "sessionStatusSince": before},
         base, "CONTRADICTED", ("runner finished marker", "reads working, not finished (finished-criteria condition 1)")),
        # Plan 2026-11-08-...-hedge-words Phase 3: a marker /unattended set ("unattended: ...") is trusted over the
        # final reply's hedge words, work-remains and promise phrases; only a negative headline or a revival vetoes.
        (ua, cf, hedgy, "FINISHED", ("runner finished marker (", "set by /unattended", "still running")),
        (ua, at(after), hedgy, "FINISHED", ("no prompt since", "set by /unattended")),
        (ua, cf, dict(hedgy, final_text="Doing it now.\n\nSAFE TO CLOSE \u2014 1 tree checked\n\nPOLICY_COMPLIANCE: ok"),
         "FINISHED", "set by /unattended"),
        (ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE. PR #3's checks are still running.\n"
                                        "- Whether it is safe to close is UNKNOWN: the lookup timed out."),
         "FINISHED", "set by /unattended"),
        # ...but not a reply that is not /unattended's report (a parent sharing a subagent's session id),
        # nor one asking the operator to act
        (ua, cf, dict(hedgy, final_text="PR #3 still needs the migration fix; I'll check it after CI."), "ACTIVE",
         "work remains"),
        (ua, cf, dict(hedgy, final_text=hedgy["final_text"] + "\n\nShall I start Phase 2?"), "ACTIVE",
         "work remains"),
        (ua, cf, dict(hedgy, final_text=hedgy["final_text"] + "\nWant me to open the PR too?"), "ACTIVE",
         "work remains"),
        (ua, {"sessionStatus": "finished", "sessionStatusSince": after}, hedgy, "FINISHED", "set by /unattended"),
        (dict(ua, finishedAt=None), {"sessionStatus": "finished", "sessionStatusSince": after}, hedgy, "ACTIVE",
         "work remains"),
        (ua, cf, dict(hedgy, final_text="Session left UNFINISHED.\nSAFE TO CLOSE \u2014 1 tree checked\nCI still running."),
         "ACTIVE", "work remains"),
        # review round 2: a quoted SAFE TO CLOSE is not the report; spelled-out negatives and questions still veto
        (ua, cf, dict(hedgy, final_text="The review subagent reported SAFE TO CLOSE for its scratch tree. I still "
                                        "need to fix the two findings it raised; I'll check them next."),
         "ACTIVE", "work remains"),
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE.\n" + t), "ACTIVE", "work remains") for t in (
            "It isn't SAFE TO CLOSE after all: qontinui-web has 2 unpushed commits.",
            "NOT  SAFE TO CLOSE \u2014 1 tree\n  qontinui-web: 2 unpushed commits",
            "Safe to close? Probably not \u2014 qontinui-web has 2 unpushed commits.",
            "Safe to close: maybe \u2014 the unpushed commit may still be there.",
            "SAFE TO CLOSE \u2014 1 tree checked; hand-off declared for 1\nUnsafe to close actually: qontinui-web is dirty.",
            "Session finished: no \u2014 PR #3 still owes a fix.",
            "SAFE TO CLOSE \u2014 1 tree checked\nDo I also open the follow-up PR? Let me know.",
            "SAFE TO CLOSE \u2014 1 tree checked\nWait \u2014 should I also open PR #4 against main?")],
        # review round 3: a quoted or recounted report is not the report; more spelled-out negatives and asks
        *[(ua, cf, dict(hedgy, final_text=t), "ACTIVE", "work remains") for t in (
            "The review subagent's report:\n> SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nI still need "
            "to fix the two findings it raised; I'll check them next.",
            "- SAFE TO CLOSE (subagent's scratch tree). The parent work is still running; I'll check it after CI.",
            "> Unattended verdict: COMPLETE\n(that was the subagent's verdict, not mine)\nStill running Phase 3 "
            "tests; I'll check them next.",
            "Session marked finished by the review subagent by mistake. I'll undo it; tests are still running.",
            "Verdict: COMPLETE for Phase 1. Phase 2 is still running; I'll check back on it.",
            "SAFE TO CLOSE \u2014 1 tree checked\nUpdate: it is no longer safe to close \u2014 2 unpushed commits.",
            "SAFE TO CLOSE \u2014 1 tree checked\nI don't think it's safe to close yet: 2 unpushed commits remain.",
            "SAFE TO CLOSE \u2014 1 tree checked\nThis session must not be closed: the migration is still running.",
            "SAFE TO CLOSE \u2014 1 tree checked\nKeep this session open; the deploy is still running.",
            "Session finished? No.\nCI is still running on PR #3; I'll check it after.",
            "Session marked finished: no \u2014 1 unit is DROPPED. I'll resume after CI.",
            "SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nI need your sign-off on the schema change; "
            "CI is still running.",
            "SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nTell me if you want Phase 2 started; CI is "
            "still running.",
            "Unattended verdict \u2014 INCOMPLETE: 1 unit DROPPED. CI is still running.")],
        # review round 4: fenced, bulleted, indented or mid-line report shapes are not the report
        *[(ua, cf, dict(hedgy, final_text=t), "ACTIVE", "work remains") for t in (
            "The review subagent returned:\n```\nSAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off "
            "declared for 1\n```\nI still need to fix the two findings it raised; I will check them next.",
            "Subagent report:\n- SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nPhase 2 is still running; "
            "I will check it after CI.",
            "Subagent report:\n    SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nPhase 2 is still "
            "running; I will check it after CI.",
            "The subagent said Unattended verdict: COMPLETE for Phase 1 only. Phase 2 is still running; I will "
            "check it after CI.",
            "Session marked finished \u2014 by the review subagent, by mistake. I will undo it; tests are still "
            "running.",
            "Session marked finished: by mistake (the subagent shares my id). Phase 2 still running; checking it next.",
            "Unattended verdict: COMPLETE.\nIt is not quite safe to close: 2 unpushed commits remain.",
            "Unattended verdict: COMPLETE.\nCorrection: NOT\nsafe to close, 2 unpushed commits.",
            "Unattended verdict: COMPLETE.\nIs this safe to close? No \u2014 2 unpushed commits.\n\na\n\nb\n\nc",
            "Unattended verdict: COMPLETE.\nLeave this session open; the migration is still running.",
            "Unattended verdict: COMPLETE.\nThis session is not done: Phase 2 is still running here.",
            "Unattended verdict: COMPLETE.\nSession NOT finished \u2014 bind attempted and failed; still waiting.")],
        # review round 5: a labelled (recounted) report shape; strict negatives; not-done; conditional answers
        *[(ua, cf, dict(hedgy, final_text=t), "ACTIVE", "work remains") for t in (
            "Subagent: Unattended verdict: COMPLETE \u2014 2 units.\nI still need to fix the two findings; I'll check "
            "CI next.",
            "Subagent verdict: SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nI still need to fix them; "
            "I'll check CI next.",
            "Review verdict: SAFE TO CLOSE \u2014 1 tree checked\nPhase 2 is still running; I'll check it after CI.",
            "Note: Unattended verdict: COMPLETE was the subagent's.\nPhase 2 is still running; I'll check it.")]
          + [(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE.\n" + t + "\nCI is still running."), "ACTIVE",
              "work remains") for t in (
            "Safe to close: negative, 2 unpushed commits.", "Safe to close: pending - CI on #3 has not finished.",
            "Safe to close: n/a until #3 lands.",
            "None of the three trees is safe to close: qontinui-web has 2 unpushed commits.",
            "Nothing here is safe to close yet.", "Session is NOT complete: Phase 2 is still running.",
            "Work is NOT complete.", "Not finished: Phase 2 is still running.", "We're not done.", "I'm not done yet.",
            "Not done yet: Phase 2 is running.", "Session should stay open: Phase 2 is still running.",
            "Safe to close: once #3 lands.", "Safe to close: only after #3 lands.", "Safe to close: when CI is green.",
            "It will be safe to close once PR #3 lands.", "Only safe to close after I push the fix.",
            "Do not close this session.", "Session is unfinished.")],
        # review round 6: conditions before the phrase, any n't, unanchored answers, synonyms, a pasted report
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + "\nCI still running.\n"
                                          "SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk"), "ACTIVE",
           "work remains") for t in (
            "When CI is green this session is safe to close.", "Once PR #3 lands it is safe to close.",
            "After the migration finishes, this is safe to close.", "It won't be safe to close.",
            "It wasn't safe to close.", "It doesn't seem safe to close yet.", "This hasn't become safe to close.",
            "- qontinui-web \u2014 safe to close: no (2 unpushed commits)", "qontinui-web safe to close: false",
            "Overall safe to close: unknown", "The session is incomplete.", "Work remains incomplete.",
            "Phase 2 is unfinished.", "Closing now would be unsafe.")],
        (ua, cf, dict(hedgy, final_text="The closeout subagent returned:\n\nUnattended verdict: COMPLETE \u2014 2 units\n"
                                        "SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\n\nI still need to fix "
                                        "the two findings it raised; I'll check them next."), "ACTIVE", "work remains"),
        # review round 7: a multi-paragraph pasted report; a self-written "carried by"; more explicit negatives
        *[(ua, cf, dict(hedgy, final_text=t), "ACTIVE", "work remains") for t in (
            "The closeout subagent returned:\n\nUnattended verdict: COMPLETE \u2014 2 units, 0 converted.\n\nFleet "
            "policy read this run.\n\nSAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; hand-off declared "
            "for 1\n\nI still need to fix the two findings it raised; I'll check them next.",
            "Subagent result:\nSAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\nI'll check CI next.",
            "From the /unattended subagent:\nUnattended verdict: COMPLETE\nPhase 2 is still running.",
            "Session marked finished \u2014 carried by the review subagent's run, by mistake. I'll undo it after CI; "
            "Phase 2 is still running.")]
          + [(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + " CI still running."),
              "ACTIVE", "work remains") for t in (
            "This session is not ready to close.", "Not safe to exit yet.", "Do not exit this session.",
            "Hold off on closing this one.", "Session not marked finished.")],
        (ua, cf, dict(hedgy, final_text="Session marked finished \u2014 carried by the runner door, read back from "
                                        "coord.\nCI is still running on PR #3."), "FINISHED", "set by /unattended"),
        # review round 11: a question on one line answered on the next stays one negative
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + "\n\nCI is still running."
                                          "\n\nA\n\nB\n\nSAFE TO CLOSE \u2014 1 tree checked, nothing only on disk\n\nC"),
           "ACTIVE", "work remains") for t in (
            "Is this safe to close?\nNo \u2014 qontinui-web has 2 unpushed commits.",
            "**Safe to close?**\nNo \u2014 qontinui-web has 2 unpushed commits.",
            "Correction: this is NOT\nSAFE TO CLOSE \u2014 2 unpushed commits.",
            "Session finished?\nNo \u2014 Phase 2 is still running.",
            # review round 12: a lower-case negator wrapped onto the headline line
            "Correction: this is not\nSAFE TO CLOSE \u2014 1 tree checked; 2 unpushed commits.",
            "It is no longer\nSAFE TO CLOSE \u2014 1 tree checked.")],
        # review round 12: closeability labels after a verdict line with no closing period
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units\n" + t + "\nCI still running."),
           "ACTIVE", "work remains") for t in ("Ready to close: no", "Closeable: no")],
        # review round 10: a "none" on one line does not negate the next line's headline; retractions
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + "\nCI is still running.\n"
                                          "SAFE TO CLOSE \u2014 2 trees checked, nothing only on disk"), "FINISHED",
           "set by /unattended") for t in ("Peer messages: none", "- Open questions: none\n",
                                          "Policy-read confidence: FULL \u2014 stop decisions: none\n")],
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + " CI still running."),
           "ACTIVE", "work remains") for t in (
            "Correction: the verdict is INCOMPLETE, Phase 2 is still running.",
            "I should not have finished the session.", "Marking it finished was a mistake.",
            "Reopening this session: Phase 2 is still running.")],
        # review round 9: an introduction with no colon; more spelled-out negatives
        (ua, cf, dict(hedgy, final_text="## Subagent result\n\nSAFE TO CLOSE \u2014 1 tree checked, nothing only on disk; "
                                        "hand-off declared for 1\n\nPhase 2 is still running; I will check it after CI."),
         "ACTIVE", "work remains"),
        (ua, cf, dict(hedgy, final_text="Here is what the closeout subagent found.\n\nUnattended verdict: COMPLETE \u2014 2 "
                                        "units\n\nI still need to fix the two findings; I'll check CI next."),
         "ACTIVE", "work remains"),
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + " CI still running."),
           "ACTIVE", "work remains") for t in (
            "Safe-to-close verdict: NO.", "Closeable: no.", "Ready to close: no.", "The finish mark was premature.",
            "Session should not have been marked finished.", "Unless CI fails, it is safe to close.")],
        # review round 8
        *[(ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\n" + t + " CI still running.\n"
                                          "SAFE TO CLOSE \u2014 1 tree checked, nothing only on disk"), "ACTIVE",
           "work remains") for t in (
            "Safe to close: I don't think so, qontinui-web has 2 unpushed commits.",
            "Safe to close: definitely not, 2 unpushed commits.", "Safe to close: nah.",
            "It'll be safe to close tomorrow, after the migration.", "The session should remain open.",
            "Keep this terminal open; the deploy is still running.")],
        (ua, cf, dict(hedgy, final_text="Here is the summary:\n\nUnattended verdict: COMPLETE \u2014 2 units\n\nSAFE TO "
                                        "CLOSE \u2014 1 tree checked\n\nI still need to fix the two findings; I'll check "
                                        "CI next."), "ACTIVE", "work remains"),
        (ua, cf, dict(hedgy, final_text="Unattended verdict: COMPLETE \u2014 3 units.\nPR #3 checks are still running; "
                                        "a gate watches the merge.\nSafe to close: no uncommitted changes or unpushed "
                                        "commits."), "FINISHED", "set by /unattended"),
        # the report shapes /unattended and /finish-session print are trusted
        (ua, cf, dict(hedgy, final_text="**Session marked finished** \u2014 carried by the runner door, read back from "
                                        "coord.\nCI is still "
                                        "running on PR #3."), "FINISHED", "set by /unattended"),
        # an old "unattended:" reason does not ride on a later finish another door made after a prompt
        (dict(ua, finishedAt=now - 120 * m), at(after), dict(hedgy, final_text="I marked the session finished, but "
                                                              "CI is still running; I'll check back on PR #3."),
         "ACTIVE", "work remains"),
        (ua, cf, dict(hedgy, final_text=hedgy["final_text"] + "\nNOT SAFE TO CLOSE \u2014 1 tree\n  x: 1 unpushed"),
         "ACTIVE", "work remains"),
        (ua, cf, dict(hedgy, final_text="**Safe to close:** no \u2014 PR #3 is open."), "ACTIVE", "work remains"),
        (ua, cf, dict(hedgy, final_text="SAFE TO CLOSE? NOT SAFE TO CLOSE \u2014 1 tree"), "ACTIVE", "work remains"),
        (ua, cf, dict(hedgy, final_text="UNKNOWN \u2014 2 trees could not be checked\nCI still running."), "ACTIVE",
         "work remains"),
        # a marker whose reason is not /unattended's keeps today's rules
        (dict(ua, finishReason="done"), {}, hedgy, "ACTIVE", "work remains"),
        (dict(ua, finishReason=None), {}, hedgy, "ACTIVE", "work remains"),
        (dict(ua, finished=False), {}, hedgy, "ACTIVE", "work remains"),
        # revived after the unattended mark: still REVIVED, never trusted (a reopened finish is no contradiction,
        # so a reply saying work remains reads ACTIVE)
        (dict(ua, finishedAt=before), {}, base, "REVIVED", "marked finished"),
        (dict(ua, finishedAt=before), {}, hedgy, "ACTIVE", "says work remains"),
        (ua, at(before), hedgy, "ACTIVE", "says work remains"),  # coord's stamp predates the prompt
        (ua, cf, dict(hedgy, last_turn_open=now - 40 * m, last_turn_open_text=wake), "ACTIVE", "says work remains"),
        # kept working 30m past the mark: never trusted, closeout phrase or not
        (ua, cf, dict(hedgy, final_text="PR #3 is pushed; its checks are still running.", last_asst=now - 30 * m,
                      last_activity=now - 30 * m), "ACTIVE", "says work remains"),
        (ua, cf, dict(hedgy, last_asst=now - 30 * m, last_activity=now - 30 * m), "CONTRADICTED", "says work remains"),
        (ua, {"sessionStatus": "working", "sessionStatusSince": now - 30 * m}, hedgy, "CONTRADICTED", "says work remains"),
        (dict(ua, finishedAt=None), {}, hedgy, "ACTIVE", "says work remains"),
        # the idleness proof still runs first: a usage-limit stop and an unanswered prompt are not relaxed
        (ua, cf, dict(hedgy, final_text="You've hit your session limit \u00b7 resets 5pm"), "LIMITED", "usage limit"),
        (ua, cf, dict(hedgy, last_prompt=now - 50 * m), "ACTIVE", "has no reply"),
        # The trust reads the wording only: served finished-criteria condition 1 still decides first, so a
        # trusted /unattended mark beside no coord status, or beside one that is not finished, never closes.
        (ua, {}, hedgy, "UNKNOWN", ("set by /unattended", "UNKNOWN (the runner served none)",
                                    "finished-criteria condition 1")),
        (ua, {"sessionStatus": "waiting_human"}, hedgy, "ACTIVE", "work remains"),  # trust off: not finished
        # N5 (plan Phase 6): the session's own scheduled wake after its runner mark opens a turn, so it is
        # revived whatever that turn's reply says (condition 4); at 5 minutes the mark's arm read FINISHED
        (mark, {}, dict(base, last_turn_open=after + 5 * m, last_turn_open_text=SCHEDULED_WAKE, final_text=woke,
                        last_asst=after + 5 * m + 30000, last_activity=after + 5 * m + 30000), "REVIVED",
         ("marked finished", "a turn opened")),
        (mark, {}, dict(base, last_turn_open=after + 40 * m, last_turn_open_text=SCHEDULED_WAKE, final_text=woke,
                        last_asst=after + 40 * m + 30000, last_activity=after + 40 * m + 30000), "REVIVED",
         ("a turn opened", "Scheduled wake-up")),
        # N8: a Session Bus delivery after the runner mark opens a turn as it does after coord's stamp...
        (mark, {}, dict(base, last_turn_open=after + 3 * m, last_turn_open_text=bus, last_asst=after + 8 * m,
                        last_activity=after + 8 * m), "REVIVED", ("a turn opened", "Session Bus")),
        # ...and the same delivery pasted as a prompt reads the same (retention)
        (mark, {}, dict(base, last_prompt=after + 3 * m, last_prompt_text="pasted: " + bus, last_asst=after + 8 * m,
                        last_turn_open=after + 3 * m, last_turn_open_text="pasted: " + bus,
                        last_activity=after + 8 * m), "REVIVED", ("marked finished", "then prompted")),
        # the dead zone: a runner mark beside a well-formed No is contradicted, and never closed
        (mark, {}, dict(base, final_text=no), "CONTRADICTED",
         ("runner finished marker (", "but the final message says work remains (not safe to close)")),
        # served finished-criteria condition 3: only a linked worktree is custody this screen can vouch for
        ({}, at(after), dict(base, cwd="/w/repo"), "CUSTODY",
         ("coord session_status=finished (", "custody: not a linked worktree (finished-criteria condition 3)",
          "is a primary checkout")),
        ({}, at(after), dict(base, cwd="/w/repo/sub"), "CUSTODY", "is a primary checkout"),
        ({}, at(after), dict(base, cwd="/w/no-repo"), "CUSTODY", "is in no repository"),
        ({}, at(after), dict(base, cwd=None), "CUSTODY", "custody UNKNOWN: the transcript records no working directory"),
        ({}, at(after), dict(base, cwd="/w/slow"), "CUSTODY", "custody UNKNOWN: git did not answer"),
        ({}, at(after), dict(base, cwd="/w/broken"), "CUSTODY", "custody UNKNOWN: git rev-parse exited 128"),
        ({}, cf, dict(shut, cwd="/w/repo"), "CUSTODY", ("closeout phrase", "is a primary checkout")),
    ]
    # Served finished-criteria condition 1 (plan Phase 2): a live row coord does not read finished is never
    # closable, whatever finish signal it carries - a closeout phrase, a nothing-owed footer, a runner mark, or
    # a runner mark revived and then closed out again - and its reason names the status and the rule.
    footer = dict(base, final_text=FT + "- **Final state:** this session owes nothing.")
    condition_one = [(row, rp, tx) for rp in ({"sessionStatus": "waiting_human"}, {"sessionStatus": "working"}, {})
                     for row, tx in (({}, shut), ({}, footer), ({"finished": True, "finishedAt": after}, base),
                                     (marker, shut))]
    # #2465 Phase 6 read over #2510's rows: a row carrying a finish signal whose final message says work
    # remains (and is not a trusted /unattended report) reads CONTRADICTED, never closed and nudged with
    # /next-move, where #2510's rows were written against the older ACTIVE.
    cases = [(row, rp, tx, "CONTRADICTED" if want == "ACTIVE" and frag == "work remains" and (
        row.get("finished") or rp.get("sessionStatus") == "finished") else want, frag)
        for row, rp, tx, want, frag in cases]
    g = globals()
    stubs = {"proc_facts": lambda table, pid, windows=WINDOWS: ([11], [], []),
             "live_session_ids": lambda claudes: {"s1"}, "raw_tail": lambda tid, chars=30000: "screen",
             # keyword-only like the real one, so a positional width in classify fails here
             "running_turn": lambda text: False, "prompt_state": lambda raw, *, cols: ("empty", ""),
             "wrap_cols": lambda term, windows=None: None, "git_rev_parse_dirs": fake_git_dirs}
    saved = {k: g[k] for k in list(stubs) + ["transcript_facts"]}
    term = {"id": "t1", "isAlive": True, "pid": 10}
    checks = 0
    try:
        g.update(stubs)
        for row, rp, tx, want, frag in cases:
            g["transcript_facts"] = lambda r, tx=tx: dict(tx)
            got, why = classify(term, dict(row, claudeSessionId="s1"), {10: (1, ["bash"])}, 15, "", {"s1": rp})
            said_ = "; ".join(why)
            if got != want or not all((f[1:] not in said_) if f.startswith("!") else (f in said_)
                                      for f in ((frag,) if isinstance(frag, str) else frag)):
                bad.append(f"classify(row={row}, readiness={rp}, final={tx['final_text'][:20]!r}) = {got} {why}, "
                           f"want {want} with {frag!r}")
        for row, rp, tx in condition_one:
            g["transcript_facts"] = lambda r, tx=tx: dict(tx)
            got, why = classify(term, dict(row, claudeSessionId="s1"), {10: (1, ["bash"])}, 15, "", {"s1": rp})
            if got in CLOSABLE or "not finished (finished-criteria condition 1)" not in "; ".join(why):
                bad.append(f"condition 1: classify(row={row}, readiness={rp}, final={tx['final_text'][-30:]!r}) = "
                           f"{got} {why}: a live row coord does not read finished must not be closable")
        # The screen reads the prompt at the pane's width, passed by keyword (plan Phase 7, D3).
        widths = []
        g["prompt_state"] = lambda raw, *, cols: widths.append(cols) or ("empty", "")
        g["wrap_cols"] = lambda term, windows=None: term.get("cols")
        g["transcript_facts"] = lambda r: dict(base)
        checks += 1
        try:
            got = classify(dict(term, cols=50), {"claudeSessionId": "s1"}, {10: (1, ["bash"])}, 15, "", {"s1": {}})
        except TypeError as e:  # a positional width: the real prompt_state takes cols by keyword only
            got = ("raised", [f"TypeError: {e}"])
        if widths != [50]:
            bad.append(f"classify's prompt read got the widths {widths}, want [50] by keyword: {got}")
        g["prompt_state"], g["wrap_cols"] = stubs["prompt_state"], stubs["wrap_cols"]
        # Condition 3 is not read for a pane whose claude has exited (EXITED-FINISHED): closing it ends no
        # session and changes no tree, and the verdict is reached before the transcript is read at all.
        asked = []
        g["git_rev_parse_dirs"] = lambda cwd, timeout=None: asked.append(cwd) or GIT_ANSWERS["/w/repo"]
        g["transcript_facts"] = lambda r: dict(base, cwd="/w/repo")
        got, why = classify({"id": "t1", "isAlive": False, "pid": 99}, {"finished": True, "claudeSessionId": "s1"},
                            {10: (1, ["bash"])}, 15, "", {"s1": {}})
        checks += 1
        if got != "EXITED-FINISHED" or asked:
            bad.append(f"an exited pane in a primary checkout = {got} {why} (git asked about {asked}), "
                       "want EXITED-FINISHED with no custody probe")
        g["git_rev_parse_dirs"] = fake_git_dirs
        # The custody probe decodes git's answer as UTF-8, the rule PR #2424 set for
        # every child read: under a cp1252 or C-locale default a non-ASCII path comes back garbled, and a
        # garbled --git-dir beside a relative --git-common-dir reads a primary checkout as a linked one.
        seen, real_run = {}, subprocess.run
        try:
            subprocess.run = lambda *a, **k: seen.update(k) or subprocess.CompletedProcess(a[0], 0, "", "")
            saved["git_rev_parse_dirs"]("/w/any")
        finally:
            subprocess.run = real_run
        checks += 1
        if (seen.get("encoding"), seen.get("errors")) != ("utf-8", "replace"):
            bad.append(f"git_rev_parse_dirs decodes git's answer with encoding={seen.get('encoding')!r} "
                       f"errors={seen.get('errors')!r}, want 'utf-8' / 'replace'")
        # screen_all carries the time into the row the nudge screen judges.
        for k, v in {"terminals": lambda: [term], "open_rows": lambda: {"t1": {"claudeSessionId": "s1"}},
                     "page_names": dict, "proc_table": dict,
                     "readiness": lambda: ({"s1": dict(cf, sessionStatusSince=7)}, None),
                     "screen_one": lambda *a: ("FINISHED", [])}.items():
            saved.setdefault(k, g[k])
            g[k] = v
        out = screen_all(15, None)[0]
        if [r.get("sessionStatusSince") for r in out] != [7]:
            bad.append(f"screen_all dropped sessionStatusSince: {out}")
    finally:
        g.update(saved)
    # last_turn_open on written transcripts: (records, want offset from t0 in s, want text prefix,
    # want last_prompt offset or None). A wake never moves last_prompt.
    t0 = 1_790_000_000_000

    def rec(kind, sec, content, **kw):
        stamp = dt.datetime.fromtimestamp((t0 + sec * 1000) / 1000, dt.timezone.utc).isoformat()
        msg = {"role": kind, "content": content}
        if "stop" in kw:
            msg["stop_reason"] = kw.pop("stop")
        return dict({"type": kind, "timestamp": stamp, "message": msg}, **kw)

    def said(sec, stop="end_turn"):
        return rec("assistant", sec, [{"type": "text", "text": "ok"}], stop=stop)
    tool = [{"type": "tool_use", "id": "u1", "name": "Bash", "input": {"command": "ls"}}]
    result = [{"type": "tool_result", "tool_use_id": "u1", "content": "x"}]
    turn_cases = [
        # a wake, a peer message (meta), then a turn that injects a skill body mid-turn
        ([rec("user", 0, "fix the bug"), said(5), rec("user", 60, wake), said(65),
          rec("user", 120, "<system-reminder>The user named this session x</system-reminder>", isMeta=True),
          rec("user", 300, "Another Claude session sent a message: hi", isMeta=True),
          rec("assistant", 301, tool, stop="tool_use"), rec("user", 302, result),
          rec("user", 303, "Base directory for this skill: /x", isMeta=True), said(304)],
         300, "Another Claude session", 0),
        # nothing after the prompt opens a turn: a reminder, a /model answered locally,
        # a tool result carrying text after a record with no stop_reason, a manual compaction
        ([rec("user", 0, "fix the bug"), rec("assistant", 10, tool), rec("user", 11, result + [
            {"type": "text", "text": "hook context"}]), said(12),
          rec("user", 100, "<system-reminder>The user named this session x</system-reminder>", isMeta=True),
          rec("user", 125, "Caveat: The messages below were generated by the user while running local commands."),
          rec("user", 130, "<command-name>/model</command-name>"), rec("user", 131, "<local-command-stdout>ok"),
          rec("user", 200, "Summary of the earlier conversation", isCompactSummary=True)],
         0, "fix the bug", None),  # last_prompt reads a tool result's text block as a prompt
        # an interrupted turn ends there, so the wake after it opens one
        ([rec("user", 0, "fix the bug"), rec("assistant", 5, tool, stop="tool_use"),
          rec("user", 6, "[Request interrupted by user for tool use]"), rec("user", 50, wake)],
         50, "<task-notification>", 0),
        # coord's Session Bus delivery is a NON-meta system reminder, and it opens a turn
        ([rec("user", 0, "fix the bug"), said(5),
          rec("user", 20, "<system-reminder>The user named this session x</system-reminder>", isMeta=True),
          rec("user", 60, "<system-reminder>Session Bus ci_rework message (blocking priority). Act on it, "
                          "then coord_ack_message message_id=m1. Message: check #3 went red</system-reminder>"),
          said(65)],
         60, "<system-reminder>Session Bus ci_rework", 0),
        # a session's own scheduled wake is a META record: it opens a turn, and is never a prompt
        ([rec("user", 0, "fix the bug"), said(5), rec("user", 60, SCHEDULED_WAKE, isMeta=True), said(65)],
         60, SCHEDULED_WAKE, 0),
    ]
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "projects", "x"))
        for k, (recs, want_at, want_text, want_prompt) in enumerate(turn_cases):
            with open(os.path.join(d, "projects", "x", "s.jsonl"), "w", encoding="utf-8") as fh:
                fh.write("".join(json.dumps(r) + "\n" for r in recs))
            got = transcript_facts({"configDir": d, "claudeSessionId": "s"})
            if got["last_turn_open"] != t0 + want_at * 1000 or not got["last_turn_open_text"].startswith(want_text) \
                    or (want_prompt is not None and got["last_prompt"] != t0 + want_prompt * 1000):
                bad.append(f"turn case {k}: last_turn_open={got['last_turn_open']} "
                           f"({got['last_turn_open_text'][:30]!r}), last_prompt={got['last_prompt']}; "
                           f"want {t0 + want_at * 1000} ({want_text!r}), last_prompt offset {want_prompt}")
            if not want_text.startswith(("<system-reminder>Session Bus", SCHEDULED_WAKE)):
                continue
            # ...so a finished session re-tasked or woken that way after its finish stays open: after coord's
            # stamp, and after the runner mark alike (plan Phase 6, N8 and N5: that arm read prompts only).
            for row, rp, finish in (({}, at(t0 + 30 * 1000), "coord's stamp"),
                                    ({"finished": True, "finishedAt": t0 + 30 * 1000}, {}, "the runner mark")):
                saved = {k: g[k] for k in list(stubs) + ["transcript_facts"]}
                try:
                    g.update(stubs, transcript_facts=lambda r, got=got: dict(got))
                    v, why = classify(term, dict(row, claudeSessionId="s1"), {10: (1, ["bash"])}, 15, "", {"s1": rp})
                finally:
                    g.update(saved)
                checks += 1
                if v != "REVIVED" or "a turn opened" not in "; ".join(why):
                    bad.append(f"{want_text[:30]!r} after {finish} = {v} {why}, want REVIVED (a turn opened)")
        # An idle compaction is not turn activity: the idle floor reads the reply before it.
        with open(os.path.join(d, "projects", "x", "s.jsonl"), "w", encoding="utf-8") as fh:
            fh.write("".join(json.dumps(r) + "\n" for r in [
                rec("user", 0, "fix the bug"), said(5),
                rec("user", 3000, "This session is being continued from a previous conversation",
                    isCompactSummary=True)]))
        got = transcript_facts({"configDir": d, "claudeSessionId": "s"})
        if got["last_activity"] != t0 + 5 * 1000:
            bad.append(f"an idle compaction moved last_activity to {got['last_activity']}, "
                       f"want {t0 + 5 * 1000} (the reply before it)")
        # ...while a compaction inside a turn is followed by that turn's own records, which count.
        with open(os.path.join(d, "projects", "x", "s.jsonl"), "w", encoding="utf-8") as fh:
            fh.write("".join(json.dumps(r) + "\n" for r in [
                rec("user", 0, "fix the bug"), rec("assistant", 5, tool, stop="tool_use"),
                rec("user", 6, result), rec("user", 60, "Summary", isCompactSummary=True), said(90)]))
        got = transcript_facts({"configDir": d, "claudeSessionId": "s"})
        if got["last_activity"] != t0 + 90 * 1000:
            bad.append(f"a mid-turn compaction left last_activity at {got['last_activity']}, "
                       f"want {t0 + 90 * 1000} (the turn's reply after it)")
    return len(cases) + 1 + len(turn_cases) + 2 + checks, len(condition_one)


def opener_cases(bad):
    """Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run Phase 3:
    opener_class sorts a turn-opening record into prompt / delivery / self, and a
    revival's reason names the class on both finish arms, a delivery past the
    stamp outranking a later self opener. Every class revives alike (one finish
    rule), so no verdict depends on the class. Literal strings. Appends
    failures; returns the count."""
    import tempfile
    bus = "<system-reminder>Session Bus ci_rework message (blocking priority). Act on it"
    wake = "<task-notification><task-id>b1</task-id> background build finished"
    n = 0
    for flags, text, want in [  # (record flags, text, want)
            ({}, "fix the bug", "prompt"),
            ({}, "<command-name>/loop</command-name>", "prompt"),  # what last_prompt counts stays a prompt
            ({}, "<pasted_content>also fix the docs</pasted_content>", "prompt"),  # SYNTHESIZED wrapper
            ({}, bus, "delivery"),
            ({}, "  " + bus, "delivery"),
            ({}, "<pasted_content>" + bus + "</system-reminder></pasted_content>", "delivery"),  # SYNTHESIZED
            ({}, '<pasted_content id="p1">\n' + bus, "delivery"),  # SYNTHESIZED: attributes, a newline
            ({"isMeta": True}, "Another Claude session sent a message: hi", "delivery"),
            ({}, "Another Claude session sent a message: hi", "delivery"),
            ({"isMeta": True}, bus, "self"),  # a meta reminder is no delivery (and opens no turn)
            ({}, "<system-reminder>Session Bus", "self"),  # not the delivery's prefix
            ({}, "<system-reminder>A background agent finished.</system-reminder>", "self"),
            ({}, wake, "self"),
            ({"isMeta": True}, SCHEDULED_WAKE, "self"),  # a wake or auto-continuation
            ({"isMeta": True}, "anything else", "self")]:  # unmatched: self
        n += 1
        if opener_class(flags, text) != want:
            bad.append(f"opener_class({flags}, {text[:40]!r}) = {opener_class(flags, text)}, want {want}")
    t0 = 1_790_000_000_000

    def rec(kind, sec, content, **kw):
        stamp = dt.datetime.fromtimestamp((t0 + sec * 1000) / 1000, dt.timezone.utc).isoformat()
        msg = {"role": kind, "content": content}
        if "stop" in kw:
            msg["stop_reason"] = kw.pop("stop")
        return dict({"type": kind, "timestamp": stamp, "message": msg}, **kw)

    def said(sec):
        return rec("assistant", sec, [{"type": "text", "text": "ok"}], stop="end_turn")
    named = {c: f"({OPENER_NAMES[c]})" for c in OPENER_NAMES}
    # (records, want last_turn_open_class, want reason fragment on both arms; a finish stamp 30 s in)
    cases = [
        # a bare Session Bus delivery
        ([rec("user", 0, "fix the bug"), said(5), rec("user", 60, bus + "</system-reminder>"), said(65)],
         "delivery", ("a turn opened", named["delivery"], "Session Bus")),
        # SYNTHESIZED: the same delivery wrapped in <pasted_content>. No code in the runner, this repo or coord
        # writes or names the wrapper, so this record's shape is assumed, not measured. last_prompt counts it
        # (it starts with no NON_PROMPT prefix); its class still names it a delivery.
        ([rec("user", 0, "fix the bug"), said(5),
          rec("user", 60, "<pasted_content>" + bus + "</system-reminder></pasted_content>"), said(65)],
         "delivery", ("then a delivery opened a turn", named["delivery"])),
        # SYNTHESIZED: pasted text that is no delivery is a prompt
        ([rec("user", 0, "fix the bug"), said(5), rec("user", 60, "<pasted_content>also fix the docs</pasted_content>"),
          said(65)], "prompt", ("then prompted", "!" + named["delivery"])),
        # a wake: the session's own turn
        ([rec("user", 0, "fix the bug"), said(5), rec("user", 60, wake), said(65)],
         "self", ("a turn opened", named["self"], "task-notification")),
        # a non-meta <system-reminder> that is no Session Bus delivery: self
        ([rec("user", 0, "fix the bug"), said(5),
          rec("user", 60, "<system-reminder>A background agent finished.</system-reminder>"), said(65)],
         "self", ("a turn opened", named["self"])),
        # a (meta) peer message, then a later wake: the newest turn is self, but the delivery past the stamp
        # names it
        ([rec("user", 0, "fix the bug"), said(5),
          rec("user", 60, "Another Claude session sent a message: rebase #12", isMeta=True), said(65),
          rec("user", 120, wake), said(125)],
         "self", ("a turn opened", named["delivery"], "Another Claude session", "!" + named["self"])),
    ]
    g = globals()
    stubs = {"proc_facts": lambda table, pid, windows=WINDOWS: ([11], [], []),
             "live_session_ids": lambda claudes: {"s1"}, "raw_tail": lambda tid, chars=30000: "screen",
             "running_turn": lambda text: False, "prompt_state": lambda raw, *, cols: ("empty", ""),
             "wrap_cols": lambda term, windows=None: None, "git_rev_parse_dirs": fake_git_dirs}
    term = {"id": "t1", "isAlive": True, "pid": 10}
    stamp = t0 + 30 * 1000
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "projects", "x"))
        for k, (recs, want_cls, frags) in enumerate(cases):
            with open(os.path.join(d, "projects", "x", "s.jsonl"), "w", encoding="utf-8") as fh:
                fh.write("".join(json.dumps(r) + "\n" for r in recs))
            got = transcript_facts({"configDir": d, "claudeSessionId": "s"})
            n += 1
            if got["last_turn_open_class"] != want_cls:
                bad.append(f"opener case {k}: last_turn_open_class={got['last_turn_open_class']}, want {want_cls}")
            for arm, row, rp in (("coord", {}, {"sessionStatus": "finished", "sessionStatusSince": stamp}),
                                 ("marker", {"finished": True, "finishedAt": stamp}, {})):
                saved = {k_: g[k_] for k_ in list(stubs) + ["transcript_facts"]}
                try:
                    g.update(stubs, transcript_facts=lambda r, got=got: dict(got))
                    v, why = classify(term, dict(row, claudeSessionId="s1"), {10: (1, ["bash"])}, 15, "", {"s1": rp})
                finally:
                    g.update(saved)
                said_ = "; ".join(why)
                n += 1
                # one revival rule: every class revives, uncoded (a revival is not a held finish)
                if v != "REVIVED" or "finish_stranded" in said_ or not all(
                        (f[1:] not in said_) if f.startswith("!") else (f in said_) for f in frags):
                    bad.append(f"opener case {k} on the {arm} arm = {v} {why}; want REVIVED with {frags}")
    return n


def compaction_cases(bad):
    """Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run Phase 6
    (N8): an idle compaction is not a running turn, and a compaction inside a
    turn is followed by a spinner that still counts. The fixture is
    SYNTHESIZED (no temporary runner was used), in generic text, ANSI-stripped
    as running_turn reads it. Its turn-end line carries no clock, so only the
    compaction rule ends the spinner. Appends failures; returns the count."""
    compact = ("\u273b Worked for 3m 12s\n\u2722 Running PreCompact hooks\u2026 (running PreCompact hook)\n"
               "\u276f\xa0 \u00b7 esc to interrupt\n\u273d Compacting conversation\u2026\n\u25cf Compacted "
               "while idle, before the prompt cache expired\nConversation compacted (ctrl+o for history)\n"
               "\u276f\xa0Checking for updates")
    n = 0
    for text, want in [(compact, False),
                       # a compaction inside a turn: the turn's next spinner follows it
                       (compact + "\n\u2722 Thinking\u2026 (esc to interrupt)", True),
                       ("\u273b Worked for 2s\n\u2722 Thinking\u2026 (esc to interrupt)", True),
                       ("\u2722 Thinking\u2026 (esc to interrupt)\n\u273b Worked for 2s", False)]:
        n += 1
        if running_turn(text) != want:
            bad.append(f"running_turn {text[-70:]!r} expected {want}")
    return n


def roster_cases(bad):
    """The verdict roster (plan Phase 2, D1): VERDICTS against classify_row's own
    return statements, this report's ORDER and CLOSABLE against VERDICTS, and
    print_report over one row of every verdict. Returns the check count."""
    import contextlib
    import io
    returned = returned_verdicts(classify_row)
    for what, extra in (("classify_row returns a verdict VERDICTS does not list", returned - set(VERDICTS)),
                        ("VERDICTS lists a verdict classify_row never returns", set(VERDICTS) - returned),
                        ("ORDER has no report row for", set(VERDICTS) - set(ORDER)),
                        ("ORDER names a verdict classify never returns", set(ORDER) - set(VERDICTS)),
                        ("CLOSABLE names a verdict classify never returns", CLOSABLE - set(VERDICTS))):
        if extra:
            bad.append(f"verdict roster: {what}: {sorted(extra)}")
    rows = [{"terminalId": f"t{i:07d}", "verdict": v, "reasons": [], "name": "", "tab": "", "account": ""}
            for i, v in enumerate(VERDICTS)]
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            print_report(rows, [])
    except ValueError as e:
        bad.append(f"print_report over every verdict raised {e!r}")
    return 6


def _tool_argv(cmd, acct="sales", cwd_file="/tmp/claude-1a2b-cwd"):
    """A Linux tool shell's argv as Claude Code builds it (measured 2026-10-23)."""
    return ["/bin/bash", "-c", f"source /x/.claude-{acct}/shell-snapshots/snapshot-bash-1-x.sh 2>/dev/null || "
            "true && shopt -u extglob 2>/dev/null || true && { \\builtin unalias -- 'unsetenv'; \\builtin unset -f -- "
            "'unsetenv'; } >/dev/null 2>&1 || true && eval '" + cmd.replace("'", "'\"'\"'")
            + f"' < /dev/null && pwd -P >| {cwd_file}"]


def zombie_live_selftest(bad):
    """ProcFacts.zombie() on real /proc (Linux only; skipped elsewhere): an unreaped child
    reads zombie; a child whose MAIN thread exited while a worker still runs reads state Z
    with an empty cmdline too, and must NOT, because it is live work. Returns cases run."""
    if WINDOWS or not os.path.isdir("/proc/self"):
        return 0
    import ctypes
    import threading
    kids = []
    try:
        dead = os.fork()
        if dead == 0:
            os._exit(0)
        kids.append(dead)
        r, w = os.pipe()
        busy = os.fork()
        if busy == 0:  # the main thread exits; the worker lives until the parent closes the pipe
            os.close(w)
            threading.Thread(target=lambda: (os.read(r, 1), os._exit(0))).start()
            ctypes.CDLL(None).pthread_exit(None)
            os._exit(1)  # pthread_exit does not return
        os.close(r)
        kids.append(busy)
        deadline, got = time.time() + 5, None
        while time.time() < deadline:
            time.sleep(0.1)
            t = proc_table() or {}
            try:
                st = open(f"/proc/{busy}/stat").read().rsplit(")", 1)[1].split()[0]
            except (OSError, IndexError):
                st = None
            if t.get(dead, (0, ["x"]))[1] == [] and st == "Z" and t.get(busy, (0, ["x"]))[1] == []:
                f = ProcFacts(t)
                got = (f.zombie(dead), f.zombie(busy))
                break
        if got is None:
            return 0  # this kernel never showed both shapes inside the window: nothing to judge
        if got != (True, False):
            bad.append(f"zombie(): unreaped child / main-thread-exited live child = {got}, want (True, False)")
        return 2
    finally:
        try:
            os.close(w)
        except (OSError, UnboundLocalError):
            pass
        for k in kids:
            try:
                os.waitpid(k, 0)
            except OSError:
                pass


def end_strays_selftest(bad):
    """close_one's signalling half, end_strays(), on wait loops this test starts
    itself (Linux only; skipped elsewhere). It signals nothing but its own children
    and reaps them on every path. A recorded stray is ended; an entry whose start
    time or argv is wrong is left alone; a loop whose condition flipped, now running
    its post-loop command, is left alone. Returns the number of cases run."""
    if WINDOWS or not os.path.isdir("/proc/self") or not os.path.exists("/bin/bash"):
        return 0
    import ctypes
    import shutil
    import signal
    import tempfile
    n, kids, d = 0, [], tempfile.mkdtemp(prefix="cfs-endstray-")
    token = f"cfs-endstray-{os.getpid()}-{os.urandom(4).hex()}"
    log, log2 = os.path.join(d, "build.log"), os.path.join(d, "other.log")
    libc, parent = ctypes.CDLL(None, use_errno=True), os.getpid()

    def die_with_parent():  # in the child, before exec: a parent that dies by any signal takes the loop with it
        libc.prctl(1, signal.SIGKILL, 0, 0, 0)  # PR_SET_PDEATHSIG
        if os.getppid() != parent:
            os._exit(1)  # the parent died before the line above ran

    def spawn(cmd):
        argv = _tool_argv(cmd, cwd_file=os.path.join(d, "cwd"))
        kid = subprocess.Popen(argv, start_new_session=True, stdin=subprocess.DEVNULL, preexec_fn=die_with_parent,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        kids.append(kid)
        return kid, argv

    def on_signal(signum, _frame):
        raise SystemExit(128 + signum)  # so the `finally` below still reaps the children

    handlers = {}
    for sig in (signal.SIGTERM, signal.SIGHUP):
        try:
            handlers[sig] = signal.signal(sig, on_signal)
        except ValueError:  # not the main thread: PR_SET_PDEATHSIG still covers a dying parent
            pass
    try:
        for f in (log, log2):
            with open(f, "w") as fh:
                fh.write("Compiling\n")
            os.utime(f, (time.time() - 3600, time.time() - 3600))
        if shutil.which("pgrep"):
            a, a_argv, a_cls = *spawn(f"until ! pgrep -f {token} >/dev/null; do sleep 0.2; done; true"), "a"
        else:
            a, a_argv, a_cls = *spawn(f"until grep -q DONE {log2}; do sleep 0.2; done; true"), "b-file"
        b, b_argv = spawn(f"until grep -q DONE {log}; do sleep 0.2; done; sleep 30")
        verdicts, deadline = None, time.time() + 5
        while time.time() < deadline:  # bounded: until both children have exec'd and been seen
            time.sleep(0.2)
            t = proc_table() or {}
            if a.pid in t and b.pid in t and t[a.pid][1] == a_argv and t[b.pid][1] == b_argv:
                f = ProcFacts(t)
                verdicts = stray_verdict(t, a.pid, f, 15)[:2], stray_verdict(t, b.pid, f, 15)[:2]
                if verdicts == ((True, a_cls), (True, "b-file")):  # a probe caught mid-read can flicker: retry
                    break
        n += 1
        if verdicts != ((True, a_cls), (True, "b-file")):
            bad.append(f"end_strays: this test's own loops did not read as strays: {verdicts}")
            return n
        sa, sb = proc_start(a.pid), proc_start(b.pid)
        n += 1
        msg = end_strays([(a.pid, sa + 1, a_argv, a_cls), (a.pid, sa, a_argv + ["x"], a_cls)], 15, wait_s=0.5)
        if a.poll() is not None or msg != "; 2 stray wait loop(s) gone with the pane":
            bad.append(f"end_strays signalled an entry whose start time or argv was wrong: {msg!r}")
        n += 1
        with open(log, "a") as fh:
            fh.write("DONE\n")  # the condition flips: the loop moves on to its post-loop sleep
        time.sleep(0.6)
        msg = end_strays([(b.pid, sb, b_argv, "b-file")], 15, wait_s=0.5)
        if b.poll() is not None or "could not end: no longer a b-file stray" not in msg:
            bad.append(f"end_strays signalled a loop whose condition flipped: {msg!r}")
        n += 1
        saved = globals()["proc_table"]
        globals()["proc_table"] = lambda: None  # the re-check cannot read the table: nothing may be sent
        try:
            msg = end_strays([(a.pid, sa, a_argv, a_cls)], 15, wait_s=0.5)
        finally:
            globals()["proc_table"] = saved
        if a.poll() is not None or msg != f"; stray pid {a.pid} outlived the pane close; could not end: process " \
                                          "table unreadable at re-check":
            bad.append(f"end_strays with an unreadable process table: {msg!r}")
        n += 1
        msg = end_strays([(a.pid, sa, a_argv, a_cls)], 15, wait_s=2)
        try:
            a.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        if msg != f"; stray pid {a.pid} outlived the pane close; ended" or a.poll() is None:
            bad.append(f"end_strays did not end a recorded, unchanged stray: {msg!r}")
    finally:
        for kid in kids:  # this test's own children, each in its own session: never anything else
            if kid.poll() is None:
                try:
                    os.killpg(kid.pid, signal.SIGKILL)
                except OSError:
                    pass
            try:
                kid.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        shutil.rmtree(d, ignore_errors=True)
        for sig, old in handlers.items():
            signal.signal(sig, old)
    return n


def stray_selftest(bad, empty_prompt=EMPTY_PROMPT_SCREEN):
    """The stray-wait-loop classifier on synthetic process tables (nothing live is
    read). Returns the number of cases run."""
    empty_prompt = empty_prompt or EMPTY_PROMPT_SCREEN
    n, now, me = 0, 1e9, _user(my_uid())
    gp, cg, out = "git push -u origin HEAD", "cargo-guard.sh test -- mcp::bridges", \
        "/scratch/claude-1000/-s-proj/f116bbce-1e85-4828-a73f-06f0b90430b7/tasks/b1r1ys0pd.output"
    # The measured d2dd1423 set: 8 self-matching loops on 2 patterns, each with a sleep child.
    loops = {445646: f'until ! pgrep -f "{gp}" >/dev/null; do sleep 15; done; cat {out}',
             464345: f'until ! pgrep -f "{gp}" >/dev/null; do sleep 15; done; cat {out}',
             1873808: f'until ! pgrep -f "{gp}" >/dev/null; do sleep 15; done; cat {out}',
             3135797: f'until ! pgrep -f "{gp}" >/dev/null; do sleep 15; done; cat {out}',
             3139942: f'until ! pgrep -f "{gp}" >/dev/null; do sleep 15; done; echo finished',
             2786305: f'until ! pgrep -f "{cg}" >/dev/null; do sleep 15; done; grep -E "^error|bridges::|test result: '
                      'ok. [1-9]|FAILED|verdict" /s/cargotest.log | head',
             3690915: f'until ! pgrep -f "{cg}" >/dev/null; do sleep 15; done; echo done',
             3743706: (f'until ! pgrep -f "{cg}" >/dev/null; do sleep 15; done; '
                       f'cat {out.rsplit("/", 1)[0]}/b2rb7udxa.output')}
    base = {1: (0, ["/sbin/init"]), 20: (1, ["pty-holder", "--", "claude"]),
            21033: (20, ["/usr/bin/claude", "--resume", "f116bbce"]), 30: (1, ["python3", "cfs.py", "--report"]),
            50: (1, ["vim", "notes.txt"])}
    d2 = dict(base)
    for p, c in loops.items():
        d2[p], d2[p + 1] = (21033, _tool_argv(c)), (p, ["sleep", "15"])
    d2[445648] = (445646, ["pgrep", "-f", gp])  # a loop's own probe caught mid-flight is a watcher, not a target

    def split(table, files=None, start=None, windows=False):
        _FACTS[:] = [table, ProcFacts(table, start=start or {}, uids={}, files=files or {}, self_pids={30}, now=now)]
        return split_strays(table, proc_facts(table, 20, windows=False)[1], 15, windows=windows)

    tdir = "/scratch/claude-1000/-s-proj/5b330eb3-5df7-459f-adf6-7927a533d510/tasks"  # a harness tasks dir, present

    def one(cmd, extra=None, files=None, start=None, kids=(("sleep", "15"),), dirs=None):
        t = {1: (0, ["/sbin/init"]), 10: (1, ["/usr/bin/claude"]), 30: (1, ["python3", "cfs.py"]),
             40: (10, _tool_argv(cmd))}
        t.update({41 + k: (40, list(argv)) for k, argv in enumerate(kids)})
        t.update(extra or {})
        files = files or {}
        # present dirs: the harness tasks dir, two ordinary `tasks` dirs, and each file's own
        if dirs is None:
            dirs = {tdir, "/s/proj/tasks", "/s/ci/tasks", "/s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks"} | {
                f.rsplit("/", 1)[0] for f in files}
        return stray_verdict(t, 40, ProcFacts(t, start=start or {}, uids={}, files=files, self_pids={30},
                                              now=now, dirs=dirs), 15)

    try:
        n += 1
        work, strays, waits = split(d2)
        if work or sorted(p for p, _, _ in strays) != sorted(loops) or {c for _, c, _ in strays} != {"a"}:
            bad.append(f"d2dd1423 set: work={work} strays={[(p, c) for p, c, _ in strays]}")
        # (2) one stray beside one real cargo test: the cargo shell is work.
        n += 1
        mix = dict(d2)
        mix[700], mix[701] = (21033, _tool_argv("cargo test -p qontinui-runner 2>&1 | tail -20")), \
            (700, ["cargo", "test", "-p", "qontinui-runner"])
        work, strays, _ = split(mix)
        if work != [700] or len(strays) != 8:
            bad.append(f"stray + cargo test: work={work}, {len(strays)} strays")
        # (4) a self-matching loop whose target still runs waits on live work.
        n += 1
        live = dict(d2)
        live[800] = (1, ["git", "push", "-u", "origin", "HEAD"])
        work, strays, waits = split(live)
        if sorted(work) != sorted(p for p in loops if gp in loops[p]) or len(strays) != 3 or \
                not waits or waits[0] != "waits on live pid 800 (self-matching loop: will never exit on its own)":
            bad.append(f"live target: work={work} strays={len(strays)} waits={waits[:1]}")
        # (6) Windows command lines are not read: every shell is work.
        n += 1
        if split(d2, windows=True)[1] or one("x")[0] or \
                stray_verdict({7: (1, ["bash.exe", UNREADABLE_CMDLINE])}, 7, ProcFacts({}, start={}, uids={}, files={},
                                                                                  self_pids=set()), 15)[0]:
            bad.append("a Windows / unreadable command line must never read as stray")
        # (7) an empty-argv child that is NOT a zombie (a live process that blanked its argv,
        # or a threaded one whose main thread exited) is work; a zombie one runs nothing.
        n += 1
        zl = "while ps aux | grep cargo >/dev/null; do sleep 5; done"
        zt = {1: (0, ["/sbin/init"]), 10: (1, ["/usr/bin/claude"]), 30: (1, ["python3", "cfs.py"]),
              40: (10, _tool_argv(zl)), 41: (40, [])}
        for zs, want in ((set(), False), ({41}, True)):
            got = stray_verdict(zt, 40, ProcFacts(zt, start={}, uids={}, files={}, self_pids={30}, now=now,
                                                  dirs=set(), zombies=zs), 15)
            if got[0] is not want:
                bad.append(f"an empty-argv child with zombies={zs}: {got}, want stray={want}")
        # The classifier, case by case: (want_stray, class or None, command, extra, files, start, kids).
        census = "/s/census/cargo.log"
        old, held = (now - 3 * 3600, set(), "Compiling x\n"), (now - 3 * 3600, {950}, "Compiling x\n")
        task = f"{tdir}/brm8jg1o0.output"
        twait = f"until grep -qE 'exit=' {task} 2>/dev/null; do sleep 20; done; cat {task}"
        esc = (f"L={census}; until grep -qE 'test result|^error|could not compile' $L || ! pgrep -f "
               "'cargo test -- zz_census' >/dev/null; do sleep 15; done; tail -8 $L")
        cases = [
            # (5) a tasks/<id>.output marker wait
            (True, "b-file", twait, None, {task: old}, None, (("sleep", "20"),)),
            (False, None, twait, None, {task: held}, None, (("sleep", "20"),)),  # a process holds it open
            (True, "b-file", f"until grep -q DONE {task}; do sleep 5; done", None,
             {task: (now - 3 * 3600, {41}, "Compiling x\n")}, None,
             (("grep", "-q", "DONE", task),)),  # the holder is the loop's own probe, caught mid-read
            (False, None, twait, None, {task: (now - 60, set(), "x\n")}, None, ()),  # changed inside the idle floor
            (False, None, f"until grep -q DONE {census}; do sleep 5; done", None,
             {census: (now - 5 * 60, set(), "x\n")}, None, ()),  # 5 minutes is under the floor, too
            (False, None, twait, None, {task: (now - 3 * 3600, set(), "exit=0\n")}, None, ()),  # already holds
            (True, "b-file", twait, None, {}, None, ()),  # a harness task file gone from a live tasks dir
            # a tasks/*.output outside the harness layout is an ordinary file: missing means "not yet"
            (False, None, "until [ -f /s/proj/tasks/build.output ]; do sleep 5; done; ./deploy.sh", None, {}, None,
             ()),
            (False, None, "until grep -q DONE /s/ci/tasks/job42.output; do sleep 30; done; ./release.sh", None, {},
             None, ()),
            (False, None, "until [ -f /s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks/out.output ]; do sleep 5; "
                          "done; ./ship.sh", None, {}, None, ()),  # a uuid dir outside the harness scratch layout
            (False, None, "L=/s/a.log; L=/s/b.log :; until grep -q DONE $L; do sleep 5; done", None,
             {"/s/a.log": old, "/s/b.log": old}, None, ()),  # a prefix assignment's persistence depends on the shell
            # only inert commands may precede the loop: a detached producer there may be the writer
            (False, None, "setsid -f bash -c 'sleep 3600; echo DONE >> /s/x.log'; until grep -q DONE /s/x.log; "
                          "do sleep 5; done", None, {"/s/x.log": old}, None, ()),
            (False, None, "X=$(setsid -f prod); until grep -q DONE /s/x.log; do sleep 5; done", None,
             {"/s/x.log": old}, None, ()),  # a value can hide a command substitution
            (True, "b-file", "S=/s; cd $S; export L=$S/x.log; echo watching; until grep -q DONE $L; do sleep 5; "
                             "done", None, {"/s/x.log": old}, None, ()),
            # a missing file that is not a task file does not exist YET: the standard wait on real work
            (False, None, "until [ -f /s/build.done ]; do sleep 10; done; ./deploy.sh", None, {}, None, ()),
            (False, None, "while [ ! -f /s/build.done ]; do sleep 10; done; git push", None, {}, None, ()),
            (False, None, "until grep -q DONE /s/build.log 2>/dev/null; do sleep 10; done; echo ok", None, {}, None,
             ()),
            # brace expansion and a changed IFS make the path unknown, even when a file of that literal name is frozen
            (False, None, "until grep -q DONE /s/{build,test}.log; do sleep 5; done", None,
             {"/s/{build,test}.log": old}, None, ()),
            (False, None, "IFS=,; L=/s/a.log,/s/b; until grep -q DONE $L; do sleep 5; done", None,
             {"/s/a.log,/s/b": old}, None, ()),
            # a body that does more than wait: a command substitution, printf -v, a write
            (False, None, f'until ! pgrep -f "{gp}"; do sleep $(git push -u origin HEAD; echo 5); done', None, {},
             None, ()),
            (False, None, 'until ! pgrep -f "zzq"; do echo $(make deploy); sleep 5; done', None, {}, None, ()),
            (False, None, "L=/s/a; until grep -q DONE $L; do printf -v L %s /s/b; sleep 5; done", None,
             {"/s/a": old}, None, ()),
            (False, None, "until grep -q DONE /s/a.log; do echo tick >> /s/a.log; sleep 5; done", None,
             {"/s/a.log": old}, None, ()),
            (True, "b-file", "until grep -q DONE /s/a.log; do sleep 5 >/dev/null 2>&1; done", None,
             {"/s/a.log": old}, None, ()),  # redirects to /dev/null are still only waiting
            # a job backgrounded before the loop may still be the writer
            (False, None, f"(sleep 9; echo x > {task}) & until [ -s {task} ]; do sleep 5; done", None,
             {task: (now - 7200, set(), "")}, None, ()),
            (True, "b-file", f"until [ -s {task} ]; do sleep 5; done", None, {task: (now - 7200, set(), "")}, None, ()),
            (False, None, f"until [ -s {task} ]; do sleep 5; done", None, {task: (now - 7200, set(), "x")}, None, ()),
            # the census's `|| ! pgrep -f` escape, with $L expanded from its literal assignment
            (True, "a+b-file", esc, None, {census: old}, None, ()),
            (False, None, esc, {90: (1, ["cargo", "test", "--", "zz_census"])}, {census: old}, None, ()),
            (False, None, "until grep -q x $NOPE; do sleep 5; done", None, {}, None, ()),  # unexpandable
            (False, None, f"L='{task} x'; until grep -q DONE $L; do sleep 5; done", None, {task: old}, None,
             ()),  # an unquoted value that word-splits is not one known path
            (True, "b-file", f"until grep -q DONE {task}; do sleep 5; done", None, {task: old}, None,
             (("sleep", "5"), ("grep", "-q", "DONE", task))),  # its own sleep and probe only
            (False, None, f"until grep -q DONE {task}; do sleep 5; done", None, {task: old}, None,
             (("sleep", "5"), ("cargo", "build"))),  # work running under the loop
            (False, None, f'until ! pgrep -f "{gp}"; do cargo build; sleep 5; done', None, {}, None, ()),  # body works
            # (7) bounded waits: class (c), never stray
            (False, "c", f"timeout 600 bash -c 'until ! pgrep -f \"{gp}\" >/dev/null; do sleep 5; done'", None, {},
             None, (("timeout", "600", "bash", "-c", "..."),)),
            (False, "c", f'i=0; until ! pgrep -f "{gp}"; do sleep 5; i=$((i+1)); done', None, {}, None, ()),
            (False, "c", (f'end=$(( $(date +%s) + 600 )); while [ $(date +%s) -lt $end ] && pgrep -f "{gp}"; '
                          'do sleep 5; done'), None, {}, None, ()),
            # self-excluding patterns are real waits, not class (a)
            (False, None, "until ! pgrep -f '[g]it push' >/dev/null; do sleep 15; done", None, {}, None, ()),
            (False, None, f'until ! pgrep -fx "{gp}" >/dev/null; do sleep 15; done', None, {}, None, ()),
            (False, None, f'until ! pgrep -fA "{gp}" >/dev/null; do sleep 15; done', None, {}, None, ()),
            (False, None, "until ! pgrep cargo; do sleep 15; done", None, {}, None, ()),  # no -f: names, not argv
            (True, "a", "while ps aux | grep cargo >/dev/null; do sleep 5; done", None, {}, None, ()),
            # a probe that exited and is not yet reaped (a zombie: empty cmdline) runs nothing
            (True, "a", "while ps aux | grep cargo >/dev/null; do sleep 5; done", None, {}, None, ((),)),
            (True, "b-file", twait, None, {task: old}, None, (("sleep", "20"), ())),
            (False, None, "while ps aux | grep cargo | grep -v grep >/dev/null; do sleep 5; done", None, {}, None,
             ()),  # -v grep drops the loop's own line too, so it ends with its target
            (False, None, "while ps -p 4242 | grep -q 4242; do sleep 5; done", None, {}, None, ()),  # ps -p: selective
            # `ps aux` prints the user column: a pattern on it still finds the live target
            (False, None, f'while ps aux | grep -q "{me}.*cargo build"; do sleep 5; done',
             {90: (1, ["cargo", "build"])}, {}, None, ()),
            (True, "a", f'while ps aux | grep -q "{me}.*cargo build"; do sleep 5; done', None, {}, None, ()),
            (False, None, 'while ps -eo args | grep -q "^cargo build"; do sleep 5; done', None, {}, None, ()),
            # kill -0: reused pid -> stray; the same process -> a real wait; a gone pid ends a while-loop
            (True, "b-pid", "while kill -0 4242 2>/dev/null; do sleep 5; done", {4242: (1, ["daemon"])}, {},
             {40: 100, 4242: 200}, ()),
            (False, None, "while kill -0 4242 2>/dev/null; do sleep 5; done", {4242: (1, ["make"])}, {},
             {40: 100, 4242: 50}, ()),
            (False, None, "while kill -0 4243 2>/dev/null; do sleep 5; done", None, {}, {40: 100}, ()),
            (False, None, f'until ! pgrep -f "{gp}"; do sleep 5; done & wait', None, {}, None, ()),  # backgrounded
            (False, None, "cat <<EOF\nx\nEOF", None, {}, None, ()),  # heredoc: not read
            # a wait for a file to GO is not proven frozen: nothing need hold a lock file open
            (False, None, "while [ -f /s/lock ]; do sleep 5; done; ./next.sh", None, {"/s/lock": old}, None, ()),
            (False, None, "until [ ! -e /s/lock ]; do sleep 5; done; ./next.sh", None, {"/s/lock": old}, None, ()),
            (False, None, "until ! [ -f /s/lock ]; do sleep 5; done; ./next.sh", None, {"/s/lock": old}, None, ()),
            (False, None, "while [ -s /s/queue ]; do sleep 5; done", None, {"/s/queue": old}, None, ()),
            # ... and written as a read, which passes now and ends only when the file changes or goes
            (False, None, "while grep -q Compiling /s/lock; do sleep 5; done; ./next.sh", None, {"/s/lock": old}, None,
             ()),
            (False, None, "until ! grep -q Compiling /s/lock; do sleep 5; done; ./next.sh", None, {"/s/lock": old},
             None, ()),
            (False, None, "while cat /s/lock >/dev/null 2>&1; do sleep 5; done; ./next.sh", None, {"/s/lock": old},
             None, ()),
            (False, None, "while tail -1 /s/lock | grep -q Compiling; do sleep 5; done", None, {"/s/lock": old}, None,
             ()),
            (False, None, "while grep -q Compiling < /s/lock; do sleep 5; done", None, {"/s/lock": old}, None, ()),
            (False, None, "while tail -1 /s/lock >/dev/null; do sleep 5; done", None, {"/s/lock": old}, None, ()),
            (False, None, "while head -1 /s/lock >/dev/null; do sleep 5; done", None, {"/s/lock": old}, None, ()),
            # `grep -v` waits for its matching lines to go
            (False, None, "until grep -qv Compiling /s/state; do sleep 5; done; ./next.sh", None, {"/s/state": old},
             None, ()),
            (False, None, "while ! grep -qv Compiling /s/state; do sleep 5; done", None, {"/s/state": old}, None, ()),
            # one lock among the files is still a wait on the lock, harness file beside it or not
            (False, None, f"while grep -q Compiling {task} /s/lock; do sleep 5; done; ./next.sh", None,
             {task: old, "/s/lock": old}, None, ()),
            # a bare read's status is its exit code: 0 on an empty file, 1 on a gone one
            (False, None, "until cat /s/empty >/dev/null; do sleep 1; done; sleep 600; ./deploy.sh", None,
             {"/s/empty": (now - 7200, set(), "")}, None, ()),
            (False, None, f"while cat {task} >/dev/null 2>&1; do sleep 1; done; sleep 600; ./deploy.sh", None, {},
             None, ()),
            (False, None, f"until cat {task} >/dev/null; do sleep 1; done", None, {}, None, ()),
            # ... a harness task's own output file stays judged, and so does a wait for a line to appear
            (True, "b-file", f"while [ -f {task} ]; do sleep 5; done", None, {task: old}, None, ()),
            (True, "b-file", "until grep -q DONE /s/a.log; do sleep 5; done", None, {"/s/a.log": old}, None, ()),
            (True, "b-file", "until tail -1 /s/a.log | grep -q DONE; do sleep 5; done", None, {"/s/a.log": old}, None,
             ()),
            (True, "b-file", "until [ -s /s/empty ]; do sleep 5; done", None,
             {"/s/empty": (now - 7200, set(), "")}, None, ()),
            # the reason a shell is busy survives: a live target still names itself
            (False, None, f'until ! pgrep -f "{gp}" && [ ! -f /s/lock ]; do sleep 5; done',
             {90: (1, ["git", "push", "-u", "origin", "HEAD"])}, {"/s/lock": old}, None, ()),
        ]
        for want, cls, cmd, extra, files, start, kids in cases:
            n += 1
            got = one(cmd, extra, files, start, kids)
            if got[0] != want or (cls is not None and got[1] != cls):
                bad.append(f"stray_verdict {cmd[:70]!r} -> {got}, want stray={want} class={cls}")
        n += 1  # the loop's own probe, started after the table was read: its parent is read live
        t0 = {10: (1, ["/usr/bin/claude"]), 40: (10, _tool_argv(f"until grep -q DONE {census}; do sleep 5; done"))}
        f0 = {census: (now - 3 * 3600, {77}, "x\n")}
        if not stray_verdict(t0, 40, ProcFacts(t0, start={}, uids={}, self_pids=set(), now=now, files=f0,
                                               ppids={77: 40}), 15)[0]:
            bad.append("a probe newer than the table must not read as a writer")
        if stray_verdict(t0, 40, ProcFacts(t0, start={}, uids={}, self_pids=set(), now=now, files=f0,
                                           ppids={77: 99}), 15)[0]:
            bad.append("a holder under another parent must still read as a writer")
        n += 1  # a caller's low --idle-min does not lower the proof
        t0 = {10: (1, ["/usr/bin/claude"]), 40: (10, _tool_argv(f"until grep -q DONE {census}; do sleep 5; done"))}
        for floor in (0, 5):
            got = stray_verdict(t0, 40, ProcFacts(t0, start={}, uids={}, self_pids=set(), now=now,
                                                  files={census: (now - 300, set(), "x\n")}), floor)
            if got[0]:
                bad.append(f"--idle-min {floor} lowered the frozen-file proof: {got}")
        n += 1  # the harness layout, but its tasks dir is gone too: the path is not known to be the harness's
        if one(twait, dirs=set())[0]:
            bad.append("a task file whose tasks dir is gone must not read as stray")
        if tool_command(_tool_argv("echo 'a b' \"$X\"")) != "echo 'a b' \"$X\"":
            bad.append(f"tool_command round trip: {tool_command(_tool_argv(chr(39)))!r}")
        for pat, ere, line, want in [(r"test rc=\|clippy rc=[1-9]", False, "clippy rc=2", True),
                                     (r"error(\[|:)", True, "error[E0425]", True), ("a+", False, "aa", False),
                                     ("a+", False, "a+", True), ("[[:alpha:]]", True, "x", None)]:
            n += 1
            r = posix_re(pat, ere)
            if (None if r is None else bool(re.search(r, line))) != want:
                bad.append(f"posix_re({pat!r}, ere={ere}) on {line!r} -> {r!r}")
        # (1)/(3) the verdict a row reaches. Linux only: on Windows every shell is work by design.
        if not WINDOWS:
            n += 2
            saved = {k: globals()[k] for k in ("raw_tail", "transcript_facts", "live_session_ids",
                                               "git_rev_parse_dirs")}
            now_ms = time.time() * 1000
            tx = {"path": "x", "last_activity": now_ms - 31 * 3600e3, "last_prompt": now_ms - 32 * 3600e3,
                  "last_prompt_text": "push it", "last_asst": now_ms - 31 * 3600e3, "final_text_at": None,
                  "final_text": "Pushed the branch.\n\n**POLICY_COMPLIANCE**\n- **Clauses applied:** finish-to-zero.",
                  "pending_tool": False, "task_prompts": 1, "work_tools_since_nudge": 0, "nudge_streak": 0,
                  "cwd": LINKED_CWD}
            globals().update(raw_tail=lambda tid, chars=30000: empty_prompt, transcript_facts=lambda row: dict(tx),
                             live_session_ids=lambda claudes: {"s"}, git_rev_parse_dirs=fake_git_dirs)
            term = {"id": "t1", "isAlive": True, "pid": 20}
            marked = {"finished": True, "finishedAt": now_ms - 31.1 * 3600e3, "claudeSessionId": "s"}
            plain = {"finished": False, "claudeSessionId": "s"}
            coord = {"s": {"sessionStatus": "finished"}}
            try:
                def verdict(table, row, ready, tasks=None, start=None):
                    _FACTS[:] = [table, ProcFacts(table, start=start or {}, uids={}, files={}, self_pids={30}, now=now,
                                                  tasks=tasks)]
                    return classify(term, row, table, 15, "", ready)
                # A runner mark closes only beside coord's finished status (finished-criteria condition 1).
                for row, ready, want in [(marked, coord, "FINISHED"), (marked, {}, "UNKNOWN"),
                                         (plain, coord, "FOOTER-ONLY"), (plain, {}, "FOOTER-ONLY")]:
                    zero, held_by = verdict(base, row, ready), verdict(d2, row, ready)
                    if zero[0] != want or held_by[0] != want or held_by[1] != zero[1] + ["8 stray wait loop(s)"]:
                        bad.append(f"stray-held row (finished={row['finished']}, ready={ready}) -> {held_by}, "
                                   f"zero shells -> {zero}, want {want}")
                got = verdict(mix, marked, {})
                shell_reason = "1 tool shell(s) running under claude (Bash/Monitor/background task)"
                if got[0] != "BUSY" or got[1][:2] != ["marked finished, but", shell_reason] or \
                        got[1][-1] != "8 stray wait loop(s)":
                    bad.append(f"stray + cargo test on a finished row -> {got}")
                got = verdict(live, marked, {})
                if got[0] != "BUSY" or "waits on live pid 800 (self-matching loop: will never exit on its own)" \
                        not in got[1]:
                    bad.append(f"self-matching loop with a live target -> {got}")
                # The runner's `record_shell` is Claude Code's record of these same shells.
                shell_veto = {"s": {"sessionStatus": "finished",
                                    "windDown": {"eligibility": "ineligible", "reason": "record_shell"}}}
                veto_text = WIND_DOWN_VETO["record_shell"]
                got = verdict(d2, marked, shell_veto)
                if got[0] != "FINISHED" or veto_text in got[1] or got[1][-1] != "8 stray wait loop(s)":
                    bad.append(f"record_shell with only strays under claude must not hold the row: {got}")
                for label, table in (("work beside the strays", mix), ("no shell seen", base)):
                    got = verdict(table, marked, shell_veto)
                    if got[0] != "BUSY" or veto_text not in got[1]:
                        bad.append(f"record_shell with {label} must still veto: {got}")
                # A job that exec'd away the snapshot argv is a shell to the record, not to proc_facts.
                tick = os.sysconf("SC_CLK_TCK")
                boot = {21033: 1000, 900: 1000 + 5 * tick, 901: 1000 + 5 * tick}  # all inside the boot window
                job = dict(d2)
                job[900] = (21033, ["python3", "long_job.py"])
                got = verdict(job, marked, shell_veto, tasks={900: "bjob"}, start=boot)
                if got[0] != "BUSY" or veto_text not in got[1]:
                    bad.append(f"record_shell with an exec'd job beside the strays must still veto: {got}")
                got = verdict(job, marked, shell_veto, start=boot)  # the same process, no task file: an MCP server
                if got[0] != "FINISHED":
                    bad.append(f"a boot-time child beside the strays must not hold record_shell: {got}")
                # A shell whose stdout is not a task file: only its start time gives it away.
                for label, start in (("started late", {21033: 1000, 900: 1000 + (SESSION_START_CHILD_S + 1) * tick}),
                                     ("with an unreadable start", {21033: 1000})):
                    got = verdict(job, marked, shell_veto, start=start)
                    if got[0] != "BUSY" or veto_text not in got[1]:
                        bad.append(f"record_shell with a child {label} beside the strays must still veto: {got}")
                for argv in (["bash", "-l"], ["claude", "-p", "go"]):  # exec'd into what proc_facts files elsewhere
                    held = dict(d2)
                    held[901] = (21033, argv)
                    got = verdict(held, marked, shell_veto, tasks={901: "bjob"}, start=boot)
                    if got[0] != "BUSY" or veto_text not in got[1]:
                        bad.append(f"record_shell with an exec'd {argv[0]} beside the strays must still veto: {got}")
                def link(target=None, exc=None):
                    def readlink(path):
                        if exc:
                            raise exc
                        return target
                    return readlink
                for rl, want in [(link("/s/uuid/tasks/b1.output"), True), (link("/s/uuid/tasks/b1.output (deleted)"), True),
                                 (link("pipe:[123]"), False), (link("/dev/pts/3"), False),
                                 (link(exc=PermissionError()), True), (link(exc=FileNotFoundError()), False)]:
                    if fd1_writes_task(1, readlink=rl) is not want:
                        bad.append(f"fd1_writes_task want {want}")
                # record_shell can hide transcript_age_unknown: no readable message time keeps the veto.
                tx["last_activity"] = None
                got = verdict(d2, marked, shell_veto)
                tx["last_activity"] = now_ms - 31 * 3600e3
                if got[0] != "BUSY" or veto_text not in got[1]:
                    bad.append(f"record_shell with no readable message time must still veto: {got}")
                got = verdict(d2, marked, shell_veto, tasks={445647: "bloop"})  # a loop's own sleep inherits fd 1
                if got[0] != "FINISHED":
                    bad.append(f"a stray's own sleep must not read as an unseen task: {got}")
                got = verdict(d2, marked, {"s": {"windDown": {"reason": "record_busy"}}})
                if got[0] != "BUSY" or WIND_DOWN_VETO["record_busy"] not in got[1]:
                    bad.append(f"only record_shell gives way to strays, not record_busy: {got}")
                # The report names each stray: what a --close of that row would end.
                saved_screen = {k: globals()[k] for k in ("terminals", "open_rows", "page_names", "proc_table",
                                                          "readiness", "output_moving")}
                globals().update(terminals=lambda: [term], open_rows=lambda: {"t1": marked}, page_names=lambda: {},
                                 proc_table=lambda: d2, readiness=lambda: ({}, None),
                                 output_moving=lambda tid: False)
                try:
                    _FACTS[:] = [d2, ProcFacts(d2, start={}, uids={}, files={}, self_pids={30}, now=now)]
                    res = screen_all(15, [])[0]
                finally:
                    globals().update(saved_screen)
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    print_report(res, [])
                if sorted(s["pid"] for s in res[0]["strays"]) != sorted(loops) or \
                        {s["class"] for s in res[0]["strays"]} != {"a"} or "stray pid 445646 (a): self-matching" \
                        not in out.getvalue():
                    bad.append(f"screen_all / print_report do not name the strays: {out.getvalue()[:200]!r}")
            finally:
                globals().update(saved)
        # --own-tasks: the direct tool-shell children of this session's claude, minus the caller's own ancestry.
        n += 1
        ot = {1: (0, ["/sbin/init"]), 10: (1, ["/usr/bin/claude"]), 11: (10, _tool_argv("python3 cfs.py --own-tasks")),
              12: (11, ["python3", "cfs.py", "--own-tasks"]),
              13: (10, _tool_argv(f'until ! pgrep -f "{gp}" >/dev/null; do sleep 15; done')), 14: (13, ["sleep", "15"]),
              15: (10, _tool_argv("cargo test")), 16: (15, ["cargo", "test"]), 17: (10, ["python3", "mcp-shim.py"]),
              18: (1, _tool_argv(f'until ! pgrep -f "{cg}" >/dev/null; do sleep 15; done'))}
        fk = ProcFacts(ot, start={}, uids={}, files={}, self_pids={1, 10, 11, 12}, now=now)
        claude, rows = own_tasks(ot, 12, 15, facts=fk, task_of={13: "bstray", 15: "blive"}.get)
        got = [(r["pid"], r["task_id"], r["stray"]) for r in rows]
        if claude != 10 or got != [(13, "bstray", True), (15, "blive", False)]:
            bad.append(f"own_tasks: claude={claude} rows={got}")
        del ot[15], ot[16]
        if [r["stray"] for r in own_tasks(ot, 12, 15, facts=fk, task_of=lambda p: None)[1]] != [True]:
            bad.append("own_tasks without the cargo task must list only the stray")
        if own_tasks({1: (0, ["init"]), 5: (1, ["bash"]), 6: (5, ["python3"])}, 6, 15, facts=fk) != (None, []):
            bad.append("own_tasks with no claude above the caller must be UNKNOWN (None)")
    finally:
        _FACTS[:] = [None, None]
    return n


# ---------------------------------------------------------------- runner reads
def read(path, key, timeout=60):
    """The value at dotted `key` of GET <runner><path>. Read through the envelope
    helper, so an absent key raises EnvelopeUnknown instead of reading as empty."""
    with urllib.request.urlopen(API + path, timeout=timeout) as r:
        body = r.read()
    value, _ = load(body, door=f"GET {path}", source=API + path).require_key(key)
    return value


def terminals():
    return read("/terminals", "data.terminals")


def newest_open_rows(rows):
    """{terminalId: row} keeping, per terminal, the open row opened LAST.

    A /clear or a resume under another account opens a new row on the same
    terminal while the old one stays open, so a terminal can carry two. The
    last one written to the dict used to win, which was whatever order the
    route served; the newest `openedAt` is the session the pane holds now.
    live_session_ids still cross-checks it against /proc."""
    out = {}
    for r in rows:
        tid = r.get("terminalId")
        if r.get("state") != "open" or not tid:
            continue
        key = (r.get("openedAt") or 0, r.get("lastSeenAt") or 0)
        cur = out.get(tid)
        if cur is None or key > (cur.get("openedAt") or 0, cur.get("lastSeenAt") or 0):
            out[tid] = r
    return out


def open_rows():
    """{terminalId: registry row} for the open sessions in the runner registry."""
    return newest_open_rows(read("/sessions/history?limit=5000", "sessions"))


def readiness():
    """({claudeSessionId: process row}, None), or ({}, why) when it cannot be read.

    Each row carries coord's `sessionStatus` (with its time, `sessionStatusSince`,
    where the runner serves one: status_since) and the runner's wind-down verdict,
    whose `reason` can veto a close and never approve one (wind_down_veto)."""
    try:
        procs = read("/restart-readiness", "terminal_sessions.processes")
    except READ_ERRORS as e:
        return {}, str(e)
    if not isinstance(procs, list):
        return {}, f"terminal_sessions.processes is {type(procs).__name__}, not a list"
    return {p["sessionId"]: p for p in procs if isinstance(p, dict) and p.get("sessionId")}, None


# The operator's restart prepare (plan
# 2026-10-21-operator-prepare-for-restart-and-progressive-restore), the
# `restart_prep` block of GET /restart-readiness. While one is `preparing` or
# `ready`, the runner has told each live session to save, end its turn and wait,
# so a paused session sits idle at an empty prompt with no finished signal:
# exactly what the overlord screens call STOPPED. Typing into it ends the pause
# and starts work the restart is about to cut off. A build that predates the
# prepare serves no field, and `null` is no readable record: neither can be in
# a prepare this runner knows about. `restoring` is after the restart.
PREPARE_HOLDS = ("preparing", "ready")
PREPARE_CLEAR = ("restoring", "done", "cancelled")


def prepare_verdict(body):
    """("clear" | "held" | "unknown", text) for a parsed /restart-readiness body.
    Pure. A block this cannot read is unknown, never clear: the callers are about
    to type into a live session."""
    if not isinstance(body, dict):
        return "unknown", f"restart prepare UNKNOWN: the readiness body is {type(body).__name__}, not an object"
    block = body.get("restart_prep")
    if block is None:
        return "clear", ""
    if not isinstance(block, dict):
        return "unknown", f"restart prepare UNKNOWN: restart_prep is {type(block).__name__}, not an object"
    state = block.get("state")
    if state in PREPARE_CLEAR:
        return "clear", ""
    if state in PREPARE_HOLDS:
        pid = str(block.get("prepare_id") or "<absent>")[:8]
        hold = block.get("hold") or "<absent - UNKNOWN>"
        return "held", (f"an operator restart prepare ({pid}, {state}, hold: {hold}) has not ended: its sessions "
                        "were told to pause, so one that reads STOPPED or LIMITED is waiting for the restart, "
                        "not stuck. An expired hold still holds this, until the prepare is cancelled or the runner "
                        "restarts on a new build")
    return "unknown", f"restart prepare UNKNOWN: restart_prep.state is {state!r}"


def prepare_status():
    """prepare_verdict over a fresh GET /restart-readiness. Unreadable is unknown."""
    try:
        with urllib.request.urlopen(API + "/restart-readiness", timeout=60) as r:
            return prepare_verdict(json.loads(r.read()))
    except (*READ_ERRORS, http.client.HTTPException, RecursionError) as e:  # IncompleteRead; absurdly nested JSON
        return "unknown", f"restart prepare UNKNOWN: GET /restart-readiness unreadable ({str(e)[:120]})"


# The runner's `windDown.reason` words. Runners on builds before and after its
# typed-signal change coexist, so both vocabularies are read: the older build's
# `grid_busy` (its own rendered grid, cursor included) and the newer build's
# reasons from Claude Code's session record, the transcript age and the input
# prompt, which replace it. Each VETO word stops a close (`record_shell` gives
# way only to proven strays: classify_row). The NO-VETO words name
# something this script establishes itself (the finished marker, the process
# table, the screen), so they leave the decision to its own checks, as they
# always have. Any other word - a reason from a build newer than this table -
# is a veto: an unread reason never approves a close.
WIND_DOWN_VETO = {
    "grid_busy": "runner's screen check: not at an empty idle prompt",
    "sideband_working": "the session reports itself working (sideband)",
    "record_busy": "Claude Code's session record says busy",
    "record_waiting": "Claude Code's session record says waiting on a human (a permission prompt or a question)",
    "record_shell": "Claude Code's session record says a background shell command is running",
    "record_unknown": "runner found no usable Claude Code session record (absent, unreadable or unrecognised)",
    "transcript_age_unknown": "runner could not read the transcript's last message time",
    "prompt_not_ready": "runner's prompt check: an unsent draft, a dialog, or no prompt in the input area",
}
WIND_DOWN_NO_VETO = {"not_finished", "live_children", "work_status_unknown", "sideband_unreadable",
                     "grid_unknown", "children_unknown"}


def wind_down_veto(wd):
    """The veto a `windDown` block carries, or None. `idleEvidence` (which grid
    clause read busy) is quoted beside it, never acted on by itself."""
    reason = (wd or {}).get("reason") if isinstance(wd, dict) else None
    if reason is not None and not isinstance(reason, str):
        return f"runner wind-down reason {reason!r} is not a string (fail closed)"
    if reason is None or reason in WIND_DOWN_NO_VETO:
        return None
    veto = WIND_DOWN_VETO.get(reason) or (
        f"runner wind-down reason {reason!r} is not one this script knows (fail closed)")
    evidence = wd.get("idleEvidence")
    return f"{veto} [idleEvidence {evidence}]" if evidence else veto

# One escape sequence: OSC / DCS / APC / PM / SOS strings (window titles,
# hyperlinks), CSI with any private prefix (`ESC[?25h`, `ESC[>0q`), charset
# designation, then any other two-byte escape.
ESC_SEQ = (r"\x1b[\]P_^X][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b\[[<=>?]?[0-9;:]*[ -/]*[@-~]"
           r"|\x1b[()*+][0-9A-Za-z]|\x1b[@-Z\\-_a-z0-9=>]")
ANSI_RE = re.compile(ESC_SEQ)


def raw_tail(tid, chars=30000):
    """The last `chars` of the pane's scrollback with its escape sequences, or None."""
    try:
        return read(f"/terminals/{tid}/buffer?decoded=true", "data.data", 30)[-chars:]
    except READ_ERRORS:
        return None


def bytes_produced(tid):
    try:
        for t in terminals():
            if t["id"] == tid:
                return t.get("totalBytesProduced")
    except READ_ERRORS:
        pass
    return None


# ---------------------------------------------------------------- the screen
SGR_RE = re.compile(r"\x1b\[([0-9;]*)m")
TOKEN_RE = re.compile(ESC_SEQ + r"|\x1b|.", re.S)
BOX = set("\xa0\u2500\u2502\u256d\u256e\u2570\u256f")


CSI_RE = re.compile(r"\x1b\[([<=>?]?[0-9;:]*)([ -/]*)([@-~])")
ABS_POS_RE = re.compile(r"\x1b\[[0-9;]*[Hf]|\x1b\[[0-9]*d")


def sgr_params(nums):
    """The parameters of one SGR sequence that are attributes in their own
    right. An extended colour (38, 48 or 58, then ;5;n or ;2;r;g;b) keeps its
    selector and drops its sub-parameters, which are numbers, not attributes:
    the 2 of a truecolor 38;2;r;g;b is not dim, and its 45 is no background.
    Claude Code draws its status line in truecolor, so reading that 2 as dim
    hid every draft typed after a frame as a dim suggestion."""
    k = 0
    while k < len(nums):
        yield nums[k]
        if nums[k] in (38, 48, 58):
            k += 2 if k + 1 < len(nums) and nums[k + 1] == 5 else 4
        k += 1


class Cursor:
    """Just enough of a terminal to answer "what does the input row hold and
    where is the cursor": cursor moves, carriage return / line feed, printable
    cells with their dim attribute, erase-in-line and erase-in-display.

    Given the pane width `cols`, it also auto-wraps the way a terminal does: a
    character written past the last column lands at the start of the next row.
    Windows' ConPTY relies on that. It writes a row that fills the width with
    no line break after it, so without the wrap every later row is off by one."""

    def __init__(self, row, col, cols=None):
        self.row, self.col, self.dim, self.cells, self.cols = row, col, False, {}, cols

    def feed(self, s):
        for m in TOKEN_RE.finditer(s):
            tok = m.group(0)
            csi = CSI_RE.fullmatch(tok)
            if csi:
                self._csi(csi.group(1), csi.group(3))
            elif tok == "\r":
                self.col = 1
            elif tok == "\n":
                self.row += 1
            elif tok == "\b":
                self.col = max(1, self.col - 1)
            elif tok.startswith("\x1b") or tok < " " or tok == "\x7f":
                continue
            else:
                w = 2 if self.cols and unicodedata.east_asian_width(tok[0]) in "WF" else 1
                if self.cols and self.col + w - 1 > self.cols:
                    self.row, self.col = self.row + 1, 1
                self.cells[(self.row, self.col)] = (tok, self.dim)
                self.col += w

    def _csi(self, params, final):
        if params[:1] in ("?", "<", "=", ">") or ":" in params:
            return  # private modes, mouse/keyboard queries, colon sub-params: no cursor effect
        nums = [int(p) if p.isdigit() else 0 for p in params.split(";")] if params else []
        n = (nums[0] if nums else 0) or 1
        if final in "Hf":
            self.row = (nums[0] if nums and nums[0] else 1)
            self.col = (nums[1] if len(nums) > 1 and nums[1] else 1)
        elif final == "A":
            self.row = max(1, self.row - n)
        elif final == "B":
            self.row += n
        elif final == "C":
            self.col = min(self.col + n, self.cols) if self.cols else self.col + n
        elif final == "D":
            self.col = max(1, self.col - n)
        elif final == "G":
            self.col = n
        elif final == "d":
            self.row = n
        elif final == "E":
            self.row, self.col = self.row + n, 1
        elif final == "F":
            self.row, self.col = max(1, self.row - n), 1
        elif final == "K":
            mode = nums[0] if nums else 0
            for (r, c) in list(self.cells):
                if r == self.row and (mode == 2 or (mode == 0 and c >= self.col) or (mode == 1 and c <= self.col)):
                    del self.cells[(r, c)]
        elif final == "J":
            mode = nums[0] if nums else 0
            for (r, c) in list(self.cells):
                if mode >= 2 or (mode == 0 and (r, c) >= (self.row, self.col)) or \
                        (mode == 1 and (r, c) <= (self.row, self.col)):
                    del self.cells[(r, c)]
        elif final == "m":
            for p in sgr_params(nums or [0]):
                if p in (0, 22):
                    self.dim = False
                elif p == 2:
                    self.dim = True


# Claude Code's prompt marker. The input row starts with it, and so does the
# sticky header that repeats the last sent prompt on a scrolled-up pane.
PROMPT_MARK = "\u276f"
PROMPT_MARK_RE = re.compile(PROMPT_MARK)
RULE_CHARS = "\u2500\u2501"  # the cells of the input box's two rule lines
# How many markers not drawn on a background the input-box search replays,
# newest first. Every frame redraws the box's marker, so it is among the newest
# few; past them the box is not on screen, and each older marker costs a replay
# of the rest of the window, which a dialog redrawn many times makes slow.
BOX_SEARCH_MARKS = 8


def on_background(raw, i):
    """True when the character at `i` is drawn on a background colour. Claude
    Code pins the last sent prompt as a header at the top of a scrolled-up
    screen, `\u276f` and all, on a grey background; the input row has none.

    The colour is read from the last full SGR reset (`ESC[m` or `ESC[0m`)
    before `i`, or from the window's start when it holds none, because a
    terminal keeps a colour across cursor moves. Windows' ConPTY sends the
    header's grey BEFORE the `ESC[H` that places it, so a scan that started at
    the last cursor position missed it."""
    bg = False
    for m in SGR_RE.finditer(raw, max(raw.rfind("\x1b[m", 0, i), raw.rfind("\x1b[0m", 0, i), 0), i):
        nums = [int(p) if p.isdigit() else 0 for p in m.group(1).split(";")] if m.group(1) else [0]
        for p in sgr_params(nums):
            if p in (0, 49):
                bg = False
            elif 40 <= p <= 47 or 100 <= p <= 107 or p == 48:
                bg = True
    return bg


def wrap_cols(term, windows=None):
    """The width prompt_state should auto-wrap at: the pane's columns on Windows,
    whose ConPTY relies on the wrap, and None elsewhere, where the stream was
    measured without it."""
    return term.get("cols") if (WINDOWS if windows is None else windows) else None


def prompt_state(raw, *, cols):
    """('empty' | 'text' | 'unknown', detail) for the input prompt as the screen
    stands at the final cursor-show in `raw`.

    `cols` is required, by keyword: the width the replay auto-wraps at, which
    every caller takes from wrap_cols(term) (the pane's columns on Windows,
    None elsewhere). ConPTY writes a row that fills the width with no line
    break after it, so a Windows pane replayed without its width reads every
    idle prompt as a draft; an omitted width is a TypeError, not that misread.

    Claude Code redraws DIFFERENTIALLY: typed characters are written by jumping
    the cursor onto the input row, with no new `\u276f`. So a `\u276f` is placed by
    replaying the stream from the last absolute cursor position before it, and
    the replay goes on to the final cursor-show.

    The `\u276f` read is the input box's, not the stream's last one. A pane
    scrolled up ("Jump to bottom (ctrl+End)") repeats the last sent prompt as
    a sticky header on row 1 and redraws it after the status line, so the
    stream's last `\u276f` is that header. The input box's is the newest `\u276f`
    not drawn on a background that, on its replayed screen, sits directly under
    a rule row, has the box's lower rule within 11 rows below it, and holds the
    visible cursor between the two rules; only the newest BOX_SEARCH_MARKS
    markers not drawn on a background are tried. When none qualifies (a dialog,
    a bare screen, a box drawn before the window), the last `\u276f` not drawn on
    a background is read instead, as before the box was looked for. Either way a
    `\u276f` with another `\u276f` left of it on its row is text typed into that row,
    not the row's own marker: the leftmost is read.

    The prompt is empty only when BOTH hold on that `\u276f`:
    1. the visible cursor sits on its row, two columns right of it (an empty
       prompt; a draft leaves it after the text, a multi-line draft on a lower
       row);
    2. no plain (non-dim) character stands right of it in the input rows, from
       its row down to the box's lower rule, once the replay is done: a draft
       (even with the cursor sent back by Home), or a dialog's selection line.
       A prompt SUGGESTION or placeholder is drawn dim (SGR 2) and does not
       count."""
    show = raw.rfind("\x1b[?25h")
    last = raw.rfind(PROMPT_MARK)
    while last >= 0 and on_background(raw, last):
        last = raw.rfind(PROMPT_MARK, 0, last)  # a sticky header repeating the last prompt
    if last < 0 or show < last or raw.rfind("\x1b[?25l") > show:
        return "unknown", "input prompt or visible cursor not found"
    anchors = [m.start() for m in ABS_POS_RE.finditer(raw, 0, show)]

    def marks_before(end):
        """The markers before offset `end` not drawn on a background, newest first."""
        return (i for i in reversed([m.start() for m in PROMPT_MARK_RE.finditer(raw, 0, end)])
                if not on_background(raw, i))
    # Every marker after `last` is drawn on a background, so the search starts there.
    for i in itertools.islice(marks_before(last + 1), BOX_SEARCH_MARKS):
        placed = _place_mark(raw, i, cols, anchors)
        # Row 1 has no row above it for a rule: the sticky header's place.
        if placed is None or placed[1] < 2:
            continue
        cur, mrow, mcol = placed
        cur.feed(raw[i:show])
        if _mark_left_of(cur, mrow, mcol):
            continue  # typed into the input row, right of the row's own marker
        rules = _rule_rows(cur)
        low = _lower_rule(rules, mrow)
        if mrow - 1 in rules and low is not None and mrow <= cur.row < low:
            return _read_input(cur, mrow, mcol, range(mrow, low))
    placed = _place_mark(raw, last, cols, anchors)
    if placed is None:
        return "unknown", "no absolute cursor position before the prompt"
    # A marker typed into the input row is drawn right of the row's own: an older
    # marker on the same row, further left. The leftmost one is read.
    for steps, i in enumerate(marks_before(last)):
        older = _place_mark(raw, i, cols, anchors)
        if older is None or older[1] != placed[1] or older[2] >= placed[2]:
            break
        if steps == BOX_SEARCH_MARKS:
            return "text", "prompt markers typed into the input row"
        last, placed = i, older
    cur, mrow, mcol = placed
    cur.feed(raw[last:show])
    low = _lower_rule(_rule_rows(cur), mrow)
    # With no rule below, the rows under the marker are status lines, not input.
    return _read_input(cur, mrow, mcol, range(mrow, low) if low is not None else (mrow,))


def _place_mark(raw, i, cols, anchors):
    """(Cursor, row, col) of the `\u276f` at `i`, replayed from the last absolute
    cursor position before it (`anchors`: their offsets, ascending), or None
    when there is none. A row filled to the width leaves the terminal's wrap
    pending, so on a width-aware replay a marker past the last column opens
    the next row."""
    k = bisect.bisect_left(anchors, i) - 1
    if k < 0:
        return None
    cur = Cursor(1, 1, cols)
    cur.feed(raw[anchors[k]:i])
    if cols and cur.col > cols:
        return cur, cur.row + 1, 1
    return cur, cur.row, cur.col


def _mark_left_of(cur, mrow, mcol):
    """True when a `\u276f` stands left of (mrow, mcol) on a replayed screen: a
    `\u276f` typed into the input row is drawn right of the row's own."""
    return any(ch == PROMPT_MARK for (r, c), (ch, _dim) in cur.cells.items() if r == mrow and c < mcol)


def _rule_rows(cur):
    """The rows of a replayed screen that hold a rule cell."""
    return {r for (r, _c), (ch, _dim) in cur.cells.items() if ch in RULE_CHARS}


def _lower_rule(rules, mrow):
    """The input box's lower rule: the first rule row within 11 rows below the
    marker's row, or None."""
    return next((r for r in range(mrow + 1, mrow + 12) if r in rules), None)


def _read_input(cur, mrow, mcol, rows):
    """prompt_state's two tests on the `\u276f` at (mrow, mcol), over the input
    `rows`, on the screen replayed to the final cursor-show."""
    want = (mrow, mcol + 2)
    if (cur.row, cur.col) != want:
        return "text", (f"cursor at row {cur.row} col {cur.col}; an empty prompt leaves it at "
                        f"row {want[0]} col {want[1]}")
    typed = "".join(ch for (r, c), (ch, dim) in sorted(cur.cells.items())
                    if r in rows and (r > mrow or c > mcol) and not dim and ch > " " and ch not in BOX)
    return ("text", typed) if typed else ("empty", "")


BUSY_SCREEN = re.compile(r"esc to interrupt", re.I)
# Claude Code ends a turn with "<star> <Verb>ed for 1m 2s - done 4:42 PM". The
# buffer is a scrollback STREAM, so an old spinner frame precedes every reply;
# only a spinner AFTER the last turn-end line means a turn is running now.
TURN_END = re.compile(r"[\u2700-\u27bf]\s*\w+\s*for\s*(?:\d+h\s*)?(?:\d+m\s*)?\d+s\b"
                      r"|\bdone\s*\d{1,2}:\d{2}\s*[AP]M\b", re.I)
# An idle compaction ("Compacted while idle, before the prompt cache expired")
# draws a spinner ("Compacting conversation... esc to interrupt") and then
# "Conversation compacted", with no turn-end line. It read as not running only
# by accident before (a later redrawn clock matched TURN_END, or the reprint
# pushed the spinner out of the read window); it is now a rule: that line ends
# a spinner too, so with no spinner after it nothing is running. A compaction
# inside a turn is followed by that turn's next spinner, which still counts.
# Plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 6 (N8).
COMPACT_END = re.compile(r"Conversation compacted", re.I)
# Status-line churn an idle Claude Code keeps redrawing (the auto-updater loops
# on some boxes), which moves totalBytesProduced without any turn running.
FOOTER_CHURN = re.compile(
    r"Auto-updat\w*|Update installed|Restart to apply|Auto-update failed|Try claude doct\w*"
    r"|new task\?\s*/clear to save[\s\d.k]*(?:tokens?)?|bypass permissions on \(shift\+tab to cycle\)"
    r"|(?:\u2190\s*)?for agents|(?:done )?\d{1,2}:\d{2}\s*[AP]M", re.I)


def churn_free(text):
    return re.sub(r"[\W_]+", "", FOOTER_CHURN.sub("", text))


def running_turn(text):
    spin = [m.start() for m in BUSY_SCREEN.finditer(text)]
    ends = [m.start() for rx in (TURN_END, COMPACT_END) for m in rx.finditer(text)]
    return bool(spin and (not ends or spin[-1] > max(ends)))


def output_moving(tid, seconds=3):
    """True when the pane printed more than status-line churn over `seconds`;
    None when that cannot be read."""
    b1, t1 = bytes_produced(tid), raw_tail(tid, 6000)
    time.sleep(seconds)
    b2, t2 = bytes_produced(tid), raw_tail(tid, 6000)
    if None in (b1, b2, t1, t2):
        return None
    if b1 == b2:
        return False
    c1, c2 = churn_free(ANSI_RE.sub("", t1)), churn_free(ANSI_RE.sub("", t2))
    return not c2.endswith(c1[-120:])


# ---------------------------------------------------------------- tab names
def page_names():
    """{pageId: tab name} from the runner webview's localStorage, for the runner
    API names. The runner keeps a non-9876 instance's pages under
    `<key>:<port>` (qontinui-runner src/lib/instance-storage.ts), so the key
    follows the port and never falls back to another runner's list.

    Only WebKitGTK (Linux) stores it as SQLite; elsewhere this returns {} and the
    report shows the page id instead."""
    port = API.rsplit(":", 1)[-1]
    key = "qontinui-terminal-pages" + ("" if port == "9876" else ":" + port)
    out = {}
    for f in glob.glob(os.path.expanduser("~/.local/share/com.qontinui.runner*/localstorage/*.localstorage")):
        try:
            c = sqlite3.connect(f"file:{f}?mode=ro", uri=True)
            row = c.execute("select value from ItemTable where key=?", (key,)).fetchone()
            c.close()
            if not row:
                continue
            v = row[0].decode("utf-16-le") if isinstance(row[0], bytes) else row[0]
            for p in json.loads(v):
                if isinstance(p, dict) and p.get("id"):
                    out.setdefault(p["id"], p.get("name") or p["id"][:8])
        except (sqlite3.Error, ValueError, UnicodeDecodeError, TypeError):
            continue
    return out


# ---------------------------------------------------------------- processes
WINDOWS = os.name == "nt"


def proc_table():
    """{pid: (ppid, argv)} from /proc on Linux or CIM on Windows, or None where
    neither can be read."""
    if WINDOWS:
        return win_proc_table()
    if not os.path.isdir("/proc/self"):
        return None
    t = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            stat = open(f"/proc/{d}/stat").read()
            ppid = int(stat.rsplit(")", 1)[1].split()[1])
            argv = open(f"/proc/{d}/cmdline", "rb").read().decode(errors="replace").split("\0")
        except (OSError, ValueError, IndexError):
            continue
        t[int(d)] = (ppid, [a for a in argv if a])
    return t


WIN_PS = ("[Console]::OutputEncoding = [Text.Encoding]::UTF8; "
          "@(Get-CimInstance Win32_Process | ForEach-Object { [pscustomobject]@{ "
          "p = [int]$_.ProcessId; pp = [int]$_.ParentProcessId; "
          "t = $(if ($_.CreationDate) { $_.CreationDate.ToFileTimeUtc() } else { 0 }); "
          "n = $_.Name; c = $_.CommandLine } }) | ConvertTo-Json -Compress")


def win_proc_table():
    """{pid: (ppid, argv)} from CIM's Win32_Process, or None when no PowerShell
    answers."""
    rows = None
    for exe in ("powershell.exe", "pwsh.exe"):
        try:
            out = subprocess.run([exe, "-NoProfile", "-NonInteractive", "-Command", WIN_PS],
                                 stdin=subprocess.DEVNULL, capture_output=True, timeout=120).stdout
            rows = json.loads(out.decode("utf-8-sig"))
            break
        except (OSError, subprocess.TimeoutExpired, subprocess.SubprocessError, ValueError):
            continue  # a hung or absent PowerShell is no table (BUSY), never a crash
    if isinstance(rows, dict):
        rows = [rows]
    if not isinstance(rows, list):
        return None
    return win_table(rows)


def win_table(rows):
    """{pid: (ppid, argv)} from CIM rows {p, pp, t (creation FILETIME), n, c}.
    Windows never re-parents an orphan, so a ppid can name a parent that died and
    whose pid a later process reused. A link to a parent created clearly after its
    child is such a stale link, and it is cut, as is a process that is its own
    parent. A cut can only hide a process, so it needs both creation times and a
    margin: an unknown time or clock jitter keeps the link.

    CIM shows no command line for some processes (elevated or system ones). Such a
    row keeps its image name plus UNREADABLE_CMDLINE, which no shell rule reads as
    bare."""
    rows = [r for r in rows if isinstance(r, dict) and isinstance(r.get("p"), int)]
    born = {r["p"]: r.get("t") or 0 for r in rows}
    t = {}
    for r in rows:
        pid, ppid = r["p"], r.get("pp") or 0
        if ppid == pid or (born[pid] and born.get(ppid) and born[ppid] > born[pid] + STALE_LINK_MARGIN):
            ppid = -1
        argv = win_argv(r.get("c")) or ([r["n"], UNREADABLE_CMDLINE] if r.get("n") else [])
        if argv and not argv[0] and r.get("n"):
            argv[0] = r["n"]  # a command line opening with whitespace has an empty argv[0]; the image names it
        t[pid] = (ppid, argv)
    return t or None


STALE_LINK_MARGIN = 10_000_000  # 1 s in FILETIME units (100 ns)
UNREADABLE_CMDLINE = "<command line unreadable>"


def win_argv(cmdline):
    """Split a Windows command line the way CommandLineToArgvW does. argv[0] ends
    at whitespace or at its closing quote, and its backslashes are literal. After
    it, 2n backslashes before a quote are n backslashes and a quote toggle, 2n+1
    are n backslashes and a literal quote, and "" inside quotes is a literal quote
    that also ends the quoted run (the shell32 rule; the MSVC runtime's keeps the
    run open). On Windows the selftest checks it against shell32 itself."""
    s = cmdline or ""
    n, out, i = len(s), [], 0
    if not s.strip():
        return out
    if s[i] == '"':  # argv[0] starts at offset 0: leading whitespace makes it empty, as in shell32
        j = s.find('"', i + 1)
        j = n if j < 0 else j
        out.append(s[i + 1:j])
        i = j + 1
    else:
        j = i
        while j < n and s[j] not in " \t":
            j += 1
        out.append(s[i:j])
        i = j
    while True:
        while i < n and s[i] in " \t":
            i += 1
        if i >= n:
            return out
        arg, quoted = [], False
        while i < n and (quoted or s[i] not in " \t"):
            if s[i] == "\\":
                j = i
                while j < n and s[j] == "\\":
                    j += 1
                if j < n and s[j] == '"':
                    arg.append("\\" * ((j - i) // 2))
                    if (j - i) % 2:
                        arg.append('"')
                        j += 1
                else:
                    arg.append(s[i:j])
                i = j
            elif s[i] == '"':
                if quoted and i + 1 < n and s[i + 1] == '"':
                    arg.append('"')
                    quoted = False
                    i += 2
                else:
                    quoted = not quoted
                    i += 1
            else:
                arg.append(s[i])
                i += 1
        out.append("".join(arg))


def shell32_argv(cmdline):
    """shell32!CommandLineToArgvW's own split, for the selftest to hold win_argv to."""
    import ctypes
    from ctypes import wintypes
    f = ctypes.windll.shell32.CommandLineToArgvW
    f.restype = ctypes.POINTER(wintypes.LPWSTR)
    f.argtypes = (wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_int))
    count = ctypes.c_int()
    p = f(cmdline, ctypes.byref(count))
    if not p:
        return None
    try:
        return [p[k] for k in range(count.value)]
    finally:
        ctypes.windll.kernel32.LocalFree(ctypes.cast(p, ctypes.c_void_p))


def subtree(table, root):
    kids = {}
    for pid, (ppid, _) in table.items():
        kids.setdefault(ppid, []).append(pid)
    out, stack, seen = [], [root], set()
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.add(p)
        if p in table:
            out.append(p)
        stack.extend(kids.get(p, []))
    return out


def exe_base(path):
    """The image name of an argv[0], on either separator, lowercased, `.exe` dropped."""
    b = re.split(r"[\\/]", path or "")[-1].lower()
    return b[:-4] if b.endswith(".exe") else b


def is_claude(argv):
    base = exe_base(argv[0]) if argv else ""
    return base == "claude" or ("node" in base and any("claude-code" in a for a in argv))


# The runner's Windows pane shell is `powershell.exe -NoLogo -NoExit -Command
# ". '<temp>\qontinui-shell-integration.ps1'"`.
PS_INTEGRATION_RE = re.compile(r"^\.\s+'[^']*shell-integration[^']*\.ps1'$", re.I)
PS_RUNS = ("-command", "-c", "-file", "-f", "-encodedcommand", "-ec", "-e")


def is_bare_shell(argv, windows=WINDOWS):
    """An interactive shell with flags only: `bash --rcfile X`, `-bash`, `zsh -l`;
    on Windows also `cmd.exe` and a PowerShell whose only command dot-sources the
    runner's shell-integration script. `bash long-job.sh` is NOT bare - it is
    running something."""
    base = exe_base(argv[0]).lstrip("-") if argv else ""
    rest = argv[1:]
    if not windows and base in ("powershell", "pwsh", "cmd"):
        return False
    if base in ("powershell", "pwsh"):
        i = 0
        while i < len(rest):
            a = rest[i].lower()
            if a in ("-command", "-c") and i + 1 < len(rest) and PS_INTEGRATION_RE.match(rest[i + 1].strip()):
                i += 2
            elif a.startswith("-") and a not in PS_RUNS:
                i += 1
            else:
                return False
        return True
    if base == "cmd":
        return all(a.startswith("/") and a.lower()[:2] not in ("/c", "/k", "/r") for a in rest)
    if base not in ("bash", "sh", "zsh", "fish", "dash"):
        return False
    i = 0
    while i < len(rest):
        if rest[i] in ("--rcfile", "--init-file"):
            i += 2
        elif rest[i].startswith("-") and rest[i] != "-c":
            i += 1
        else:
            return False
    return True


WIN_TOOL_SHELLS = ("bash", "sh", "powershell", "pwsh")
SNAPSHOT = "shell-snapshots/snapshot-"  # what a Linux tool shell sources
SESSION_START_CHILD_S = 60  # the runner's SESSION_START_CHILD_WINDOW_S: MCP servers start inside it
IDLE_MIN = 15  # --idle-min's default, and the floor of the frozen-file proof whatever a caller passes
TASK_OUTPUT_RE = re.compile(r"/tasks/([^/]+)\.output(?: \(deleted\))?$")  # where a tool shell's fd 1 points
# A harness task's output file: .../claude-<uid>/<project dir>/<session uuid>/tasks/<id>.output,
# the project dir being the session's cwd with every "/" turned into "-" (measured on every
# live task file on one Linux box, 2026-10-24). Only this layout is the harness's.
HARNESS_TASK_FILE_RE = re.compile(r"/claude-\d+/-[^/]*/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
                                  r"/tasks/[^/]+\.output$")


def proc_facts(table, pid, windows=WINDOWS):
    """(claude_pids, tool_shells, other_pids) under the pane pid. A tool shell is
    the shell Claude Code runs a Bash, PowerShell or Monitor call in, foreground or
    background, so its presence means work is still in flight. On Linux it sources
    a `shell-snapshots/snapshot-` file. On Windows its command line names no such
    file, so there it is any bash or PowerShell that is a direct child of claude.
    MCP stdio servers run under `cmd.exe /c`, so they do not count."""
    pids = subtree(table, pid)
    claudes = [p for p in pids if is_claude(table[p][1])]
    shells = [p for p in pids if any(SNAPSHOT in a for a in table[p][1])
              or (windows and table[p][0] in claudes and table[p][1]
                  and exe_base(table[p][1][0]) in WIN_TOOL_SHELLS)]
    other = [p for p in pids if p not in claudes and p not in shells and not is_bare_shell(table[p][1], windows)
             and not (windows and table[p][1] and exe_base(table[p][1][0]) == "conhost")]
    return claudes, shells, other


def pid_alive(pid):
    """Whether `pid` still names a live process. On Windows a reused pid reads as
    alive, which can only make a close fail to verify."""
    if not WINDOWS:
        return os.path.exists(f"/proc/{pid}")
    import ctypes
    from ctypes import wintypes
    k = ctypes.WinDLL("kernel32", use_last_error=True)
    k.OpenProcess.restype = wintypes.HANDLE
    k.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    k.GetExitCodeProcess.argtypes = (wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD))
    k.CloseHandle.argtypes = (wintypes.HANDLE,)
    h = k.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return ctypes.get_last_error() == 5  # ERROR_ACCESS_DENIED: it exists
    try:
        code = wintypes.DWORD()
        return not k.GetExitCodeProcess(h, ctypes.byref(code)) or code.value == 259  # STILL_ACTIVE
    finally:
        k.CloseHandle(h)


def config_dir_of(table, pid):
    """CLAUDE_CONFIG_DIR of the pane's top claude (the first under pane `pid`),
    read from its environment - the directory it really writes its transcript
    to. "" when it runs the default (~/.claude), None when that claude's
    environment is unreadable (an exiting process, a permission refusal, or
    Windows, which has no /proc/<pid>/environ). A nested claude (a `claude -p`
    from a tool call) is never consulted: it may run under another dir."""
    claudes = proc_facts(table, pid)[0] if not WINDOWS and table and pid in table else []
    if not claudes:
        return None
    try:
        env = open(f"/proc/{claudes[0]}/environ", "rb").read().decode(errors="replace").split(chr(0))
    except OSError:
        return None
    if not any("=" in e for e in env):
        return None  # an exiting process shows an empty environment: unreadable, not "default"
    cd = next((e.split("=", 1)[1] for e in env if e.startswith("CLAUDE_CONFIG_DIR=")), "")
    return resolve_config_dir(cd, lambda: os.readlink(f"/proc/{claudes[0]}/cwd"))


def resolve_config_dir(cd, read_cwd):
    """A CLAUDE_CONFIG_DIR value as an absolute path. "" stays "" (the default
    dir). A relative value is joined to THAT process's cwd, which `read_cwd()`
    returns (a /proc/<pid>/cwd readlink) - not this script's cwd; None when that
    cwd is unreadable or deleted, since there is then no directory to resolve
    it against."""
    if not cd:
        return ""
    cd = os.path.expanduser(cd)
    if not os.path.isabs(cd):
        try:
            cwd = read_cwd()
        except OSError:
            return None
        if cwd.endswith(" (deleted)"):
            return None
        cd = os.path.join(cwd, cd)
    return os.path.normpath(cd)


def with_live_config_dir(row, table, pid, read=None):
    """`row` with configDir taken from the running claude when one is readable:
    the process's own config dir is where it writes, and the registry's can be
    blank. `read` stands in for config_dir_of in the selftest."""
    cd = (read or config_dir_of)(table, pid) if row else None
    return dict(row, configDir=cd or os.path.expanduser("~/.claude")) if cd is not None else row


def account_of(row):
    """The account label, or the config dir's suffix when the registry labels it
    "unknown" (`.claude-paktis` is `paktis`, `.claude` is `default`, any other
    dir its own name)."""
    label = ((row or {}).get("account") or {}).get("label")
    if label and label != "unknown":
        return label
    cd = ((row or {}).get("configDir") or "").rstrip("/" + chr(92))
    def canon(p):
        return os.path.normcase(os.path.realpath(os.path.expanduser(p)))
    if cd and canon(cd) == canon("~/.claude"):
        return "default"  # only THIS user's default dir; another home's .claude keeps its name
    base = os.path.basename(cd.replace(chr(92), "/"))
    return (base[len(".claude-"):] if base.startswith(".claude-") else base) or label or "?"


def live_session_ids(claudes):
    """The session ids the running claude processes hold their scratch
    `<session-id>/tasks` directory open for - the CURRENT id, which `/clear`
    changes. Empty when none is visible."""
    ids = set()
    for p in claudes:
        try:
            fds = os.listdir(f"/proc/{p}/fd")
        except OSError:
            continue
        for fd in fds:
            try:
                m = re.search(r"/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/tasks$",
                              os.readlink(f"/proc/{p}/fd/{fd}"))
            except OSError:
                continue
            if m:
                ids.add(m.group(1))
    return ids


# ---------------------------------------------------------------- stray wait loops
# A background wait loop whose condition can never change is not work: it would
# hold a finished session BUSY forever. The commonest shape is self-matching:
# `until ! pgrep -f X` reads the loop's OWN `bash -c` command line, which holds X,
# so it never ends (2026-10-23: 15 such loops on one box, 31-48 h old). The rest
# poll a file nothing writes any more, or a pid that is gone or was reused.
# A shell is STRAY only when every part of its loop condition is proven frozen
# and the condition still says "loop"; anything this cannot read is work.
SH_OPS = ("&&", "||", "&>", ";;", "|&", ">>", ">|", ">&", "<&", "<<", "<>", ";", "&", "|", "(", ")", "<", ">", "\n")
SH_REDIRS = (">", ">>", ">|", "&>", ">&", "<&", "<>", "<")
# A bound (an outer timeout, a counter or a deadline) turns "never" into "later": class (c), never stray.
BOUNDED_RE = re.compile(r"\btimeout\b|\bSECONDS\b|\bdate\b[^;&|\n]*\+%s|\bseq\b|\s-(?:lt|le|gt|ge)\b|\$\(\(|\(\(|"
                        r"\blet\b|\bbreak\b")
LOOP_BODY = ("sleep", ":", "true", "echo", "printf")  # a body that only waits
# What may run before the loop. Anything else might start the writer the loop
# waits on outside the shell's subtree (`setsid -f …`, a daemon) or change state.
PRE_LOOP = (":", "true", "cd", "echo", "printf", "export")
PROBES = ("pgrep", "grep", "egrep", "fgrep")
FILE_MAX = 16 << 20  # a polled file larger than this is not read: its wait is not judged


class ShError(Exception):
    """Shell syntax this reader does not model."""


class Word:
    """One shell word: ("lit", text) / ("var", name) / ("unk", raw) parts, and
    whether an unquoted part holds a glob character."""
    __slots__ = ("glob", "segs")

    def __init__(self):
        self.segs, self.glob = [], False

    def add(self, kind, text):
        if kind == "lit" and self.segs and self.segs[-1][0] == "lit":
            self.segs[-1] = ("lit", self.segs[-1][1] + text)
        else:
            self.segs.append((kind, text))

    def text(self, env=None):
        """The word's value, expanding $NAME from `env`; None when any part cannot be
        known, or when a value would word-split or glob (whitespace, `*?[`)."""
        if self.glob:
            return None
        out = []
        for kind, t in self.segs:
            if kind == "lit":
                out.append(t)
            elif kind == "var" and isinstance((env or {}).get(t), str) and not re.search(r"[\s*?\[]", env[t]):
                out.append(env[t])
            else:
                return None
        return "".join(out)


def _sh_skip(s, i, close):
    """The index just past the `close` that ends the $( / ${ / ` group opened before i."""
    depth, n = 1, len(s)
    while i < n:
        c = s[i]
        if c == "\\":
            i += 2
            continue
        if c == "'" and close != "`":
            j = s.find("'", i + 1)
            if j < 0:
                raise ShError("unterminated quote")
            i = j + 1
            continue
        if close == ")" and c == "(":
            depth += 1
        elif c == close:
            depth -= 1
            if not depth:
                return i + 1
        i += 1
    raise ShError("unterminated expansion")


def _sh_dollar(s, i, w):
    """Lex a $-expansion at s[i] into word w; return the index after it."""
    nxt = s[i + 1] if i + 1 < len(s) else ""
    if nxt == "'":
        raise ShError("ANSI-C quoting")
    if nxt == "(":
        j = _sh_skip(s, i + 2, ")")
        w.add("unk", s[i:j])
        return j
    if nxt == "{":
        j = _sh_skip(s, i + 2, "}")
        name = s[i + 2:j - 1]
        w.add("var" if re.fullmatch(r"[A-Za-z_]\w*", name) else "unk", name)
        return j
    m = re.match(r"[A-Za-z_]\w*", s[i + 1:])
    if m:
        w.add("var", m.group(0))
        return i + 1 + m.end()
    if nxt and nxt in "?$!#@*-0123456789":
        w.add("unk", s[i:i + 2])
        return i + 2
    w.add("lit", "$")
    return i + 1


def sh_lex(s):
    """Shell tokens of `s`: Word objects and operator strings. Raises ShError on
    syntax this reader does not model (a heredoc, ANSI-C quoting, an open quote)."""
    toks, w, i, n = [], None, 0, len(s)

    def end():
        nonlocal w
        if w is not None:
            toks.append(w)
            w = None

    while i < n:
        c = s[i]
        if c in " \t":
            end()
            i += 1
        elif c == "\\":
            if s[i + 1:i + 2] != "\n":
                w = w or Word()
                w.add("lit", s[i + 1:i + 2])
            i += 2
        elif c == "#" and w is None:
            j = s.find("\n", i)
            i = n if j < 0 else j
        elif c == "'":
            j = s.find("'", i + 1)
            if j < 0:
                raise ShError("unterminated quote")
            w = w or Word()
            w.add("lit", s[i + 1:j])
            i = j + 1
        elif c == '"':
            w, i = w or Word(), i + 1
            while True:
                if i >= n:
                    raise ShError("unterminated quote")
                d = s[i]
                if d == '"':
                    i += 1
                    break
                if d == "\\" and s[i + 1:i + 2] in ('$', '`', '"', "\\", "\n"):
                    w.add("lit", s[i + 1] if s[i + 1] != "\n" else "")
                    i += 2
                elif d == "$":
                    i = _sh_dollar(s, i, w)
                elif d == "`":
                    j = _sh_skip(s, i + 1, "`")
                    w.add("unk", s[i:j])
                    i = j
                else:
                    w.add("lit", d)
                    i += 1
        elif c == "$":
            w = w or Word()
            i = _sh_dollar(s, i, w)
        elif c == "`":
            j = _sh_skip(s, i + 1, "`")
            w = w or Word()
            w.add("unk", s[i:j])
            i = j
        else:
            op = next((o for o in SH_OPS if s.startswith(o, i)), None)
            if op is None:
                w = w or Word()
                if c in "*?" or (c == "[" and re.match(r"\[[^\s;&|()<>]*\]", s[i:])) or \
                        (c == "{" and re.match(r"\{[^\s;&|()<>{}]*(?:,|\.\.)[^\s;&|()<>{}]*\}", s[i:])):
                    w.glob = True  # a glob or a brace expansion: not one known word
                w.add("lit", c)
                i += 1
                continue
            if op == "<<":
                raise ShError("heredoc")
            if op in SH_REDIRS and w is not None and (w.text() or "").isdigit():
                w = None  # an io-number (`2>`) belongs to its redirect
            end()
            toks.append(op)
            i += len(op)
    end()
    return toks


class _Sh:
    """A recursive-descent reader of the shell grammar a wait loop is written in:
    lists, and-or chains, pipelines, simple commands, until/while/if/for loops,
    `{ }` and `( )` groups. Anything else raises ShError."""

    def __init__(self, toks):
        self.t, self.i = toks, 0

    def peek(self):
        return self.t[self.i] if self.i < len(self.t) else None

    def kw(self, names):
        tok = self.peek()
        return isinstance(tok, Word) and tok.text() in names

    def expect(self, name):
        if not self.kw((name,)):
            raise ShError(f"expected {name}")
        self.i += 1

    def parse_list(self, stop=()):
        items = []
        while True:
            while self.peek() in (";", "\n"):
                self.i += 1
            if self.peek() is None or self.peek() == ")" or self.kw(stop):
                return items
            ao = self.and_or()
            sep = self.peek()
            if sep in (";", "&", "\n"):
                self.i += 1
            elif not (sep is None or sep == ")" or self.kw(stop)):
                raise ShError(f"unexpected {sep!r}")
            items.append((ao, sep if sep in (";", "&", "\n") else None))

    def and_or(self):
        chain = [(None, self.pipeline())]
        while self.peek() in ("&&", "||"):
            op = self.peek()
            self.i += 1
            while self.peek() == "\n":
                self.i += 1
            chain.append((op, self.pipeline()))
        return chain

    def pipeline(self):
        neg = self.kw(("!",))
        self.i += neg
        cmds = [self.command()]
        while self.peek() in ("|", "|&"):
            self.i += 1
            while self.peek() == "\n":
                self.i += 1
            cmds.append(self.command())
        return neg, cmds

    def redirs(self):
        out = []
        while self.peek() in SH_REDIRS:
            op = self.peek()
            self.i += 1
            if not isinstance(self.peek(), Word):
                raise ShError("redirect without a target")
            out.append((op, self.peek()))
            self.i += 1
        return out

    def command(self):
        if self.kw(("until", "while")):
            kind = self.peek().text()
            self.i += 1
            cond = self.parse_list(("do",))
            self.expect("do")
            body = self.parse_list(("done",))
            self.expect("done")
            return {"kind": kind, "cond": cond, "body": body, "redirs": self.redirs()}
        if self.kw(("if",)):
            self.i += 1
            while True:
                self.parse_list(("then",))
                self.expect("then")
                self.parse_list(("elif", "else", "fi"))
                if self.kw(("elif",)):
                    self.i += 1
                    continue
                if self.kw(("else",)):
                    self.i += 1
                    self.parse_list(("fi",))
                self.expect("fi")
                return {"kind": "compound", "redirs": self.redirs()}
        if self.kw(("for", "select")):
            self.i += 2
            if self.kw(("in",)):
                while isinstance(self.peek(), Word):
                    self.i += 1
            while self.peek() in (";", "\n"):
                self.i += 1
            self.expect("do")
            self.parse_list(("done",))
            self.expect("done")
            return {"kind": "compound", "redirs": self.redirs()}
        if self.kw(("{",)):
            self.i += 1
            self.parse_list(("}",))
            self.expect("}")
            return {"kind": "compound", "redirs": self.redirs()}
        if self.peek() == "(":
            self.i += 1
            self.parse_list()
            if self.peek() != ")":
                raise ShError("unclosed subshell")
            self.i += 1
            return {"kind": "compound", "redirs": self.redirs()}
        if self.kw(("case", "function", "then", "do", "done", "fi", "elif", "else", "esac", "}")):
            raise ShError(f"unmodelled {self.peek().text()}")
        words, redirs = [], []
        while True:
            tok = self.peek()
            if isinstance(tok, Word):
                words.append(tok)
                self.i += 1
            elif tok in SH_REDIRS:
                redirs += self.redirs()
            else:
                break
        if not words and not redirs:
            raise ShError(f"empty command at {self.peek()!r}")
        return {"kind": "simple", "words": words, "redirs": redirs}


def parse_sh(s):
    """The parsed list of `s`, or None when it is not shell this reader models."""
    try:
        p = _Sh(sh_lex(s))
        items = p.parse_list()
        if p.peek() is not None:
            raise ShError("trailing tokens")
        return items
    except (ShError, IndexError):
        return None


def tool_command(argv):
    """The command a Linux tool shell runs: the word Claude Code hands `eval` in
    `bash -c "source <snapshot> ... && eval '<command>' < /dev/null && pwd -P >| <file>"`.
    None when argv is not that shape."""
    script = next((a for a in argv[1:] if SNAPSHOT in a), None)
    try:
        toks = sh_lex(script) if script else []
    except ShError:
        return None
    for k, tok in enumerate(toks[:-1]):
        if isinstance(tok, Word) and tok.text() == "eval" and isinstance(toks[k + 1], Word):
            return toks[k + 1].text()
    return None


def proc_stat(pid):
    """/proc/<pid>/stat's fields after the command name, so index 0 is field 3
    (state): ppid is [1], pgid [2], start time in clock ticks [19]. None when unreadable."""
    try:
        return open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    except (OSError, IndexError):
        return None


def proc_start(pid):
    """The process's start time in clock ticks (/proc/<pid>/stat field 22), or None."""
    st = proc_stat(pid)
    try:
        return int(st[19]) if st else None
    except (ValueError, IndexError):
        return None


def my_uid():
    """This process's uid, or None where the OS has none (Windows)."""
    return os.getuid() if hasattr(os, "getuid") else None


def own_tree(table, me):
    """`me`, everything under it, and its ancestors: the screener's own processes."""
    out = set(subtree(table, me)) | {me}
    p = table.get(me, (None,))[0]
    while p in table and p not in out:
        out.add(p)
        p = table[p][0]
    return out


def posix_re(pat, ere):
    """A Python regex for a GNU BRE (`ere` False) or ERE pattern, or None for
    syntax this does not model (bracket classes, back-references, a backslash
    inside brackets)."""
    out, i, n = [], 0, len(pat)
    while i < n:
        c = pat[i]
        if c == "\\":
            d = pat[i + 1:i + 2]
            if not d or d.isdigit():
                return None
            if d in "wWsSbB":
                out.append("\\" + d)
            elif d in "<>":
                out.append(r"\b(?=\w)" if d == "<" else r"\b(?<=\w)")
            elif not ere and d in "(){}|+?":
                out.append(d)
            elif d in ".[]()*+?{}|^$\\/-":
                out.append("\\" + d)
            else:
                return None
            i += 2
        elif c == "[":
            k = i + 1 + (pat[i + 1:i + 2] == "^")
            k += pat[k:k + 1] == "]"
            j = pat.find("]", k)
            if j < 0:
                return None
            body = pat[i + 1:j]
            neg = body.startswith("^")
            core = body[1:] if neg else body
            if "\\" in core or "[" in core or "--" in core:
                return None
            core = core.replace("]", r"\]").replace("&", r"\&").replace("~", r"\~").replace("|", r"\|")
            out.append("[" + ("^" if neg else "") + core + "]")
            i = j + 1
        else:
            out.append(c if c in ".*^$" or (ere and c in "(){}|+?") else re.escape(c))
            i += 1
    return "".join(out)


def grep_spec(words):
    """(patterns, mode, icase, invert, wordx, files) of a grep command's literal
    words, or None for options this does not model."""
    pats, icase, invert, wordx, pos = [], False, False, "", []
    mode = {"egrep": "E", "fgrep": "F"}.get(exe_base(words[0]), "G")
    i, opts = 1, True
    while i < len(words):
        a = words[i]
        i += 1
        if not opts or a == "-" or not a.startswith("-"):
            pos.append(a)
            continue
        if a == "--":
            opts = False
            continue
        if a.startswith("--"):
            name, _, val = a[2:].partition("=")
            if name in ("quiet", "silent", "count", "no-messages", "line-number", "with-filename", "no-filename",
                        "files-with-matches", "only-matching", "color", "colour", "max-count"):
                i += name == "max-count" and not val
                continue
            flag = {"invert-match": "v", "ignore-case": "i", "word-regexp": "w", "line-regexp": "x",
                    "extended-regexp": "E", "fixed-strings": "F", "basic-regexp": "G"}.get(name)
            if name == "regexp" and val:
                pats.append(val)
                continue
            if flag is None:
                return None
            a = "-" + flag
        for k, f in enumerate(a[1:], 1):
            if f in "qscnhHlob":
                continue
            if f in "EFG":
                mode = f
            elif f in "iy":
                icase = True
            elif f == "v":
                invert = True
            elif f in "wx":
                wordx = f
            elif f in "em":
                val = a[k + 1:] or (words[i] if i < len(words) else None)
                if val is None:
                    return None
                if not a[k + 1:]:
                    i += 1
                if f == "e":
                    pats.append(val)
                break
            else:
                return None  # -f, -r, -z, -P, -L, context lines ...: not modelled
    if not pats:
        if not pos:
            return None
        pats = [pos.pop(0)]
    return [p for q in pats for p in q.split("\n")], mode, icase, invert, wordx, pos


def grep_matcher(spec):
    """line -> selected, for a grep_spec; None when the pattern does not translate."""
    pats, mode, icase, invert, wordx, _ = spec
    parts = []
    for p in pats:
        r = re.escape(p) if mode == "F" else posix_re(p, mode == "E")
        if r is None:
            return None
        parts.append(r)
    rx = "|".join(f"(?:{r})" for r in parts)
    if wordx == "w":
        rx = rf"(?<![A-Za-z0-9_])(?:{rx})(?![A-Za-z0-9_])"
    try:
        c = re.compile(rx, re.I if icase else 0)
    except (re.error, OverflowError):
        return None
    hit = c.fullmatch if wordx == "x" else c.search
    return lambda line: (hit(line) is None) if invert else (hit(line) is not None)


def pgrep_spec(words):
    """(pattern, icase, exact, no_ancestors) of a `pgrep -f` command, or None:
    without -f pgrep reads process names, not command lines, and the selection
    options (-u, -P, -g, ...) are not modelled."""
    full = icase = exact = anc = False
    pats, i = [], 1
    while i < len(words):
        a = words[i]
        i += 1
        if a in ("--full",):
            full = True
        elif a in ("--ignore-case",):
            icase = True
        elif a in ("--exact",):
            exact = True
        elif a in ("--ignore-ancestors",):
            anc = True
        elif a in ("--list-full", "--list-name", "--count", "--newest", "--oldest", "--lightweight"):
            pass
        elif a.startswith("--delimiter"):
            i += "=" not in a
        elif a.startswith("-") and len(a) > 1:
            for k, f in enumerate(a[1:], 1):
                if f == "f":
                    full = True
                elif f == "i":
                    icase = True
                elif f == "x":
                    exact = True
                elif f == "A":
                    anc = True
                elif f in "alcnow":
                    continue
                elif f == "d":
                    i += not a[k + 1:]
                    break
                else:
                    return None
        else:
            pats.append(a)
    if not full or len(pats) != 1:
        return None
    return pats[0], icase, exact, anc


def ps_all(words):
    """None when a `ps` command does not list every process with its full command
    line; else "args" for an args-only format (`ps -eo args`), "full" otherwise."""
    allp, i, fmt_only = False, 1, None
    while i < len(words):
        a = words[i]
        i += 1
        if a in ("--no-headers", "--no-heading", "-w", "-ww"):
            continue
        if a in ("--cols", "--columns", "--width", "--sort"):
            i += 1
            continue
        fmt = None
        if a in ("-o", "o", "-O", "O", "--format", "-eo", "axo", "-eO"):
            fmt = words[i] if i < len(words) else ""
            i += 1
            allp = allp or a in ("-eo", "axo", "-eO")
            if not re.search(r"\b(?:args|cmd|command)\b", fmt):
                return None
            fmt_only = re.fullmatch(r"(?:args|cmd|command)=?", fmt) is not None and a not in ("-O", "O", "-eO")
            continue
        if re.fullmatch(r"-?[auxwfjlvhSe]+", a) and "a" in a and "x" in a and not a.startswith("-e"):
            allp = True
        elif re.fullmatch(r"-[eAfFlwjHyM]+", a) and ("e" in a or "A" in a):
            allp = True
        else:
            return None
    return ("args" if fmt_only else "full") if allp else None


class ProcFacts:
    """The /proc reads the stray classifier needs beyond the (ppid, argv) table,
    read lazily and at most once per table. A selftest passes `start`, `uids`,
    `files` and `self_pids` and reads nothing live: `files` maps a path to
    (mtime, holder pids, text), and a path missing from it is gone; `dirs` is the
    set of directories that exist (by default, those holding a `files` path)."""

    def __init__(self, table, start=None, uids=None, files=None, self_pids=None, now=None, dirs=None, zombies=None,
                 tasks=None, ppids=None):
        self.table, self._start, self._uids, self._files, self._tasks = table, start, uids, files, tasks
        self._zombies = None if zombies is None else set(zombies)
        self._ppids = ppids
        self._dirs = set(dirs) if dirs is not None else (
            None if files is None else {f.rsplit("/", 1)[0] for f in files})
        self.now = time.time() if now is None else now
        self.self_pids = own_tree(table, os.getpid()) if self_pids is None else set(self_pids)
        self._holders, self._probes = None, {}

    def start(self, pid):
        """The process's start time in clock ticks (/proc/<pid>/stat field 22), or None."""
        return self._start.get(pid) if self._start is not None else proc_start(pid)

    def zombie(self, pid):
        """True when `pid` has wholly exited and awaits its reap, or is gone. Linux only.
        State Z alone is not enough: a process whose MAIN thread called pthread_exit()
        while other threads still run also reads Z with an empty cmdline, and it is live
        work. So a zombie must also have exactly one task left. Any read that fails is
        False (work). A selftest (`start` passed) has no /proc: `zombies` names its
        zombies there, and without it an empty argv in the table stands for one."""
        if self._start is not None:
            return pid in self._zombies if self._zombies is not None else True
        if WINDOWS:
            return False
        try:
            stat = open(f"/proc/{pid}/stat").read()
        except FileNotFoundError:
            return True  # already reaped: it runs nothing
        except OSError:
            return False
        try:
            if stat.rsplit(")", 1)[1].split()[0] not in ("Z", "X"):
                return False
            return len(os.listdir(f"/proc/{pid}/task")) <= 1
        except FileNotFoundError:
            return True  # reaped between the two reads
        except (OSError, IndexError):
            return False

    def isdir(self, path):
        return path in self._dirs if self._dirs is not None else os.path.isdir(path)

    def ppid(self, pid):
        """`pid`'s parent read now, for a process newer than the table, or None. A selftest reads `ppids` alone."""
        if self._files is not None:
            return (self._ppids or {}).get(pid)
        st = proc_stat(pid)
        return int(st[1]) if st and st[1].isdigit() else None

    def writes_task(self, pid):
        """Whether `pid` may write a harness task file. A selftest reads `tasks` alone."""
        return bool((self._tasks or {}).get(pid)) if self._files is not None else fd1_writes_task(pid)

    def uid(self, pid):
        if self._uids is not None:
            return self._uids[pid] if pid in self._uids else my_uid()
        try:
            return os.stat(f"/proc/{pid}").st_uid
        except OSError:
            return None

    def probes(self, pid):
        """The patterns `pid` greps or pgreps for, from its own argv and any shell script it carries."""
        if pid not in self._probes:
            self._probes[pid] = probes_of(self.table[pid][1])
        return self._probes[pid]

    def file(self, path):
        """("gone", None, set(), None) / ("ok", mtime, holders, text) / None when unreadable.
        `text` is None for a file too large to read. Holders are every process with
        an fd open on it except the screener's own; a process whose fds this user
        cannot list (another uid's, or a non-dumpable one) is not seen."""
        if self._files is not None:
            if path not in self._files:
                return "gone", None, set(), None
            mtime, holders, text = self._files[path]
            return "ok", mtime, set(holders) - self.self_pids, text
        try:
            st = os.stat(path)
        except FileNotFoundError:
            return "gone", None, set(), None
        except OSError:
            return None
        if st.st_mode & 0o170000 != 0o100000:  # a regular file only
            return None
        text = None
        if st.st_size <= FILE_MAX:
            try:
                with open(path, encoding="utf-8", errors="replace") as fh:
                    text = fh.read()
            except OSError:
                return None
        if self._holders is None:
            self._holders = {}
            for d in os.listdir("/proc"):
                if not d.isdigit():
                    continue
                try:
                    fds = os.listdir(f"/proc/{d}/fd")
                except OSError:
                    continue
                for fd in fds:
                    try:
                        target = os.readlink(f"/proc/{d}/fd/{fd}")
                    except OSError:
                        continue
                    self._holders.setdefault(target.removesuffix(" (deleted)"), set()).add(int(d))
        holders = self._holders.get(os.path.realpath(path), set()) - self.self_pids
        return "ok", st.st_mtime, holders, text


def probes_of(argv, depth=0):
    """The patterns a process greps or pgreps for: its own when it IS pgrep or grep,
    plus those of every pgrep/grep inside a shell script it carries (`bash -c`,
    `eval`, a watcher's `--probe`), three levels deep."""
    out = set()
    if argv and exe_base(argv[0]) in PROBES:
        out |= _probe_pats(argv)
    for a in (argv[1:] if depth < 3 else []):
        if "grep" in a and re.search(r"\s", a):
            try:
                toks = sh_lex(a)
            except ShError:
                continue
            run = []
            for tok in toks + [";"]:
                if isinstance(tok, Word):
                    run.append(tok.text())
                    continue
                while run and run[0] in ("until", "while", "if", "elif", "then", "do", "else", "!", "{", "time",
                                         "exec", "command", "nohup"):
                    run.pop(0)
                if run and run[0] is not None and exe_base(run[0]) in PROBES:
                    out |= _probe_pats([w or "" for w in run])
                for w in run:
                    if w and "grep" in w:
                        out |= probes_of(["sh", w], depth + 1)
                run = []
    return out


def _probe_pats(words):
    if exe_base(words[0]) == "pgrep":
        spec = pgrep_spec(words)
        return {spec[0]} if spec else {w for w in words[1:] if w and not w.startswith("-")}
    spec = grep_spec(words)
    return set(spec[0]) if spec else set()


def _pre_loop(c, env, record):
    """Whether one command before a wait loop is inert: plain assignments,
    `export`, `cd`, `:`, `true`, or a literal `echo`/`printf` (no -v), with every
    word, assigned value and redirect target readable (a value can hide a command
    substitution). When `record`, the assignments that persist are added to
    `env`; else they make those names unknown. Setting IFS is not inert."""
    if c["kind"] != "simple":
        return False
    words = c["words"]
    sets = [re.match(r"([A-Za-z_]\w*)=", w.segs[0][1]) if w.segs and w.segs[0][0] == "lit" else None
            for w in words]
    values = {}
    for w, m in zip(words, sets):
        if m:
            value = Word()
            for kind, t in [("lit", w.segs[0][1][m.end():])] + w.segs[1:]:
                value.add(kind, t)
            value.glob = w.glob
            values[id(w)] = value.text(env)
            if values[id(w)] is None or m.group(1) == "IFS":
                return False
    k = next((i for i, m in enumerate(sets) if m is None), len(words))
    cmd = [w.text(env) for w in words[k:]]
    if cmd and (None in cmd or cmd[0] not in PRE_LOOP or (cmd[0] == "printf" and "-v" in cmd)):
        return False
    if any(t.text(env) is None for _, t in c["redirs"]):
        return False
    persist = list(zip(words[:k], sets[:k])) if not cmd else (
        [(w, m) for w, m in zip(words[k + 1:], sets[k + 1:]) if m] if cmd[0] == "export" else [])
    for w, m in persist:
        env[m.group(1)] = values[id(w)] if record else None
    for m in sets[:k] if cmd else []:
        env[m.group(1)] = None  # `X=v :` keeps X in a POSIX-mode shell and drops it in bash's default mode
    return True


def _cond_atoms(cond, env):
    """The condition of a loop as [(negated, [command words...], [input files])] per
    pipeline plus the and-or operators, or None when it is not one and-or chain of
    simple commands with literal words."""
    if len(cond) != 1 or cond[0][1] == "&":
        return None
    chain = []
    for op, (neg, cmds) in cond[0][0]:
        pipe = []
        for c in cmds:
            if c["kind"] != "simple" or not c["words"]:
                return None
            words = [w.text(env) for w in c["words"]]
            if None in words:
                return None
            inputs = []
            for rop, target in c["redirs"]:
                t = target.text(env)
                if t is None:
                    return None
                if rop == "<":
                    inputs.append(t)
            pipe.append((words, inputs))
        chain.append((op, neg, pipe))
    return chain


def _lines(text):
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def _ps_line(table, p, facts, args_only):
    """The line `ps` prints for p: the command alone for an args-only format
    (`-o args`), else user, pid and ppid before it (`ps aux` / `ps -ef`). Other
    columns (%CPU, STAT, TTY, times) are not modelled."""
    cmd = " ".join(table[p][1])
    if args_only:
        return cmd
    return f"{_user(facts.uid(p))} {p} {table[p][0]} {cmd}"


def _user(uid):
    try:
        import pwd
        return pwd.getpwuid(uid).pw_name
    except (ImportError, KeyError, TypeError, OverflowError):
        return str(uid)


def _atom(table, shell, pipe, facts, idle_min):
    """One condition pipeline -> (status, frozen, kind, label) or None (not modelled).
    `status` is the exit status it has now; `frozen` means it can never change."""
    words, inputs = pipe[0]
    base = exe_base(words[0])
    own = " ".join(table[shell][1])
    if base == "kill" and len(pipe) == 1 and len(words) == 3 and words[1] == "-0" and words[2].isdigit():
        pid = int(words[2])
        if pid not in table:
            return 1, True, "b-pid", f"kill -0 {pid}: pid gone"
        st, born = facts.start(pid), facts.start(shell)
        if st is None or born is None:
            return None
        status = 0 if facts.uid(pid) == my_uid() else 1
        if st > born:
            return status, True, "b-pid", f"kill -0 {pid}: pid reused by a process newer than the loop"
        return status, False, "b-pid", f"waits on live pid {pid}"
    if base in ("[", "test", "[[") and len(pipe) == 1:
        args = words[1:]
        if base != "test":
            if not args or args[-1] != ("]" if base == "[" else "]]"):
                return None
            args = args[:-1]
        neg = bool(args) and args[0] == "!"
        args = args[1:] if neg else args
        if len(args) != 2 or args[0] not in ("-f", "-e", "-s"):
            return None
        files, test = [args[1]], args[0]
        filters = []
    elif base in ("ps", "pgrep"):
        return _probe_atom(table, shell, pipe, facts, own)
    elif base in ("cat", "tail", "head") + PROBES:
        files, test, neg = list(inputs), None, False
        rest = words[1:]
        if base in PROBES:
            spec = grep_spec(words)
            if spec is None:
                return None
            files += spec[5]
            filters = [spec]
        else:
            n, k = None, 0
            while k < len(rest):
                a = rest[k]
                if base != "cat" and re.fullmatch(r"-n\d+|-\d+", a):
                    n = int(a.lstrip("-n"))
                elif base != "cat" and a == "-n" and k + 1 < len(rest) and rest[k + 1].isdigit():
                    n, k = int(rest[k + 1]), k + 1
                elif a.startswith("-"):
                    return None  # tail -f, -c, ...: not a one-shot read
                else:
                    files.append(a)
                k += 1
            filters = [(base, 10 if n is None else n)]
        for words2, inputs2 in pipe[1:]:
            spec = grep_spec(words2) if exe_base(words2[0]) in PROBES else None
            if spec is None or spec[5] or inputs2:
                return None
            filters.append(spec)
    else:
        return None
    if not files:
        return None
    frozen, label, lines, texts = True, [], [], []
    for f in files:
        if not f.startswith("/"):  # a /proc-model path: POSIX rules on every OS the selftest runs on
            return None
        got = facts.file(f)
        if got is None:
            return None
        state, mtime, holders, text = got
        # The loop's own `grep` mid-read is no writer, even one started after the table was read.
        own_procs = set(subtree(table, shell))
        holders = {h for h in holders if h not in own_procs and facts.ppid(h) not in own_procs}
        texts.append(text if state == "ok" else False)
        if state == "gone":
            # A missing file is usually one that does not exist YET: `until [ -f
            # build.done ]` waits on work that will create it, and no holder can
            # show for a file nobody has opened. The one exception is a harness
            # task's output file in its own session's tasks/ dir, still present:
            # the harness creates it when the task starts, so once it is gone
            # from a live tasks dir the task is gone and nothing will write it.
            if HARNESS_TASK_FILE_RE.search(f) and facts.isdir(f.rsplit("/", 1)[0]):
                label.append(f"{f} is gone from its session's tasks dir (its task ended)")
            else:
                frozen = False
                label.append(f"{f} does not exist yet")
            continue
        if holders:
            frozen = False
            label.append(f"{f} is held open by pid {min(holders)}")
        elif facts.now - mtime < max(idle_min, IDLE_MIN) * 60:  # a low --idle-min never lowers this proof
            frozen = False
            label.append(f"{f} changed {fmt_age(mtime * 1000)} ago")
        else:
            label.append(f"{f} unchanged for {fmt_age(mtime * 1000)}, no process outside the loop holds it")
        # A file that passes its own test, in a loop still looping, is a wait for it
        # to go away. Nothing need hold a lock file open and its mtime predicts nothing,
        # so that is not proven frozen. A harness task's output file is the exception:
        # the harness leaves it in place when its task ends, so old and unheld is normal.
        if state == "ok" and test is not None and (test != "-s" or text != "") \
                and not HARNESS_TASK_FILE_RE.search(f):
            return 1, False, "b-file", f"{f} passes its test now: this waits for it to go"
        if test is None:
            if text is None:
                return None
            lines += _lines(text)
    if test is not None:
        # texts[0]: False = gone; None = present but too large to read (so not empty)
        ok = texts[0] is not False and (test != "-s" or texts[0] is None or texts[0] != "")
        status = 0 if ok != neg else 1
    else:
        for flt in filters:
            if flt[0] in ("cat", "tail", "head"):
                lines = lines if flt[0] == "cat" else (lines[-flt[1]:] if flt[0] == "tail" else lines[:flt[1]])
                continue
            m = grep_matcher(flt)
            if m is None:
                return None
            lines = [ln for ln in lines if m(ln)]
        status = 0 if lines else 1
        # A bare read that printed nothing: its status is its exit code, not its output,
        # 0 on an empty file and 1 on a gone one. Not modelled.
        if len(filters) == 1 and filters[0][0] in ("cat", "tail", "head") and not lines:
            return 1, False, "b-file", f"{files[0]} reads empty: a bare read's status is not known"
        # The same wait written as a read: it passes now, so it ends only when a file
        # changes or goes (`while grep -q holder lock`), and `grep -v` waits for its
        # matching lines to go. Nothing here can rule either out.
        inverted = any(flt[0] not in ("cat", "tail", "head") and flt[3] for flt in filters)
        if (status == 0 or inverted) and not all(HARNESS_TASK_FILE_RE.search(f) for f in files):
            return 1, False, "b-file", f"{files[0]} can only change or go for this to end"
    return status, frozen, "b-file", "; ".join(label)


def _probe_atom(table, shell, pipe, facts, own):
    """A pgrep -f or ps|grep condition -> (status, frozen, "a", label) or None."""
    words, inputs = pipe[0]
    if inputs:
        return None
    if exe_base(words[0]) == "pgrep":
        if len(pipe) != 1:
            return None
        spec = pgrep_spec(words)
        if spec is None:
            return None
        pat, icase, exact, anc = spec
        r = posix_re(pat, True)
        if r is None:
            return None
        try:
            rx = re.compile(r, re.I if icase else 0)
        except (re.error, OverflowError):
            return None
        hit = rx.fullmatch if exact else rx.search
        # -A drops pgrep's ancestors: the loop shell and everything above it
        excluded = own_tree(table, shell) - (set(subtree(table, shell)) - {shell}) if anc else set()
        line = lambda p: " ".join(table[p][1])  # noqa: E731
        selfm = shell not in excluded and hit(own) is not None
        pats = {pat}
    else:
        kind = ps_all(words)
        if kind is None or len(pipe) < 2:
            return None
        filters = []
        for words2, inputs2 in pipe[1:]:
            spec = grep_spec(words2) if exe_base(words2[0]) in PROBES else None
            m = grep_matcher(spec) if spec and not spec[5] and not inputs2 else None
            if m is None:
                return None
            filters.append(m)
        hit = lambda s: all(m(s) for m in filters) or None  # noqa: E731
        excluded, line = set(), lambda p: _ps_line(table, p, facts, kind == "args")  # noqa: E731
        selfm = hit(line(shell)) is not None
        pats = set()
        for words2, _ in pipe[1:]:
            spec = grep_spec(words2)
            pats |= set() if spec[3] else set(spec[0])  # what the chain selects for, not what -v drops
        if not pats:
            return None
    if not selfm:
        return 1, False, "a", "not self-matching"  # it ends when its target does: a real wait
    targets = sorted(p for p in table if p not in facts.self_pids and p not in excluded and table[p][1]
                     and hit(line(p)) is not None and not (facts.probes(p) & pats))
    shown = "pgrep" if exe_base(words[0]) == "pgrep" else "ps|grep"
    if targets:
        return 0, False, "a", f"waits on live pid {targets[0]} (self-matching loop: will never exit on its own)"
    return 0, True, "a", f"self-matching {shown} {min(pats)!r}, no live target"


def stray_verdict(table, pid, facts, idle_min):
    """_stray_verdict(), failing safe: a command line this reader trips on is work,
    never a crash of the whole screen."""
    try:
        return _stray_verdict(table, pid, facts, idle_min)
    except Exception as e:  # noqa: BLE001 - any failure here must read as "not stray"
        return False, None, f"not judged ({type(e).__name__})"


def _stray_verdict(table, pid, facts, idle_min):
    """(stray, class, why) for one Linux tool shell. Stray only when the shell runs
    one unbounded until/while loop whose body only sleeps, nothing but that loop's
    own probes and sleeps runs under it, every part of its condition is frozen
    (class a: a self-matching pgrep -f / ps|grep with no live target; b-file: a
    file no process outside the loop holds open, unchanged past the idle floor, which
    the condition does not already pass on, or a harness task's output file, even
    one that is gone; b-pid: a kill -0 pid that is gone or was
    reused), and the condition still says "loop".
    `class` is "a", "b-file", "b-pid" or a "+" mix for a stray, "c" for a bounded
    wait, None otherwise."""
    argv = table[pid][1]
    cmd = tool_command(argv) if UNREADABLE_CMDLINE not in argv else None
    if cmd is None:
        return False, None, "command line not readable as a tool shell"
    if re.search(r"\b(?:until|while)\b", cmd) and BOUNDED_RE.search(cmd):
        return False, "c", "bounded wait (a timeout, counter or deadline ends it)"
    items = parse_sh(cmd)
    if items is None:
        return False, None, "not shell this classifier reads"
    loops = [k for k, (ao, sep) in enumerate(items)
             if len(ao) == 1 and not ao[0][1][0] and len(ao[0][1][1]) == 1
             and ao[0][1][1][0]["kind"] in ("until", "while") and sep != "&"]
    if len(loops) != 1:
        return False, None, "not one wait loop"
    env = {}
    for ao, sep in items[:loops[0]]:
        if sep == "&":
            return False, None, "starts a background job before its loop"
        single = len(ao) == 1 and len(ao[0][1][1]) == 1
        for _, (_, cmds) in ao:
            for c in cmds:
                if not _pre_loop(c, env, single):
                    return False, None, "runs more than assignments, cd and literal echo before its loop"
    loop = items[loops[0]][0][0][1][1][0]
    for ao, _ in loop["body"]:
        for _, (_, cmds) in ao:
            for c in cmds:
                words = [w.text() for w in c.get("words", [])]
                if c["kind"] != "simple" or not words or words[0] not in LOOP_BODY or None in words \
                        or (words[0] == "printf" and "-v" in words) \
                        or any(t.text() != "/dev/null" and not (op == ">&" and (t.text() or "").isdigit())
                               for op, t in c["redirs"]):
                    return False, None, "the loop body does more than wait"
    chain = _cond_atoms(loop["cond"], env or {})
    if chain is None:
        return False, None, "loop condition not readable"
    allowed = {"sleep"} | {exe_base(w[0]) for _, _, pipe in chain for w, _ in pipe}
    for d in subtree(table, pid):
        dv = table[d][1]
        if not dv and facts.zombie(d):
            # A child that has exited and is not yet reaped reads an EMPTY cmdline (the kernel
            # frees its argv at exit): most often the loop's own probe, caught between its
            # exit and bash's wait. It runs nothing. Read as work, it made a real stray miss
            # one screen at random, and flaked the end_strays selftest on slower CI runners.
            # A LIVE process with an empty cmdline (one that blanked its own argv) is still work.
            continue
        if d != pid and dv != argv and not (dv and exe_base(dv[0]) in allowed
                                            and not (exe_base(dv[0]) == "tail" and any(
                                                a.startswith(("-f", "-F", "--follow")) for a in dv[1:]))):
            return False, None, "runs " + " ".join(dv)[:60]
    status, parts = None, []
    for op, neg, pipe in chain:
        atom = _atom(table, pid, pipe, facts, idle_min)
        if atom is None:
            return False, None, "loop condition not judged"
        s = (1 if atom[0] == 0 else 0) if neg else atom[0]
        if op is None or (op == "&&" and status == 0) or (op == "||" and status != 0):
            status = s
        parts.append(atom)
    live = next((a for a in parts if not a[1]), None)
    if live:
        return False, None, live[3]
    if (status == 0) != (loop["kind"] == "while"):
        return False, None, "its condition already holds: the loop ends on its next pass"
    return True, "+".join(sorted({a[2] for a in parts})), "; ".join(a[3] for a in parts)


_FACTS = [None, None]  # (table, ProcFacts): one set of /proc reads per process table


def split_strays(table, shells, idle_min, windows=WINDOWS):
    """(work, strays, waits) for a pane's tool shells: the shells that are work, the
    proven-stray wait loops as (pid, class, why), and the reasons of self-matching
    loops that still wait on live work. Windows' command lines are not read, so
    there every shell is work."""
    if windows or not shells:
        return list(shells), [], []
    if _FACTS[0] is not table:
        _FACTS[:] = [table, ProcFacts(table)]
    work, strays, waits = [], [], []
    for p in shells:
        stray, cls, why = stray_verdict(table, p, _FACTS[1], idle_min)
        if stray:
            strays.append((p, cls, why))
        else:
            work.append(p)
            if why.startswith("waits on live pid") and "self-matching" in why and why not in waits:
                waits.append(why)
    return work, strays, waits


def task_id_of(pid):
    """The harness task id a tool shell writes to: the stem of the
    `<...>/tasks/<task_id>.output` its fd 1 points at, or None."""
    try:
        m = TASK_OUTPUT_RE.search(os.readlink(f"/proc/{pid}/fd/1"))
    except OSError:
        return None
    return m.group(1) if m else None


def fd1_writes_task(pid, readlink=os.readlink):
    """Whether `pid`'s fd 1 is a harness task file. An fd this user cannot read is
    True, because "could not tell" is not "no"; a process or fd that is gone is False."""
    try:
        return TASK_OUTPUT_RE.search(readlink(f"/proc/{pid}/fd/1")) is not None
    except FileNotFoundError:
        return False
    except OSError:
        return True


def unseen_work(table, pane, claudes, strays):
    """Processes under the pane, outside the strays' own subtrees (a loop's `sleep`
    inherits its fd 1), that may be work proc_facts cannot call a tool shell. A tool
    shell that ran `exec job` no longer names the snapshot in its argv, yet it is
    still a shell to Claude Code's record. So: any that may write a harness task file,
    and any direct child of a claude started after that claude's boot window (the
    start-time half of the runner's child_signals_work). A start time that cannot be
    read counts as late. Work that does neither is not seen: a job exec'd inside the
    window whose stdout is not a task file, and work under a boot-time child."""
    facts = _FACTS[1]
    mine = {d for p, _, _ in strays for d in subtree(table, p)}
    window = SESSION_START_CHILD_S * os.sysconf("SC_CLK_TCK")

    def late(child, claude):
        born, started = facts.start(claude), facts.start(child)
        return born is None or started is None or started - born > window

    found = [p for p in subtree(table, pane) if p not in mine and facts.writes_task(p)]
    return found + [p for p, (ppid, argv) in table.items() if ppid in claudes and p not in mine and p not in found
                    and not (not argv and facts.zombie(p)) and late(p, ppid)]  # an unreaped child runs nothing


def own_tasks(table, me, idle_min, facts=None, task_of=task_id_of):
    """(claude_pid, rows) for the session running `me`: one row per tool shell that
    is a direct child of the nearest claude above `me`, `me`'s own ancestry left
    out, as {task_id, pid, stray, class, why, command}. claude_pid is None when no
    claude runs above `me`. A subagent shares its parent's claude, so from one
    this lists the parent's tasks too."""
    ancestry, claude, p = {me}, None, table.get(me, (None,))[0]
    while p in table and p not in ancestry:
        if is_claude(table[p][1]):
            claude = p
            break
        ancestry.add(p)
        p = table[p][0]
    if claude is None:
        return None, []
    facts = facts or ProcFacts(table)
    rows = []
    for c in sorted(table):
        ppid, argv = table[c]
        if ppid != claude or c in ancestry or not any(SNAPSHOT in a for a in argv):
            continue
        stray, cls, why = stray_verdict(table, c, facts, idle_min)
        cmd = tool_command(argv) or " ".join(argv)
        rows.append({"task_id": task_of(c), "pid": c, "stray": stray, "class": cls, "why": why,
                     "command": re.sub(r"\s+", " ", cmd).strip()[:120]})
    return claude, rows


def own_tasks_main(idle_min, as_json):
    """--own-tasks: list this session's background tasks. Exit 0 when none is live
    work (every one is a proven stray, or there are none), 1 when one is, 3 UNKNOWN
    (no /proc, the process table unreadable, or no claude above this process)."""
    table = None if WINDOWS else proc_table()
    claude, rows = own_tasks(table, os.getpid(), idle_min) if table else (None, [])
    if claude is None:
        why = "no /proc on this OS" if WINDOWS or table is None else "this process runs under no claude"
        if as_json:
            print(json.dumps({"verdict": "UNKNOWN", "reason": why, "claude_pid": None, "tasks": []}))
        else:
            print(f"UNKNOWN: background tasks cannot be enumerated: {why}")
        return 3
    live = [r for r in rows if not r["stray"]]
    if as_json:
        print(json.dumps({"verdict": "LIVE" if live else "CLEAR", "claude_pid": claude, "tasks": rows}, indent=1))
        return 1 if live else 0
    print(f"own tasks of claude pid {claude}: {len(rows)} background task(s), {len(rows) - len(live)} stray, "
          f"{len(live)} live")
    for r in rows:
        tid = r["task_id"] or "-"
        if r["stray"]:
            print(f"STRAY {tid} pid={r['pid']} {r['class']}: {r['command']}")
        else:
            print(f"LIVE {tid} pid={r['pid']} {r['why']}: {r['command']}")
    return 1 if live else 0


# ---------------------------------------------------------------- transcript
def ms(iso):
    return dt.datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() * 1000


def fmt_age(msv):
    if msv is None:
        return "?"
    m = (time.time() * 1000 - msv) / 60000
    return f"{m:.0f}m" if m < 120 else f"{m / 60:.1f}h" if m < 2880 else f"{m / 1440:.1f}d"


# What one JSONL line can raise while it is parsed and its time read: a cut line, a
# bad time, a non-object line, deep nesting.
BAD_RECORD = (ValueError, TypeError, KeyError, AttributeError, RecursionError)


def newest_record(path):
    """Timestamp (ms) of the newest user/assistant record in a JSONL file, or
    None. Reads the last 64 KiB, and the whole file only when that window holds
    no such record: one record can be larger than the window."""
    try:
        size = os.path.getsize(path)
        for start in ([size - 65536, 0] if size > 65536 else [0]):
            with open(path, "rb") as f:
                f.seek(start)
                lines = f.read().split(b"\n")  # never splitlines(): a raw U+2028 is legal inside a JSON string
            for line in reversed(lines):
                try:
                    e = json.loads(line.decode(errors="replace"))
                    if e.get("type") in ("user", "assistant") and e.get("timestamp") and not e.get("isCompactSummary"):
                        return ms(e["timestamp"])
                except BAD_RECORD:
                    continue  # a line the window cut, a non-object line, an unreadable time
    except OSError:
        return None
    return None


# /next-move as a prompt, in either form: the slash command that ran, or the
# typed text. nudge-stopped-sessions.py aliases this, so its nudge screen and
# the segment counts below read one definition.
NEXT_MOVE_PROMPT = re.compile(r"<command-name>/next-move</command-name>|^/next-move(?![\w-])")
# A tool call that changes the work, as opposed to /next-move's own
# classification, which reads files and coord and records an intervention.
# Every mcp__coord* call is bookkeeping, so only file edits and git/gh writes count,
# in the session's own transcript or in a subagent's (an `Agent` call itself is
# not work: what the subagent then does is what counts). A PR write goes through
# gh or the runner's own `qontinui-pr create`.
WORK_TOOLS = ("Edit", "MultiEdit", "Write", "NotebookEdit")
WORK_BASH = re.compile(r"\bgit(?:\s+(?:-[Cc]\s+\S+|--?[\w-]+(?:=\S+)?))*\s+(?:commit|push|commit-tree)\b"
                       r"|\bgh\s+pr\s+(?:create|merge|edit|ready)\b"
                       r"|\bqontinui-pr(?:\.exe)?\s+create\b")


# Where /next-move writes its own scratch (the E1 row file):
# an edit there is classification, not work.
SCRATCH_PATH = re.compile(r"(?:^|[\\/])(?:\.qontinui[\\/]scratch|scratchpad|\.agent-scratch)(?:[\\/]|$)"
                          r"|^(?:/tmp|/var/tmp)(?:/|$)|[\\/]AppData[\\/]Local[\\/]Temp(?:[\\/]|$)", re.I)


def is_work_tool(block):
    """True for a tool_use block that edits a file outside a scratch or temp
    directory, or writes git history / a PR."""
    name = block.get("name") or ""
    inp = block.get("input")
    if name in WORK_TOOLS:
        target = str((inp or {}).get("file_path") or (inp or {}).get("notebook_path") or "") if isinstance(inp, dict) else ""
        return not SCRATCH_PATH.search(target)
    return name == "Bash" and isinstance(inp, dict) and bool(WORK_BASH.search(str(inp.get("command") or "")))


def nudge_facts(segments):
    """(work_tools_since_nudge, nudge_streak) from the prompt segments
    transcript_facts collects, each {"nudge", "work", "final"}. The streak
    counts consecutive /next-move segments with no work-class tool call,
    walking back from the newest; one answered only by a usage-limit notice
    is skipped (it never ran), and any other prompt or a productive
    /next-move ends the walk."""
    last = max((i for i, s in enumerate(segments) if s["nudge"]), default=None)
    if last is None:
        return 0, 0
    work = sum(s["work"] for s in segments[last:])
    streak = 0
    for s in reversed(segments):
        if not s["nudge"]:
            break
        if not s["work"] and LIMIT_RE.search(s["final"][-300:]):
            continue
        if s["work"]:
            break
        streak += 1
    return work, streak


def nudge_replied(segments):
    """True when the newest prompt is a /next-move the session really answered:
    the model itself wrote text after it. A /next-move with no reply, or only
    Claude Code's own notice (a limit, an expired login, an API error), never ran
    its turn, so finding no work after it measures nothing."""
    return bool(segments) and segments[-1]["nudge"] and bool(segments[-1].get("replied"))


def model_wrote(e, text):
    """False for assistant-shaped text Claude Code wrote itself. Those records
    are flagged, or attributed to the <synthetic> model; LIMIT_RE covers a limit
    notice from a transcript that predates the flag."""
    if e.get("isApiErrorMessage") or e["message"].get("model") == "<synthetic>":
        return False
    return not LIMIT_RE.search(text[-300:])


def subagent_work(path, segments):
    """Add a subagent transcript's work-class tool calls to the segment each
    falls in by time (the newest segment that started at or before it)."""
    try:
        lines = open(path, encoding="utf-8", errors="replace")
    except OSError:
        return
    with lines:
        for line in lines:
            try:
                e = json.loads(line)
                at = ms(e["timestamp"]) if e.get("type") == "assistant" and e.get("timestamp") else None
            except BAD_RECORD:
                continue
            content = (e.get("message") or {}).get("content") if at and isinstance(e.get("message"), dict) else None
            for b in content if isinstance(content, list) else []:
                if isinstance(b, dict) and b.get("type") == "tool_use" and is_work_tool(b):
                    seg = next((sg for sg in reversed(segments) if sg["at"] <= at), None)
                    if seg is not None:
                        seg["work"] += 1


# User records that open no turn even outside one: a local command's caveat and
# output, an interrupt, and a META system reminder (a /rename writes one). A
# non-meta <system-reminder> record is a delivery that does open one: coord's
# Session Bus `ci_rework` / `stuck_pr_nudge` messages arrive that way.
NOT_A_TURN = ("<local-command", "Caveat:", "<system-reminder>", "[Request interrupted")
# The three classes of turn opener (plan
# 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 3). A
# *prompt* is what `last_prompt` counts (a non-meta record whose text starts
# with no NON_PROMPT prefix), text wrapped in <pasted_content> included. A
# *delivery* is coord's Session Bus message - a non-meta record whose text
# starts SESSION_BUS, bare or wrapped in <pasted_content> - or a peer message.
# Everything else that opens a turn is *self*: a <task-notification>, a /loop
# or ScheduleWakeup wake, a usage-limit auto-continuation, any other non-meta
# <system-reminder>, and any opener that matches no class. The class only NAMES
# the opener in a revival's reason: every class revives a finish alike
# (finish_state, one rule; served finished-criteria condition 4). The
# <pasted_content> wrapper's exact shape is not written down in the repos this
# reads, so PASTED_RE accepts the tag with any attributes, and the --selftest
# records that pin it are labelled SYNTHESIZED.
PASTED_RE = re.compile(r"^<pasted_content\b[^>]*>\s*")
SESSION_BUS = "<system-reminder>Session Bus "
PEER_MESSAGE = "Another Claude session sent a message"
OPENER_NAMES = {"prompt": "a prompt", "delivery": "a delivery: a Session Bus or peer message",
                "self": "the session's own turn: a wake, a task notification or a system reminder"}
OPEN_KEYS = ("last_turn_open", "last_turn_open_text", "last_turn_open_class", "last_revive_open",
             "last_revive_open_text", "last_revive_open_class")


def opener_class(e, text):
    """'prompt', 'delivery' or 'self' for a user record that opened a turn. Pure."""
    t = text.lstrip()
    inner = PASTED_RE.sub("", t, count=1)
    meta = bool(e.get("isMeta"))
    if inner.startswith(PEER_MESSAGE) or (not meta and inner.startswith(SESSION_BUS)):
        return "delivery"
    if not meta and t and not t.startswith(NON_PROMPT):
        return "prompt"
    return "self"


def turn_open(f, turn, e, content, text, at):
    """Fold one user record into f["last_turn_open"] / ["last_turn_open_text"]:
    the newest record that opened a turn, of ANY kind - a real prompt, and
    also what `last_prompt` leaves out: a <task-notification>, a /loop or
    ScheduleWakeup wake, a peer message, a usage-limit auto-continuation
    (meta or not). A record arriving inside a running turn (a tool result, a
    skill body injected after a Skill call), a compaction summary, a NOT_A_TURN
    record (a <system-reminder> one only when meta) and a built-in command
    Claude Code answers itself (/model) open none. Each opener is also filed by
    opener_class: its class into `last_turn_open_class`, and the newest prompt
    or delivery into `last_revive_open` (with its `_text` and `_class`), so a
    delivery is still named when a later wake opened the newest turn.
    `turn` is {"in": a turn is running, "before": the OPEN_KEYS to restore}."""
    t = text.lstrip()
    if t.startswith(LOCAL_OUTPUT):
        if turn["before"] is not None and f["last_turn_open_text"].startswith("<command-name>"):
            f.update(turn["before"])
        turn["before"] = None
    elif t.startswith("[Request interrupted"):
        turn["in"] = False  # the interrupted turn ended without an end_turn record
    elif not (turn["in"] or not t or e.get("isCompactSummary") or (
            t.startswith(NOT_A_TURN) and (e.get("isMeta") or not t.startswith("<system-reminder>"))) or (
            isinstance(content, list) and any(isinstance(b, dict) and b.get("type") == "tool_result" for b in content))):
        turn["before"] = {k: f[k] for k in OPEN_KEYS}
        cls = opener_class(e, text)
        f["last_turn_open"], f["last_turn_open_text"], f["last_turn_open_class"] = at, t, cls
        if cls != "self":
            f["last_revive_open"], f["last_revive_open_text"], f["last_revive_open_class"] = at, t, cls


def last_ran_prompt(segments):
    """When the newest prompt that ran its turn was given, or None when none
    did. A /next-move with no reply of the model's own and no work tool call
    never ran (nudge_replied), so it is skipped: a screen weighing whether a
    prompt revived a finished session reads this rather than last_prompt."""
    for s in reversed(segments):
        if not (s["nudge"] and not s.get("replied") and not s["work"]):
            return s["at"]
    return None


def transcript_facts(row):
    cd = row.get("configDir") or os.path.expanduser("~/.claude")
    sid = row.get("claudeSessionId") or ""
    hits = glob.glob(os.path.join(cd, "projects", "*", f"{sid}.jsonl")) if sid else []
    if not hits:
        return None
    path = max(hits, key=os.path.getmtime)
    # `last_activity` is the newest user/assistant record, NOT the file mtime:
    # Claude Code keeps appending metadata records (mode, permission-mode,
    # pr-link, away_summary) to an idle session's transcript. A background
    # subagent writes its own <sid>/subagents/*.jsonl, so those count too.
    # A built-in command Claude Code answers itself (/model, /effort,
    # /rate-limit-options) is recorded as a <command-name> prompt followed by a
    # <local-command-stdout> record, with no assistant turn. It is not an
    # instruction to the session, so that stdout record takes the prompt back off
    # `last_prompt`, and `task_prompts` counts only the prompts left. /clear
    # writes no stdout record, so it is matched by name: it still moves
    # `last_prompt` (a cleared tab is being reused, so it is not left finished),
    # but it is not a task. (/rename writes a
    # `system` record, which this reader never counted.) `final_text_at` is when
    # `final_text` was written: a later tool-only assistant turn moves
    # `last_asst` but not the text. Each real prompt opens a segment, so the
    # nudge screen can tell a /next-move that led to work from one that only
    # classified (nudge_facts); `cwd` is the newest user record's.
    # `last_turn_open` is the newest record that opened a turn of any kind
    # (turn_open), so a wake that `last_prompt` ignores can still be read;
    # `last_turn_open_class` and `last_revive_open` name its class (opener_class).
    f = {"path": path, "last_activity": None, "last_prompt": None, "last_prompt_text": "",
         "last_asst": None, "final_text": "", "final_text_at": None, "pending_tool": False,
         "task_prompts": 0, "work_tools_since_nudge": 0, "nudge_streak": 0, "nudge_replied": False, "cwd": None,
         "last_turn_open": None, "last_turn_open_text": "", "last_turn_open_class": None,
         "last_revive_open": None, "last_revive_open_text": "", "last_revive_open_class": None}
    open_tools = set()
    turn = {"in": False, "before": None}
    segments = []
    before = None  # (last_prompt, last_prompt_text) as they stood before the newest prompt
    for line in open(path, encoding="utf-8", errors="replace"):  # never the locale codepage (cp1252 on Windows)
        try:
            e = json.loads(line)
            ts = e.get("timestamp")
            if not ts or not isinstance(e.get("message"), dict):
                continue
            at = ms(ts)
        except BAD_RECORD:
            continue  # a non-object line, an unreadable time or deep nesting
        content = e["message"].get("content")
        # A compaction summary is not activity: Claude Code compacts an IDLE
        # session on its own ("Compacted while idle, before the prompt cache
        # expired"), writing an isCompactSummary user record with no turn around
        # it, which read as fresh activity and held a finished tab BUSY for the
        # whole idle floor (2026-11-03: the transcript's `system` record carried
        # that message, 2 s after the summary). A compaction inside a turn is
        # followed by that turn's own records, which still count.
        if e.get("type") in ("user", "assistant") and not e.get("isCompactSummary"):
            f["last_activity"] = at
        if e.get("type") == "user":
            if e.get("cwd"):
                f["cwd"] = e["cwd"]
            if isinstance(content, list):
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "tool_result":
                        open_tools.discard(b.get("tool_use_id"))
            text = content if isinstance(content, str) else " ".join(
                b.get("text", "") for b in content or [] if isinstance(b, dict) and b.get("type") == "text")
            turn_open(f, turn, e, content, text, at)
            if e.get("isMeta") or e.get("isCompactSummary"):
                continue
            if text.lstrip().startswith(LOCAL_OUTPUT):
                if before is not None and f["last_prompt_text"].startswith("<command-name>"):
                    f["task_prompts"] -= not f["last_prompt_text"].startswith(CLEAR)
                    f["last_prompt"], f["last_prompt_text"] = before
                    segments.pop()
                before = None
            elif text.strip() and not text.lstrip().startswith(NON_PROMPT):
                before = (f["last_prompt"], f["last_prompt_text"])
                f["last_prompt"], f["last_prompt_text"] = at, text.strip()
                f["task_prompts"] += not text.lstrip().startswith(CLEAR)
                segments.append({"nudge": bool(NEXT_MOVE_PROMPT.search(text.strip())), "work": 0, "final": "",
                                 "at": at})
        elif e.get("type") == "assistant":
            before = None
            turn["in"], turn["before"] = e["message"].get("stop_reason") == "tool_use", None
            f["last_asst"] = at
            for b in content if isinstance(content, list) else []:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "text" and b.get("text", "").strip():
                    f["final_text"], f["final_text_at"] = b["text"], at
                    if segments:
                        segments[-1]["final"] = b["text"]
                        segments[-1]["replied"] = segments[-1].get("replied") or model_wrote(e, b["text"])
                elif b.get("type") == "tool_use":
                    open_tools.add(b.get("id"))
                    if segments and is_work_tool(b):
                        segments[-1]["work"] += 1
    f["pending_tool"] = bool(open_tools)
    # The streak walks back over the trailing run of /next-moves, so a subagent
    # active at any point in that run can end it, not only one active since the newest.
    run_from = None
    for s in reversed(segments):
        if not s["nudge"]:
            break
        run_from = s["at"]
    for sub in glob.glob(os.path.join(os.path.dirname(path), sid, "subagents", "*.jsonl")):
        t = newest_record(sub)
        if t and t > (f["last_activity"] or 0):
            f["last_activity"] = t
        if run_from is not None and t and t >= run_from:
            subagent_work(sub, segments)
    f["work_tools_since_nudge"], f["nudge_streak"] = nudge_facts(segments)
    f["nudge_replied"] = nudge_replied(segments)
    f["last_ran_prompt"] = last_ran_prompt(segments)
    return f


# ---------------------------------------------------------------- screening
# What --close closes, and what triage-sessions.py predicts it will close. A live
# session is FINISHED only when coord's session_status reads finished (served
# finished-criteria condition 1), by the one finish rule both screens read
# (finish_state), and its directory is a linked worktree (condition 3, CUSTODY
# otherwise); EXITED-FINISHED is a pane whose claude has already exited, so
# closing it ends no session. CLOSEOUT and FOOTER-FINISHED rest on the final
# reply's own words, which coord has not recorded: never closed.
CLOSABLE = {"FINISHED", "EXITED-FINISHED"}
# Every verdict classify() returns. Both report orders (ORDER here and in
# nudge-stopped-sessions.py) are checked against it by each helper's selftest,
# and the selftest reads classify_row's own return statements against it, so a
# verdict added without a report row fails CI instead of a run's print_report.
VERDICTS = ("SELF", "SHELL", "UNKNOWN", "BUSY", "LIMITED", "REVIVED", "FINISHED", "CLOSEOUT", "FOOTER-FINISHED",
            "FOOTER-ONLY", "CONTRADICTED", "CUSTODY", "EXITED-FINISHED", "EXITED", "ACTIVE")


def returned_verdicts(*fns):
    """Every string a function returns as the first element of a tuple (both arms
    of a conditional included), read from its source with `ast`: the ground truth
    the verdict-roster checks compare a hand-written list against."""
    import ast
    import inspect
    import textwrap
    out = set()

    def heads(node):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            out.add(node.value)
        elif isinstance(node, ast.IfExp):
            heads(node.body)
            heads(node.orelse)
    for fn in fns:
        for n in ast.walk(ast.parse(textwrap.dedent(inspect.getsource(fn)))):
            if isinstance(n, ast.Return) and isinstance(n.value, ast.Tuple) and n.value.elts:
                heads(n.value.elts[0])
    return out


def coord_unfinished(rp):
    """Served finished-criteria condition 1 for a live session: why coord's
    session_status withholds a close, or None when it reads `finished`. A status
    the runner did not serve (an older build, an unresolved row) is UNKNOWN, and
    an UNKNOWN withholds the close too."""
    status = (rp or {}).get("sessionStatus")
    if status == "finished":
        return None
    if isinstance(status, str) and status:
        return f"coord session_status reads {status}, not finished (finished-criteria condition 1)"
    return "coord session_status UNKNOWN (the runner served none), so not finished (finished-criteria condition 1)"


SINCE_FLOOR_MS = 1_500_000_000_000  # mid-2017: anything earlier is a seconds-scale value, not a stamp
SINCE_FUTURE_MS = 300_000  # clock skew allowed past now; a stamp later than that is not a time either


def status_since(rp, now_ms=None):
    """Coord's finish time for a session, from the runner's `sessionStatusSince`
    (unix ms: when coord's session_status last changed; coord also restamps a
    finished->finished report once plan Phase 2 lands), or None. UNKNOWN, never
    a time: absent, null, not an integer, a boolean, 0 or negative, and an
    implausible value (before SINCE_FLOOR_MS, or more than SINCE_FUTURE_MS past
    `now_ms`). Runner builds that predate the field send none, and the plan's
    Phase 3(c) contract leaves it absent for unresolved rows and nested subagents."""
    v = (rp or {}).get("sessionStatusSince")
    now_ms = time.time() * 1000 if now_ms is None else now_ms
    if isinstance(v, bool) or not isinstance(v, int) or not SINCE_FLOOR_MS <= v <= now_ms + SINCE_FUTURE_MS:
        return None
    return v


def finish_since(rp, coord_finished, now_ms=None):
    """Coord's usable finish time for a coord-finished row, else None, judged
    against `now_ms` (the caller's clock; None reads this one)."""
    now_ms = time.time() * 1000 if now_ms is None else now_ms
    since = status_since(rp, now_ms) if coord_finished else None
    if since is not None and since > now_ms:
        # The revival comparison ASSUMES coord's clock and this box's agree to well
        # under the 1 s band (NTP-synced). It does not measure skew, so it cannot
        # see a skew that has already been absorbed by the time a close is
        # considered (a past stamp says nothing about skew). This guard covers
        # only the rare case it CAN see: a stamp still ahead of this clock proves
        # coord runs ahead, so it is not compared with a local prompt time and
        # the coord-only rule decides. Closing is the unsafe direction;
        # status_since still admits the stamp for the nudge side.
        return None
    return since


UNATTENDED_REASON = "unattended:"


def unattended_trusted(row, tx, since, neg, status=None):
    """A reason line when the final reply's wording must NOT veto this row, else None.

    /unattended marks the runner finish (finishReason "unattended: ...") only when every unit is LANDED,
    WATCHED or RECORDED AND its own Step 4.9 headline reads SAFE TO CLOSE (unattended.md, the gate table),
    and it sets that mark BEFORE it writes the final report. The structured decision is made; the prose after
    it is a report, so a hedge word, a work-remains phrase or a promise phrase in it does not re-open it. What
    still does:
    - an explicit negative headline (negative_headline);
    - a veto in TRUST_KEEPS or OPERATOR_ASK: a question or an ask for the operator to act (a real
      `Session finished: no` is one of negative_headline's);
    - a reply that is not that report at all (UNATTENDED_REPORT: a subagent shares its parent's session id, so
      a stray "unattended:" finish can land on a parent that is still talking);
    - a coord status read back as anything but finished, or a missing finishedAt;
    - any sign the session was revived after EITHER finish time - a prompt or a turn-opening record later
      than finishedAt + 1 s, or than coord's time - 1 s where usable (so an old "unattended:" reason does not
      ride on a later finish some other door made), or a reply more than 10 min after the earlier of the two.
      These are stricter than classify_row's own (whose runner arm checks prompts only, and whose 10-min guard
      a closeout phrase exempts): this path relaxes a veto, so it takes the stricter test.
    Every idleness check ran before this and is unchanged."""
    why = row.get("finishReason")
    if not row.get("finished") or not isinstance(why, str) or not why.lstrip().startswith(UNATTENDED_REASON):
        return None
    final = tx["final_text"]
    if negative_headline(final) or any(label in TRUST_KEEPS for label in neg) or OPERATOR_ASK.search(final):
        return None
    if not own_report(final):
        return None  # the reply is not the report /unattended writes (a parent session sharing the id?)
    if status not in (None, "finished"):  # coord reads it back in some other state: not this rule's to relax
        return None
    fa = row.get("finishedAt")
    if isinstance(fa, bool) or not isinstance(fa, (int, float)) or fa <= 0:
        return None  # in both arms: a marker with no time of its own is not trusted
    cut = fa + 1000 if since is None else min(fa + 1000, since - 1000)
    if any(t and t > cut for t in (tx.get("last_prompt"), tx.get("last_turn_open"))):
        return None
    if tx.get("last_asst") and tx["last_asst"] - (fa if since is None else min(fa, since)) > 10 * 60000:
        return None  # stricter than classify_row's guard, which a closeout phrase exempts: here nothing does
    return (f"runner marker set by /unattended ({why.strip()[:50]}): its gate already required SAFE TO CLOSE, so "
            f"the reply's hedge/work-remains/promise wording is not consulted (would have vetoed: {neg[0]})")


def reply_reading(row, rp, tx, now_ms=None):
    """(pos, neg, trusted) for a row's final reply: phrase_hits' labels, and
    unattended_trusted's reason line (None when the reply's wording is not
    relaxed). The one reading both screens hand finish_state as
    `hits=(pos, [] if trusted else neg)` (finish_hits), so close and nudge judge a
    trusted /unattended row alike. `row` carries the runner mark (finished,
    finishedAt, finishReason), `rp` coord's sessionStatus / sessionStatusSince;
    a missing transcript reads as no reply. Trust is judged against `now_ms`,
    the caller's own clock (the one it hands finish_state)."""
    row, rp = row or {}, rp or {}
    pos, neg = phrase_hits(tx["final_text"]) if tx and tx.get("final_text") else ([], [])
    since = finish_since(rp, rp.get("sessionStatus") == "finished", now_ms)
    trusted = unattended_trusted(row, tx, since, neg, rp.get("sessionStatus")) if neg else None
    return pos, neg, trusted


def finish_hits(row, rp, tx, now_ms=None):
    """The `hits` argument finish_state takes on both screens (reply_reading),
    judged against the same `now_ms` the caller hands finish_state."""
    pos, neg, trusted = reply_reading(row, rp, tx, now_ms)
    return pos, [] if trusted else neg


BAND_MS = 1000  # a turn-opening record within 1 s of coord's stamp is the stamp's own band
KEPT_WORKING_MS = 10 * 60000  # assistant work this far past a finish, with no closeout, reopened it
# finish_state's answer: `state` one of FINISH_STATES, `at` the finish time judged against (unix ms; None when
# unconfirmed), `by` whose time that is ("coord" or "marker"; None when unconfirmed), `why` the reasons.
# `rule`, when set, names the screen rule that holds a finish the row's structured signal states (STRANDED_CODES),
# or is `untimed_mark` on an `unconfirmed` finish neither coord nor the runner mark can date (not a stranded code).
Finish = collections.namedtuple("Finish", "state at by why rule", defaults=(None,))
FINISH_STATES = ("revived", "in_band", "contradicted", "unconfirmed", "finished")


def _age(at):
    return fmt_age(at) if at else "?"


def _turn_text(tx, key):
    return repr((tx.get(key) or "")[:60])


def _reopened(tx, at, by, pos):
    """(what, why) for the turn after the finish time `at` that reopened the
    session - what is "prompt" (a real prompt), "turn" (any other record that
    opens a turn, last_turn_open) or "work" (assistant work more than 10 minutes
    past it with no closeout) - or None. A record must be more than 1 s after.
    A turn's reason names its opener's class (opener_class), a delivery past
    the stamp outranking a later self opener; every class revives alike."""
    lp, opened, asst = tx.get("last_prompt") or 0, tx.get("last_turn_open") or 0, tx.get("last_asst") or 0
    src = f"coord finished {_age(at)} ago" if by == "coord" else f"marked finished {_age(at)} ago"
    revive = tx.get("last_revive_open") or 0
    delivered = tx.get("last_revive_open_class") == "delivery" and revive > at + BAND_MS
    if lp > at + BAND_MS:
        if delivered and revive == lp:  # a pasted delivery, which last_prompt also counts
            return "prompt", (f"{src}, then a delivery opened a turn {fmt_age(lp)} ago "
                              f"({OPENER_NAMES['delivery']}): {_turn_text(tx, 'last_revive_open_text')}")
        return "prompt", f"{src}, then prompted {fmt_age(lp)} ago: {_turn_text(tx, 'last_prompt_text')}"
    if opened > at + BAND_MS:
        at_, key, cls = ((revive, "last_revive_open_text", "delivery") if delivered
                         else (opened, "last_turn_open_text", tx.get("last_turn_open_class")))
        named = f" ({OPENER_NAMES[cls]})" if cls in OPENER_NAMES else ""
        return "turn", (f"{src}, then a turn opened {fmt_age(at_)} ago with no prompt{named}: "
                        f"{_turn_text(tx, key)}")
    if asst and asst - at > KEPT_WORKING_MS and not pos:
        after = "coord's finish time" if by == "coord" else "its finish mark"
        return "work", f"kept working {(asst - at) / 60000:.0f}m after {after} and its final message is not a closeout"
    return None


def _in_band(tx, at):
    """Why the last turn sits inside coord's stamp band (more than 1 s before it
    is a finish, more than 1 s after it a revival), or None."""
    for t, key, what, how in ((tx.get("last_prompt"), "last_prompt_text", "then prompted", ""),
                              (tx.get("last_turn_open"), "last_turn_open_text", "then a turn opened", " with no prompt")):
        if t and at - BAND_MS < t <= at + BAND_MS:
            return (f"coord finished {_age(at)} ago, {what} {fmt_age(t)} ago{how}, inside that stamp's 1 s band: "
                    f"{_turn_text(tx, key)}")
    return None


def finish_state(row, rp, tx, now_ms, hits=None):
    """The one finish rule both screens read (plan
    2026-11-07-triage-sessions-every-page-run-defects-aftercare-veto-reader-revival-and-concurrent-passes
    Phase 6), or None when the row carries no finish signal: no runner mark
    (`row` finished / finishedAt / finishReason) and coord's session_status (`rp`
    sessionStatus / sessionStatusSince) not finished. `tx` is transcript_facts'
    dict (None reads as no turn and no final message); `hits` is phrase_hits of
    its final message when the caller already has it. Returns a Finish.

    The finish time is coord's sessionStatusSince where usable (status_since; a
    stamp later than `now_ms` is not, because it proves coord's clock runs ahead
    of this one and closing is the unsafe direction), else the runner mark's
    finishedAt. The states, in the order they are decided:

      unconfirmed   coord says finished with no usable time, and no runner mark
                    settles it: there is none, its finishedAt is unusable, or
                    only work past it reopened it. Nothing says that finish is
                    wrong, and nothing dates it. (A prompt or turn opener after a
                    TIMED mark is `revived` instead, by="marker": condition 4
                    cannot be ruled out against an untimed coord finish.)
      revived       after the finish time a real prompt arrived, or any record
                    that opens a turn (last_turn_open: a Session Bus delivery, a
                    /loop or ScheduleWakeup wake, a task notification), or
                    assistant work ran more than 10 minutes past it with no
                    closeout. More than 1 s after the stamp, for coord's time and
                    the runner mark alike (served finished-criteria condition 4).
      in_band       a prompt or turn-opening record within 1 s either side of
                    coord's stamp (the runner mark's band is one-sided). close
                    keeps the tab (REVIVED); nudge holds it (HELD).
      contradicted  none of those, but the final message carries a well-formed
                    No or a work-remains phrase (phrase_hits' negatives), or
                    coord's readable session_status is not finished while the
                    runner mark says finished. close never closes it; nudge sends
                    /next-move, which reconciles the two signals.
      finished      none of the above.

    `rule` names the screen rule that holds a finish its structured signal
    states (plan 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run
    Phase 2): phrase_veto or coord_status (contradicted), kept_working
    (revived by work alone), in_band. A revival by a prompt or by a turn opener
    has none: the session was reopened, not held. classify_row adds it to the
    row only when the finish signal was positively read (stranded_line).
    `untimed_mark` tags an unconfirmed finish where coord serves no usable
    time and the runner mark has no usable finishedAt either; classify_row
    reads it UNKNOWN. It is not a stranded code: a missing time is an unknown
    input, not a detector hit."""
    row, rp, tx = row or {}, rp or {}, tx or {}
    status = rp.get("sessionStatus")
    marked = bool(row.get("finished"))
    if not marked and status != "finished":
        return None
    if hits is None:
        hits = phrase_hits(tx["final_text"]) if (tx.get("final_text") or "").strip() else ([], [])
    pos, neg = hits
    fa = row.get("finishedAt")
    fa = fa if isinstance(fa, (int, float)) and not isinstance(fa, bool) else 0
    since = status_since(rp, now_ms) if status == "finished" else None
    if since is not None and since > now_ms:
        # The comparison below ASSUMES coord's clock and this box's agree to well
        # under the 1 s band (NTP-synced). It does not measure skew, so it cannot
        # see a skew already absorbed (a past stamp says nothing about skew). A
        # stamp still ahead of this clock proves coord runs ahead, so it is not
        # compared with local times, for either screen.
        since = None
    at, by = (since, "coord") if since is not None else (fa, "marker")
    if status == "finished" and since is None:
        untimed = "coord session_status=finished (no finish time to compare a revival against)"
        mat = marker_at(row) if marked else None
        if marked and mat is None:
            # A runner mark with no usable finishedAt beside coord's untimed finish: no
            # time on either side dates any turn, so a reopening can be neither claimed
            # nor ruled out. Unconfirmed, tagged untimed_mark: close reads it UNKNOWN
            # (never closed), nudge holds it.
            return Finish("unconfirmed", None, None,
                          [untimed, "its runner finish mark has no usable time either, so no turn can be dated "
                                    "against either finish"], "untimed_mark")
        reopened = _reopened(tx, mat, "marker", pos) if marked else None
        if reopened and reopened[0] in ("prompt", "turn"):
            # A prompt or turn opener after the runner mark, and coord's finish carries no
            # time to weigh it against: whether coord's finish came after that turn is
            # UNKNOWN, so served finished-criteria condition 4 (a turn newer than the
            # finish keeps it open) cannot be ruled out. Revived, whatever the reply
            # says: close keeps the tab (REVIVED), nudge sends /next-move.
            return Finish("revived", mat, "marker",
                          [f"coord session_status=finished with no finish time, and the session was reopened after "
                           f"its runner finish mark: {reopened[1]}"])
        if not marked or reopened:
            return Finish("unconfirmed", None, None, [untimed] + ([reopened[1]] if reopened else []))
    reopened = _reopened(tx, at, by, pos)
    if reopened:
        what, why = reopened
        if pos and by == "coord":
            why = [why, "its reply closes out, but coord's finish time predates that " + what]
        elif pos:
            why = [why, "then closed out again in its final reply: " + pos[0]]
        else:
            why = [why]
        return Finish("revived", at, by, why, "kept_working" if what == "work" else None)
    band = _in_band(tx, at) if by == "coord" else None
    if band:
        return Finish("in_band", at, by, [band], "in_band")
    reason = row.get("finishReason")
    signal = (f"coord session_status=finished ({_age(at)} ago)" if by == "coord" else
              f"runner finished marker ({_age(at)} ago" + (f": {reason[:70]}" if reason else "") + ")")
    against = (["the final message says work remains (" + neg[0] + ")"] if neg else []) + (
        [f"coord session_status reads {status}, not finished (finished-criteria condition 1)"]
        if by == "marker" and isinstance(status, str) and status and status != "finished" else [])
    if against:
        return Finish("contradicted", at, by, [f"{signal}, but {against[0]}"] + [f"and {x}" for x in against[1:]],
                      "phrase_veto" if neg else "coord_status")
    return Finish("finished", at, by, [f"{signal}, no prompt since"])


# A finished session that a screen rule holds (plan
# 2026-11-07-triage-sessions-ten-gaps-from-an-every-page-act-run, Phase 2): the
# row's structured finish signal was READ (a runner mark with a usable
# finishedAt, or coord session_status=finished with a usable time), yet a screen
# rule keeps it off the closable set. No verdict word changes (CONTRADICTED and
# REVIVED stay what they are); the row gains one reason line
# `finish_stranded.<rule>: <what holds it>` - the `<prefix>.<reason>` shape of
# the pass ledger's other reason codes - and screen_all lifts the code onto the
# row as `reason_code` (None when uncoded). An input that could not be read
# fires no code: an unreadable transcript or screen never reaches a coded arm
# (UNKNOWN / BUSY first), a mark with no usable time and coord's finish with no
# time (unconfirmed) code nothing, and screen_all drops every code when the
# readiness read failed (coded_row).
STRANDED_CODES = ("phrase_veto", "kept_working", "coord_status", "in_band")
STRANDED_HOLDS = {
    "phrase_veto": "a finish its own final message contradicts (work remains): never closed",
    "coord_status": "a runner mark coord's readable session_status does not confirm: the mark alone never closes it",
    "kept_working": "it kept working past its finish with no new closeout: kept open, not closed",
    "in_band": "a turn inside coord's 1 s stamp band: close keeps the tab and nudge holds it",
}
STRANDED_RE = re.compile(r"^(finish_stranded\.(?:" + "|".join(STRANDED_CODES) + r")): ")


def marker_at(row):
    """The runner finish mark's time when the mark is set and its finishedAt is
    a positive number, else None. Pure."""
    fa = (row or {}).get("finishedAt")
    if (row or {}).get("finished") and isinstance(fa, (int, float)) and not isinstance(fa, bool) and fa > 0:
        return fa
    return None


def stranded_line(fs, row):
    """The `finish_stranded.<rule>: ...` reason line for a finish a screen rule
    holds, or None: no rule, or a finish signal not positively read (a runner
    mark with no usable time). Pure."""
    if fs is None or fs.rule not in STRANDED_CODES or (fs.by == "marker" and marker_at(row) is None):
        return None
    return f"finish_stranded.{fs.rule}: {STRANDED_HOLDS[fs.rule]}"


def stranded_code(reasons):
    """The finish_stranded code a reason list carries, or None. Pure."""
    for w in reasons or []:
        m = STRANDED_RE.match(w) if isinstance(w, str) else None
        if m:
            return m.group(1)
    return None


def coded_row(why, ready_err):
    """(reason_code, reasons) for a screened row. Pure. A failed readiness read
    is an unknown input, so it fires no detector: the code and its line are both
    dropped."""
    if ready_err:
        return None, [w for w in why if not (isinstance(w, str) and STRANDED_RE.match(w))]
    return stranded_code(why), why


CUSTODY_TIMEOUT_S = 10


def git_rev_parse_dirs(cwd, timeout=CUSTODY_TIMEOUT_S):
    """`git -C <cwd> rev-parse --git-dir --git-common-dir` as (exit code, stdout,
    stderr), under git_scope_env so an inherited GIT_DIR cannot answer for
    another repository; None when git could not be run or did not finish. The
    selftests stub this, so they never run git against a real repository."""
    try:
        p = subprocess.run(["git", "-C", cwd, "rev-parse", "--git-dir", "--git-common-dir"], capture_output=True,
                           text=True, encoding="utf-8", errors="replace", timeout=timeout,
                           env=git_scope_env())
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    return p.returncode, p.stdout, p.stderr


def custody_refusal(cwd):
    """Served finished-criteria condition 3, as far as this screen can read it:
    None when the session's directory is in a LINKED worktree (its git dir is not
    the common one), the trees /next-move's custody probe reads; else why the
    close is withheld. A primary checkout mixes peers' state, so it is never
    judged clean, and neither is a directory in no repository. No directory
    recorded, one that is gone, or a git that does not answer is UNKNOWN, and an
    UNKNOWN withholds the close the same way."""
    rule = "(finished-criteria condition 3)"
    if not cwd:
        return f"custody UNKNOWN: the transcript records no working directory {rule}"
    got = git_rev_parse_dirs(cwd)
    if got is None:
        return f"custody UNKNOWN: git did not answer for {cwd} {rule}"
    code, out, err = got
    if code != 0:
        if "not a git repository" in (err or "").lower():
            return f"custody: not a linked worktree {rule}: {cwd} is in no repository"
        gone = "" if os.path.isdir(cwd) else " (the directory no longer exists)"
        return f"custody UNKNOWN: git rev-parse exited {code} for {cwd}{gone} {rule}"
    lines = [x.strip() for x in (out or "").splitlines() if x.strip()]
    if len(lines) != 2:
        return f"custody UNKNOWN: git rev-parse printed {len(lines)} line(s) for {cwd}, not 2 {rule}"
    git_dir, common = (os.path.normcase(os.path.realpath(os.path.join(cwd, x))) for x in lines)
    if git_dir == common:
        return f"custody: not a linked worktree {rule}: {cwd} is a primary checkout"
    return None


def classify(term, row, table, idle_min, own_tid, ready, strays=None):
    """Return (verdict, reasons[]). The verdicts are VERDICTS; CLOSABLE are the
    ones --close acts on.
    A proven-stray wait loop (split_strays) is not work: it never makes a row
    BUSY and never finishes one; it only adds an `N stray wait loop(s)` note to
    whatever verdict the row reaches. Pass a list as `strays` to receive them as
    (pid, class, why)."""
    strays = [] if strays is None else strays
    verdict, why = classify_row(term, row, table, idle_min, own_tid, ready, strays)
    return verdict, why + ([f"{len(strays)} stray wait loop(s)"] if strays else [])


def classify_row(term, row, table, idle_min, own_tid, ready, strays):
    """classify()'s verdict, with the pane's stray wait loops appended to `strays`."""
    if term["id"] == own_tid:
        return "SELF", ["the terminal running this command"]
    if row is None:
        return "SHELL", ["no Claude session registered for this terminal"]
    finished = bool(row.get("finished"))
    if not term.get("isAlive") and (table is None or term.get("pid") not in table):
        return ("EXITED-FINISHED" if finished else "EXITED"), ["the pane's process has exited"]
    if table is None:
        return "BUSY", ["no process table (neither /proc nor Windows CIM answered): "
                        "cannot prove no tool is running"]
    if term.get("pid") not in table:
        return "UNKNOWN", [f"the pane's pid {term.get('pid')} is not in the process table"]
    claudes, shells, other = proc_facts(table, term["pid"])
    if not claudes:
        if other:
            return "UNKNOWN", ["no claude process, but the pane runs: " + " ".join(table[other[0]][1])[:80]]
        return ("EXITED-FINISHED" if finished else "EXITED"), ["claude has exited; only a shell is left"]
    live = live_session_ids(claudes)
    if live and row.get("claudeSessionId") not in live:
        return "UNKNOWN", [f"claude now runs session {sorted(live)[0][:8]}, not the registry's "
                           f"{str(row.get('claudeSessionId'))[:8]} (after /clear?)"]
    binding = [] if live else ["(session binding unverified: " + (
        "Windows' open-handle list is not reachable from the stdlib)" if WINDOWS
        else "claude holds no <session-id>/tasks dir open)")]
    tx = transcript_facts(row)
    if tx is None:
        return "UNKNOWN", ["transcript not found under " + (row.get("configDir") or "~/.claude")]

    work, found, waits = split_strays(table, shells, idle_min)
    strays.extend(found)
    busy = []
    if work:
        busy.append(f"{len(work)} tool shell(s) running under claude (Bash/Monitor/background task)")
        busy += waits
    if tx["pending_tool"]:
        busy.append("last assistant turn has a tool call with no result")
    idle_for = (time.time() * 1000 - (tx["last_activity"] or 0)) / 60000
    if idle_for < idle_min:
        busy.append(f"last turn activity {idle_for:.0f}m ago (< {idle_min}m idle floor)")
    rp = ready.get(row.get("claudeSessionId")) or {}
    wind_down = rp.get("windDown")
    veto = wind_down_veto(wind_down)
    # `record_shell` gives way only when the shells the record can be reporting
    # are all proven strays. Any work, no shell seen at all, or a job that exec'd
    # away the snapshot argv (unseen_work): the veto stands. The
    # runner stops at its first failing rule, so this reason can hide
    # `prompt_not_ready`, which the prompt check below covers, and
    # `transcript_age_unknown`, which `last_activity` covers here.
    if veto and found and not work and tx["last_activity"] and isinstance(wind_down, dict) \
            and wind_down.get("reason") == "record_shell" and not unseen_work(table, term["pid"], claudes, found):
        veto = None
    if veto:
        busy.append(veto)
    raw = raw_tail(term["id"])
    if raw is None:
        busy.append("screen unreadable")
    else:
        if running_turn(ANSI_RE.sub("", raw[-4000:])):
            busy.append("screen shows a running turn ('esc to interrupt' after the last turn end)")
        state, typed = prompt_state(raw, cols=wrap_cols(term))
        if state == "text":
            busy.append(f"text at the input prompt (unsent draft or a dialog): {typed[:50]!r}")
        elif state == "unknown":
            busy.append("input prompt not found on screen")
    coord_finished = rp.get("sessionStatus") == "finished"
    if busy:
        if finished or coord_finished:
            busy.insert(0, "marked finished, but")
        return "BUSY", busy

    if LIMIT_RE.search(tx["final_text"][-300:]):
        return "LIMITED", ["stopped by an account usage limit mid-work: " + tx["final_text"].strip()[-80:]]
    if tx["last_prompt"] and (tx["last_asst"] or 0) < tx["last_prompt"]:
        return "ACTIVE", [f"a prompt from {fmt_age(tx['last_prompt'])} ago has no reply"]
    # A runner mark /unattended set relaxes how the final reply's wording is read
    # (plan 2026-11-08-...-hedge-words Phase 3): a trusted row's hedge, work-remains
    # and promise phrases neither veto it nor contradict its finish. The trust
    # never stands in for coord's finished status (condition 1 below).
    now = time.time() * 1000  # one clock for the trust read and the finish rule
    pos, neg, trusted = reply_reading(row, rp, tx, now)
    note = [trusted] if trusted else []
    # One finish rule for both screens (finish_state, plan Phase 6): coord's finish
    # time where usable, else the runner mark's; a later prompt or any turn-opening
    # record reopens it (served finished-criteria condition 4); a finish the
    # session's own record contradicts is CONTRADICTED, never closed, and the nudge
    # screen sends it /next-move. Served condition 1 then withholds every finish coord
    # does not confirm (`unrecorded`), and condition 3 every one whose directory is
    # not a linked worktree (custody_refusal).
    fs = finish_state(row, rp, tx, now, (pos, [] if trusted else neg))
    held = [line for line in [stranded_line(fs, row)] if line]  # finish_stranded.<rule> (plan Phase 2)
    if fs is not None and fs.state == "contradicted":
        return "CONTRADICTED", fs.why + binding + held
    if neg and not trusted:
        return "ACTIVE", ["final message says work remains: " + neg[0]]
    unrecorded = coord_unfinished(rp)
    footer = FOOTER_RE.search(tx["final_text"][-2500:])
    stated = footer_nothing_owed(tx["final_text"][-2500:]) if footer else None
    if fs is None:
        # No finish signal at all: the reply's own words, which coord has not recorded.
        if pos:
            return "CLOSEOUT", ["closeout phrase without a finished marker: " + pos[0], unrecorded] + binding
        if stated:
            return "FOOTER-FINISHED", [f"footer final state, no marker: {stated[:90]!r}", unrecorded] + binding
        if footer:
            return "FOOTER-ONLY", ["only a POLICY_COMPLIANCE footer - printed every turn, not proof of finish"]
        return "ACTIVE", ["no finished marker, no closeout phrase"]
    if fs.state == "revived" and fs.by == "marker" and pos and unrecorded:
        # Revived after its runner mark, then closed out again in its final reply: on a
        # runner build predating plan Phase 3 a re-finish leaves finishedAt where it
        # was, so only coord can record it (condition 1). The mark is the finish time
        # only while coord does not read finished, so `unrecorded` is set here.
        return "CLOSEOUT", fs.why + [unrecorded] + binding
    if fs.state in ("revived", "in_band"):
        return "REVIVED", fs.why + held
    if fs.state == "unconfirmed":
        # Coord says finished with no usable time: the coord-only rule, where the
        # final reply's own closeout or nothing-owed footer is the evidence. A runner
        # mark with no usable time beside it (finish_state's untimed_mark) dates
        # nothing: UNKNOWN, never closed. Nothing reopened it, so it is not REVIVED.
        if fs.rule == "untimed_mark":
            return "UNKNOWN", fs.why + binding
        if pos:
            finish = ["coord session_status=finished, closeout phrase: " + pos[0]] + fs.why[1:]
        elif stated:
            finish = [f"coord session_status=finished, footer final state: {stated[:90]!r}"] + fs.why[1:]
        elif footer:
            return "FOOTER-ONLY", ["only a POLICY_COMPLIANCE footer - printed every turn, not proof of finish"]
        else:
            return "REVIVED", ["coord says finished, but the final message is not a closeout"] + fs.why[1:]
    elif unrecorded:
        # The runner mark settles the finish, but the runner serves no coord status: UNKNOWN.
        return "UNKNOWN", fs.why + note + [unrecorded] + binding
    else:
        finish = fs.why
    custody = custody_refusal(tx.get("cwd"))
    if custody:
        return "CUSTODY", finish + note + [custody] + binding
    return "FINISHED", finish + note + binding


def screen_one(term, row, table, idle_min, own, ready, strays=None):
    """classify(), plus the slow output-moving sample for rows that would close."""
    verdict, why = classify(term, row, table, idle_min, own, ready, strays)
    if verdict in CLOSABLE - {"EXITED-FINISHED"}:
        moving = output_moving(term["id"])
        if moving is not False:
            verdict = "BUSY"
            why = ["terminal output still moving" if moving else "terminal output could not be sampled"] + why
    return verdict, why


def screen_all(idle_min, only):
    """(rows, warnings, ready_err): one verdict row per terminal, what was
    degraded, and why readiness could not be read (None when it could)."""
    terms = terminals()
    rows = open_rows()
    pages = page_names()
    table = proc_table()
    ready, ready_err = readiness()
    own = os.environ.get("QONTINUI_TERMINAL_ID", "")
    warnings = []
    if ready_err:
        warnings.append(f"runner readiness unreadable ({ready_err[:160]}): coord session status and the "
                        "runner's screen and sideband vetoes were NOT used, so --close is refused")
    if table is None:
        warnings.append("no process table (neither /proc nor Windows CIM answered): every live session "
                        "reads BUSY, so nothing is closable here")
    if not pages:
        warnings.append("tab names unreadable (the runner's page list is only read from WebKitGTK storage)")
    if only:
        terms = [t for t in terms if any(t["id"].startswith(o) for o in only)]
    out = []
    for t in terms:
        row = with_live_config_dir(rows.get(t["id"]), table, t.get("pid"))
        found = []
        verdict, why = screen_one(t, row, table, idle_min, own, ready, found)
        code, why = coded_row(why, ready_err)
        acct = account_of(row)
        out.append({
            "terminalId": t["id"], "verdict": verdict, "reasons": why, "reason_code": code,
            "strays": [{"pid": p, "class": c, "why": w} for p, c, w in found],
            "name": (row or {}).get("resumeName") or t.get("title") or "",
            "title": t.get("title") or "", "account": acct,
            "tab": pages.get(t.get("pageId") or "default") or f"page {str(t.get('pageId'))[:8]}",
            "claudeSessionId": (row or {}).get("claudeSessionId"),
            "finishedAt": (row or {}).get("finishedAt"),
            "sessionStatus": (ready.get((row or {}).get("claudeSessionId")) or {}).get("sessionStatus"),
            "sessionStatusSince": (ready.get((row or {}).get("claudeSessionId")) or {}).get("sessionStatusSince"),
        })
    return out, warnings, ready_err


# The report's order: the CLOSABLE verdicts first, then the finishes coord does not confirm.
ORDER = ["FINISHED", "EXITED-FINISHED", "CUSTODY", "CLOSEOUT", "FOOTER-FINISHED", "CONTRADICTED", "REVIVED",
         "FOOTER-ONLY", "BUSY", "LIMITED", "ACTIVE", "EXITED", "UNKNOWN", "SHELL", "SELF"]


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
        print(f"  {r['terminalId'][:8]}  {r['tab']:<14.14}  {r['account']:<13.13}  {r['name'][:60]}")
        print(f"            - {'; '.join(r['reasons'])[:200]}")
        for s in r.get("strays") or []:  # what a --close of this row would end if it outlives the pane
            print(f"            - stray pid {s['pid']} ({s['class']}): {s['why'][:160]}")
    print_stranded(res)


STRANDED_HEADER = "== FINISHED, HELD BY A SCREEN RULE (not closed)"


def print_stranded(res):
    """The rows whose structured finish signal was read but a screen rule holds
    (reason_code set), each with its code, what holds it and the reason that
    states the finish. Printed even when empty, so a zero is a read zero.
    triage-sessions.py prints the same section over its rows, which carry the
    nudge verdict, so the close screen's verdict is read from `screen`."""
    held = [r for r in res if r.get("reason_code")]
    print(f"\n{STRANDED_HEADER}: {len(held)}")
    for r in held:
        v = r.get("screen") or r["verdict"]
        first = next((w for w in r.get("reasons") or [] if isinstance(w, str) and not STRANDED_RE.match(w)), "")
        print(f"  {r['terminalId'][:8]}  {r.get('tab', ''):<14.14}  {r.get('account', ''):<13.13}  "
              f"{r.get('name', '')[:60]}")
        print(f"            - [{v}] {r['reason_code']}: {first[:180]}")


def close_one(tid, idle_min, own, allowed):
    """Re-screen, then DELETE /terminals/{id}, then read back. Returns (ok, msg)."""
    t = next((x for x in terminals() if x["id"] == tid), None)
    if t is None:
        return False, "terminal no longer listed (already closed?)"
    ready, err = readiness()
    if err:
        return False, f"readiness unreadable at re-screen ({err[:80]})"
    table = proc_table()
    found = []
    row = with_live_config_dir(open_rows().get(tid), table, t.get("pid"))
    verdict, why = screen_one(t, row, table, idle_min, own, ready, found)
    if verdict not in allowed:
        return False, f"re-screen now says {verdict}: {'; '.join(why)}"
    claudes = proc_facts(table, t["pid"])[0] if table and t.get("pid") in table else []
    # The runner's pane kill reaches a stray background shell by neither route:
    # the in-process pane SIGTERMs the root pid alone, the PTY holder signals
    # the PTY child and its group, and a background shell leads its own session
    # and group (pid = pgid = sid), so no hangup reaches it either. Record each
    # stray now; only these, still stray and unchanged, are ever signalled below.
    recorded = [(p, proc_start(p), table[p][1], cls) for p, cls, _ in found]
    sent = "sent"
    try:
        req = urllib.request.Request(f"{API}/terminals/{tid}", method="DELETE")
        urllib.request.urlopen(req, timeout=60).read()
    except urllib.error.HTTPError as e:
        return False, f"DELETE answered {e.code}: {e.read()[:200]!r}"
    except Exception as e:  # the runner may have acted anyway: the read-back decides
        sent = f"errored ({type(e).__name__}: {e})"
    listed, alive = None, claudes
    for _ in range(20):
        time.sleep(0.5)
        try:
            listed = any(x["id"] == tid for x in terminals())
        except READ_ERRORS:
            listed = None
        alive = [p for p in claudes if pid_alive(p)]
        if listed is False and not alive:
            return True, f"DELETE {sent}; read back: tab gone from /terminals, claude process gone" + \
                end_strays(recorded, idle_min)
    return False, f"DELETE {sent}; read back: listed={listed}, claude pids still alive={alive}"


def stray_now(pid, start, argv):
    """Whether `pid` is still the recorded process: same start time and argv (a
    reused pid or a zombie is not)."""
    try:
        av = [a for a in open(f"/proc/{pid}/cmdline", "rb").read().decode(errors="replace").split("\0") if a]
    except OSError:
        return False
    return start is not None and proc_start(pid) == start and av == argv


def still_stray(pid, start, argv, cls, idle_min):
    """Why `pid` may NOT be signalled, or None when it may: it must be the
    recorded process (start time and argv) AND, on a fresh process table, still
    a stray of the same class. A loop whose condition flipped moves on to its
    post-loop commands in the same process, with the same argv and start time,
    so identity alone would not tell that work from the loop. Returns
    (reason, table)."""
    if not stray_now(pid, start, argv):
        return "gone", None
    table = proc_table()
    if not table or pid not in table:
        return "process table unreadable at re-check", None
    stray, now_cls, why = stray_verdict(table, pid, ProcFacts(table), idle_min)
    if not stray or now_cls != cls:
        return f"no longer a {cls} stray ({why[:80]})", table
    return None, table


def _signal_pid(pid, start, argv, sig):
    """Send `sig` to `pid` only while it is still (start, argv): through a pidfd
    where the OS has one, so a pid reused after the check cannot be hit."""
    import signal
    fd = None
    if hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"):
        try:
            fd = os.pidfd_open(pid)
        except OSError:
            return False
    try:
        if not stray_now(pid, start, argv):
            return False
        if fd is not None:
            signal.pidfd_send_signal(fd, sig)
        else:
            os.kill(pid, sig)
        return True
    finally:
        if fd is not None:
            os.close(fd)


def _group_is_the_loop(table, pid):
    """Whether every member of pid's process group is pid itself, a fork of it,
    or one of its own sleeps and probes, so that signalling the group ends
    nothing but the loop."""
    allowed = {"sleep", "ps", "cat", "tail", "head"} | set(PROBES)
    mine = set(subtree(table, pid))
    for p, (_, av) in table.items():
        st = proc_stat(p)
        if not st or st[2] != str(pid):
            continue
        if p not in mine or not (p == pid or av == table[pid][1] or (av and exe_base(av[0]) in allowed)):
            return False
    return True


def end_strays(recorded, idle_min, wait_s=5.0):
    """The close message's stray suffix. Each recorded stray of this pane that
    outlived the close is ended (Linux only), and only while it is still the
    recorded process AND still a stray of its recorded class on a fresh process
    table, checked again before every signal: SIGTERM, then SIGKILL after
    `wait_s`. The signal goes to its process group only when that group holds
    nothing but the loop and its own sleeps and probes, else to the pid alone."""
    import signal
    out, gone = [], 0
    for pid, start, argv, cls in recorded:
        if WINDOWS or not stray_now(pid, start, argv):
            gone += 1
            continue
        why, sent = "still alive after SIGKILL", False
        try:
            for sig, wait in ((signal.SIGTERM, wait_s), (signal.SIGKILL, 2.0)):
                refusal, table = still_stray(pid, start, argv, cls, idle_min)
                if refusal == "gone":
                    break
                if refusal:
                    why = refusal
                    break
                if _group_is_the_loop(table, pid) and os.getpgid(pid) == pid:
                    os.killpg(pid, sig)  # a group id is not reused while its group exists
                elif not _signal_pid(pid, start, argv, sig):
                    break
                sent = True
                deadline = time.time() + wait
                while time.time() < deadline and stray_now(pid, start, argv):
                    time.sleep(0.1)
                if not stray_now(pid, start, argv):
                    break
        except OSError as e:
            why = f"{type(e).__name__}: {e}"
        if not stray_now(pid, start, argv) and not sent:
            gone += 1  # it went by itself before anything was sent
            continue
        out.append(f"stray pid {pid} outlived the pane close; "
                   + ("ended" if not stray_now(pid, start, argv) else f"could not end: {why}"))
    if gone:
        out.insert(0, f"{gone} stray wait loop(s) gone with the pane")
    return "".join("; " + s for s in out)


def build_parser():
    """This script's argument parser. triage-sessions.py's selftest parses the
    argv its close step builds with it, so a flag this script does not take fails
    that selftest instead of the first acting pass."""
    ap = argparse.ArgumentParser()
    ap.add_argument("--close", action="store_true")
    ap.add_argument("--idle-min", type=int, default=IDLE_MIN)
    ap.add_argument("--only", default="")
    ap.add_argument("--rows-out", metavar="PATH",
                    help="append one JSON line per attempted close to PATH (triage-sessions.py's pass ledger)")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    ap.add_argument("--selftest", action="store_true", help="run the regression table and exit; reads nothing")
    ap.add_argument("--own-tasks", action="store_true",
                    help="list THIS session's background tasks as STRAY or LIVE (reads /proc only); exit 0 none "
                         "live, 1 a live one, 3 UNKNOWN")
    return ap


def main():
    a = build_parser().parse_args()
    # A piped Windows console is cp1252: write UTF-8 so a name, reason or prompt
    # glyph never dies with UnicodeEncodeError after the work is done.
    for _stream in (sys.stdout, sys.stderr):
        _reconfigure = getattr(_stream, "reconfigure", None)
        if _reconfigure is None:
            continue
        try:
            _reconfigure(encoding="utf-8", errors="backslashreplace")
        except (ValueError, OSError):
            pass
    if a.selftest:
        return selftest()
    if a.own_tasks:
        return own_tasks_main(a.idle_min, a.json)
    if a.port:
        global API
        API = f"http://127.0.0.1:{a.port}"
    only = [o for o in a.only.split(",") if o]
    try:
        res, warnings, ready_err = screen_all(a.idle_min, only)
    except READ_ERRORS as e:
        print(f"UNKNOWN: could not read the runner at {API}: {e}", file=sys.stderr)
        return 3
    if a.json:
        print(json.dumps({"warnings": warnings, "sessions": res}, indent=1))
    else:
        print_report(res, warnings)
    if not a.close:
        return 0
    if ready_err:
        print("\nUNKNOWN: --close refused - GET /restart-readiness could not be read", file=sys.stderr)
        return 3
    want = CLOSABLE
    targets = [r for r in res if r["verdict"] in want]
    print(f"\nclosing {len(targets)} terminal(s)")
    own = os.environ.get("QONTINUI_TERMINAL_ID", "")
    failed = 0
    for r in targets:
        try:
            ok, msg = close_one(r["terminalId"], a.idle_min, own, want)
        except READ_ERRORS as e:
            ok, msg = False, f"UNKNOWN, not closed: {e}"
        failed += not ok
        print(f"  {'CLOSED ' if ok else 'SKIPPED'} {r['terminalId'][:8]}  {r['tab']:<14.14}  "
              f"{r['account']:<13.13}  {r['name'][:50]} - {msg}")
        if a.rows_out:
            append_rows_out(a.rows_out, {"terminal_id": r["terminalId"], "action": "close",
                                         "action_outcome": close_outcome(ok, msg),
                                         "detail": {"screened_as": r["verdict"], "message": msg[:300]}})
    return 1 if failed else 0


# close_one starts every message it returns after the DELETE went out with this:
# "DELETE answered <code>" (the runner refused it), "DELETE sent; read back ..."
# and "DELETE errored (...); read back ...".
DELETE_SENT = "DELETE "


def close_outcome(ok, msg):
    """The pass-ledger outcome of one close_one result: done once the close
    read back; failed when the DELETE reached the runner and was refused or
    did not read back; skipped when the re-screen held the row before it."""
    if ok:
        return "done"
    return "failed" if msg.startswith(DELETE_SENT) else "skipped"


def append_rows_out(path, row):
    """Append one acted row to a --rows-out file as one JSON line. A write that
    fails is warned about and never raised: the record must not change what
    the act did."""
    try:
        with open(path, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, separators=(",", ":")) + "\n")
    except OSError as e:
        print(f"WARNING: --rows-out {path}: row for {str(row.get('terminal_id'))[:8]} not written ({e})",
              file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())
