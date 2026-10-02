"""HTTP to the runner under test, with every response body observed.

Everything the scenario reads from the runner passes through `Runner.request`,
which hands the body to an `Observer`. The observer keeps each text (as an
evidence file) so the fleet-noun scan covers every served response and every
rendered UI string of the run -- the dynamic half of Phase A -- rather than
only the fields a step happened to assert on.

`Transport` is the one seam the offline tests replace: they hand in a fake
that answers from a table, so the step logic runs with no socket.
"""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol


class TransportError(Exception):
    """No HTTP answer at all: refused, reset, timed out."""


@dataclass
class Response:
    status: int
    body: str
    json: Any = None  # parsed body, or None when the body is not JSON

    @property
    def envelope_data(self) -> Any:
        """`data` of the runner's ApiResponse envelope, else the body itself."""
        if isinstance(self.json, dict) and "data" in self.json:
            return self.json["data"]
        return self.json


class Transport(Protocol):
    def __call__(
        self, method: str, url: str, body: bytes | None, timeout: float
    ) -> tuple[int, str]: ...


def urllib_transport(
    method: str, url: str, body: bytes | None, timeout: float
) -> tuple[int, str]:
    req = urllib.request.Request(url, data=body, method=method)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as e:
        try:
            text = e.read().decode("utf-8", errors="replace")
        except OSError:
            text = ""
        return e.code, text
    except (urllib.error.URLError, OSError, TimeoutError) as e:
        raise TransportError(f"{method} {url}: {e}") from e


@dataclass
class Observation:
    step: str
    source: str
    ref: str
    # "ui": a 2xx rendered-page text; "http": any other served body (and the
    # harness's own listings). The fleet-noun verdict needs at least one "ui".
    kind: str = "http"


@dataclass
class Observer:
    """Records every text the run saw, as numbered evidence files."""

    evidence_dir: Path
    redactions: list[tuple[re.Pattern[str], str]] = field(default_factory=list)
    observations: list[Observation] = field(default_factory=list)
    texts: list[tuple[Observation, str]] = field(default_factory=list)

    def add_redaction(self, literal: str, placeholder: str) -> None:
        """Replace the box's OWN paths before scanning.

        The runner legitimately reports where it runs: the CI user's home, the
        temp dir, the fixture path, the install dir. On an external operator's
        box those are the operator's own paths, not fleet nouns, but the
        vocabulary's machine_path class cannot tell `C:\\Users\\runneradmin`
        from a maintainer's profile. Each such path is replaced by a named
        placeholder, matched case-insensitively with either slash, so only
        strings the box did not supply are left to hit. A planted
        `D:/qontinui-root` is not one of them.
        """
        if not literal or len(literal) < 4:
            return
        parts = [re.escape(p) for p in re.split(r"[\\/]+", literal.rstrip("\\/")) if p]
        if not parts:
            return
        lead = r"[\\/]*" if literal[:1] in "\\/" else ""
        pat = re.compile(lead + r"(?:\\\\|\\|/)+".join(parts), re.IGNORECASE)
        self.redactions.append((pat, placeholder))
        # Longest first, so a nested path is replaced before its parent.
        self.redactions.sort(key=lambda rp: -len(rp[0].pattern))

    def redact(self, text: str) -> str:
        for pat, ph in self.redactions:
            text = pat.sub(ph, text)
        return text

    def observe(self, step: str, source: str, text: str, kind: str = "http") -> str:
        self.evidence_dir.mkdir(parents=True, exist_ok=True)
        n = len(self.observations) + 1
        safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", f"{step}-{source}")[:80]
        ref = f"evidence/{n:03d}-{safe}.txt"
        (self.evidence_dir / Path(ref).name).write_text(text, encoding="utf-8")
        obs = Observation(step=step, source=source, ref=ref, kind=kind)
        self.observations.append(obs)
        self.texts.append((obs, self.redact(text)))
        return ref


@dataclass
class Runner:
    base: str
    observer: Observer
    transport: Callable[..., tuple[int, str]] = urllib_transport
    step: str = "-"

    def request(
        self,
        method: str,
        path: str,
        payload: Any = None,
        timeout: float = 30.0,
        ui: bool = False,
    ) -> tuple[Response, str]:
        """`ui=True` marks a rendered-page read: its body counts as UI text for
        the fleet-noun verdict only when the runner answered 2xx."""
        body = json.dumps(payload).encode("utf-8") if payload is not None else None
        status, text = self.transport(method, self.base + path, body, timeout)
        kind = "ui" if ui and 200 <= status < 300 else "http"
        ref = self.observer.observe(self.step, f"{method} {path} {status}", text, kind)
        try:
            parsed = json.loads(text) if text.strip() else None
        except ValueError:
            parsed = None
        return Response(status=status, body=text, json=parsed), ref

    def get(self, path: str, timeout: float = 30.0) -> tuple[Response, str]:
        return self.request("GET", path, None, timeout)

    def post(
        self, path: str, payload: Any = None, timeout: float = 30.0
    ) -> tuple[Response, str]:
        return self.request(
            "POST", path, payload if payload is not None else {}, timeout
        )
