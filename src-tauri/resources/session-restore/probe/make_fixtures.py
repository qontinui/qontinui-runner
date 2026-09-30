#!/usr/bin/env python3
"""Turn a probe's scratch recording into committed golden fixtures.

Plan `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
Phase 1. Reads one or more `events.jsonl` files written by `recorder.py` (and
by an http sink using `recorder.skeleton`) and writes, per event, the distinct
TYPE SKELETONS observed:

    src-tauri/tests/fixtures/claude-events/<cli-version>/<event>.json
    {"cli_version", "channel": "hook" | "statusline", "event", "variants": [...]}

Only `keys` (the skeleton) is carried over — never `enums`, timestamps, pids or
anything else in the scratch file — and every leaf is re-checked to be a type
name before anything is written. `PROBE.md` (the five answers) is written by
hand beside the output.

Usage: make_fixtures.py <cli-version> <out-dir> <events.jsonl>...
"""

import json
import os
import sys

LEAF_TYPES = {"string", "number", "boolean", "null"}


def check(skeleton, at):
    if isinstance(skeleton, str):
        if skeleton not in LEAF_TYPES:
            raise SystemExit("%s: %r is not a type name — refusing to write a value" % (at, skeleton))
    elif isinstance(skeleton, list):
        for i, element in enumerate(skeleton):
            check(element, "%s[%d]" % (at, i))
    elif isinstance(skeleton, dict):
        for key, child in skeleton.items():
            check(child, "%s.%s" % (at, key))
    else:
        raise SystemExit("%s: literal %r in a skeleton" % (at, skeleton))


def main():
    if len(sys.argv) < 4:
        raise SystemExit(__doc__)
    version, out_dir, inputs = sys.argv[1], sys.argv[2], sys.argv[3:]
    by_event = {}
    for path in inputs:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                record = json.loads(line)
                mode = record.get("mode")
                if mode in ("hook", "http"):
                    event = record.get("event")
                elif mode == "statusline" and record.get("phase") == "start":
                    event = "statusline"
                else:
                    continue
                if not event or not isinstance(record.get("keys"), dict):
                    continue
                check(record["keys"], "%s:%s" % (path, event))
                variants = by_event.setdefault(event, [])
                if record["keys"] not in variants:
                    variants.append(record["keys"])
    os.makedirs(out_dir, exist_ok=True)
    for event, variants in sorted(by_event.items()):
        variants.sort(key=lambda v: (len(json.dumps(v)), json.dumps(v, sort_keys=True)))
        doc = {
            "cli_version": version,
            "channel": "statusline" if event == "statusline" else "hook",
            "event": event,
            "variants": variants,
        }
        with open(os.path.join(out_dir, event + ".json"), "w", encoding="utf-8") as handle:
            json.dump(doc, handle, indent=2, sort_keys=True)
            handle.write("\n")
        print("%-18s %d variant(s)" % (event, len(variants)))


if __name__ == "__main__":
    main()
