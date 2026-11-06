"""The python owner of `${NAME}` / `${NAME:-default}` expansion for .mcp.json
nonce reads - the twin of scripts/lib/mcp-env-ref.sh, pinned to the same case
table by scripts/mcp-env-ref-test.sh.

ascii-only-source

Plan 2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages.
The runner writes the coord-mcp entry's nonce (http headers) and the stdio
shim's `--credential` path as `${QONTINUI_COORD_MCP_NONCE_<K>:-<workdir nonce>}` /
`${QONTINUI_COORD_MCP_CREDENTIAL_<K>:-<file>}` (K = 16 uppercase hex; older
docs carry the bare name, older still a literal). The expander is generic over
the variable name. Claude Code expands those from its
own environment; a door that reads the same file must expand them from ITS own
environment the same way, or it sends the literal reference as a bearer.

Semantics (see mcp-env-ref.sh for the full statement):
  ${NAME:-default}  value when NAME is ALLOWLISTED and set non-empty, else
                    `default`
  ${NAME}           the same lookup, else EnvRefError
                    ("UNEXPANDED_ENV_REF <NAME>: ...") - never the literal text
  no `${`           passthrough; `${` without a closing `}` is literal
  malformed name    (`${X-v}`, `${X:=v}`) EnvRefError naming
                    "<malformed reference>" - the body is never echoed

ALLOWLISTED names only: ^QONTINUI_COORD_MCP_(NONCE|CREDENTIAL)(_[0-9A-F]{16})?$
is the only name ever read from the environment; every other well-formed name
reads as UNSET. The sweep doors read sibling .mcp.json files (a cloned repo's
included) and send the expanded header to that file's URL, so a generic lookup
would let `Bearer ${GH_TOKEN:-}` exfiltrate this process's token.

URL gate - no environment value is ever sent off-box: every door that sends
the expanded value to a URL read from the same file uses expand_env_ref_for_url,
which reads the environment only when url_is_loopback(url) holds (a leading
case-insensitive http:// or https://; the authority up to the first of / ? #;
no userinfo, no character outside [A-Za-z0-9.:[]-], a numeric port; host
exactly localhost, [::1] or 127.a.b.c with CANONICAL decimal octets <= 255 -
no leading zero, which would send `127.08.0.1` to DNS) and resolves on the
DEFAULT arm otherwise. Same rule as mcp_url_is_loopback in mcp-env-ref.sh.

PORT gate - the environment's nonce goes to the HOSTING RUNNER only (finding
e4c596d6; plan 2026-11-06-a-walked-up-mcp-json-gets-the-sessions-nonce-on-any-loopback-port):
loopback alone is not enough, because the doors find their .mcp.json by
walking UP from a directory, and a file planted in a directory the walk
reaches first could name `http://127.0.0.1:<any port>/x` with this terminal's
`${QONTINUI_COORD_MCP_NONCE_<K>}` and receive it. The terminal nonce has ONE
audience, the runner that hosts this terminal, whose API port (the port its
coord-mcp proxy is served on) the runner exports as $QONTINUI_RUNNER_API_PORT.
So expand_env_ref_for_url reads the environment only when
url_port_is_hosting_runner(url) holds: the URL is loopback (above), its host is
EXACTLY 127.0.0.1 (the one host the runner writes and binds - `localhost`,
`[::1]` and 127.0.0.2 are loopback a planted listener can hold on the runner's
port), AND its effective port (the explicit one, else 80 for http / 443 for https) equals
$QONTINUI_RUNNER_API_PORT (1-5 ASCII digits, 1..65535, compared as integers,
read from the same mapping the expansion reads). Every other URL resolves on
the DEFAULT arm - the file's own literal, never this process's secret. An unset
or malformed $QONTINUI_RUNNER_API_PORT reads the environment for no URL.
Same rule as mcp_url_port_is_hosting_runner in mcp-env-ref.sh and
Test-CoordMcpHostingRunnerUrl in coord-credential.psm1. Only the `127.0.0.1`
SPELLING gets the environment value: a `localhost:<runner port>` door resolves
on its default unless its caller normalizes the host first (coord-mcp-shim.py
does). $QONTINUI_RUNNER_API_PORT is trusted as the host's port, so an operator
who points it at another runner directs the terminal nonce there (a planted
file cannot set it).

The error message names the VARIABLE only - never a value, never the default
(the default is a nonce).
"""

import os
import re

UNEXPANDED_ENV_REF = "UNEXPANDED_ENV_REF"
MALFORMED_REFERENCE = "<malformed reference>"

_IDENTIFIER = re.compile(r"\A[A-Za-z_][A-Za-z0-9_]*\Z")
_ALLOWLIST = re.compile(r"\AQONTINUI_COORD_MCP_(NONCE|CREDENTIAL)(_[0-9A-F]{16})?\Z")


_SCHEME = re.compile(r"\A[Hh][Tt][Tt][Pp][Ss]?://")
_AUTHORITY_CHARS = re.compile(r"\A[A-Za-z0-9.:\[\]-]+\Z")
_PORT = re.compile(r"\A[0-9]+\Z")
# CANONICAL decimal octets only: `127.08.0.1` is not an address to
# curl/getaddrinfo, so it would be sent to DNS. `[0-9]` in `re` is the ASCII
# codepoint range (never `\d`, which matches U+0661 and friends).
_OCTET = r"(0|[1-9][0-9]{0,2})"
_LOOPBACK_V4 = re.compile(r"\A127\." + _OCTET + r"\." + _OCTET + r"\." + _OCTET + r"\Z")


def _authority(url):
    """(scheme length, authority) of `url`, or None without an http(s) scheme."""
    m = _SCHEME.match(url)
    if not m:
        return None
    rest = url[m.end():]
    cut = len(rest)
    for ch in "/?#":
        i = rest.find(ch)
        if i != -1 and i < cut:
            cut = i
    return m.end(), rest[:cut]


def url_is_loopback(url):
    """True iff `url` is strictly this box's loopback (see the module doc)."""
    if not isinstance(url, str):
        return False
    parsed = _authority(url)
    if parsed is None:
        return False
    auth = parsed[1]
    if not _AUTHORITY_CHARS.match(auth):
        return False
    if auth == "[::1]":
        return True
    if auth.startswith("[::1]:"):
        return bool(_PORT.match(auth[6:]))
    if "[" in auth or "]" in auth:
        return False
    host, sep, port = auth.partition(":")
    if sep and not _PORT.match(port):
        return False
    if host.lower() == "localhost":
        return True
    v4 = _LOOPBACK_V4.match(host)
    return bool(v4) and all(int(o) <= 255 for o in v4.groups())


RUNNER_API_PORT_ENV = "QONTINUI_RUNNER_API_PORT"
RUNNER_HOST = "127.0.0.1"
_API_PORT = re.compile(r"\A[0-9]{1,5}\Z")


def url_port_is_hosting_runner(url, environ=None):
    """True iff `url` is loopback AND its effective port is the hosting
    runner's $QONTINUI_RUNNER_API_PORT, read from `environ` (default
    os.environ). See "PORT gate" in the module doc."""
    if not url_is_loopback(url):
        return False
    env = os.environ if environ is None else environ
    api = env.get(RUNNER_API_PORT_ENV) or ""
    if not _API_PORT.match(api) or not 1 <= int(api) <= 65535:
        return False
    scheme_len, auth = _authority(url)
    default = 443 if scheme_len == len("https://") else 80
    host, _, port = auth.partition(":")
    # Exactly the host the runner writes and binds. `localhost`, `[::1]` and
    # any other 127.x.y.z are loopback too, but a planted listener can hold
    # them on the runner's port while the runner holds only 127.0.0.1.
    if host != RUNNER_HOST:
        return False
    if not port:
        return default == int(api)
    # url_is_loopback already holds the port to ASCII digits; bound its length
    # before int() so a 4000-digit port is a plain mismatch.
    return len(port) <= 5 and int(port) == int(api)


def expand_env_ref_for_url(value, url, environ=None):
    """expand_env_ref for a value about to be sent to `url`: the environment
    arm only for the HOSTING RUNNER's loopback port, the DEFAULT arm for every
    other URL (see "URL gate" and "PORT gate" in the module doc)."""
    return expand_env_ref(value, environ if url_port_is_hosting_runner(url, environ) else {})


def env_ref_allowlisted(name):
    """True iff `name` may be read from the environment at all."""
    return bool(_ALLOWLIST.match(name or ""))


class EnvRefError(ValueError):
    """An env reference with no default whose variable is unset or empty."""

    def __init__(self, name):
        self.name = name
        if name == MALFORMED_REFERENCE:
            super().__init__(
                "%s %s: the .mcp.json value carries a ${...} whose name part is not "
                "an identifier - refusing to send it as a credential (the body is "
                "withheld: it may hold a nonce)" % (UNEXPANDED_ENV_REF, name)
            )
            return
        super().__init__(
            "%s %s: the .mcp.json value references ${%s} with no default and %s "
            "is unset, empty or not an allowlisted name in this process's environment - refusing to send "
            "the literal reference as a credential (a LOCAL fact about this "
            "environment, not a coord verdict)" % (UNEXPANDED_ENV_REF, name, name, name)
        )


def env_ref_present(value):
    """True iff `value` carries a `${...}` reference."""
    if not isinstance(value, str):
        return False
    start = value.find("${")
    return start != -1 and value.find("}", start + 2) != -1


def expand_env_ref(value, environ=None):
    """Expand every `${NAME}` / `${NAME:-default}` in `value`.

    Raises EnvRefError for a no-default reference whose variable is unset or
    empty. A non-string is returned unchanged.
    """
    if not isinstance(value, str) or "${" not in value:
        return value
    env = os.environ if environ is None else environ
    out = []
    rest = value
    while True:
        start = rest.find("${")
        if start == -1:
            out.append(rest)
            break
        end = rest.find("}", start + 2)
        if end == -1:
            out.append(rest)  # unterminated: literal
            break
        out.append(rest[:start])
        body = rest[start + 2:end]
        rest = rest[end + 1:]
        if ":-" in body:
            name, default = body.split(":-", 1)
            has_default = True
        else:
            name, default, has_default = body, "", False
        if not _IDENTIFIER.match(name):
            raise EnvRefError(MALFORMED_REFERENCE)
        current = (env.get(name) or "") if env_ref_allowlisted(name) else ""
        if current:
            out.append(current)
        elif has_default:
            out.append(default)
        else:
            raise EnvRefError(name)
    return "".join(out)
