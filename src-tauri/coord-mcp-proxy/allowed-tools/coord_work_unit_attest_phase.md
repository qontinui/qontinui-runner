Plan 2026-09-20-coord-delivery-has-no-verb-for-a-phase-that-ships-no-pr
Phase 4 (qontinui-coord#2616): attest a phase that ships no PR. It cannot
forge `shipped` — coord's I1+I2 mean an attestation never creates delivery
nor demotes; it only releases a phase-coverage pin on a unit that already
has a real landed PR, disclosed as a `PHASE ATTESTED WITHOUT A PR` gap.
