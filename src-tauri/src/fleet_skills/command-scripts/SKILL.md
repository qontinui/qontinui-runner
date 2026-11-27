---
name: command-scripts
description: "Carrier for the helper scripts that bundled slash commands call (under _scripts/). Not a skill to run: it exists so the helpers reach every session the commands reach."
user-invocable: false
---

# command-scripts

This skill carries helper scripts for the bundled slash commands. There is
nothing here to run directly.

A slash command is one markdown file. It has no directory of its own, so it has
nowhere to keep a helper script beside it. Skills do have a directory, and the
runner writes every file of a skill into a spawned session's
`.claude/skills/<name>/`. So the helpers that commands need live in this
skill's `_scripts/` directory, and they travel wherever the skill is provisioned.

## What is here

`_scripts/<rel>` mirrors `scripts/<rel>` in the repository that publishes
these skills, byte for byte:

| Helper | Used by | Needs |
|---|---|---|
| `_scripts/qontinui-http.py` | `/analyze-automation` | Python 3 and the `qontinui-mcp` package (`qontinui_mcp.client`, which brings `httpx`), installed from a source checkout: it is not on PyPI |
| `_scripts/lib/envelope.py` | `/workflow-runs`, `/permission-prompts`, `/triage-sessions-improve`, `_scripts/close-finished-sessions.py`, `_scripts/runner-layout.py`, `_scripts/nudge-stopped-sessions.py`, `_scripts/triage-sessions.py` | Python 3.9+, stdlib only |
| `_scripts/close-finished-sessions.py` | `/close-finished-sessions`, `/finish-session`, `/triage-sessions-improve`, `_scripts/nudge-stopped-sessions.py`, `_scripts/runner-sessions.py`, `_scripts/triage-sessions.py`, `_scripts/name-session.py` | Python 3.9+, stdlib only, and `_scripts/lib/envelope.py` beside it. Closing needs a `/proc` process table (Linux) |
| `_scripts/nudge-stopped-sessions.py` | `/nudge-stopped-sessions`, `/triage-sessions-improve`, `_scripts/triage-sessions.py` | Python 3.9+, stdlib only, and `_scripts/close-finished-sessions.py` beside it. Sending needs a `/proc` process table (Linux). Its `/next-move` holds read coord through the `coord-revive` skill (bash, curl) and pull requests through `gh`; without them those reads say UNKNOWN (`--no-coord`, `--no-gh` skip them) |
| `_scripts/name-session.py` | `/name` | Python 3.9+, stdlib only, and `_scripts/close-finished-sessions.py` beside it. Setting a name needs a runner tab (`$QONTINUI_TERMINAL_ID`) |
| `_scripts/runner-layout.py` | `/layout`, `_scripts/triage-sessions.py` | Python 3.9+, stdlib only, and `_scripts/lib/envelope.py` beside it. Setting a page needs a runner build whose `/layout` takes a window count and a page |
| `_scripts/runner-sessions.py` | `/runner-sessions`, `/triage-sessions-improve` | Python 3.9+, stdlib only, and `_scripts/close-finished-sessions.py` beside it. Reads only |
| `_scripts/triage-sessions.py` | `/triage-sessions`, `/triage-sessions-improve` | Python 3.9+, stdlib only, and `_scripts/nudge-stopped-sessions.py`, `_scripts/close-finished-sessions.py` and `_scripts/runner-layout.py` beside it. Acting needs a `/proc` process table (Linux). Its nudge step reads coord and `gh` as `nudge-stopped-sessions.py` does. Its pass ledger needs `git` (the scripts' blob shas) and a coord door: the `coord-revive` skill (bash, curl) over this session's `.mcp.json`, or an unexpired device JWT (`$COORD_DEVICE_JWT` or `~/.qontinui/coord-device-jwt`) for coord's `/mcp`. Without them the run still triages and ends `ledger UNKNOWN: <why>` |
| `_scripts/mobile-feedback.py` | `/mobile-dev`, `/mobile-verify` | Python 3 and Android `adb` |

## How a command finds a helper

A command body does not use a fixed path. It walks from the current directory
up through each parent, looking for
`.claude/skills/command-scripts/_scripts/<rel>`, and then it tries the user-level
skills directory under the home directory. That one walk covers the three places
the skill can be:

- the session directory, when the runner provisioned it;
- a checkout or worktree below a directory whose `.claude/` holds the skill;
- the user-level skills directory.

When the walk finds nothing, the command says where it looked and stops. It
never falls back to a path inside one machine's checkout of the repository
that publishes these skills.

## `scripts/` is the source; `_scripts/` is a render

Edit a helper in the publishing repository's `scripts/`, then copy it here.
Never edit the copy. Check #64 (`lint-skill-script-render.py`) holds the two
byte-identical. It also fails when a rendered file is missing from the runner's
bundled copy of this skill.

The table's *Used by* column lists slash commands and the `_scripts/` helpers
only. Tooling in the publishing repository can load a render too, so
search that repository before you change what a helper exports.

## Running a helper, and importing one

Run a helper by path (`python3 <path> ...`) wherever you can. Python writes no
bytecode for a file it runs by path (`python3 -m` is different: it caches the
module it runs), and every helper that loads a sibling or `lib/envelope.py`
turns bytecode writing off before that load.

A fence or a script that IMPORTS a file from `_scripts/` instead, such as
`sys.path.insert(0, "<_scripts>/lib"); import envelope` or a
`spec_from_file_location` load of a helper, must turn bytecode writing off for
that load itself, with one of:

- `PYTHONDONTWRITEBYTECODE=1` in its environment (`/permission-prompts` and
  `/triage-sessions-improve` export it from the `env.sh` that each of their
  importing fences sources first);
- `python3 -B`;
- `sys.dont_write_bytecode = True` before the load, restoring the previous value
  afterwards when the importer is itself imported as a library.

The helper's own `sys.dont_write_bytecode = True` cannot stand in for that: the
importer's loader writes `__pycache__/<file stem>.<cache tag>.pyc` beside the
file (for example `close-finished-sessions.cpython-313.pyc`) before any line of
the helper runs. A provisioned skill directory must not collect a
`__pycache__`. Check #64 walks the working-tree render of the checkout it runs
in, never a provisioned copy. In a clean CI checkout the gitignored cache never
appears, so only a box where an importer wrote one will see it. It reports a
cache as a bytecode cache, naming the module whose importer wrote it.
