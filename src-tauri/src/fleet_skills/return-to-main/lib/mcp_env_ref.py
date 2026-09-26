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


def url_is_loopback(url):
    """True iff `url` is strictly this box's loopback (see the module doc)."""
    if not isinstance(url, str):
        return False
    m = _SCHEME.match(url)
    if not m:
        return False
    rest = url[m.end():]
    cut = len(rest)
    for ch in "/?#":
        i = rest.find(ch)
        if i != -1 and i < cut:
            cut = i
    auth = rest[:cut]
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


def expand_env_ref_for_url(value, url, environ=None):
    """expand_env_ref for a value about to be sent to `url`: the environment
    arm only for a strictly-loopback URL, the DEFAULT arm otherwise."""
    return expand_env_ref(value, environ if url_is_loopback(url) else {})


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
