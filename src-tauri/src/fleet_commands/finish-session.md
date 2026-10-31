---
description: "Mark THIS session finished — the work axis, not liveness. Records `session_status=finished` on the session's coord row, so a rebuilt runner offers only UNFINISHED sessions for resume. Reversible with --undo (a transition back to `working`). Never closes the session or touches the process."
argument-hint: "[--reason \"<why>\"] [--undo] [--session <claude-session-uuid>] [--force]"
allowed-tools: Read, Bash, ToolSearch, Grep
---

# /finish-session — declare this session's work complete

Marks a session **FINISHED** on the WORK axis. One durable consequence: the
runner stops offering it for resume, so after you rebuild the runner you get back
only the sessions that still have work left.

> **This never touches the process.** Finishing is pure metadata. The session
> keeps running until it exits on its own, you can keep typing in it, and
> `--undo` lifts the mark (a transition back to `working` — coord has no "unset"
> status, so it reverses the CONSEQUENCES, not the history; see Part B step 2).
> If you want the session GONE, close it — that is a different axis and a
> different action.

**`finished` is not `closed`.** They are orthogonal, and conflating them is the
mistake this command exists to avoid:

| | `state` (liveness) | `finished` (work) |
|---|---|---|
| asks | was it running? | is there anything left to do? |
| set by | the PTY exiting, a crash, an explicit close | you, or `/unattended` |
| a live session that is done | `open` | **`finished`** |
| a session that crashed mid-task | `closed` | **not finished** |

**A session paused for an operator restart prepare is not finished.** The
runner's *Prepare for restart* asks each live Claude session to save and pause, and
then restores the paused ones after the rebuild. Do not mark a session finished
just to get it out of the prepare's way. Marking it finished means the bulk
**Restore all** skips it after the rebuild (it still has its own **Restore**),
and the prepare's verdict counts a finished row as done
rather than waiting for it. Mark it finished only when its work really is
done. The prepare's state is the `restart_prep` block of `/restart-readiness`.

---

## Part A — build the payload (transport-independent)

**0. Are you a SUBAGENT? If so, STOP — before you resolve anything.**

A Claude Code subagent — a context spawned by the `Agent` tool inside another
session — **inherits its parent's `$CLAUDE_CODE_SESSION_ID`**. It is one harness
session; a subagent is a context within it, not a session of its own. So step 1
resolves an id that is perfectly valid and **is not yours**, and every rung below
then faithfully finishes *the parent*.

**That is a work-loss path, not a bookkeeping slip.** `finished` trips coord's
universal prompting gate (`qontinui-coord` `sessions.rs`,
`prompting_allowed_decision` — *"the single chokepoint every coord-originated
outbound prompt path MUST consult"*), which returns false for
`WaitingHuman | Finished`. Finishing a live parent therefore **silences every
coord-originated prompt into it** — gate continuations, next-step dispatch,
directed messages — while the write returns success to you and nothing is
delivered to them.

So, from a subagent:

- **Do not run this command.** Report the closeout as **owed by the parent**, in
  those words, so the parent's own `/unattended` can act on it.
- If a `finished` has **already** been written from a subagent, `--undo` reverses
  it. Do that first, then report it.

> **This step is placed BEFORE step 1 deliberately.** The fallback order in step 1
> *prefers* the shared value, so a check placed after resolution is looking at an
> id that already looks legitimate. There is nothing later to catch it: measured
> 2026-09-04 from inside a real subagent, the two contexts are **not separable by
> any mechanical signal** — a subagent is a context in the same OS process, so it
> inherits the parent's environment byte-for-byte (`CLAUDE_PID`, the messaging
> socket and `CLAUDE_CODE_CHILD_SESSION=1` are all identical in both), and no
> `CLAUDE_AGENT_TYPE` / `CLAUDE_SUBAGENT` / `CLAUDE_CODE_TASK_ID` exists.
> **You are the only thing that knows** — your own system prompt says you are a
> subagent — which is why this arm is an instruction to you and not a test in a
> script. Step 0.5 below is the identity-free backstop for when it is not read.
> Plan `2026-09-04-closeout-commands-have-no-subagent-arm-and-finish-their-parent`.

**0.5. Refuse while this session still holds live claims — whoever you are.**

Step 0 depends on an agent reading it. This check does not depend on anyone:
unreleased claims under the id you are about to mark `finished` are direct
evidence that the session has work in flight.

```bash
# The fence resolves the SAME rungs step 1 does, INCLUDING the box-global file.
# Env-only, this lookup lands on `nosession.ledger` for exactly the sessions that
# reach rung 3 — so the backstop passed vacuously for the one id most likely to
# be foreign. The file is read here only to FIND a ledger; it is not usable as a
# finish subject until step 1a below proves it is yours.
SID="${CLAUDE_CODE_SESSION_ID:-${QONTINUI_AGENT_SESSION_ID:-}}"
[ -n "$SID" ] || SID="$(cat "$HOME/.qontinui/agent_session_id" 2>/dev/null || true)"
LEDGER="$HOME/.qontinui/claim-ledger/${SID:-nosession}.ledger"
[ -f "$LEDGER" ] && grep -v '^#' "$LEDGER" | grep . && echo "LIVE CLAIMS ABOVE"
```

Rows printed ⇒ **do not finish.** Name them, say the session still holds live
coord claims, and stop. The legitimate closeouts (`/implement-plan` Step 6,
`/vet-imp` Step 5) release every row before they reach any finish, so a genuinely
complete session prints nothing here. `--force` overrides, and like
`coord-claim-heartbeat.sh stop --force` it is a deliberate act rather than a
default reached by falling through.

**1. Resolve YOUR session id. This is the one step you must not skip.**

```bash
printf '%s\n' "${CLAUDE_CODE_SESSION_ID:-<unset>}"
```

Fall back, in order: `$CLAUDE_CODE_SESSION_ID` → `$QONTINUI_AGENT_SESSION_ID` →
`~/.qontinui/agent_session_id` → the explicit `--session <uuid>` argument.
**The order is unchanged — but every rung after the first is now subject to the
MANDATORY ownership check in step 1a below.** An id from rung 2, 3 or 4 is not a
finish subject until `coord_orient` confirms it is yours, and a rung that cannot
prove it REFUSES rather than falls through.

> ⚠️ **If you cannot resolve a session id, STOP and say so. Do not proceed.**
>
> `coord_report_status` used to resolve an OMITTED `claude_code_session_id` to
> *the most recently started active session on this DEVICE* — on a box running
> several concurrent sessions, a **peer's** row — and still return success. Coord
> now refuses that call (`harness_session_id_required`; the tool's description
> says there is no device-wide fallback), but an id from the wrong rung is still
> accepted when it names a row this device owns, so a mis-resolved finish still
> silently marks somebody else's session done and removes it from THEIR resume
> set.
>
> This fleet routinely runs a dozen sessions per box. Every rung below passes the
> id explicitly, and a rung that cannot must refuse rather than fall back.

**1a. Ownership-check any id that did NOT come from `$CLAUDE_CODE_SESSION_ID`.
MANDATORY. It REFUSES; it does not fall through.**

The ⚠️ box above is about an **omitted** id — one you never resolved. This is a
different hazard and needs its own check: an id you **did** resolve, that is
well-formed, live, and **somebody else's**. Rungs 2, 3 and 4 can each hand you
one, and rung 3 routinely does.

- **Rung 3 is a box-global scalar with no namespacing whatsoever.**
  `$HOME/.qontinui/agent_session_id` is written by a `SessionStart` stamp hook,
  where one is installed on this box; registered with matcher `*`, such a hook
  fires on **startup, resume, clear AND compact**. One file serves every session on the box and the
  last writer wins, so it holds whichever session on this box started, resumed,
  cleared or compacted **last** — not necessarily yours. Its siblings in the same directory
  (`~/.qontinui/session-names/`, `session-titles/`, `claim-ledger/`) are
  per-session *namespaces*; this one file is a single scalar.
- **It has been measured pointing at a LIVE peer twice** — once at a peer 31
  minutes older than the reader, and once rewritten by a peer's spawn
  **mid-closeout**.
- **Finishing a peer is a work-loss path, not a bookkeeping slip.** `finished`
  returns false from `prompting_allowed_decision` (the chokepoint described
  above step 0), so it **silences every coord-originated prompt into that peer
  while returning success to you**. Un-finishing that row is now a sanctioned
  repair (plan `2026-09-05-a-misdirected-finished-write-is-unrepairable`
  Phase 2: moving a row off `finished` is accepted only from the row's own
  device + tenant — `coord_report_status` refuses anyone else at session
  resolution (`session_not_owned_by_caller`, or `caller_owns_no_active_session`
  when the caller owns no active row) and must be given that row's bound
  `claude_code_session_id`; `PATCH /sessions/:id` refuses a request with no
  device identity (`unfinish_requires_device_identity`) or a foreign one
  (`session_not_owned_by_caller`)) — but nothing REPORTS a misdirected
  finish, so the repair only happens if someone notices. Prevention is still the
  only control that works unattended.
- **The same rung can hand you a value coord rejects outright.**
  A stamp hook that cannot find a real id may write a `synthetic-<uuid>`
  placeholder instead; that is not a UUID. The two failure shapes — silently a peer, or loudly
  invalid — are **not distinguishable at the file**.

**The check.** The resolved id must come back from `coord_orient` as *the
caller's own*: a `your_session` block whose `coord_session_id` is non-null, whose
`reason` is absent/null, and which is answering about **the id you sent**.

```bash
RESOLVED='<the id rung 2, 3 or 4 gave you>'
# Spell the door absolutely, substituting the session workdir: the directory
# the runner provisioned this session's `.claude/` into (on a fleet box that
# is the workspace root, whose `.claude` links into the config repo). A
# relative `.claude/skills/...` resolves only from a directory that carries
# that tree, which most agent worktrees do not. If THIS path is also absent
# (no `coord-revive` skill provisioned in this workdir), bash's own
# "No such file or directory" is a DIFFERENT signal than the table's exit
# codes below (which assume the script itself ran) — read it as the same
# "door did not carry the call" row, not as a JSON-RPC refusal.
. <session-workdir>/.claude/skills/coord-revive/_scripts/lib/envelope.sh
R=$(mktemp)
bash <session-workdir>/.claude/skills/coord-revive/coord-revive.sh \
  call coord_orient "{\"claude_code_session_id\":\"$RESOLVED\"}" > "$R"
echo "door exit: $?"
# The block the table below reads. Read the door exit FIRST: a non-zero door
# exit leaves "$R" empty, so the read's own exit then says nothing about
# `your_session`. "$R" is kept so `cat "$R"` shows what the door answered.
envelope_mcp_body coord_orient "$R" | envelope_require coord_orient your_session -
echo "read exit: $?"
```

That verb reads the nearest `.mcp.json` above `$PWD` and never mints a nonce, so
it works from any harness with no MCP client. Read the verdict from both exit codes
**and** the body, door exit first — none alone is enough:

| Signal | Meaning | Action |
|---|---|---|
| door exit 0, read exit 0, `coord_session_id` non-null, **no** `reason`, echoing the id you sent | the id is yours | **proceed** |
| door exit 0, read exit 0, **any** `reason` (`session_not_owned_by_caller`, `caller_owns_no_active_session`, …) | coord answered and refused | **REFUSE** |
| door exit 0, read exit 0, but the id echoed is not the one you sent | you are reading somebody else's answer | **REFUSE** |
| door exit 0, read exit non-zero — `3` (no `your_session` block, an `isError` result, a body that is not JSON) or `127` (no usable JSON reader on this box: no `jq`, and the python arm finds no `envelope.py` beside `envelope.sh`) | the answer could not be read as a `your_session` block | **UNKNOWN — REFUSE** |
| door exit 3 — the tool answered with a JSON-RPC error | answered-and-refused | **REFUSE** |
| door exit 1 — the door did not carry the call (no provisioned nonce, 401, dead port, timeout) | the check is **UNKNOWN**, never passed | **REFUSE** |

**REFUSE means: this rung is dead. Do not fall through to the next rung and do
not proceed to Part B.** Report it in these words — *"session NOT finished: the
resolved id could not be proven to be mine"* — naming which rung produced the id,
and the `reason` string or exit code that refused it. That is a complete,
honest outcome; a finish written to an unproven id is not.

If the refusal is `session_not_owned_by_caller` or `caller_owns_no_active_session`
for **your own** `$CLAUDE_CODE_SESSION_ID` (rung 1, which this check does not
gate), you are in dossier
`finish-session-no-safe-rung-for-unregistered-session` — see step 1b below,
which repairs your own row (operator-launched) or ends at its terminal state
(runner-hosted). Reaching for rung 3 to escape it is the
exact act this check exists to stop.

> **`--force` does not lift this check.** That flag is step 0.5's override for
> live claims. There is no override here: the whole point is that a session which
> cannot prove the id is its own has no way to make the write safe.

**1b. Bind your own row when coord cannot find it — then write.**

**When.** Rung 1 — your own `$CLAUDE_CODE_SESSION_ID`, which step 1a does not
gate — is refused `session_not_owned_by_caller` or
`caller_owns_no_active_session`: by `coord_orient`'s `your_session.reason`, or
by `coord_report_status` in Part B Steps 2-3. Either string usually means coord holds no
row *bound* to that id (`NotOwned` is also where coord lands when its ownership
probe could not run). It does **not** mean the id is foreign, and it is not a
terminal state yet: the door that fixes it is `coord_bind_self_session`.
This step applies to rung 1 only; an id from rungs 2-4 that step 1a refused
stays refused.

> **Coord's own refusal may argue against this step — and it is STILL SERVED.**
> The `session_not_owned_by_caller` hint, served from `qontinui-coord`
> `59fcdb564` and still word for word at `07a192bae` (its correction is
> qontinui-coord#2608, not landed when this was written), says *"Do NOT reach
> for `create_if_absent` here: you already own rows …"*. Seeing it live does
> not make it apply: this step governs whatever build is serving it. Its hazard (a second row) is real only for a
> runner-hosted session whose own row the runner created and never bound. So
> this step overrides that hint on the operator-launched arm below, and agrees
> with it on the runner-hosted one. Declining the create arm on an
> operator-launched session because the hint said so is the recorded failure
> (occurrence `64fdaffd`).

**Branch FIRST on whether the runner spawned you.** That is identity, and it is
the only discriminator that exists before the write. Read the variable by name,
never with an env dump (the session environment carries plaintext passwords):

```bash
# Non-EMPTY is the predicate, not "set": printenv exits 0 for an empty value.
[ -n "$(printenv QONTINUI_RUNNER_CONTEXT)" ] && echo runner-hosted || echo operator-launched
```

- **Empty — no runner context seen (normally operator-launched).** An
  operator-launched session has no runner-created row, so the create arm cannot
  fork it. Call
  `coord_bind_self_session{claude_code_session_id: <your id>, create_if_absent: true}`.
  An empty variable is not PROOF of that, though: a headless runner spawn may not
  export it, and an older runner build exports nothing (`/whereami` reads it as
  UNKNOWN there). So a non-zero `unbound_sibling_rows` here is **most likely
  peers'** unbound rows on this device+tenant — but name it in the report as a
  possible fork, never as zero.
- **Non-empty — runner-hosted. Take the runner rung, and do NOT create.** Run
  Part B Step 1 FIRST if you have not already — but only once steps 2
  (direction) and 3 (reason) below are decided, since Step 1's call carries both:
  an `--undo` sends `{"finished": false}`, never the finish snippet as written.
  That rung is path-addressed and reaches coord on the runner's own credential,
  without this ownership predicate, so it is the door this arm exists for. For a
  FINISH, a `coord.queued: true` there, read back `finishSynced: true`, is done.
  The `finishSynced` read-back applies to a finish only; after an `--undo` never
  re-POST (a POST without `finished` finishes the session again) — Step 1 says
  what to read instead. A `coord.queued: true` on the read-back is a write the
  runner has (re-)queued: wait and re-read as its table says.

  When Step 1's table still leaves the coord half owed (a `404`,
  `changed: "marker"` with `coord.queued: false`, a `"none"` / `"reason_only"`
  with `finishSynced: false` and `coord.queued: false`, no `coord` key, any answer
  the table calls UNKNOWN, no runner answering `/health`, a `finishSynced` still
  `false` after the prescribed wait, or an `--undo` coord still reads as
  `finished`), **do NOT create a row.** The runner created your row at spawn and
  binds it itself — the same rule `/unattended` Step 4.9's
  `worktree-handoff.sh` follows (*"A runner-hosted session lets the runner bind
  its own row"*). A create here can fork you: the new row takes every status and
  `finished` write while your worktrees, claims and branch stay on the runner's
  row. So the refusal is the terminal state on this arm: report the session's
  coord half **not** written — `runner-hosted, own row unbound` — quoting both
  the reason string and Step 1's answer (`coord.reason`, or the status you got),
  and record it as an occurrence on dossier
  `finish-session-no-safe-rung-for-unregistered-session`. A runner-hosted row the
  runner left unbound is a runner defect to fix, not a row for the session to
  replace.

  This holds for `caller_owns_no_active_session` too, deliberately, even though
  that string says the device + tenant own NO active row and coord's own hint
  for it names `create_if_absent`. On this arm "no row" means the runner's own
  registration has not reached coord (its outbox may still hold it, e.g. coord
  was unreachable at spawn, or the runner's credential resolves to a different
  binding than this session's on a multi-bound device). A row created now
  becomes a SECOND row the moment that registration drains, with the same fork
  shape as above. So report it — the runner registration is what is missing —
  rather than create. `worktree-handoff.sh` makes no coord row call at all on
  this arm, for the same reason: the runner binds its own row.

  **Do NOT bind a row you picked out of `coord_orient`'s `session_activity`**,
  even though coord's own hint and tool description suggest it. Those rows carry
  no harness id, and while you are unbound, writes addressed by your harness id never reach your
  row — a `correlation_topic` or `work_unit_slug` you sent in a refused write
  landed nowhere, so a row showing it is a **peer's**. A wrong pick is accepted
  (ownership is device+tenant), passes the read-back below, and then writes
  `finished` onto that peer, and nothing reports it (plan
  `2026-09-05-a-misdirected-finished-write-is-unrepairable`). A fork is
  reported by `unbound_sibling_rows`; a misdirected finish is reported by
  nothing. The named-row arm
  (`coord_session_id`) is only for a row id you already hold from your own
  provenance, never one inferred from a listing — and nothing on this fleet
  exports one today; a rung 2-4 value is a harness id, never a `coord_session_id`.

**`unbound_sibling_rows` is post-hoc.** Coord computes it after the create, so it
detects a fork, it never prevents one — which is why the runner-context branch
above comes first. `null` means coord could not count: UNKNOWN, not zero.

**Why the finish write may create when Step 4.9's hand-off does not.**
`worktree-handoff.sh` answers `session_not_owned_by_caller` with
`coord_row_unbound` and creates nothing, because a transcript save is skippable
and the string only says this device owns SOME active row — usually a peer's.
The finish write is not skippable: no row, no `finished`. And for an
operator-launched session none of those rows can be its own (the runner never
made one), so the create can fork it only if the empty variable is wrong —
which is why `unbound_sibling_rows` is still read and named. When invoked
from `/unattended` (whose Step 4.9 output is what names the trees), after the
read-back below succeeds, **re-run `worktree-handoff.sh` with the same `--path`
and `--pr` for every tree Step 4.9 reported `transcript: not saved:
coord_row_unbound`**: `coord_orient` now
resolves the bound row, so the save is no longer declined at the row step. Read
the re-run's own `transcript:` line rather than assuming it saved (it can still
stop on the runner), and print it in place of Step 4.9's old one. A clean save
with `--pr` also clears the stale `Coord-Handoff-Transcript:` line from the PR
(`pr-trailer: stale trailer cleared`); without `--pr` it cannot.

**When `coord_bind_self_session` is not on your tool surface** (load it via
`ToolSearch` first if it is a deferred name), carry the same call over the
session's own nonce (substitute `<session-workdir>` as in the ownership check
above):

```bash
bash <session-workdir>/.claude/skills/coord-revive/coord-revive.sh \
  call coord_bind_self_session '{"claude_code_session_id":"<your id>","create_if_absent":true}'
# envelope-ok: the bind's answer is not the verdict; the coord_orient read-back below is
```

That verb uses the nearest `.mcp.json` at or above `$PWD`, which is not
necessarily the door the cascade reported LIVE (finding `16df6f41`). Run it from
the directory whose `.mcp.json` IS the live one — often the workspace root — and
read a `COORD_MCP_PROXY_UNAUTHORIZED` from one cwd as "wrong `.mcp.json`", not
as a dead door.

**Read back before you write.** `coord_orient` with the same
`claude_code_session_id` must return `your_session.coord_session_id` non-null
with no `reason`. Only then go on to Part B's finish write, and then the
read-back at the end of this file.

**What this step is not.**

- `create_if_absent` is for a session coord holds **no** row for. It is **not** a
  repair for a WRONG binding: `already_bound_to_another_harness_session` and
  `harness_session_already_bound` are refusals to report, not to route around.
- `subject_already_registered: true` means coord found a row already carrying
  your harness id — which may be a **sibling** sharing it (a subagent inherits
  the parent's id; see step 0). Check `coord_orient`'s `your_session` before
  writing a terminal status to it.
- If the bind itself is refused, or no door carries it, that is the terminal
  state: report the session **not** finished, quoting the reason string or the
  door failure.

**2. Decide the direction.** `--undo` unmarks; otherwise finish.

**3. Compose the reason** (optional but wanted): what was completed, e.g.
`"3 PRs open, all green"`. It is recorded on the runner row and shown in the
Previous-sessions list.

---

## Part B — the transport cascade (try in order; stop at the first that works)

**Each step is validated by a cheap probe before you trust it.** Always report
which step carried the write.

**Part A still runs first, all of it.** Steps 0 (subagent stop), 0.5 (live-claims
fence) and 1 / 1a (resolve the id; ownership-check any id that did not come from
`$CLAUDE_CODE_SESSION_ID`) apply unchanged BEFORE Step 1 below. Step 1 is
path-addressed: it finishes exactly the id in its URL, whoever that id belongs
to. That makes it immune to a peer-landing unscoped spelling. It does not make a
wrong id safe, so a wrong id is still a wrong finish.

### Step 1 — Runner-local, path-addressed: `POST /sessions/<id>/finish` (probe: `/health` 200)

This is the one rung whose target is the URL path. It names no work unit and
cannot resolve to a peer by inference. It is also the only rung that corrects
the runner's resume set directly rather than through a coord round trip. On a
runner build carrying plan
`2026-09-20-the-one-finish-door-that-names-its-target-in-the-path-is-disowned-by-both-closeout-commands`
Phase 1, it also queues the `finished` write to coord through the runner's
outbox, on the RUNNER's device credential. It does not touch this session's MCP
transport or coord's ownership predicate, so it works in the case Part A step 1b
exists for without a bind.

**Probe — both halves.** Identity: `$QONTINUI_RUNNER_CONTEXT` is non-empty
(`/whereami`; read it by name, never with an env dump). Liveness: the runner
answers `/health`. The port is `9876` for the primary runner only. A session a
secondary or temp runner spawned is served on that runner's port, which its
provisioned `.mcp.json` coord-mcp url usually names. Treat that as a hint, not
proof: the nearest `.mcp.json` above `$PWD` can belong to another runner or be
stale, so a `404` on a non-9876 port means "not tracked there", not "not
runner-hosted". An empty variable is not proof you
are unhosted (a headless spawn or an older build may not export it). The call is
harmless for an id this runner does not track, which gets a `404`, so a live
`/health` is enough to try it.

**The call.** One block, so no variable leaks in from an earlier shell. Put the
Part A id in `SID`, the probe's port in `PORT`, and the reason from Part A step 3
in the body.

```bash
SID='<the Part A id>'; PORT=9876   # or the 127.0.0.1 port your .mcp.json coord-mcp url names
REASON='<why — e.g. unattended: 7 units, all landed>'   # no single quote inside; JSON-escaped below
BODY="$(python3 -c 'import json,sys; print(json.dumps({"reason": sys.argv[1]}))' "$REASON" 2>/dev/null \
  || python -c 'import json,sys; print(json.dumps({"reason": sys.argv[1]}))' "$REASON" 2>/dev/null)"
[ -n "$BODY" ] || { echo "could not build the JSON body (no python): stop, do not send"; exit 1; }
: "${SID:?no session id: stop, do not send}"
printf '%s' "$SID" | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' \
  || { echo "SID is not a session uuid (unsubstituted placeholder?): stop"; exit 1; }
[ -n "$(printenv QONTINUI_RUNNER_CONTEXT)" ] || echo "note: no runner context seen (UNKNOWN, not proof of unhosted)"
[ "$(curl -s -m 15 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" = 200 ] \
  || { echo "runner not answering on $PORT: skip to Step 2"; exit 0; }
curl -s -m 15 -w '\nHTTP %{http_code}\n' -X POST \
  -H 'Content-Type: application/json' \
  -d "$BODY" \
  "http://127.0.0.1:$PORT/sessions/$SID/finish"
# --undo: the same call with -d '{"finished": false}'
```

A runner that does not answer `/health` means skip to Step 2. That is not a
failure of this rung. Say "runner not answering" in the report.

**Read the verdict from the BODY, never from the status line alone. Branch on
`changed` FIRST.** On a no-op or a reason-only rewrite, `coord.queued` answers
"was the finished marker (re-)queued just now": `true` means an owed write was
queued again (the mark was not yet synced), `false` with `not_requeued` means
nothing was owed. Either way `session.finishSynced` is what says whether coord
has ACKed it.

| Answer | Meaning | Action |
|---|---|---|
| `404` | this runner holds no lifecycle record for that id. The session is not runner-hosted, is an SDK session spawned through `POST /sessions/spawn`, or belongs to a different runner instance. **On a build without the plan's Phase 1** a `404` also means "already in the requested state, and no reason was sent", and the body text says so | fall through to Step 2 |
| `200`, `changed: "marker"`, `coord.queued: true` | resume set corrected AND the coord write is queued to `coord.coordSessionId` | **done**. Report this rung and the `coordSessionId`, then (for a finish only — never after an `--undo`) do the read-back below |
| `200`, `changed: "marker"`, `coord.queued: false` | the runner's resume set IS corrected, and coord is NOT told (`coord.reason`: `no_coord_session`, `outbox_write_failed` or `no_forwarder`) | say so in those words, quoting `coord.reason`, and **continue to Step 2** for the coord half. If Step 2 is then refused, Part A step 1b's runner-hosted arm applies — no create; terminal state `runner-hosted, own row unbound` |
| `200`, `changed: "none"` or `"reason_only"`, `finishSynced: true` | already finished and coord has ACKed it. A `reason_only` rewrote the reason **locally** only — the runner's finished row carries no reason text, so no reason from this rung ever reaches coord | **done** for the coord half. Do not report a new reason as having reached coord |
| `200`, `changed: "none"` or `"reason_only"`, `finishSynced: false` or absent, `coord.queued: true` | the mark is finished but not yet ACKed, and the runner has just re-queued the owed write — this is the normal answer to the read-back below, taken before the outbox drained | wait ~15 s (the runner's outbox drains every 1–5 s; longer while coord is backing off), re-read once with the same no-reason POST. `true` then: done. Still `false`: continue to Step 2 |
| `200`, `changed: "none"` or `"reason_only"`, `finishSynced: false` or absent, `coord.queued: false` | coord does not have the current state and nothing was re-queued (`coord.reason` names why; after an `--undo` this is the normal no-op answer) | quote `coord.reason` and continue to Step 2 |
| any other status (`500` = the runner's lifecycle store lock is poisoned, so whether it knows the id is itself unknown; `503` = lifecycle store unavailable), `HTTP 000`, or an empty or non-JSON body | the runner died or could not act between the probe and the call. Nothing is established | **UNKNOWN** — say so, and continue to Step 2 |
| `200` with **no `coord` key at all** | the running runner build predates Phase 1. Its mark may or may not ever reach coord | treat `queued` as **UNKNOWN** and continue to Step 2. To confirm: `gitSha` from `GET /health`, then `git -C qontinui-runner merge-base --is-ancestor <the plan's Phase 1 commit> <gitSha>` after a fetch. Exit 128 means the sha is absent from your clone, which is UNKNOWN too |

**An absent `finishSynced` is not `true`.** A build carrying Phase 1 always
returns the key in this response; only an older build (or an on-disk record)
omits it, and a record with no `finishSynced` key reads as not synced.

**Run this once.** A second finish POST that carries a reason lands in
`reason_only`. If another caller (e.g. `/rewind-session`) already sent it, read
that answer rather than sending again.

**Read back once.** `coord.queued: true` means the write is queued, not that it
landed. A `PermanentFailure` at coord drops the outbox row and leaves
`finishSynced` false for good. So, **for a finish only**, re-issue the same POST
with `-d '{}'`, which carries no reason. Never send it after an `--undo`: an
absent `finished` means `true`, so it would finish the session again. It answers `changed: "none"` and the current
`session.finishSynced`. `true` means coord acknowledged the write. `false` with
`coord.queued: true` is the expected first answer (the runner re-queued the owed
write): apply the table's wait-and-re-read row. `false` after that one re-read
means not yet, or never: continue to Step 2 for the coord half rather than
reporting coord told. Do not send a reason on the
read-back. That is the `reason_only` rewrite above. **For `--undo` there is no
runner read-back.** Confirm coord's half with the coord
read-back at the end of this file (`session_status` no longer `finished`), and
run Step 2's `working` transition when it still reads `finished`.

### Step 2 — Native MCP tool `coord_report_status` (probe: tool present)

If `coord_report_status` is in this session's tool set, call it with
`session_status: "finished"` **and `claude_code_session_id` set to the id from
Part A**. (Load it via `ToolSearch` if it is a deferred tool name.) Tenant and
device derive server-side.

**A finish is progress-only.** Omit `work_unit_id`, `correlation_topic` and
`status_text`, because a finish is not an intent and names no work unit. That
holds on a coord build carrying the progress-only shape (plan
`2026-09-20-the-one-finish-door-that-names-its-target-in-the-path-is-disowned-by-both-closeout-commands`
Phase 2). If the call is refused `missing required argument`, re-issue it with
all three, naming what this session was actually working on, never a
placeholder. An invented work unit pollutes the peer-visible work ledger.

- **Probe:** the tool exists. **unknown / method-not-found** = masked → fall to
  Step 3, do not stop.
- **`--undo` runs here too, as a transition to `working`.** Coord's
  `SessionStatus` is `Working | Blocked | Stalled | WaitingHuman | Finished`
  (`qontinui-coord` `crates/coord/src/sessions.rs`) — there is **no "unset"
  value**, so no call can erase a status. `session_status: "working"` is
  therefore what an undo means: it takes the session off the terminal work axis
  and, because `Finished` is one of the two statuses that GATE coord-originated
  prompting, it is what restores gate continuations, next-step dispatch and
  directed messages to a session that was wrongly finished.

  Say that plainly when you report an undo: **it is a transition, not an
  erasure.** Coord keeps the status history, and the status the session held
  before it was finished is not recoverable by this command — if it was
  `blocked` or `waiting_human`, re-declare that explicitly afterwards.

  > This is what makes step 0's subagent recovery real. A `finished` written
  > from a subagent silences every coord-originated prompt into the PARENT; the
  > `working` transition is the call that lifts that. Step 1's
  > `{"finished": false}` lifts the runner's local mark as well, and queues coord's
  > half only when its body says `coord.queued: true`.

### Step 3 — Auto-discovered loopback proxy, same tool (probe: `tools/list` → HTTP 200)

A runner-provisioned `.mcp.json` points coord-mcp at a loopback proxy. **Two
header shapes are live** — `Authorization: Bearer <nonce>` and the legacy
`X-Coord-Mcp-Proxy-Key: <nonce>` — and both coexist on disk indefinitely, so read
either. A nonce written as `${QONTINUI_COORD_MCP_NONCE_<K>:-<nonce>}` is an env reference
(plan `2026-09-22-one-coord-mcp-nonce-per-terminal-so-the-terminal-leg-engages`):
expand it from your own environment before use - the variable when set and
non-empty, else the default - with `mcp_expand_env_ref` from
`<session-workdir>/.claude/skills/coord-revive/_scripts/lib/mcp-env-ref.sh`, and never send the literal text as a bearer. Only the
allowlisted `QONTINUI_COORD_MCP_(NONCE|CREDENTIAL)[_<K>]` names are ever read from
the environment - any other name reads as unset - so a sibling `.mcp.json` from a
cloned repo cannot pull a `${GH_TOKEN}` into a header it sends elsewhere.

The workspace-root `.mcp.json` is routinely stale while a **sibling repo's** is
live, so **probe every candidate and use the first whose `tools/list` returns
HTTP 200**. Resolve the workspace root with `git rev-parse --git-common-dir`,
never `--show-toplevel` — the latter names the worktree inside a linked worktree.

Then JSON-RPC `tools/call` `coord_report_status` with the same arguments as Step 2.

**Three rungs is the whole cascade.** Steps 2 and 3 carry the SAME tool to the
SAME server over different transports, and share one ownership predicate. Step 1
is the only rung independent of both: a different door, path-addressed, on the
runner's credential. That matters for how you read a failure — see Step 4 below.

The history, because the previous version of this file said the opposite. On
2026-09-05 the runner route did not exist on any build, and this file deleted
the rung
(plan `2026-09-03-a-finished-session-cannot-record-that-it-finished`). It landed
on 2026-09-10 as `qontinui-runner` `44870ff25` (#1271, a coord fast-forward land
that GitHub shows as `CLOSED`, not merged). The file kept denying it for three
weeks. Up to that runner's Phase 1 it wrote the local mark and told coord only
for AI-plane sessions, with no word in the response about which. The plan above
makes it coord-reaching and self-describing on a runner build carrying its
Phase 1, and put it back here first. On an older build the table's
"no `coord` key" row applies.

### Step 4 — Honest failure, and it has two shapes (never a silent no-op)

Name **every** rung you probed and distinguish "not attempted" from
"unavailable". A Step 1 that answered `changed: "marker"` with `coord.queued: false` is
neither: say
*"resume set corrected locally; coord NOT told"* beside the coord-half failure
below. That is a half-finish, and it is reported as one, never as finished and
never as nothing. Do not report the session finished. If a `.coord-mcp-status`
breadcrumb is present, quote it **with its age** — a stamp older than 30 minutes
is UNKNOWN, not a fault and not health.

Then say **which** of these two you hit. They look alike in a report and have
different remedies:

- **A transport floor** — no coord door answered. The MCP tool is masked and every
  loopback proxy candidate failed its `tools/list` probe, or a call came back
  `"Command failed with no output"` (a dead cached transport: the write is
  presumed **LOST**). Remedy: **`bash <session-workdir>/.claude/skills/coord-revive/coord-revive.sh --floor-claim`** from
  the real cwd (substitute `<session-workdir>` as in Step 1a's ownership check;
  left unsubstituted it also prints `No such file or directory`) — paste its
  `FLOOR-CLAIM:` block verbatim into the report,
  re-issue over the door it reports LIVE, verify by read. The floor is
  reportable ONLY as that block reading `verdict=FLOOR`; a `verdict=UNKNOWN`
  (exit 5: sampled under this box's own load, or an unreadable load) is
  reported as UNKNOWN with its reason, never as a floor.
- **An authorization verdict** — a door ANSWERED, and refused. Since
  `qontinui-coord#2052` the refusal is one of two strings, and they mean
  different things:

  - **`caller_owns_no_active_session`** — absence. Your device + tenant own no
    active `coord.sessions` row at all; coord was never told this session
    exists.
  - **`session_not_owned_by_caller`** — you own rows, and this id resolved to
    none of them. Often the row is simply **unbound**: its
    `claude_code_session_id` is still NULL, which makes it invisible to the
    resolver. (Before `#2052` this string also covered absence and was glossed
    as "no active session row owned by this device"; on a build predating
    `#2052` it still carries no diagnostic weight.)

  Either refusal is served policy `coordination`
  `report-status-must-name-its-own-session` working correctly — it is what stops
  an unscoped write landing on a peer's row — so **never** "fix" it by dropping
  the id.

  Re-running Steps 2-3 cannot help: Step 3 carries the same tool to the same
  server, so per `coordination`
  `briefing-mandated-door-that-answers-disabled` it "can only produce the same
  sentence more slowly". **For a runner-hosted session, Step 1 is the door that
  does not use this predicate.** If you have not run it, run it now. A
  `coord.queued: true` there, read back `finishSynced: true`, settles the coord
  half with no bind. **Otherwise the remedy is Part A step 1b, by host.** A
  runner-hosted session does NOT create a row: its terminal state is
  `runner-hosted, own row unbound`, reported with the refusal's reason and Step
  1's answer quoted (step 1b's runner-hosted arm). An operator-launched session
  binds its own row with `coord_bind_self_session{create_if_absent: true}`,
  reads it back, and re-issues the write; only when that bind is itself
  refused, or no door carries it, record a finding naming the session id, the
  device and the bind's refusal, and report the session **not** finished.

---

## Read back before you report

A returned success is necessary, not sufficient. Confirm by read:

- **coord — the authoritative read-back.** `GET
  /coord/sessions/work-status?claude_code_session_ids=<id>` (a
  device-authed read). Check `statuses[<id>].session_status == "finished"` **and
  that `coord_session_id` is the row you meant** — the harness id is not unique,
  so the door reports which row answered.

- **runner — the local half, when Step 1 carried a FINISH.** The `-d '{}'`
  re-POST that Step 1's read-back describes. Never after an `--undo`, where it
  would re-finish the session. Its `session.finishSynced` says whether coord
  acknowledged, and its `changed: "none"` says the local mark holds.

> **`GET http://127.0.0.1:9876/sessions/history` is not a read-back of coord.**
> On a runner build carrying `44870ff25` its rows carry `finished`, `finishedAt`
> and `finishReason`, so it shows the LOCAL mark. It carries no `finishSynced`,
> so it cannot say whether coord was told. On an older build those keys are
> absent from every row. An absent key is UNKNOWN, not `false`. Reading it as
> "not finished" is the `silent-empty-is-unknown` misreading with a 200 attached.

Three answers that are NOT "not finished", and must never be reported as such:
`unknown` (coord resolved no session), `invalid` (the id was not a UUID), and
`sessionBridgeColumnPresent: false` (the join could not be expressed on that
database). Each is UNKNOWN — say so.

A **"Command failed with no output"** from an MCP call is a dead cached
transport, not a masked tool: the write is presumed **LOST**. Run
**`/coord-revive`**, re-issue over the door it reports LIVE, and verify by read.

---

## Report

One line, then the evidence:

> **Session finished** — `<id>` · carried by <rung> · read back <where>.

State: the session id you actually wrote to, the step that carried it (Step 1
runner-local, Step 2 native tool, or Step 3 loopback proxy), and the reason
recorded. For Step 1, also give `coord.queued`, `coordSessionId` and the
read-back `finishSynced`. When Step 1 corrected the resume set but coord's half
came from Step 2 or 3, name both. **Coord is the store other sessions read**,
and its read-back is what verifies the coord half.

If no rung landed, say so and say which of Step 4's shapes you hit — a
transport floor, an authorization verdict, or the runner-hosted `own row unbound`
terminal state. Either is a complete, honest
outcome; neither is a session you may report finished.

Never report a session finished on the strength of a request you issued but did
not read back.
