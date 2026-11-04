The overlord's four doors are IN, added WITH coord's grant rather than after a
session hits `-32601` — the order `coord_memory_supersede.md` (this directory)
argues for. coord's grant (qontinui-coord#2913 for the proof reads,
qontinui-coord#2957 for the ledger pair) puts all four on the device floor
(`DEVICE_DEFAULT_TOOLS`), each
with a `DoorAdmits::DeviceAgent` HTTP twin, so withholding them here would be
a transport asymmetry rather than a boundary:

* proof reads (plan
  `2026-10-03-overlord-never-finishes-a-session-on-an-unproven-claim`) —
  `coord_watch_verdict` (twin `GET /coord/overlord/watch-verdict`)
  and `coord_session_obligations` (twin
  `GET /coord/overlord/session-obligations/:claude_code_session_id`). These are
  what lets `/next-move` PROVE a session owes nothing before it calls that
  session finished; a session that cannot read them can only assume, and an
  assumed "nothing owed" is precisely the false finish the plan exists to end.
  Both are reads and carry no authority a session lacks.
* the intervention ledger (plan
  `2026-10-01-overlord-session-supervisor-classifies-stops-and-absorbs-avoidable-escalations`
  Phase 2) — `coord_overlord_record` (twin
  `POST /coord/overlord/interventions`) and `coord_overlord_interventions`
  (twin `GET` on the same route), `/next-move`'s store-of-record. The write is
  an append-only, self-attributed row about the caller's own intervention, the
  same shape as `coord_post_finding`: it records what an overlord did, and
  performs nothing.

* the triage-pass ledger (plan
  `2026-10-23-the-overlord-keeps-no-record-of-session-flow-so-its-development-runs-on-anecdote`
  Phase 2b) — `coord_overlord_pass_record` (twin `POST /coord/overlord/passes`),
  the store-of-record for one `/triage-sessions` pass and its per-terminal
  rows, so session FLOW (time-to-close, nudge→progress) is measured rather
  than recalled. Like `coord_overlord_record` it is an append-only,
  self-attributed write that performs nothing. Its reads
  (`coord_query_overlord_passes`, `coord_query_overlord_flow`) need no entry:
  they ride the `coord_query_` read-family prefix.

Plan 2026-10-23-the-overlord-keeps-no-record-of-session-flow-so-its-development-runs-on-anecdote
Phase 2b: the triage-PASS ledger write (twin `POST /coord/overlord/passes`).
An append-only, self-attributed record of one /triage-sessions pass and its
per-terminal rows; it records what the overlord observed and did, and
performs nothing. Its two reads (`coord_query_overlord_passes`,
`coord_query_overlord_flow`) ride the `coord_query_` prefix (`../allowed-tool-prefixes/coord_query_.md`).
