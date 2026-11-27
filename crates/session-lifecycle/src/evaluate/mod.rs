//! The pure evaluators `decide` reads its axes from (architecture §2.1, §2.4):
//! the registry record, the transcript facts (W with provenance, S including the
//! schedule rules, P, R), custody over git outputs (K, and G7's predicate from
//! `scripts/session-closeable.sh`), and `restore_class`.
//!
//! Every evaluator takes the READ and returns a typed answer; none performs I/O.
//! An unreadable input is `Unknown`, never a default (§2.1).

pub mod custody;
pub mod registry;
pub mod restore;
pub mod transcript;

pub use custody::{
    attribute_dirty_path, custody_axis, custody_of_tree, Attribution, CoordTreeRef, DirtyPath,
    PatchLanded, PeerSlot, Probe, ResidueVerdict, SlotRead, StatusProbe, TreeProbe, UpstreamProbe,
};
pub use registry::{
    activity_from, input_from, ComposerWitness, FunnelDraft, RegStatus, RegistryRecord,
    StopPayload, TurnState,
};
pub use restore::{
    boot_promotes, derive_authority, restore_class, ApprovalProof, Authority, BootPromotion,
    ManifestRow, RestoreClass, SessionClosedRecord, StopVerdict,
};
pub use transcript::{
    declared_work, declaring_uses, finish_stamp, parked_from, revived, selfconts_from, Arrivals,
    CoordWork, DeclareTool, DeclaringUse, FinishEvidence, InstanceBounds, RecordKind, Scope,
    ToolUse, TranscriptRecord, TurnOrigin,
};
