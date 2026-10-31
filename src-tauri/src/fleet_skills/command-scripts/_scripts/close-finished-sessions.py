#!/usr/bin/env python3
"""Screen the runner's terminal sessions for finished ones; optionally close them.

Usage: close-finished-sessions.py [--close] [--idle-min N]
                                  [--only ID[,ID..]] [--json] [--port N]

The report is always printed. --close then closes the FINISHED and
EXITED-FINISHED, CLOSEOUT and FOOTER-FINISHED rows,
re-screening each one immediately before its close and reading the close back.
Read-only unless --close. Stdlib only. The command /close-finished-sessions
documents every verdict and the evidence it rests on.

Exit codes: 0 done (every attempted close verified), 1 a close was skipped or
did not verify, 3 UNKNOWN: the runner could not be read (nothing screened), or
--close was refused because /restart-readiness could not be read.
"""
import argparse
import datetime as dt
import glob
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

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))
from envelope import EnvelopeUnknown, load  # noqa: E402

API = "http://127.0.0.1:" + (os.environ.get("QONTINUI_RUNNER_API_PORT") or "9876")
READ_ERRORS = (urllib.error.URLError, OSError, ValueError, EnvelopeUnknown)

# ---------------------------------------------------------------- phrases
# Closeout PHRASES are a screen, not a verdict: a close rests on a structured
# finished signal (the runner marker or coord's session_status) plus the
# idleness proof. A phrase is load-bearing only where no structured finish time
# exists - a coord-only finish, a session revived and then closed out again, and
# the phrase-only CLOSEOUT rows - so it is matched against a WHITELIST of
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
    r"uncommitted|unpushed|unverified|unreachable|dirty|failed|continu\w*|by mistake|mistakenly)\b|n't\b|\?", re.I)


def sentence_at(text, start):
    """The rest of the sentence that begins at `start`."""
    return re.split(r"(?<=[.!?])\s|\n", text[start:start + 400], maxsplit=1)[0]


# One clause of the report after the phrase. The report's first sentence must be
# a list of these, so "2 trees checked, 1 has work only on disk" and "yes;
# operator approval is outstanding" are not a yes: their second clause is not
# one of the report clauses.
SAFE_CLAUSE = (
    r"(?:yes"
    r"|\d+ (?:repository )?(?:trees?|worktrees?) (?:checked|touched|allocated|edited)(?: or (?:edited|touched))?"
    r"(?: by this session)?(?: \([^()\n]*\))?"
    r"|no (?:allocated )?(?:worktrees?|trees?|repository trees?|working trees?|commits)(?: (?:were|was))?"
    r"(?: (?:allocated|touched|edited|checked|created)(?: or (?:edited|touched))?| to hand off)?(?: by this session)?"
    r"|nothing (?:left |of this session's )?(?:exists |is )?(?:left )?only on (?:this machine's )?disk"
    r"|no work (?:exists |is )?(?:left )?only on (?:this machine's )?disk"
    r"|nothing to hand off"
    r"|hand-?off declared for \d+(?: \([^()\n]*\))?"
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
    r"(?![ \t]+(?:allocated[ \t]+)?(?:worktrees?|trees?|repository|working|commits|hand-?offs?)\b)"
    r"(?![ \t]+work (?:exists |is )?(?:left )?only on (?:this machine's )?disk\b))\b)", re.I)
# The line before a mention introduces a quotation of the format, not a report.
QUOTED_EXAMPLE = re.compile(r"(?:format|example|e\.g\.|template|looks like|such as)[^\n]*:[ \t]*\n"
                            r"(?:[ \t]*\n)*$", re.I)
DISK_LEFT = re.compile(r"\bonly on (?:this machine's )?disk\b", re.I)
NO_WORK_BEFORE_DISK = re.compile(r"\bno work (?:exists |is )?(?:left )?$", re.I)
# The label form `**Safe to close?** SAFE TO CLOSE - <report>`: a bare "Safe to close?" opening its line and
# followed at once by a FULL yes - the bare headline ending its sentence, or the headline and a report the
# grammar accepts - is a label, not a question, and is dropped. Anything else keeps its `?`, so
# "Safe to close? Unclear", "Safe to close? Safe to close, I think." and "Safe to close? NOT SAFE TO CLOSE" veto.
SAFE_LABEL = re.compile(r"(?m)^([ \t>-]*)safe[ _-]to[ _-]close\?[ \t]+(safe[ _-]to[ _-]close)\b([^\n]*)$",
                        re.I)


def _drop_safe_label(m):
    rest = m.group(3)
    sep = re.match(r"[ \t]*[:=\u2014\u2013-][ \t]*", rest)
    report = sentence_at(rest, sep.end()) if sep else ""
    full_yes = (re.fullmatch(r"[ \t]*[.!]?[ \t]*", rest) or re.match(r"[ \t]*[.!][ \t]", rest)
                or (sep and SAFE_REPORT_LINE.match(report)
                    and not (ZERO_CHECKED.search(report) and not NO_TREES.search(report))))
    # The rest of the line must not take it back ("Safe to close? Safe to close. Not really though.").
    full_yes = full_yes and not (HEDGE.search(rest) or neg_hits(rest))
    return m.group(1) + m.group(2) + rest if full_yes else m.group(0)


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
    for m in SAFE_RE.finditer(text):
        line_start = text.rfind("\n", 0, m.start()) + 1
        before = text[line_start:m.start()]
        line = text[m.end():].split("\n", 1)[0]
        whole = before + m.group(0) + sentence_at(line, 0)  # the mention's own sentence
        disk = [d for d in DISK_LEFT.finditer(whole)
                if "nothing" not in whole[max(0, d.start() - 45):d.start()].lower()
                and not NO_WORK_BEFORE_DISK.search(whole[:d.start()])]
        if (SAFE_NEG_BEFORE.search(text[max(0, m.start() - 60):m.start()]) or SAFE_NEG_AFTER.match(line)
                or HEDGE.search(whole) or disk):
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
            if HEDGE.search(following) or neg_hits(following):
                no = True
            else:
                yes = True
            continue
        after = re.match(r"[ \t]*[.!][ \t]", line)
        if after:  # "Safe to close. <the rest of its line>": that rest must not take it back
            rest = line[after.end():]
            if HEDGE.search(rest) or neg_hits(rest):
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
]
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


def neg_hits(text):
    """The NEGATIVE labels found in `text`, with a recorded next step masked for "states the next step"."""
    return [label for label, p in NEG_RE
            if p.search(_mask_recorded_next(text) if label == "states the next step" else text)]
POS_RE = [(label, re.compile(p, re.I)) for label, p in POSITIVE]
NEG_RE = [(label, re.compile(p, re.I)) for label, p in NEGATIVE]
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
    for label, p in POS_RE:
        for m in p.finditer(tail):
            # The phrase may itself contain a hedge word ("needs no follow-up").
            if not HEDGE.search(sentence_at(tail, m.start()).replace(m.group(0), " ")):
                pos.append(label)
                break
    neg = (["not safe to close"] if no else []) + neg_hits(tail[-1000:]) + promise_hits(bare[-1000:])
    if re.search(r"\bsession finished[ \t]*[:.\u2014\u2013-]?[ \t]*(?:no|not)\b", tail, re.I):
        neg.append("session finished: no")
    # The policy footer follows the real ending; a question sits above it. Cut at
    # the LAST footer heading, then drop rules and list markers before testing.
    heads = list(FOOTER_HEAD_RE.finditer(text))
    body = text[:heads[-1].start()] if heads else text
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
]


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
    empty = "\x1b[H\r\x1b[17B\u276f\xa0\r\x1b[50C\x1b[2Bfor agents\x1b[K\x1b[20;1H\x1b[18;3H\x1b[?25h"
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
        if prompt_state(raw)[0] != want:
            bad.append(f"prompt_state {raw!r} expected {want}, got {prompt_state(raw)}")
    # ConPTY (Windows) writes a border that fills the width with no line break, so
    # the marker after it is on the next row only once the replay auto-wraps.
    wrapped = ("\x1b[H" + "─" * 10 + "❯\xa0\x1b[K\r\n" + "─" * 10 + "  status\x1b[K"
               "\x1b[2;3H\x1b[?25h")
    for raw, cols, want in [(wrapped, 10, "empty"), (wrapped.replace("❯\xa0", "❯\xa0draft"), 10, "text"),
                            (wrapped, None, "text"),
                            # five double-width characters fill the same 10 columns
                            (wrapped.replace("─" * 10 + "❯", "界" * 5 + "❯", 1), 10, "empty")]:
        if prompt_state(raw, cols)[0] != want:
            bad.append(f"prompt_state cols={cols} {raw!r} expected {want}, got {prompt_state(raw, cols)}")
    c = Cursor(1, 1, 10)
    c.feed("\x1b[20Cx")  # cursor-forward stops at the right margin; it never wraps
    if (1, 10) not in c.cells:
        bad.append(f"cursor-forward past the margin must clamp to it: {c.cells}")
    c = Cursor(1, 1)
    c.feed("界x")  # with no width (Linux) every character is one column, as measured
    if (1, 2) not in c.cells:
        bad.append(f"a wide character must take one column when no width is set: {c.cells}")
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
    for line in bad:
        print("SELFTEST FAIL: " + line)
    print(f"selftest: {len(PHRASE_CASES)} phrase cases, {len(WIND_DOWN_CASES)} wind-down reason cases, "
          f"{len(NO_VETO_CASES)} no-veto cases + turn-end, shell "
          f"and prompt cases, {len(bad)} failure(s)")
    return 1 if bad else 0


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


def open_rows():
    """{terminalId: registry row} for the open sessions in the runner registry."""
    return {r["terminalId"]: r for r in read("/sessions/history?limit=5000", "sessions")
            if r.get("state") == "open" and r.get("terminalId")}


def readiness():
    """({claudeSessionId: process row}, None), or ({}, why) when it cannot be read.

    Each row carries coord's `sessionStatus` and the runner's wind-down verdict,
    whose `reason` can veto a close and never approve one (wind_down_veto)."""
    try:
        procs = read("/restart-readiness", "terminal_sessions.processes")
    except READ_ERRORS as e:
        return {}, str(e)
    if not isinstance(procs, list):
        return {}, f"terminal_sessions.processes is {type(procs).__name__}, not a list"
    return {p["sessionId"]: p for p in procs if isinstance(p, dict) and p.get("sessionId")}, None


# The runner's `windDown.reason` words. Runners on builds before and after its
# typed-signal change coexist, so both vocabularies are read: the older build's
# `grid_busy` (its own rendered grid, cursor included) and the newer build's
# reasons from Claude Code's session record, the transcript age and the input
# prompt, which replace it. Each VETO word stops a close. The NO-VETO words name
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
            for p in nums or [0]:
                if p in (0, 22):
                    self.dim = False
                elif p == 2:
                    self.dim = True


def on_background(raw, i):
    """True when the character at `i` is drawn on a background colour. Claude
    Code pins the last sent prompt as a header at the top of the screen, `\u276f`
    and all, on a grey background; the input row has none."""
    anchors = list(ABS_POS_RE.finditer(raw, 0, i))
    bg = False
    for m in SGR_RE.finditer(raw, anchors[-1].start() if anchors else max(0, i - 200), i):
        nums = [int(p) if p.isdigit() else 0 for p in m.group(1).split(";")] if m.group(1) else [0]
        k = 0
        while k < len(nums):
            p = nums[k]
            if p in (0, 49):
                bg = False
            elif 40 <= p <= 47 or 100 <= p <= 107:
                bg = True
            elif p in (38, 48):
                bg = bg or p == 48
                k += 2 if k + 1 < len(nums) and nums[k + 1] == 5 else 4
            k += 1
    return bg


def wrap_cols(term, windows=None):
    """The width prompt_state should auto-wrap at: the pane's columns on Windows,
    whose ConPTY relies on the wrap, and None elsewhere, where the stream was
    measured without it."""
    return term.get("cols") if (WINDOWS if windows is None else windows) else None


def prompt_state(raw, cols=None):
    """('empty' | 'text' | 'unknown', detail) for the input prompt as the screen
    stands at the end of `raw`.

    Claude Code redraws DIFFERENTIALLY: typed characters are written by jumping
    the cursor onto the input row, with no new `\u276f`. So the stream is replayed
    from the last absolute cursor position before the final `\u276f` up to the
    final cursor-show, and the prompt is empty only when BOTH hold:
    1. the visible cursor sits on the `\u276f` row, two columns right of it (an
       empty prompt; a draft leaves it after the text, a multi-line draft on a
       lower row);
    2. no plain (non-dim) character stands right of `\u276f` on its row once the
       replay is done - a draft (even with the cursor sent back by Home), or a
       dialog's selection line. A prompt SUGGESTION or placeholder is drawn dim (SGR 2) and does not
       count."""
    i = raw.rfind("\u276f")
    while i >= 0 and on_background(raw, i):
        i = raw.rfind("\u276f", 0, i)  # the sticky header repeating the last prompt, not the input row
    show = raw.rfind("\x1b[?25h")
    if i < 0 or show < i or raw.rfind("\x1b[?25l") > show:
        return "unknown", "input prompt or visible cursor not found"
    anchors = list(ABS_POS_RE.finditer(raw, 0, i))
    if not anchors:
        return "unknown", "no absolute cursor position before the prompt"
    cur = Cursor(1, 1, cols)
    cur.feed(raw[anchors[-1].start():i])
    mrow, mcol = cur.row, cur.col
    if cols and mcol > cols:
        mrow, mcol = mrow + 1, 1  # a wrap pending at the end of a full row: the marker opens the next one
    cur.feed(raw[i:show])
    want = (mrow, mcol + 2)
    if (cur.row, cur.col) != want:
        return "text", (f"cursor at row {cur.row} col {cur.col}; an empty prompt leaves it at "
                        f"row {want[0]} col {want[1]}")
    # The input can span rows below the marker, down to the box's lower border.
    rows = [mrow]
    for r in range(mrow + 1, mrow + 12):
        if any(ch in "\u2500\u2501" for (rr, _), (ch, _) in cur.cells.items() if rr == r):
            break
        rows.append(r)
    else:
        rows = [mrow]  # no border below: the rows under the marker are status lines, not input
    typed = "".join(ch for (r, c), (ch, dim) in sorted(cur.cells.items())
                    if r in rows and (r > mrow or c > mcol) and not dim and ch > " " and ch not in BOX)
    if typed:
        return "text", typed
    return "empty", ""


BUSY_SCREEN = re.compile(r"esc to interrupt", re.I)
# Claude Code ends a turn with "<star> <Verb>ed for 1m 2s - done 4:42 PM". The
# buffer is a scrollback STREAM, so an old spinner frame precedes every reply;
# only a spinner AFTER the last turn-end line means a turn is running now.
TURN_END = re.compile(r"[\u2700-\u27bf]\s*\w+\s*for\s*(?:\d+h\s*)?(?:\d+m\s*)?\d+s\b"
                      r"|\bdone\s*\d{1,2}:\d{2}\s*[AP]M\b", re.I)
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
    ends = [m.start() for m in TURN_END.finditer(text)]
    return bool(spin and (not ends or spin[-1] > ends[-1]))


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


def proc_facts(table, pid, windows=WINDOWS):
    """(claude_pids, tool_shells, other_pids) under the pane pid. A tool shell is
    the shell Claude Code runs a Bash, PowerShell or Monitor call in, foreground or
    background, so its presence means work is still in flight. On Linux it sources
    a `shell-snapshots/snapshot-` file. On Windows its command line names no such
    file, so there it is any bash or PowerShell that is a direct child of claude.
    MCP stdio servers run under `cmd.exe /c`, so they do not count."""
    pids = subtree(table, pid)
    claudes = [p for p in pids if is_claude(table[p][1])]
    shells = [p for p in pids if any("shell-snapshots/snapshot-" in a for a in table[p][1])
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


# ---------------------------------------------------------------- transcript
def ms(iso):
    return dt.datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() * 1000


def fmt_age(msv):
    if msv is None:
        return "?"
    m = (time.time() * 1000 - msv) / 60000
    return f"{m:.0f}m" if m < 120 else f"{m / 60:.1f}h" if m < 2880 else f"{m / 1440:.1f}d"


def newest_record(path):
    """Timestamp (ms) of the newest user/assistant record in the last 64 KiB of a
    JSONL file, or None."""
    try:
        with open(path, "rb") as f:
            f.seek(max(0, os.path.getsize(path) - 65536))
            lines = f.read().decode(errors="replace").splitlines()
    except OSError:
        return None
    for line in reversed(lines):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("type") in ("user", "assistant") and e.get("timestamp"):
            return ms(e["timestamp"])
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
    # `last_asst` but not the text.
    f = {"path": path, "last_activity": None, "last_prompt": None, "last_prompt_text": "",
         "last_asst": None, "final_text": "", "final_text_at": None, "pending_tool": False,
         "task_prompts": 0}
    open_tools = set()
    before = None  # (last_prompt, last_prompt_text) as they stood before the newest prompt
    for line in open(path, encoding="utf-8", errors="replace"):  # never the locale codepage (cp1252 on Windows)
        try:
            e = json.loads(line)
        except ValueError:
            continue
        ts = e.get("timestamp")
        if not ts or not isinstance(e.get("message"), dict):
            continue
        content = e["message"].get("content")
        if e.get("type") in ("user", "assistant"):
            f["last_activity"] = ms(ts)
        if e.get("type") == "user":
            if isinstance(content, list):
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "tool_result":
                        open_tools.discard(b.get("tool_use_id"))
            if e.get("isMeta") or e.get("isCompactSummary"):
                continue
            text = content if isinstance(content, str) else " ".join(
                b.get("text", "") for b in content or [] if isinstance(b, dict) and b.get("type") == "text")
            if text.lstrip().startswith(LOCAL_OUTPUT):
                if before is not None and f["last_prompt_text"].startswith("<command-name>"):
                    f["task_prompts"] -= not f["last_prompt_text"].startswith(CLEAR)
                    f["last_prompt"], f["last_prompt_text"] = before
                before = None
            elif text.strip() and not text.lstrip().startswith(NON_PROMPT):
                before = (f["last_prompt"], f["last_prompt_text"])
                f["last_prompt"], f["last_prompt_text"] = ms(ts), text.strip()
                f["task_prompts"] += not text.lstrip().startswith(CLEAR)
        elif e.get("type") == "assistant":
            before = None
            f["last_asst"] = ms(ts)
            for b in content if isinstance(content, list) else []:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "text" and b.get("text", "").strip():
                    f["final_text"], f["final_text_at"] = b["text"], ms(ts)
                elif b.get("type") == "tool_use":
                    open_tools.add(b.get("id"))
    f["pending_tool"] = bool(open_tools)
    for sub in glob.glob(os.path.join(os.path.dirname(path), sid, "subagents", "*.jsonl")):
        t = newest_record(sub)
        if t and t > (f["last_activity"] or 0):
            f["last_activity"] = t
    return f


# ---------------------------------------------------------------- screening
# CLOSEOUT and FOOTER-FINISHED rows rest on the final reply's own words plus the idleness proof.
CLOSABLE = {"FINISHED", "CLOSEOUT", "FOOTER-FINISHED", "EXITED-FINISHED"}


def classify(term, row, table, idle_min, own_tid, ready):
    """Return (verdict, reasons[]). Verdicts: SELF, SHELL, UNKNOWN, BUSY, LIMITED,
    REVIVED, FINISHED, CLOSEOUT, FOOTER-FINISHED, FOOTER-ONLY, EXITED-FINISHED, EXITED, ACTIVE."""
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

    busy = []
    if shells:
        busy.append(f"{len(shells)} tool shell(s) running under claude (Bash/Monitor/background task)")
    if tx["pending_tool"]:
        busy.append("last assistant turn has a tool call with no result")
    idle_for = (time.time() * 1000 - (tx["last_activity"] or 0)) / 60000
    if idle_for < idle_min:
        busy.append(f"last turn activity {idle_for:.0f}m ago (< {idle_min}m idle floor)")
    rp = ready.get(row.get("claudeSessionId")) or {}
    veto = wind_down_veto(rp.get("windDown"))
    if veto:
        busy.append(veto)
    raw = raw_tail(term["id"])
    if raw is None:
        busy.append("screen unreadable")
    else:
        if running_turn(ANSI_RE.sub("", raw[-4000:])):
            busy.append("screen shows a running turn ('esc to interrupt' after the last turn end)")
        state, typed = prompt_state(raw, wrap_cols(term))
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
    pos, neg = phrase_hits(tx["final_text"])
    if neg:
        return "ACTIVE", ["final message says work remains: " + neg[0]]
    if finished:
        fa = row.get("finishedAt") or 0
        if tx["last_prompt"] and tx["last_prompt"] > fa + 1000:
            if pos:
                # A re-finish leaves finishedAt where it was (the runner answers a
                # re-mark as a no-op), so the closeout in the reply is the evidence.
                return "CLOSEOUT", [f"revived after its {fmt_age(fa)}-old finish mark, then closed out "
                                    "again in its final reply: " + pos[0]] + binding
            return "REVIVED", [f"marked finished {fmt_age(fa)} ago, then prompted "
                               f"{fmt_age(tx['last_prompt'])} ago: {tx['last_prompt_text'][:60]!r}"]
        if tx["last_asst"] and tx["last_asst"] - fa > 10 * 60000 and not pos:
            return "REVIVED", [f"kept working {(tx['last_asst'] - fa) / 60000:.0f}m after its finish mark "
                               "and its final message is not a closeout"]
        why = row.get("finishReason")
        return "FINISHED", ["runner finished marker" + (f" ({why[:70]})" if why else "")] + binding
    if pos:
        # coord's status carries no finish time this script can read, so a
        # coord-only finish is only as good as the closeout line in the reply.
        src = "coord session_status=finished, closeout phrase: " if coord_finished else \
            "closeout phrase without a finished marker: "
        return ("FINISHED" if coord_finished else "CLOSEOUT"), [src + pos[0]] + binding
    if FOOTER_RE.search(tx["final_text"][-2500:]):
        stated = footer_nothing_owed(tx["final_text"][-2500:])
        if stated:
            return "FOOTER-FINISHED", [f"footer final state, no marker: {stated[:90]!r}"] + binding
        return "FOOTER-ONLY", ["only a POLICY_COMPLIANCE footer - printed every turn, not proof of finish"]
    if coord_finished:
        return "REVIVED", ["coord says finished, but the final message is not a closeout"]
    return "ACTIVE", ["no finished marker, no closeout phrase"]


def screen_one(term, row, table, idle_min, own, ready):
    """classify(), plus the slow output-moving sample for rows that would close."""
    verdict, why = classify(term, row, table, idle_min, own, ready)
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
        row = rows.get(t["id"])
        verdict, why = screen_one(t, row, table, idle_min, own, ready)
        acct = ((row or {}).get("account") or {}).get("label") or re.sub(
            r"^\.claude-?", "", os.path.basename((row or {}).get("configDir") or "")) or "?"
        out.append({
            "terminalId": t["id"], "verdict": verdict, "reasons": why,
            "name": (row or {}).get("resumeName") or t.get("title") or "",
            "title": t.get("title") or "", "account": acct,
            "tab": pages.get(t.get("pageId") or "default") or f"page {str(t.get('pageId'))[:8]}",
            "claudeSessionId": (row or {}).get("claudeSessionId"),
            "finishedAt": (row or {}).get("finishedAt"),
            "sessionStatus": (ready.get((row or {}).get("claudeSessionId")) or {}).get("sessionStatus"),
        })
    return out, warnings, ready_err


ORDER = ["FINISHED", "CLOSEOUT", "FOOTER-FINISHED", "EXITED-FINISHED", "REVIVED", "FOOTER-ONLY", "BUSY", "LIMITED", "ACTIVE",
         "EXITED", "UNKNOWN", "SHELL", "SELF"]


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


def close_one(tid, idle_min, own, allowed):
    """Re-screen, then DELETE /terminals/{id}, then read back. Returns (ok, msg)."""
    t = next((x for x in terminals() if x["id"] == tid), None)
    if t is None:
        return False, "terminal no longer listed (already closed?)"
    ready, err = readiness()
    if err:
        return False, f"readiness unreadable at re-screen ({err[:80]})"
    table = proc_table()
    verdict, why = screen_one(t, open_rows().get(tid), table, idle_min, own, ready)
    if verdict not in allowed:
        return False, f"re-screen now says {verdict}: {'; '.join(why)}"
    claudes = proc_facts(table, t["pid"])[0] if table and t.get("pid") in table else []
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
            return True, f"DELETE {sent}; read back: tab gone from /terminals, claude process gone"
    return False, f"DELETE {sent}; read back: listed={listed}, claude pids still alive={alive}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--close", action="store_true")
    ap.add_argument("--idle-min", type=int, default=15)
    ap.add_argument("--only", default="")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--port", help="runner API port (default $QONTINUI_RUNNER_API_PORT or 9876)")
    ap.add_argument("--selftest", action="store_true", help="run the regression table and exit; reads nothing")
    a = ap.parse_args()
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
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
