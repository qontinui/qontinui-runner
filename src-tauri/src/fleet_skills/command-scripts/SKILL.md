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
| `_scripts/qontinui-http.py` | `/analyze-automation` | Python 3 with `httpx` and the `qontinui-mcp` package (`qontinui_mcp.client`) |
| `_scripts/lib/envelope.py` | `/workflow-runs` | Python 3.9+, stdlib only |
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
