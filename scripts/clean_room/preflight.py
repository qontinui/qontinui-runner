"""Assert the box is FOREIGN before the clean-room scenario measures anything.

A clean-room run on a dirty box is not a pass: if a sibling checkout, a
dev-stack listener, the supervisor or a QONTINUI_* variable is present, the
published runner may be quietly leaning on it, and the run would report the
maintainers' environment rather than an external operator's.

Every predicate is derived from qontinui-schemas `fleet-nouns.toml` rather than
a list kept here:

  * SIBLING CHECKOUTS. Each directory entry under each probe root is tested as
    the relative path `../<name>` against the `repo_layout` class -- the class
    that exists to recognise a sibling checkout (`../qontinui-schemas`,
    `../ui-bridge`, `qontinui-root`, ...). A new sibling added to the vocabulary
    is picked up here with no edit.
  * DEV PORTS. Every TCP port p for which `127.0.0.1:<p>` hits `dev_ports` or
    `supervisor_dependency` is probed for a listener, on the IPv4 and IPv6
    loopback. The port list is therefore whatever the vocabulary says it is.
  * The product's own port (9876, a `product_constant` in the vocabulary) must
    also be FREE before install: a runner already answering there would be
    measured instead of the one this run installs.
  * ENVIRONMENT. Any variable whose name starts with `QONTINUI_`.

Three verdicts: `clean`, `not_clean` (-> every scenario step is
unknown(box_not_clean)), and `incomplete` (a probe itself could not run ->
unknown(preflight_incomplete)). A probe that could not run is never read as
"nothing found".

The probe functions are injectable so the verdict logic is unit-tested offline
(scripts/tests/test_clean_room.py); `main()` wires the real ones.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import string
import sys
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from clean_room.fleet_nouns import (
        Vocabulary,
        VocabularyUnavailable,
        load_vocabulary,
    )
else:
    from .fleet_nouns import Vocabulary, VocabularyUnavailable, load_vocabulary

PRODUCT_PORT = 9876
SIBLING_CLASS = "repo_layout"
PORT_CLASSES = ("dev_ports", "supervisor_dependency")
ENV_PREFIX = "QONTINUI_"

# Listing a directory: returns entry names that are directories, or raises.
ListDirs = Callable[[str], list[str]]
# Probing a port: True = something accepted a connection, False = refused.
# Raises when the probe could not decide (timeout, unexpected socket error).
ProbePort = Callable[[str, int], bool]


@dataclass
class PreflightResult:
    verdict: str  # clean | not_clean | incomplete
    findings: list[dict] = field(default_factory=list)
    probe_errors: list[dict] = field(default_factory=list)
    probed: dict = field(default_factory=dict)
    # Sibling-named directories that ARE on the box and were declared by the
    # caller as the measuring instrument (the harness checkout itself), each
    # with its reason. Recorded, never hidden; they do not make the box dirty.
    acknowledged: list[dict] = field(default_factory=list)

    def to_json(self) -> dict:
        return {
            "verdict": self.verdict,
            "findings": self.findings,
            "acknowledged": self.acknowledged,
            "probe_errors": self.probe_errors,
            "probed": self.probed,
        }


def _norm(path: str) -> str:
    return os.path.normcase(os.path.normpath(path))


def vocabulary_ports(vocab: Vocabulary) -> list[int]:
    """Every TCP port the vocabulary calls a dev-stack or supervisor port."""
    ports = [
        p
        for p in range(1, 65536)
        if any(vocab.hits_class(cid, f"127.0.0.1:{p}") for cid in PORT_CLASSES)
    ]
    if not ports:
        # A vocabulary that names no port at all is a broken vocabulary, not a
        # clean box: probing nothing would certify everything.
        raise VocabularyUnavailable(
            f"{vocab.path}: classes {PORT_CLASSES} match no `127.0.0.1:<port>` at all"
        )
    return ports


def assess(
    vocab: Vocabulary,
    probe_roots: Iterable[str],
    env: Mapping[str, str],
    list_dirs: ListDirs,
    probe_port: ProbePort,
    hosts: Iterable[str] = ("127.0.0.1", "::1"),
    acknowledge: Mapping[str, str] | None = None,
) -> PreflightResult:
    """`acknowledge` maps a directory path to the reason it may exist: only an
    EXACT path match (case- and separator-normalised) is excused, never a name,
    so a second sibling-named directory anywhere is still a finding."""
    ack = {_norm(k): (k, v) for k, v in (acknowledge or {}).items()}
    acknowledged: list[dict] = []
    findings: list[dict] = []
    errors: list[dict] = []
    roots = list(dict.fromkeys(probe_roots))
    ports = vocabulary_ports(vocab) + [PRODUCT_PORT]
    hosts = list(hosts)

    for root in roots:
        try:
            names = list_dirs(root)
        except FileNotFoundError:
            continue  # an absent probe root holds no sibling; stated in `probed`
        except OSError as exc:
            errors.append({"probe": "sibling_dirs", "root": root, "error": str(exc)})
            continue
        for name in names:
            if vocab.hits_class(SIBLING_CLASS, f"../{name}"):
                path = str(Path(root) / name)
                if _norm(path) in ack:
                    acknowledged.append({"path": path, "why": ack[_norm(path)][1]})
                    continue
                findings.append(
                    {
                        "kind": "sibling_checkout",
                        "path": path,
                        "class_id": SIBLING_CLASS,
                    }
                )

    for port in ports:
        for host in hosts:
            try:
                listening = probe_port(host, port)
            except OSError as exc:
                errors.append(
                    {"probe": "listener", "host": host, "port": port, "error": str(exc)}
                )
                continue
            if listening:
                findings.append(
                    {
                        "kind": "product_port_occupied"
                        if port == PRODUCT_PORT
                        else "dev_listener",
                        "host": host,
                        "port": port,
                    }
                )

    for name in sorted(env):
        if name.upper().startswith(ENV_PREFIX):
            # The NAME only: a value may be a credential.
            findings.append({"kind": "qontinui_env", "name": name})

    probed = {
        "roots": roots,
        "ports": ports,
        "hosts": hosts,
        "env_prefix": ENV_PREFIX,
        "vocabulary": vocab.path,
        "vocabulary_version": vocab.version,
    }
    if findings:
        verdict = "not_clean"
    elif errors:
        verdict = "incomplete"
    else:
        verdict = "clean"
    return PreflightResult(
        verdict=verdict,
        findings=findings,
        probe_errors=errors,
        probed=probed,
        acknowledged=acknowledged,
    )


# ---------------------------------------------------------------------------
# Real probes.
# ---------------------------------------------------------------------------
def real_list_dirs(root: str) -> list[str]:
    with os.scandir(root) as it:
        out = []
        for e in it:
            try:
                if e.is_dir():
                    out.append(e.name)
            except OSError:
                # An entry we cannot stat (a dangling junction) is still a NAME
                # that could be a sibling checkout; judge it by name.
                out.append(e.name)
        return out


def real_probe_port(host: str, port: int, timeout: float = 5.0) -> bool:
    family = socket.AF_INET6 if ":" in host else socket.AF_INET
    try:
        s = socket.socket(family, socket.SOCK_STREAM)
    except OSError:
        if family == socket.AF_INET6:
            # No IPv6 stack on this box: nothing can listen on ::1.
            return False
        raise
    s.settimeout(timeout)
    try:
        s.connect((host, port))
        return True
    except ConnectionRefusedError:
        return False
    except OSError as exc:
        # EADDRNOTAVAIL on ::1 when the loopback has no IPv6 address: no listener
        # can exist there either.
        if family == socket.AF_INET6 and getattr(exc, "errno", None) in (99, 10049):
            return False
        raise
    finally:
        s.close()


def filesystem_roots() -> list[str]:
    if os.name == "nt":
        listdrives = getattr(os, "listdrives", None)
        if listdrives is not None:
            return list(listdrives())
        return [f"{d}:\\" for d in string.ascii_uppercase if os.path.exists(f"{d}:\\")]
    return ["/"]


def default_probe_roots(extra: Iterable[str]) -> list[str]:
    home = Path.home()
    roots = [str(p) for p in extra]
    roots += [str(home), str(home / "Projects"), str(home / "source" / "repos")]
    roots += filesystem_roots()
    return roots


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--vocabulary", required=True, help="path to qontinui-schemas fleet-nouns.toml"
    )
    ap.add_argument(
        "--probe-root",
        action="append",
        default=[],
        help="an extra directory whose entries are checked for sibling checkouts "
        "(the foreign repo's parent and the runner's working directory belong here)",
    )
    ap.add_argument(
        "--acknowledge",
        action="append",
        default=[],
        metavar="PATH=WHY",
        help="a sibling-named directory that is the measuring instrument itself "
        "(the harness checkout), excused by exact path and recorded with its reason",
    )
    ap.add_argument(
        "--harness-checkout",
        help="the harness's own checkout. GitHub names it (and the directory holding "
        "it) after the repo, so both are sibling-named. Its parent and grandparent "
        "become probe roots -- anything ELSE sibling-named there is a finding -- and "
        "the checkout and its parent are acknowledged by exact path.",
    )
    ap.add_argument("--out", required=True, help="where to write the preflight JSON")
    args = ap.parse_args(argv)
    acknowledge = {}
    extra_roots = list(args.probe_root)
    if args.harness_checkout:
        checkout = Path(args.harness_checkout).resolve()
        extra_roots += [str(checkout.parent), str(checkout.parent.parent)]
        acknowledge[str(checkout)] = (
            "the harness checkout: the measuring instrument, named after the repo by "
            "GitHub; the runner is never launched from it or pointed at it"
        )
        acknowledge[str(checkout.parent)] = (
            "the directory GitHub creates to hold the harness checkout, named after the repo"
        )
    for item in args.acknowledge:
        path, sep, why = item.partition("=")
        if not sep or not path or not why:
            ap.error(f"--acknowledge wants PATH=WHY, got {item!r}")
        acknowledge[path] = why

    try:
        vocab = load_vocabulary(args.vocabulary)
        result = assess(
            vocab,
            default_probe_roots(extra_roots),
            dict(os.environ),
            real_list_dirs,
            real_probe_port,
            acknowledge=acknowledge,
        ).to_json()
    except VocabularyUnavailable as exc:
        result = {
            "verdict": "vocabulary_unavailable",
            "findings": [],
            "probe_errors": [{"probe": "vocabulary", "error": str(exc)}],
            "probed": {},
        }

    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"clean-room preflight: {result['verdict']}")
    for f in result["findings"]:
        print(f"  finding: {json.dumps(f)}")
    for e in result["probe_errors"]:
        print(f"  probe error: {json.dumps(e)}")
    # Reporting only: the verdict travels in the JSON, never as a red step.
    return 0


if __name__ == "__main__":
    sys.exit(main())
