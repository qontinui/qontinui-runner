#!/usr/bin/env python3
"""analyze-launch-failure.py -- helpers for the /analyze-launch-failure command.

Verbs (read stdin, write to stdout):

  scan    Classify a retry pane's text buffer (the ANSI-stripped PTY stream the
          runner serves at /terminals/{id}/buffer?format=text). The launch
          command is read from the environment variable RETRY_CMD, never argv,
          so a recorded command with quotes or `$` arrives byte for byte.
          Prints one line, or nothing:
            seen                  the Claude REPL is up and ready
            refused-session-id    the pinned --session-id is already in use
            failed:<line>         the earliest TERMINAL failure line (login or
                                  limit screen, missing binary, refused
                                  credential) between the echo and the first
                                  ready marker, or the earliest BLOCKING screen
                                  below it; never a transcript line. REDACTED.
                                  Final: such a screen does not turn into a
                                  REPL.
            noise:<line>          no ready marker yet and no terminal line, but
                                  a failure-shaped line (ENOENT, ECONNREFUSED,
                                  TLS ...), REDACTED. Not final: a REPL can
                                  still come up, so keep polling; at the
                                  deadline it is UNKNOWN with this line quoted.
            no-echo               the typed command's echo was not found, so no
                                  line can be attributed to the launch (UNKNOWN)
            (nothing)             no verdict yet; keep polling. A caller that
                                  times out on nothing records UNKNOWN, never a
                                  failure: a slow first launch reads the same.
  redact  Redact credential-shaped strings from stdin (every line that leaves
          the machine goes through this). It is a heuristic with a measured
          bound, not a guarantee:
            - redacted: a value under a credential key or flag, URL userinfo,
              Authorization/Bearer/Cookie values, known vendor prefixes (sk-,
              ghp_, AKIA, eyJ...), and a free-standing 32+ character run that
              mixes upper and lower case at random (base64, base64url,
              alphanumeric: about 1-5 in 10000 kept at 32-40 characters,
              none kept at 48 or more, in 10000-draw measurements). A run that
              starts like a path (`/`, `~`, `@`, a drive letter) with three or
              more segments, one a plain lower-case word, is judged piece by
              piece, so a random piece inside a real path is redacted alone (in
              10000-draw measurements a 32-byte token embedded in a path kept
              an 8-character piece about 1 time in 1000 for base64url and 5
              in 1000 for standard base64);
            - KEPT on purpose: a free-standing single-case run (hex, lower or
              upper alphanumeric, base32) at any length and any unkeyed token
              under 32 characters, because shas, digests, ids, plan stems and
              error codes have exactly those shapes. Quote as few lines as
              the evidence needs.

Ordering:
  - The echo anchor is the FIRST line that ends with the command, whatever else
    it holds. If no line ends with it, whitespace and carriage returns are
    removed from both sides and the first line completing the command is
    taken, since a long echo can be split across PTY lines. When the anchor is
    itself a shell not-found error with no prompt before the command, that line
    is the failure.
  - On a resumed launch (`--resume`, `--continue`, `-r`, `-c`), transcript
    blocks never count, for any verdict, the ready marker included: a line
    opening with `❯`, `>`, `⎿`, `●` or `⏺` and the lines indented deeper than
    it, across blank lines. On a fresh launch only the `❯`/`>` prompt redraw
    is masked. Such a retry replays old
    messages, above and below the ready marker, that can quote any failure
    text. A fresh launch replays nothing, so its own `⎿ API Error` counts.
  - Between the echo and the first ready marker a TERMINAL line wins and the
    earliest is quoted. Below the ready marker only Claude's own BLOCKING
    screens count (a TUI can draw a login notice below its footer).
  - Any other failure-shaped line is `noise:` while no ready marker has
    appeared, and is dropped once one has (the REPL survived it).
"""
from __future__ import annotations

import os
import re
import sys

for _stream in (sys.stdout, sys.stderr):
    _reconfigure = getattr(_stream, "reconfigure", None)
    if _reconfigure is not None:
        try:
            _reconfigure(encoding="utf-8", errors="replace")
        except (ValueError, OSError):
            pass

READY = (r"\? for shortcuts", r"esc to interrupt", r"bypass permissions on")
FAILURE = (
    r"command not found",
    r"Unknown command",
    r"\bENOENT\b",
    r"No such file or directory",
    r"is not recognized as",
    r"\b(?:usage|session|5-hour|weekly|opus) limit reached\b",
    r"Select login method",
    r"Please run /login",
    r"Invalid API key",
    r"OAuth token has expired",
    r"API Error: 40[13]\b",
    r"\bECONNREFUSED\b",
    r"\bENOTFOUND\b",
    r"\bETIMEDOUT\b",
    r"\bECONNRESET\b",
    r"\b(error|unable|fail\w*|expired|self[- ]signed)\b.*\b(certificate|TLS|SSL)\b",
    r"\b(certificate|TLS|SSL)\b.*\b(error|unable|fail\w*|expired|verify|self[- ]signed)\b",
    r"\b(UNABLE_TO_VERIFY_LEAF_SIGNATURE|CERT_HAS_EXPIRED|SELF_SIGNED_CERT_IN_CHAIN|DEPTH_ZERO_SELF_SIGNED_CERT)\b",
    r"API Error: Connection error",
    r"\b403 Forbidden\b",
    r"\bstatus(?: code)?[ :=]+403\b",
)
REFUSED = r"session[ -]?id\b.*already in use"
# Claude's own screens that block a session that has otherwise come up; these
# are the only terminal lines looked for below the ready marker.
BLOCKING = (
    r"Select login method",
    r"Please run /login",
    r"Invalid API key",
    r"OAuth token has expired",
    r"API Error: 40[13]\b",
    r"\b(?:usage|session|5-hour|weekly|opus) limit reached\b",
)
# Failures that end a launch even when a ready marker follows (a login or
# limit screen, a missing binary, a refused credential). Any OTHER failure
# line printed before the ready marker is noise the REPL survived.
TERMINAL = (
    r"command not found",
    r"Unknown command",
    r"is not recognized as",
    r"\b(?:usage|session|5-hour|weekly|opus) limit reached\b",
    r"Select login method",
    r"Please run /login",
    r"Invalid API key",
    r"OAuth token has expired",
    r"API Error: 40[13]\b",
)

# Identifiers that are evidence, never secrets: UUIDs. (Git shas and sha256
# digests survive the opaque-run rule on their own: they have no upper case.
# A hex secret under a credential key is redacted by the key rules first.)
_KEEP = re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", re.I)
# Shell-reported "this command does not exist" lines, which can end with the
# command itself ("zsh: command not found: claude").
_NOT_FOUND = re.compile(r"command not found|Unknown command|is not recognized as", re.I)
_CORE = r"(?:token(?!izer)|signature|secret|passphrase|passw(?:or)?d|pass\b|private[_-]?key|api[_-]?key|auth\b|sig\b|cookie|credential)"
# A value that is evidence, not a secret: a path, a number, a signal name.
# Status words a failure line uses after a credential key ("token: expired").
_STATUS = r"(?:expired|invalid_grant|invalid_token|invalid|failed|failure|required|missing|denied|revoked|refused|rejected|unauthorized|forbidden|not|none|null|unset|empty|undefined|true|false|error|ok|no|such|permission|the|an?)"
# Where a value ends: whitespace, a delimiter, a sentence stop, or the line end.
_END = r"(?=[\s\"'&;,]|[.!?](?:\s|$)|$)"
# Each exemption covers the WHOLE value, never a prefix of it: a multi-segment
# path, a signal name, or a status word. A number is kept only under a key that
# names a count (see _key_value).
_EVIDENCE_VALUE = rf"(?!(?:[/~]|[A-Za-z]:[\\/])[^\s\"'&;,]*(?<=[/\\])[a-z][a-z_.-]{{2,}}(?=[/\\\s\"'&;,]|$)[^\s\"'&;,]*{_END}|SIG[A-Z]+{_END}|{_STATUS}{_END})"
_KEY = rf"[\w.-]*{_CORE}[\w.-]*"
# A snake/env-style name: the credential word joined to another by `_`.
_SNAKE_KEY = rf"(?:\w*_\w*{_CORE}\w*|\w*{_CORE}\w*_\w*)"


# Glyphs that open a rendered transcript line: a user turn (`❯`, the form
# current renders draw; `>` is kept for renders that draw that instead), a
# tool result (`⎿`) and an assistant turn (`●`, or `⏺` on macOS).
# Known limit: the runner's ANSI strip drops cursor moves, so a TUI redraw can
# glue transcript text onto the END of a live line, where a line-start mask
# cannot see it.
_PROMPT = ("\u276f", ">")
_TRANSCRIPT = _PROMPT + ("\u23bf", "\u25cf", "\u23fa")


def _is_terminal(line: str) -> bool:
    return any(re.search(p, line, re.I) for p in TERMINAL)


def _is_failure(line: str) -> bool:
    return any(re.search(p, line, re.I) for p in FAILURE)


def _key_value(m: re.Match) -> str:
    """Redact a credential key's value, except a plain number under a key that
    names a count (`token_count: 512`, `max_tokens 4096`), and except when the
    "key" is a file name (`credentials.json: Permission denied`)."""
    key, value = m.group(1), m.group(2)
    if re.search(r"\.\w{2,5}[\"']?[ \t]*[:=]?[ \t]*$", key):
        return m.group(0)
    if value.isdigit() and re.search(r"(?i)count|max|min|limit|len|size|num|total", key):
        return m.group(0)
    return key + "[REDACTED]"


def redact(s: str) -> str:
    # URL userinfo (scheme://user:secret@host), the shape a proxy URL in a
    # network error prints.
    # The password may itself hold `@` or `/`: it runs to the `@` that is
    # followed by a host.
    s = re.sub(r"(\b[a-z][\w+.-]*://[^:/\s@]*:)\S+?@(?=[\w.-]+(?::\d+)?(?:[/\s'\"]|$))", r"\1[REDACTED]@", s, flags=re.I)
    # A token used as the whole userinfo (https://<token>@host).
    s = re.sub(r"(\b[a-z][\w+.-]*://)[^\s/@:]{16,}@", r"\1[REDACTED]@", s, flags=re.I)
    # A cookie header carries several pairs; all of them go.
    s = re.sub(r"(?im)(\b(?:set-)?cookie[ \t]*:[ \t]*)(\S[^\n]*?)[ \t]*$", r"\1[REDACTED]", s)
    # Authorization with or without a scheme word (Bearer, Basic, Token ...).
    s = re.sub(r"(?i)(authorization[ \t]*[:=][ \t]*)(\w+[ \t]+)?\S+", r"\1[REDACTED]", s)
    s = re.sub(r"(?i)\b(bearer|basic)[ \t]+[A-Za-z0-9._~+/=-]{6,}", r"\1 [REDACTED]", s)
    # A password, passphrase or secret can hold spaces and may be all digits
    # (a PIN): its value runs to the end of the line, and only a status word
    # standing alone is kept ("password: required").
    s = re.sub(
        rf"(?im)((?:\w*(?:passphrase|password|passwd|secret)|\bpin)\b[\"']?[ \t]*[:=][ \t]*)"
        rf"(?!(?:{_STATUS}|SIG[A-Z]+|(?:[/~]|[A-Za-z]:[\\/])\S*[/\\]\S*)[ \t]*$)(?!\[REDACTED)(\S[^\n]*?)[ \t]*$",
        r"\1[REDACTED]",
        s,
    )
    # key=value / key: value, where the key names a credential. A quoted value
    # runs to its closing quote. Whitespace separates key and value only for a
    # flag (--token X) or a snake/env-style name (session_token X), never for a
    # prose word ("OAuth token has expired").
    s = re.sub(rf"(?i)({_KEY}[\"']?[ \t]*[:=][ \t]*)([\"'])(.*?)\2", r"\1\2[REDACTED]\2", s)
    s = re.sub(rf"(?i)({_KEY}[\"']?[ \t]*[:=][ \t]*)(?![\"'\[]){_EVIDENCE_VALUE}([^\s\"'&;,]+)", _key_value, s)
    s = re.sub(rf"(?i)((?:--?{_KEY}|\b{_SNAKE_KEY}|\b(?:api[_-]?key|auth[_-]?token|access[_-]?token|refresh[_-]?token|client[_-]?secret)\b)[ \t]+)(?![-=:\[]){_EVIDENCE_VALUE}([^\s\"'&;,]+)", _key_value, s)
    # `-p<secret>` glued to a short password flag, as mysql and friends take it.
    s = re.sub(r"(?<![\w-])(-p)(?!rint0?\b)(?=[^\s\"'&;,]*[A-Za-z])(?=[^\s\"'&;,]*\d)(?=\S{4,})[^\s\"'&;,]+", r"\1[REDACTED]", s)
    # An OAuth callback's authorization code, plain or URL-encoded.
    s = re.sub(r"(?i)([?&](?:code|access_token|refresh_token|id_token)(?:=|%3D))[^&\s\"']+", r"\1[REDACTED]", s)
    # `auth <value>` and `Bearer: <value>`.
    s = re.sub(rf"(?i)\b(auth|bearer)[ \t]*:?[ \t]+(?![-=\[]){_EVIDENCE_VALUE}([^\s\"'&;,]{{6,}})", r"\1 [REDACTED]", s)
    # Well-known key prefixes.
    s = re.sub(r"\bsk-[A-Za-z0-9_-]{6,}", "[REDACTED]", s)
    s = re.sub(r"\bsk_(?:live|test)_[A-Za-z0-9]{6,}", "[REDACTED]", s)
    s = re.sub(r"\bAIza[0-9A-Za-z_-]{30,}", "[REDACTED]", s)
    s = re.sub(r"\bglpat-[0-9A-Za-z_-]{10,}", "[REDACTED]", s)
    s = re.sub(r"\b(?:ghp|gho|ghu|ghs|ghr)_[0-9A-Za-z]{20,}", "[REDACTED]", s)
    s = re.sub(r"\bgithub_pat_[0-9A-Za-z_]{20,}", "[REDACTED]", s)
    s = re.sub(r"\bnpm_[0-9A-Za-z]{20,}", "[REDACTED]", s)
    s = re.sub(r"\bdckr_pat_[0-9A-Za-z_-]{20,}", "[REDACTED]", s)
    s = re.sub(r"\bhf_[0-9A-Za-z]{20,}", "[REDACTED]", s)
    s = re.sub(r"\bxox[abposr]-[0-9A-Za-z-]{10,}", "[REDACTED]", s)
    s = re.sub(r"\beyJ[\w-]+\.[\w-]+\.[\w-]*", "[REDACTED-JWT]", s)
    s = re.sub(r"\bAKIA[0-9A-Z]{12,}\b", "[REDACTED]", s)
    # Long opaque runs. Path segments (`/`, `\\`, `.`) survive, because a missing
    # path is the evidence of an ENOENT; UUIDs and git shas are kept too.
    kept: dict[str, str] = {}

    def _keep(m: re.Match) -> str:
        key = f"\x00{len(kept)}\x00"
        kept[key] = m.group(0)
        return key

    def _flips(x: str) -> tuple[int, int]:
        ls = re.sub(r"[^A-Za-z]", "", x)
        return sum(1 for a, b in zip(ls, ls[1:]) if a.isupper() != b.isupper()), len(ls)

    _WORDISH = re.compile(r"(?:Mc|Mac|[A-Z]{1,4}[a-z]{2,}|[a-z]{2,})+[0-9]{0,4}|[0-9]{1,4}")

    def _random_piece(pm: re.Match) -> str:
        # A path segment, split on `-`, `_` and `.` into sub-pieces. A sub-piece
        # is random when it mixes case in a way words do not
        # (`2DTsDk1V`); words, CamelCase names (`AppData`, `McDonald`,
        # `Claude2`) and numbers are kept. When any sub-piece is random, every
        # sub-piece holding a capital or a digit goes too, so a base64url
        # secret cut up by its own `-` and `_` does not survive in pieces.
        seg = pm.group(0)
        subs = re.split(r"([-_.])", seg)
        def random_sub(x: str) -> bool:
            # Lower-case hex or uuid pieces are evidence; mixed case, capitals
            # with digits, and base64's `+` or `=` padding are not.
            if len(x) < 4 or _WORDISH.fullmatch(x):
                return False
            up, low, dig = (bool(re.search(c, x)) for c in (r"[A-Z]", r"[a-z]", r"[0-9]"))
            return (up and low) or (up and dig) or "+" in x or x.endswith("=")
        if not any(random_sub(x) for x in subs[::2]):
            return seg
        return "".join(
            "[REDACTED]" if i % 2 == 0 and x and (random_sub(x) or re.search(r"[A-Z0-9]", x)) else x
            for i, x in enumerate(subs)
        )

    def _opaque(m: re.Match) -> str:
        # A secret is a long run that mixes upper and lower case at random:
        # its letters flip case about every other character. Words, paths and
        # CamelCase names flip rarely, and an ERROR_CODE, a plan stem, a git sha
        # or a sha256 digest has only one case, so all of those are kept.
        run = m.group(0)
        segs = [x for x in re.split(r"[/\\]", run) if x]
        # (A real path always holds a plain lower-case word segment such as
        # `usr`, `lib`, `home` or `node_modules`; a random base64 run that
        # happens to start with `/` almost never does.)
        if (
            len(segs) >= 3
            and re.match(r"(?:[/~@\\]|[A-Za-z]:[/\\])", run)
            and any(re.fullmatch(r"[a-z][a-z_.-]{2,}", x) for x in segs)
        ):
            # A path: judge it piece by piece, so a random piece inside it (an
            # npm temp dir such as `.claude-code-2DTsDk1V`) is redacted alone
            # and the rest, which is the ENOENT evidence, survives.
            return re.sub(r"[A-Za-z0-9+=_.-]+", _random_piece, run)
        if not (re.search(r"[A-Z]", run) and re.search(r"[a-z]", run)):
            return run
        flips, n = _flips(run)
        if flips <= 0.12 * n:
            return run
        # A CamelCase name made only of letters (UnhandledPromiseRejectionWarning).
        if re.fullmatch(r"(?:[A-Z]{1,3}[a-z]{2,})+", run):
            return run
        # A relative path of CamelCase words: three or more segments, and in
        # each one every capital starts a word of at least two lower-case letters.
        camel = re.compile(r"(?:[A-Z]{1,3}[a-z]{2,}|[A-Z]{2,6}(?![a-z])|[a-z0-9_.-]+)+")
        if len(segs) >= 3 and all(camel.fullmatch(x) for x in segs):
            return run
        return "[REDACTED]"

    s = _KEEP.sub(_keep, s)
    s = re.sub(r"(?<![\w/\\+=.@-])[A-Za-z0-9+/\\=_.@-]{32,}(?![\w/\\+=@-])", _opaque, s)
    for key, val in kept.items():
        s = s.replace(key, val)
    return s


def _find_anchor(lines: list[str], cmd: str) -> int | None:
    """Index of the echo line. The FIRST line ending with the command wins,
    whatever else it contains (a prompt such as `~/ssl-proxy $` must not
    disqualify it)."""
    # Prefer an echo: a prompt character before the command. An rc file's own
    # "zsh: command not found: claude" printed before the prompt must not win.
    prompt_echo = re.compile(r"[$%#>❯][ \t]*" + re.escape(cmd) + r"[ \t]*$")
    for i, line in enumerate(lines):
        if prompt_echo.search(line):
            return i
    for i, line in enumerate(lines):
        if line.rstrip().endswith(cmd):
            return i
    # A long echo can be split across PTY lines or redrawn with `\r`: compare
    # with whitespace removed and stop at the first line that completes it.
    squashed = re.sub(r"\s+", "", cmd)
    if not squashed:
        return None
    joined = ""
    for i, line in enumerate(lines):
        joined += re.sub(r"\s+", "", line)
        if squashed in joined:
            return i
        if len(joined) > 4 * len(squashed):
            joined = joined[-2 * len(squashed):]
    return None


def scan(text: str, cmd: str) -> str:
    cmd = cmd.strip()
    if not cmd:
        return "no-echo"
    lines = text.splitlines()
    anchor = _find_anchor(lines, cmd)
    if anchor is None:
        return "no-echo"
    # A shell's own not-found error can be the first line ending with the
    # command when the echo itself is missing ("zsh: command not found: claude").
    if _NOT_FOUND.search(lines[anchor]) and not re.search(r"[$%#>]\s*\S*" + re.escape(cmd) + r"\s*$", lines[anchor]):
        return "failed:" + redact(lines[anchor].strip())[:200]
    after = lines[anchor + 1:]
    # Transcript blocks a `--resume`/`--continue` retry replays: a line opening
    # with a _TRANSCRIPT glyph, and every following line indented deeper than that
    # marker, across blank lines, until a line at or left of its column. None
    # of it is the launch's own output. (An indented live screen right after a
    # block is masked too; that fails toward UNKNOWN, never toward `failed`.)
    # Only a resumed session replays history. A fresh launch's own `⎿ API
    # Error: 401` lines are its output; only its `❯`/`>` prompt redraw is masked.
    resumed = re.search(r"(?:^|\s)(?:--resume|--continue|-r|-c)(?:\s|=|$)", cmd) is not None
    live_mask: list[bool] = []
    block_col: int | None = None
    for line in after:
        stripped = line.lstrip()
        col = len(line) - len(stripped)
        if not resumed:
            # A fresh launch replays nothing, but redraws its own prompt as a
            # `❯ <prompt>` (or `> <prompt>`) line; that line is the operator's
            # text, not output.
            live_mask.append(not stripped.startswith(_PROMPT))
        elif not stripped:
            live_mask.append(True)  # a blank line inside a block keeps it open
        elif stripped.startswith(_TRANSCRIPT):
            block_col = col if block_col is None else min(block_col, col)
            live_mask.append(False)
        elif block_col is not None and stripped and col > block_col:
            live_mask.append(False)
        else:
            if stripped:
                block_col = None
            live_mask.append(True)
    ready_at = next(
        (i for i, line in enumerate(after) if live_mask[i] and any(re.search(p, line, re.I) for p in READY)), None
    )
    before_ready = after if ready_at is None else after[:ready_at]
    live_before = [line for line, ok in zip(before_ready, live_mask) if ok]
    if re.search(REFUSED, "\n".join(live_before), re.I):
        return "refused-session-id"
    # A missing binary named by its own path ("bash: <dir>/claude: No such file or
    # directory") is terminal, like "command not found".
    # argv0 skips leading VAR=value words and an `env` wrapper; the name must
    # stand as a whole path component, optionally quoted (`env: 'claude': ...`),
    # so `.../.claude: No such file or directory` does not match.
    words = [t for t in cmd.split() if "=" not in t]
    while words and words[0] in ("env", "command", "exec", "nohup"):
        words = [t for t in words[1:] if "=" not in t]
    argv0 = words[0].strip("'\"") if words else ""
    missing = (
        re.compile(r"(?<![\w.-])['\"]?" + re.escape(argv0) + r"['\"]?: No such file or directory") if argv0 else None
    )
    # A `--resume` retry replays old messages, above and below the ready
    # marker, and they can quote any failure text: transcript lines never
    # count. Above the ready marker every TERMINAL line counts; below it only
    # Claude's own BLOCKING screens do (a TUI can draw a login notice below its
    # footer in the same redraw).
    before_terminal = [line for line in live_before if _is_terminal(line) or (missing and missing.search(line))]
    after_ready = [] if ready_at is None else [
        line for line, ok in zip(after[ready_at + 1:], live_mask[ready_at + 1:]) if ok
    ]
    late = [line for line in after_ready if any(re.search(p, line, re.I) for p in BLOCKING)]
    terminal = before_terminal or late
    if terminal:
        return "failed:" + redact(terminal[0].strip())[:200]
    if ready_at is not None:
        return "seen"
    noise = [line for line in live_before if _is_failure(line)]
    if noise:
        return "noise:" + redact(noise[0].strip())[:200]
    return ""

def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[1] not in ("scan", "redact"):
        print("usage: analyze-launch-failure.py scan|redact  (stdin; scan reads RETRY_CMD)", file=sys.stderr)
        return 4
    data = sys.stdin.read()
    if argv[1] == "redact":
        sys.stdout.write(redact(data))
        return 0
    out = scan(data, os.environ.get("RETRY_CMD", ""))
    if out:
        print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
