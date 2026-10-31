#!/usr/bin/env python3
"""The plan status-stamp parse -- the one implementation, stdlib only.

Check #34's `lint-plan-status.py` is the stamp's parse owner, and its module
docstring is where the grammar is argued ("THE PARSE", "THE KEY IS
CASE-INSENSITIVE ...", the no-colon form). The parse itself lives HERE so it can
travel: `lint-plan-status.py` imports `detector_reach`, which a runner-bundled
skill cannot carry, so a byte-identical copy of this file sits in the
return-to-main skill's `lib/` beside the shared one in the config repo's script
`lib/` (held identical by `return-to-main-skill-test.sh` assertion I2).
`lint-plan-status.py` imports every name below and re-exports it, so its
by-path importers read `LPS.<name>` exactly as before. A new stamp shape is
fixed in this file, once, and reaches every reader.

Keep it stdlib-only and import-free of anything beside it: on a device with no
checkout this file is all there is.

Run it::

    python plan_stamp.py --token  <file|->   # the lowercase lifecycle TOKEN
    python plan_stamp.py --bounds <file|->   # "<first> <last>" 1-based lines

`--token` prints `read_stamp`'s TOKEN lowercased (`shipped`, `in progress`,
`premise falsified`; the empty string when the stamp line says nothing after
its key) -- the same word `lint-plan-status.py --stamp` prints in upper case.
`--bounds` prints the first and last line numbers of the region a caller may
IGNORE as "the stamp": the body-first status block, narrowed for an unquoted
stamp to its line plus the `Key: value` lines directly under it
(`status_block_span(..., metadata_only=True)`), numbered the way `sed` numbers
them (by `\\n` alone), so a shell caller can `sed "<first>,<last>d"` it out of
the file.

Exit 0 = a stamp was found and printed; 3 = no stamp in the first
`STATUS_WINDOW_LINES` lines (UNKNOWN -- never "draft", never "no block"); 2 =
usage, or the input could not be read. The same codes `lint-plan-status.py
--stamp` / `--depends-on` use. Any other exit (1 is Python's own exit on an
uncaught exception) means the reader FAILED: no answer, never "no stamp".

Plan 2026-10-16-two-stamp-readers-still-bypass-the-parse-owner-land-evidence-and-depends-on
Phase 2.
"""

from __future__ import annotations

import re
import sys

# --------------------------------------------------------------------------
# Vocabulary the tokenizer knows
# --------------------------------------------------------------------------

#: `plan-discipline`'s eight, in tier order. Both spellings of the two-word one
#: are listed because both occur in the corpus: `IN PROGRESS` (132 bodies) and
#: `IN_PROGRESS` (3). They are the same status; keeping both here means neither
#: spelling is reported as an invented word.
POLICY_VOCAB = (
    "ready",
    "shipped",
    "vetted",
    "superseded",
    "obsolete",
    "draft",
    "in progress",
    "in_progress",
    "blocked",
)

#: The runner adapter's `PlanConvention::operator_default()` - the vocabulary the
#: ingestion path actually tokenizes with, and therefore the one that decides
#: what reaches `coord.work_units` intact. It is the policy's eight minus
#: `ready`/`blocked`, plus `partial`, `not started` and `implemented`.
#:
#: The two underscore spellings are not additions to the adapter's nine: the
#: adapter emits every matched phrase underscore-joined (`in progress` ->
#: `in_progress`), so those ARE its canonical output forms, and a body already
#: spelled `IN_PROGRESS` flows through it byte-identically. Listing only the
#: spaced spelling here would report the adapter's own output as off-vocabulary.
ADAPTER_VOCAB = (
    "draft",
    "vetted",
    "in progress",
    "in_progress",
    "shipped",
    "partial",
    "not started",
    "not_started",
    "superseded",
    "obsolete",
    "implemented",
)

#: Multi-word phrases the TOKENIZER always knows, whatever the classifying
#: vocabulary is. See `lint-plan-status.py`'s docstring: tokenizing with the classifying
#: vocabulary alone makes the reported WORD depend on the arm, which turns a
#: reporting artifact into what looks like a finding.
#:
#: `premise falsified` is here for `--stamp`: `/vet-imp-sweep` filter 3b
#: names `PREMISE FALSIFIED` as a TERMINAL token, and a tokenizer that knew only
#: the first word would hand that screen `PREMISE`, which it reads as UNKNOWN and
#: keeps -- the wasted-dispatch direction this mode exists to close. It is in no
#: vocabulary, so it moves no classification; it only stops the reported WORD
#: being truncated.
TOKENIZER_PHRASES = ("in progress", "not started", "premise falsified")


#: `/vet-imp-sweep` filter 3b's TERMINAL tokens, as `--stamp` prints them. Used
#: by `lint-plan-status.py --stamp-census` and to build `NOCOLON_STAMP_WORDS`; the
#: command file's list is the authority, and a word added there and not here
#: under-counts the census, never the screen.
SWEEP_TERMINAL_TOKENS = frozenset({
    "SHIPPED", "LANDED", "SUPERSEDED", "OBSOLETE", "REJECTED", "REFUTED",
    "PREMISE FALSIFIED", "WITHDRAWN", "DECIDED", "COMPLETE",
})

#: Lifecycle words a no-colon stamp may open with: both classifying
#: vocabularies and every word `/vet-imp-sweep` 3b treats as TERMINAL. Kept as
#: one set, so a word recognised in the colon form is recognised here too.
NOCOLON_STAMP_WORDS = tuple(sorted(
    {w.replace("_", " ") for w in POLICY_VOCAB + ADAPTER_VOCAB}
    | set(TOKENIZER_PHRASES)
    | {t.lower() for t in SWEEP_TERMINAL_TOKENS},
    key=lambda p: (-len(p), p),
))


# --------------------------------------------------------------------------
# The stamp KEY
# --------------------------------------------------------------------------

#: How many lines from the top of a plan carry a status stamp. A stamp sits
#: between the H1 and the body; past this, `Status:` is prose. 60, not 40: see
#: `lint-plan-status.py`'s docstring ("THE PARSE") and the `stamp-at-line-45` fixture.
STATUS_WINDOW_LINES = 60

#: Leading blockquote markers, however deeply nested.
BLOCKQUOTE_RE = re.compile(r"^(?:\s*>\s?)+")

#: The COLON stamp. The key is case-insensitive, and only list/heading markers
#: may precede it - see `lint-plan-status.py`'s docstring. A colon-less key is a stamp only
#: in the bounded bold form `NOCOLON_STATUS_RE` reads; `stamp_remainder` is the
#: one test over both.
STATUS_RE = re.compile(r"^[-*+#\s]*Status\s*:\s*(.*)$", re.IGNORECASE)

#: The NO-COLON stamp: `> **STATUS SHIPPED 2026-09-01 — ...` or
#: `> **Status 2026-09-24 — ...`. Matched on the RAW line, because the bold
#: opener is what makes it a stamp. With the `**` stripped, `Status of the
#: migration` is ordinary prose. The word after the key must be a lifecycle word
#: (`NOCOLON_STAMP_WORDS`) or an ISO date. Without that bound, a
#: `**STATUS UPDATE 2026-07-29 ...**` or `**STATUS DIVERGENCE, both readings
#: kept.**` history block would be read as the stamp, and as the first match in
#: the window it would shadow the real `Status:` stamp below it. Group 1 is the
#: remainder after the key.
NOCOLON_STATUS_RE = re.compile(
    r"^(?:\s*>\s?)*\s*(?:[-*+]\s+|#+\s+)?\*\*Status\s+(?!\*{0,2}\s*:)(.*)$", re.IGNORECASE
)

ISO_DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}\b")


def stamp_remainder(line: str) -> str | None:
    """The text after the stamp key on `line`, markers stripped, or None when
    `line` is not a stamp line.

    This is the ONE key test. `parse_status`, `status_block_lines` and
    `read_stamp` all call it, so a new key shape is added here once. It accepts
    two forms. The COLON form is every shape `STATUS_RE` reads after
    `strip_markers` (blockquote, `**Status:**`, `**Status**:`, list-bulleted,
    bare `Status:`). The bold NO-COLON form is `NOCOLON_STATUS_RE`, bounded by
    `NOCOLON_STAMP_WORDS` or a date.
    """
    match = STATUS_RE.search(strip_markers(line))
    if match is not None:
        return match.group(1)
    match = NOCOLON_STATUS_RE.match(line)
    if match is None:
        return None
    rest = strip_markers(match.group(1)).strip()
    if ISO_DATE_RE.match(rest) or match_phrase(rest, NOCOLON_STAMP_WORDS) is not None:
        return rest
    return None


def tokenizer_phrases(vocab: tuple[str, ...]) -> tuple[str, ...]:
    """The phrase set used to tokenize, longest first for a stable scan order."""
    phrases = set(TOKENIZER_PHRASES) | {w for w in vocab if " " in w}
    return tuple(sorted(phrases, key=lambda p: (-len(p), p)))


# --------------------------------------------------------------------------
# The parse
# --------------------------------------------------------------------------


def strip_markers(line: str) -> str:
    """Drop blockquote, bold and inline-code markers, keeping the rest byte-for-byte.

    Backticks are stripped for the same reason `**` is: they are emphasis around
    the stamp, not part of it. A real plan on `origin/main` stamps
    ``## STATUS: `REWORKED 2026-08-23` `` and without this the status word parses
    as ``\u0060reworked`` - same verdict, wrong word, and it groups separately
    from every other `reworked` in the report.
    """
    return BLOCKQUOTE_RE.sub("", line).replace("**", "").replace("`", "")


def match_phrase(stamp: str, phrases: tuple[str, ...]) -> str | None:
    """Longest known phrase at the start of `stamp`, word-boundary terminated.

    Ported from the runner adapter's `match_known_status`, boundary rule
    included: `drafty` must not match `draft`, or a typo becomes a valid status.
    """
    low = stamp.lower()
    best: str | None = None
    for phrase in phrases:
        if not low.startswith(phrase):
            continue
        rest = low[len(phrase) :]
        if rest and (rest[0].isalnum() or rest[0] == "_"):
            continue
        if best is None or len(phrase) > len(best):
            best = phrase
    return best


def normalize_token(token: str) -> str:
    """Lowercase, drop trailing stamp punctuation, join whitespace with `_`.

    The adapter's `normalize_status`, so an unrecognized stamp reaches the tally
    in the same shape the ingestion path pushes to coord.
    """
    cleaned = token.strip().rstrip(",.:*")
    return re.sub(r"\s+", "_", cleaned).lower()


def status_word(remainder: str, phrases: tuple[str, ...]) -> str:
    """The status word at the start of a stamp's remainder (the text after
    `Status:`, markers already stripped). The phrase VERBATIM when one matched,
    else the normalized first token; the empty string for an empty remainder."""
    remainder = remainder.strip()
    phrase = match_phrase(remainder, phrases)
    if phrase is not None:
        return phrase
    token = re.split(r"[\s.,]", remainder)[0].strip()
    return normalize_token(token)


def parse_status(text: str, phrases: tuple[str, ...]) -> str | None:
    """The plan's status word, or None when it has no status block.

    Returns the phrase VERBATIM when one matched (`in progress`, not
    `in_progress`) and the normalized first token otherwise. A `Status:` whose
    remainder is empty parses as the empty string - which is a status block that
    says nothing, not an absent one, and is reported as off-vocabulary rather
    than silently folded into `no_status_block`.
    """
    for line in text.splitlines()[:STATUS_WINDOW_LINES]:
        remainder = stamp_remainder(line)
        if remainder is None:
            continue
        return status_word(remainder, phrases)
    return None


# --------------------------------------------------------------------------
# The status BLOCK -- the one locator every stamp screen calls
# --------------------------------------------------------------------------
#
# `_status-writers.md` names `lint-plan-status.py` (with this module) the stamp's parse owner. The locator
# below used to live in `lint-plan-stamp-vs-coord.py`, and three more readers
# (`/vet-imp-sweep` filter 3b's `grep '^> \*\*Status:'`, `_span_common.py`,
# `nondispatch-span-verdict.py`) each anchored on the blockquote form alone, so a
# plain `**Status:** SHIPPED` line read as "no stamp" -> UNKNOWN -> KEEP and a
# shipped plan was dispatched. One locator, here, means a new stamp shape is
# fixed once (plan
# 2026-09-21-the-sweep-status-screen-is-blockquote-anchored-so-a-plain-status-line-reads-as-unknown).


def _frontmatter_end(lines: list[str]) -> int:
    """Index of the closing `---` of a leading YAML frontmatter, or -1."""
    if lines and lines[0].strip() == "---":
        return next((i for i in range(1, len(lines)) if lines[i].strip() == "---"), -1)
    return -1


def status_block_lines(text: str, *, body_first: bool = False) -> list[str] | None:
    """The status block's PHYSICAL lines, unstripped, or None.

    The stamp line is found exactly as `parse_status` finds it: the first
    `STATUS_RE` match on a marker-stripped line within the `STATUS_WINDOW_LINES`
    window. A blockquoted stamp takes every following `>` line (a bare `>` line
    included); a bare stamp takes its paragraph, up to a blank line or a heading;
    a `status:` key inside YAML frontmatter is one line, because the keys below
    it are metadata, not stamp prose.

    `body_first=True` passes over a frontmatter `status:` key when a stamp also
    sits in the body inside the window, and falls back to the key only when none
    does. The stamp SCREENS want that: measured 2026-10-05 on dev-notes
    `origin/main`, 3 of the 19 plans with frontmatter carry `status: DRAFT`
    above a body stamp reading SHIPPED / OBSOLETE / SUPERSEDED, and the body
    stamp is the one every writer maintains. The default (`False`) is check
    #34's own first-match parse, unchanged.
    """
    lines = text.splitlines()
    span = status_block_span(lines, body_first=body_first)
    return None if span is None else lines[span[0] : span[1]]


#: The keys a stamp's own metadata lines carry, spelled as the corpus spells
#: them. An EXPLICIT list, never "any short key before a colon": `Note: we will
#: move the cache into a sidecar.` or `Next step: ...` is prose that happens to
#: open with a colon, and an extent that swallowed it would let the
#: plan-document arm ignore substance. The list is the keys MEASURED directly
#: under an unquoted (plain / list / bare) stamp on dev-notes origin/main
#: 07d9c897 -- `Repos`, `Date`, `Repo`, `Dossier`, `Depends-On`, `Parent`,
#: `Source`, `Finding`, `Origin`, `Authored`, `Discovered by` -- plus `Owner`.
#: The prose-bearing `Evidence`, `Trigger`, `Loop` and `Defeats` seen there are
#: deliberately absent, and so are the writers' own keys (`History`, `Read`,
#: `Checks`, ...): writers emit those inside a `>` blockquote, whose whole run
#: is the extent anyway, and under an unquoted stamp `History: rewrote the
#: scheduler` is substance. A key missing here makes the arm compare that line,
#: i.e. abstain -- the safe direction.
STAMP_METADATA_KEYS: tuple[str, ...] = (
    "Authored", "Date", "Depends-On", "Discovered by", "Dossier", "Finding",
    "Origin", "Owner", "Parent", "Repo", "Repos", "Source",
)

#: A stamp metadata line, markers already stripped: an optional list bullet, a
#: key from `STAMP_METADATA_KEYS`, a colon, a non-empty value
#: (`- Depends-On: x`, `Owner: y`, `Repos: a, b`).
METADATA_LINE_RE = re.compile(
    r"^\s*(?:[-*+]\s+)?(?:"
    + "|".join(re.escape(k) for k in sorted(STAMP_METADATA_KEYS, key=len, reverse=True))
    + r")\s*:\s*\S"
)


def status_block_span(
    lines: list[str], *, body_first: bool = False, metadata_only: bool = False
) -> tuple[int, int] | None:
    """The status block as a half-open `(start, end)` index range into `lines`,
    or None. The ONE extent rule -- `status_block_lines` slices it -- exposed
    as indices for a caller that must cut the block out of the file (the
    return-to-main `land-evidence.sh` plan-document arm compares two copies with
    their status blocks removed, by line number).

    `metadata_only=True` narrows an UNQUOTED stamp's extent from its whole
    paragraph to the stamp line plus the `Key: value` lines directly under it
    (`METADATA_LINE_RE`), stopping at the first line that is not one. That is
    the extent a caller may IGNORE: a design sentence written under a plain
    `**Status:** X` line is substance, and a comparison that skipped the whole
    paragraph would let it pass as a status change. A quoted stamp's extent is
    unchanged -- the `>` run is the stamp's own container.
    """
    frontmatter_end = _frontmatter_end(lines)
    hits = [
        i
        for i, line in enumerate(lines[:STATUS_WINDOW_LINES])
        if stamp_remainder(line) is not None
    ]
    if not hits:
        return None
    index = hits[0]
    if body_first:
        index = next((i for i in hits if i > frontmatter_end), index)
    if index < frontmatter_end:
        return index, index + 1
    end = index + 1
    quoted = BLOCKQUOTE_RE.match(lines[index]) is not None
    for follow in lines[index + 1 :]:
        if quoted:
            if BLOCKQUOTE_RE.match(follow) is None:
                break
        elif not follow.strip() or follow.lstrip().startswith("#"):
            break
        elif metadata_only and METADATA_LINE_RE.match(strip_markers(follow)) is None:
            break
        end += 1
    return index, end


def join_block(lines: list[str]) -> str:
    """Marker-stripped, whitespace-trimmed, space-joined, blank lines dropped."""
    parts = (strip_markers(line).strip() for line in lines)
    return " ".join(part for part in parts if part)


def status_block(text: str, *, body_first: bool = False) -> str | None:
    """The whole status block, marker-stripped and joined, or None.

    `status_block(t) is None` iff `parse_status(t) is None` (the same window, the
    same key regex). See `status_block_lines` for the extent rules.
    """
    lines = status_block_lines(text, body_first=body_first)
    return None if lines is None else join_block(lines)


def stamp_token(word: str, phrases: tuple[str, ...]) -> str:
    """The `--stamp` TOKEN column for a `status_word` result: UPPERCASED, and an
    underscore spelling of a known multi-word phrase folded to its SPACED form,
    so `IN_PROGRESS` and `IN PROGRESS` both print `IN PROGRESS` and
    `premise_falsified` prints `PREMISE FALSIFIED`. Any other word is printed as
    the tokenizer took it (`partial` -> `PARTIAL`, an invented `foo_bar` ->
    `FOO_BAR`); the empty word (`**Status:**` with nothing after it on its line)
    prints as the empty string."""
    spaced = word.replace("_", " ")
    if spaced in phrases:
        word = spaced
    return word.upper()


def read_stamp(text: str) -> tuple[str, str] | None:
    """(TOKEN, joined block) for `--stamp`, or None when the window holds no stamp.

    The block is located with `body_first=True`; the token is `status_word` over
    the located stamp LINE (markers stripped first, so in `**Status:** SHIPPED`
    the word is `SHIPPED`, not `**`), with the default tokenizer phrases.
    """
    lines = status_block_lines(text, body_first=True)
    if lines is None:
        return None
    phrases = tokenizer_phrases(POLICY_VOCAB)
    remainder = stamp_remainder(lines[0])
    word = status_word(remainder, phrases) if remainder is not None else ""
    return stamp_token(word, phrases), join_block(lines)


# --------------------------------------------------------------------------
# `Depends-On:` -- the shared dependency rule, read from the located block
# --------------------------------------------------------------------------
#
# `/vet-plan`, `/verify-plan-status` and `/implement-plan` each carried this rule
# as prose anchored on "the first `> **Status:` block", so a plain or list-form
# stamp's `Depends-On:` was never seen and the plan read UNBLOCKED -- dispatched
# over an unmet dependency (plan
# 2026-10-16-two-stamp-readers-still-bypass-the-parse-owner-land-evidence-and-depends-on
# Phase 1). The rule is the one those bodies state, steps 1-4:
#
# 1. find EVERY case-sensitive `Depends-On:` occurrence in the HEADER REGION:
#    every physical line from the located stamp's first line down to the first
#    `##`-level heading, bounded by the window (`depends_on_region_lines`);
# 2. per occurrence, read only the remainder of that PHYSICAL line;
# 3. keep only `YYYY-MM-DD-<kebab-slug>` stem tokens (a bare date is no stem);
# 4. union the stems, deduped, order-preserving.
#
# Step 2 is why this walks physical lines, never `join_block`: a joined block
# would turn a stem that PROSE names on a later line into a phantom dep.
#
# Step 1's region is wider than the status block on purpose. Plans carry their
# `Depends-On:` in a metadata list or line just BELOW a blockquote stamp (e.g.
# `- **Depends-On**: <stem>` after a blank line), which a block-only read missed
# -- the plan read unblocked. Measured at dev-notes origin/main 07d9c897 by
# comparing `read_depends_on` with `depends_on_from_lines(status_block_lines(
# body, body_first=True))` over every `git archive origin/main plans` body: the
# two differ on 29 of 2674 plans. The first `##` heading OUTSIDE a fenced code
# block is where the header ends and body prose begins.

#: A heading of level two or deeper: the end of the header region.
HEADER_END_RE = re.compile(r"^\s*#{2,}\s")

#: A fenced-code-block delimiter: up to three spaces, then a run of three or
#: more backticks or tildes (group 1), then the rest of the line (group 2). A
#: fence closes only on the SAME character, a run at least as long as the
#: opener's, and nothing after it -- so ```` around an inner ``` stays open.
FENCE_RE = re.compile(r"^\s{0,3}(`{3,}|~{3,})(.*)$")

DEPENDS_ON_KEY = "Depends-On:"

#: A plan stem: a date and at least one `-word` after it, matched WHOLE. The
#: lookarounds reject a token that runs on into a non-stem character rather
#: than truncating it: `2026-06-02-fix-API-thing` is not a stem, and a `\b`
#: anchor read it as the phantom `2026-06-02-fix`.
DEPENDS_ON_STEM_RE = re.compile(r"(?<![\w-])\d{4}-\d{2}-\d{2}-[a-z0-9]+(?:-[a-z0-9]+)*(?![\w-])")


def depends_on_from_lines(lines: list[str]) -> list[str]:
    """Steps 1-4 over physical block lines, markers stripped per line."""
    stems: list[str] = []
    for line in lines:
        stripped = strip_markers(line)
        start = stripped.find(DEPENDS_ON_KEY)
        while start != -1:
            remainder = stripped[start + len(DEPENDS_ON_KEY) :]
            for stem in DEPENDS_ON_STEM_RE.findall(remainder):
                if stem not in stems:
                    stems.append(stem)
            start = stripped.find(DEPENDS_ON_KEY, start + 1)
    return stems


def depends_on_region_lines(text: str) -> list[str] | None:
    """The header region's physical lines, or None when the window holds no stamp:
    from the first line of the block `--stamp` locates
    (`status_block_span(body_first=True)`) down to, not including, the first
    `##`-level heading outside a fenced code block, and never past
    `STATUS_WINDOW_LINES`."""
    lines = text.splitlines()
    span = status_block_span(lines, body_first=True)
    if span is None:
        return None
    start = span[0]
    stop = min(len(lines), max(STATUS_WINDOW_LINES, span[1]))
    end, fence, heading_in_fence = stop, None, None
    for i in range(start + 1, stop):
        run = FENCE_RE.match(lines[i])
        if run is not None:
            if fence is None:
                fence, heading_in_fence = run.group(1), None
                continue
            if run.group(1)[0] == fence[0] and len(run.group(1)) >= len(fence) and not run.group(2).strip():
                fence = None
                continue
        if HEADER_END_RE.match(lines[i]):
            if fence is None:
                end = i
                break
            if heading_in_fence is None:
                heading_in_fence = i
    else:
        # An UNCLOSED fence: its first `##` is the end after all, so a stray
        # opener never stretches the region to the window limit.
        if fence is not None and heading_in_fence is not None:
            end = heading_in_fence
    return lines[start:end]


def read_depends_on(text: str) -> list[str] | None:
    """The plan's `Depends-On:` stems, or None when the window holds no stamp.

    Every stamp shape is covered (`> **Status:** ... Depends-On: x`,
    `**Status:** X` with a `Depends-On:` line under it, list-form
    `- **Depends-On**: x`), and so is a metadata line below the stamp's own block
    but above the first `##` heading.
    """
    lines = depends_on_region_lines(text)
    return None if lines is None else depends_on_from_lines(lines)


# --------------------------------------------------------------------------
# CLI: --token / --bounds
# --------------------------------------------------------------------------

STAMP_FOUND = 0
STAMP_ABSENT = 3
USAGE = 2

_USAGE_TEXT = "usage: plan_stamp.py (--token | --bounds) <file|->"


def _read_source(source: str) -> str | None:
    """A plan body from a path or `-` (stdin), decoded as UTF-8 whatever the
    locale says, or None when it cannot be read."""
    try:
        if source == "-":
            raw = sys.stdin.buffer.read()
        else:
            with open(source, "rb") as handle:
                raw = handle.read()
    except OSError as exc:
        print(f"error: cannot read {source}: {exc}", file=sys.stderr)
        return None
    return raw.decode("utf-8", errors="replace")


def physical_lines(text: str) -> list[str]:
    """`text` split on `\\n` alone, a trailing `\\r` dropped per line -- the
    numbering `sed` and `awk` use. `str.splitlines` also breaks on `\\f`,
    `\\x1c`-`\\x1e`, `\\u2028` and friends, which would shift every line number
    after one of them."""
    return [line[:-1] if line.endswith("\r") else line for line in text.split("\n")]


def stamp_bounds(text: str) -> tuple[int, int] | None:
    """1-based inclusive `(first, last)` of the IGNORABLE stamp extent, or None:
    the body-first status block with `metadata_only=True` -- a quoted stamp's
    whole `>` run, or an unquoted stamp's line plus the `STAMP_METADATA_KEYS`
    lines directly under it. Not the whole `status_block_lines` extent: prose
    in an unquoted stamp's paragraph is substance a caller must compare."""
    span = status_block_span(physical_lines(text), body_first=True, metadata_only=True)
    return None if span is None else (span[0] + 1, span[1])


def main(argv: list[str]) -> int:
    # A UTF-8 stamp word printed to a cp1252 console (Windows) must not raise:
    # a mangled glyph costs legibility, a traceback costs the answer. The same
    # guard `lint-plan-status.py` carries.
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is None:
            continue
        try:
            reconfigure(encoding="utf-8", errors="replace")
        except (ValueError, OSError):
            pass
    if len(argv) != 3 or argv[1] not in ("--token", "--bounds"):
        print(_USAGE_TEXT, file=sys.stderr)
        return USAGE
    text = _read_source(argv[2])
    if text is None:
        return USAGE
    if argv[1] == "--token":
        found = read_stamp(text)
        answer = None if found is None else found[0].lower()
    else:
        bounds = stamp_bounds(text)
        answer = None if bounds is None else f"{bounds[0]} {bounds[1]}"
    if answer is None:
        print(f"no status stamp in the first {STATUS_WINDOW_LINES} lines (UNKNOWN)", file=sys.stderr)
        return STAMP_ABSENT
    print(answer)
    return STAMP_FOUND


if __name__ == "__main__":
    sys.exit(main(sys.argv))
