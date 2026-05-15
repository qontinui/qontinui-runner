"""Migrate IRState.requiredElements -> IRState.assertions across all on-disk
state-machine.derived.json spec files.

The IR schema changed: each state's ``requiredElements: IRElementCriteria[]`` has
been replaced by ``assertions: IRAssertion[]``. This script rewrites every
``specs/pages/*/state-machine.derived.json`` under ``qontinui-runner/`` in place.

Idempotent: states that already have ``assertions`` and no ``requiredElements``
are left alone. ``requiredElements`` is deleted from each state after migration
(no backward-compat dual keys).
"""

from __future__ import annotations

import glob
import json
import sys
from pathlib import Path
from typing import Any

# Resolve the repo root relative to this script: <runner>/scripts/<this>.py
SCRIPT_DIR = Path(__file__).resolve().parent
RUNNER_ROOT = SCRIPT_DIR.parent
SPECS_GLOB = str(RUNNER_ROOT / "specs" / "pages" / "*" / "state-machine.derived.json")


def build_assertion(state_id: str, state_name: str, idx: int, criterion: dict[str, Any]) -> dict[str, Any]:
    """Construct an IRAssertion from a single legacy IRElementCriteria entry."""
    return {
        "id": f"{state_id}-elem-{idx}",
        "description": f"Required element {idx} for state {state_name}",
        "category": "element-presence",
        "severity": "critical",
        "assertionType": "exists",
        "target": {
            "type": "search",
            "criteria": criterion,
            "label": f"Required element for {state_name}",
        },
        "source": "migrated",
        "reviewed": False,
        "enabled": True,
    }


def migrate_state(state: dict[str, Any], file_path: Path) -> bool:
    """Mutate a single state dict in place. Returns True if changed."""
    has_required = "requiredElements" in state
    has_assertions = "assertions" in state

    # Idempotent: state already migrated.
    if has_assertions and not has_required:
        return False

    state_id = state.get("id") or "<unknown-id>"
    state_name = state.get("name") or state_id

    required_elements = state.get("requiredElements") if has_required else []
    if required_elements is None:
        required_elements = []

    if not isinstance(required_elements, list):
        print(
            f"  WARN: {file_path.name} state {state_id!r} has non-list requiredElements "
            f"({type(required_elements).__name__}); treating as empty.",
            file=sys.stderr,
        )
        required_elements = []

    assertions: list[dict[str, Any]] = []
    for idx, criterion in enumerate(required_elements):
        if not isinstance(criterion, dict):
            print(
                f"  WARN: {file_path.name} state {state_id!r} requiredElements[{idx}] "
                f"is not an object ({type(criterion).__name__}); skipping entry.",
                file=sys.stderr,
            )
            continue
        assertions.append(build_assertion(state_id, state_name, idx, criterion))

    # Preserve key ordering: insert "assertions" where "requiredElements" was;
    # if requiredElements was absent, append after "name" if present, else at end.
    new_state: dict[str, Any] = {}
    inserted = False
    for key, value in state.items():
        if key == "requiredElements":
            new_state["assertions"] = assertions
            inserted = True
            continue  # drop requiredElements entirely
        new_state[key] = value
    if not inserted:
        new_state["assertions"] = assertions

    state.clear()
    state.update(new_state)
    return True


def migrate_file(path: Path) -> tuple[bool, int, list[str]]:
    """Migrate one spec file. Returns (changed, states_migrated, anomalies)."""
    anomalies: list[str] = []
    try:
        with path.open("r", encoding="utf-8") as f:
            doc = json.load(f)
    except json.JSONDecodeError as exc:
        anomalies.append(f"JSON parse error: {exc}")
        return (False, 0, anomalies)

    if not isinstance(doc, dict):
        anomalies.append(f"top-level is not an object ({type(doc).__name__})")
        return (False, 0, anomalies)

    states = doc.get("states")
    if not isinstance(states, list):
        anomalies.append(f"'states' missing or not a list ({type(states).__name__})")
        return (False, 0, anomalies)

    changed_states = 0
    for i, state in enumerate(states):
        if not isinstance(state, dict):
            anomalies.append(f"states[{i}] is not an object ({type(state).__name__}); skipped")
            continue
        if migrate_state(state, path):
            changed_states += 1

    if changed_states == 0:
        return (False, 0, anomalies)

    with path.open("w", encoding="utf-8", newline="\n") as f:
        json.dump(doc, f, indent=2, ensure_ascii=False)
        f.write("\n")

    return (True, changed_states, anomalies)


def main() -> int:
    files = sorted(glob.glob(SPECS_GLOB))
    if not files:
        print(f"No files matched {SPECS_GLOB}", file=sys.stderr)
        return 1

    files_changed = 0
    files_skipped = 0
    total_states_migrated = 0
    all_anomalies: list[tuple[str, str]] = []

    for raw_path in files:
        path = Path(raw_path)
        changed, n_states, anomalies = migrate_file(path)
        for a in anomalies:
            all_anomalies.append((str(path), a))
        if changed:
            files_changed += 1
            total_states_migrated += n_states
            print(f"  migrated: {path.relative_to(RUNNER_ROOT)} ({n_states} state(s))")
        else:
            files_skipped += 1

    print()
    print(f"Scanned:       {len(files)}")
    print(f"Files changed: {files_changed}")
    print(f"Files skipped: {files_skipped} (already migrated or no changes needed)")
    print(f"States migrated total: {total_states_migrated}")
    if all_anomalies:
        print()
        print(f"Anomalies ({len(all_anomalies)}):")
        for f, msg in all_anomalies:
            print(f"  - {f}: {msg}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
