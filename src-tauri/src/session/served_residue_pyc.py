"""Bytecode-cache checks for the runner's served-corpus residue rule.

A VERBATIM copy of the `pyc_*` functions in qontinui-claude-config
`scripts/session-closeable.sh` (its served-corpus classifier, plan
2026-11-03-the-runners-served-claude-corpus-makes-every-no-op-closeout-not-closeable
item 5), copied at qontinui-claude-config 6a9fb0899. The runner cannot
recompile Python bytecode in Rust, and "this interpreter's exact bytecode of
that source" is a property only an interpreter can test, so
`session/served_residue.rs` runs this file with `python3 -I` and reads one
reason per item ('' = the check passed). Keep it in step with the script: two
closeability authorities must not drift.

argv[1] is JSON: {"root": <tree>, "items": [{"mode": "mismatch"|"not_cache",
"rel": <pyc>, "source": <py>, "tag": <cache tag>, "opt": <0|1|2>}]}. stdout is
a JSON list of reasons, one per item, in order.
"""
import importlib.util
import io
import json
import marshal
import os
import sys
import types

PYC_MAX = 16 * 1024 * 1024  # a cache larger than this is not read, and not taken
# The second byte of every CPython 3 magic number (see the script for the
# measured history).
CPYTHON3_MAGIC_HIGH = (0x0C, 0x0D, 0x0E)
root = ""


def pyc_magic(rel):
    """The first 16 bytes of the file at <rel> (fewer when it is shorter) when
    it opens as every interpreter's bytecode does: a magic number (two bytes,
    then CR LF) in at least the 8 bytes every .pyc header has -- CPython 3.2
    wrote magic and mtime, 3.3 to 3.6 added the source size (12 bytes), 3.7
    on puts a flags word after the magic (16). Else None. (A file that cannot
    be read never gets here: the listing loop made it UNKNOWN.)"""
    try:
        with open(os.path.join(root, rel), "rb") as fh:
            lead = fh.read(16)
    except OSError:
        return None
    if len(lead) < 8 or lead[2:4] != b"\r\n":  # mc:pyc-magic
        return None
    return lead


def pyc_mismatch(rel, lead, source, tag, opt):
    """'' when the .pyc at <rel> is this interpreter's bytecode of <source>, byte
    for byte: its magic is this interpreter's, in its 16-byte header layout
    (flags 0, timestamp-based, or 1 or 3, source-hash-based), its tag this
    interpreter's cache tag, its header records the source's size or source
    hash, and its body is marshal.dumps(compile(source)) under the filename
    the body itself records and the optimisation level its name gives. Else
    why not. A cache another interpreter wrote is not taken: its code cannot
    be checked here."""
    if lead[:4] != importlib.util.MAGIC_NUMBER:  # mc:pyc-interp
        return "it is another interpreter's bytecode (magic %s), whose code cannot be checked here" % lead[:4].hex()
    if len(lead) < 16 or int.from_bytes(lead[4:8], "little") not in (0, 1, 3):
        return "it carries this interpreter's magic but not its 16-byte header layout"
    if tag != sys.implementation.cache_tag:
        return "its tag %s is not this interpreter's (%s)" % (tag, sys.implementation.cache_tag)
    try:
        with open(os.path.join(root, source), "rb") as fh:
            src = fh.read()
        with open(os.path.join(root, rel), "rb") as fh:
            data = fh.read(PYC_MAX + 1)
    except OSError as exc:
        return "it or %s could not be read: %s" % (json.dumps(source), exc)
    if len(data) > PYC_MAX:
        return "it is larger than a bytecode cache this check reads"
    if int.from_bytes(data[4:8], "little") == 0:
        if int.from_bytes(data[12:16], "little") != len(src) & 0xFFFFFFFF:  # mc:pyc-size
            return "its header records a source size other than %s's" % json.dumps(source)
    elif data[8:16] != importlib.util.source_hash(src):
        return "its header records a source hash other than %s's" % json.dumps(source)
    body = data[16:]
    try:
        fn = marshal.loads(body).co_filename
    except Exception as exc:  # not a code object
        return "its body is not a code object compiled from %s (%s)" % (json.dumps(source), type(exc).__name__)
    if not pyc_names_source(fn, source):  # mc:pyc-cofn
        return "its code records %s as its file, not %s itself" % (json.dumps(str(fn)[:120]), json.dumps(source))
    try:
        code = compile(src, fn, "exec", dont_inherit=True, optimize=opt)
    except Exception as exc:  # a source that does not compile
        return "%s does not compile (%s)" % (json.dumps(source), type(exc).__name__)
    if marshal.dumps(code) != body:  # mc:pyc-body
        return "its code is not this interpreter's compilation of %s" % json.dumps(source)
    return ""


def pyc_names_source(fn, source):
    """True when <fn>, the filename a .pyc's code records, names <source> in
    this tree itself: a normalised absolute path that resolves to it, as an
    import of that very file writes it. Not a bare name, a relative path, a
    path with `..` in it or text: the name is compiled into the cache, so it
    is where a session's words could ride. (Measured: each of the 56
    served-skill caches on this box that recompiles byte for byte records
    exactly its source's absolute path.)"""
    if not isinstance(fn, str) or not os.path.isabs(fn) or os.path.normpath(fn) != fn:
        return False
    return os.path.realpath(fn) == os.path.realpath(os.path.join(root, source))


def pyc_not_cache(rel, lead):
    """'' when the .pyc at <rel>, beside a TRACKED source, may be left out as a
    cache of it: another CPython 3 interpreter's, by its magic, with a code
    object's type byte where that interpreter's header ends (its marshal
    format may differ, so that one byte is all of its body read), or this
    interpreter's 16-byte layout around a body that is one code object and
    nothing after it -- compiled from whatever version of the source, since a
    stale cache is ordinary. Else why not."""
    if lead[:4] != importlib.util.MAGIC_NUMBER:
        if lead[1] not in CPYTHON3_MAGIC_HIGH:  # mc:pyc-plausible
            return "its magic %s is no CPython 3 interpreter's (whose second byte is 0x0c to 0x0e)" % lead[:4].hex()
        # That interpreter's code object starts where its header ends -- 16
        # bytes from 3.7a4 (magic 3392, PEP 552), 12 from 3.3 (3210), 8
        # before -- and marshal opens it with TYPE_CODE, 'c', or 'c' |
        # FLAG_REF (0xe3) from 3.4: text with a CPython 3 second byte rarely
        # has either there.
        word = int.from_bytes(lead[:2], "little")
        start = 16 if word >= 3392 else 12 if word >= 3210 else 8
        try:
            with open(os.path.join(root, rel), "rb") as fh:
                fh.seek(start)
                kind = fh.read(1)
        except OSError as exc:
            return "it could not be read: %s" % exc
        if kind not in (b"c", b"\xe3"):  # mc:pyc-other-code
            return "its magic %s is CPython 3's, but no code object starts its body (byte %d is %s)" % (
                lead[:4].hex(), start, kind.hex() or "past its end")
        return ""
    if len(lead) < 16 or int.from_bytes(lead[4:8], "little") not in (0, 1, 3):
        return "it carries this interpreter's magic but not its 16-byte header layout"
    try:
        with open(os.path.join(root, rel), "rb") as fh:
            data = fh.read(PYC_MAX + 1)
    except OSError as exc:
        return "it could not be read: %s" % exc
    if len(data) > PYC_MAX:
        return "it is larger than a bytecode cache this check reads"
    stream = io.BytesIO(data[16:])
    try:
        body = marshal.load(stream)
    except Exception as exc:  # not marshal data at all
        return "its body is not a code object (%s)" % type(exc).__name__
    if not isinstance(body, types.CodeType):  # mc:pyc-code-object
        return "its body is not a code object (it unmarshals to %s)" % type(body).__name__
    # marshal.load stops at the end of the one object it reads: whatever
    # follows it is not the cache's.
    if stream.tell() != len(data) - 16:  # mc:pyc-whole-body
        return "%d byte(s) follow the code object in its body" % (len(data) - 16 - stream.tell())
    return ""


def main():
    global root
    req = json.loads(sys.argv[1])
    root = req["root"]
    out = []
    for item in req["items"]:
        try:
            lead = pyc_magic(item["rel"])
            if lead is None:
                out.append("it does not open as any interpreter's bytecode does")
            elif item["mode"] == "mismatch":
                out.append(pyc_mismatch(item["rel"], lead, item["source"], item["tag"], int(item["opt"])))
            else:
                out.append(pyc_not_cache(item["rel"], lead))
        except Exception as exc:  # a crash on one item leaves it unjudged, never a pass
            out.append("the bytecode check crashed: %s: %s" % (type(exc).__name__, str(exc)[:200]))
    sys.stdout.write(json.dumps(out))


main()
