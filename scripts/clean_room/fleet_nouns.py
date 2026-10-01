"""The fleet-noun vocabulary, read from qontinui-schemas `fleet-nouns.toml`.

This module carries NO copy of the vocabulary. It loads the file the caller
names and implements the consumer contract stated in that file's header
("HOW A CONSUMER MATCHES"), using Python `re` -- one of the three engines the
file is validated against:

  * match ONE LINE at a time, line terminator stripped;
  * compile each `pattern` / `exclude` from the string, no flags;
  * EXCLUDE is span-scoped: a pattern match is discarded when its span
    overlaps any exclude match on the same line;
  * a line is a hit for a class when at least one pattern match survives.

A missing or unparseable file is an error the caller must render as UNKNOWN
(`vocabulary_unavailable`), never as "no fleet nouns".
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

if sys.version_info < (3, 11):  # pragma: no cover - guarded at import time
    raise RuntimeError("clean_room needs Python >= 3.11 (tomllib)")

import tomllib


class VocabularyUnavailable(Exception):
    """The vocabulary file is absent, unreadable, or not the expected shape."""


@dataclass(frozen=True)
class FleetNounClass:
    id: str
    pattern: re.Pattern[str]
    exclude: re.Pattern[str] | None


@dataclass(frozen=True)
class Hit:
    class_id: str
    match: str
    line: str


@dataclass(frozen=True)
class Vocabulary:
    path: str
    version: int
    classes: tuple[FleetNounClass, ...]

    def class_by_id(self, class_id: str) -> FleetNounClass:
        for c in self.classes:
            if c.id == class_id:
                return c
        raise VocabularyUnavailable(
            f"{self.path}: no [[class]] with id {class_id!r} "
            f"(present: {', '.join(c.id for c in self.classes)})"
        )

    def scan_line(self, line: str) -> list[Hit]:
        """Every class that hits `line`, one Hit per surviving match."""
        hits: list[Hit] = []
        for c in self.classes:
            excl_spans = (
                [m.span() for m in c.exclude.finditer(line)] if c.exclude else []
            )
            for m in c.pattern.finditer(line):
                s, e = m.span()
                if any(s < xe and xs < e for xs, xe in excl_spans):
                    continue
                hits.append(Hit(class_id=c.id, match=m.group(0), line=line))
        return hits

    def scan_text(self, text: str) -> list[Hit]:
        hits: list[Hit] = []
        # splitlines() also splits on \r, \x0b, \x1c.. and U+2028; the contract
        # asks for \n / \r\n termination only, so split on \n and strip one \r.
        for raw in text.split("\n"):
            line = raw.removesuffix("\r")
            if line:
                hits.extend(self.scan_line(line))
        return hits

    def hits_class(self, class_id: str, text: str) -> bool:
        # An unknown id is a vocabulary error, not a "no".
        only = Vocabulary(self.path, self.version, (self.class_by_id(class_id),))
        return bool(only.scan_text(text))


def load_vocabulary(path: str | Path) -> Vocabulary:
    p = Path(path)
    try:
        raw = p.read_bytes()
    except OSError as exc:
        raise VocabularyUnavailable(f"cannot read {p}: {exc}") from exc
    try:
        doc = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise VocabularyUnavailable(f"cannot parse {p}: {exc}") from exc

    version = doc.get("version")
    if not isinstance(version, int):
        raise VocabularyUnavailable(f"{p}: top-level integer `version` missing")
    raw_classes = doc.get("class")
    if not isinstance(raw_classes, list) or not raw_classes:
        raise VocabularyUnavailable(f"{p}: no [[class]] tables")

    classes: list[FleetNounClass] = []
    for i, rc in enumerate(raw_classes):
        cid = rc.get("id")
        pattern = rc.get("pattern")
        if not isinstance(cid, str) or not isinstance(pattern, str):
            raise VocabularyUnavailable(
                f"{p}: [[class]] #{i} lacks a string id/pattern"
            )
        exclude = rc.get("exclude")
        try:
            classes.append(
                FleetNounClass(
                    id=cid,
                    pattern=re.compile(pattern),
                    exclude=re.compile(exclude)
                    if isinstance(exclude, str) and exclude
                    else None,
                )
            )
        except re.error as exc:
            raise VocabularyUnavailable(
                f"{p}: class {cid!r} does not compile: {exc}"
            ) from exc
    return Vocabulary(path=str(p), version=version, classes=tuple(classes))
